package io.github.lensbridge.protocol

import java.net.URLDecoder
import java.nio.ByteBuffer
import java.security.MessageDigest
import java.util.Base64

/** Lensbridge protocol lbp/1 — see docs/PROTOCOL.md. */
object Lbp {
    const val PORT = 4748
    const val HTTP_PORT = 4747
    const val SERVICE_TYPE = "_lensbridge._tcp"
    const val PROTO = 1
    val MAGIC = "LBP1".toByteArray(Charsets.US_ASCII)
    const val MAX_FRAME = 8 * 1024 * 1024
    const val CLIENT_HELLO_LEN = 28
    const val SERVER_HELLO_LEN = 21

    const val TYPE_CONTROL = 0
    const val TYPE_VIDEO = 1
    const val TYPE_AUDIO = 2

    const val CODEC_H264 = 2
    const val CODEC_PCM = 16
    const val FLAG_KEY = 1
    const val FLAG_CONFIG = 2
    const val FLAG_DISCONT = 4

    const val STATUS_OK = 0
    const val STATUS_UNKNOWN_KEY = 1
    const val STATUS_BUSY = 2
    const val STATUS_UNSUPPORTED = 3

    fun keyId(key: ByteArray): ByteArray = MessageDigest.getInstance("SHA-256").digest(key).copyOf(8)

    fun serverHello(status: Int, nonceS: ByteArray?): ByteArray {
        val out = ByteArray(SERVER_HELLO_LEN)
        System.arraycopy(MAGIC, 0, out, 0, 4)
        out[4] = status.toByte()
        if (nonceS != null) System.arraycopy(nonceS, 0, out, 5, 16)
        return out
    }

    /** 14-byte media header + payload. */
    fun media(codec: Int, flags: Int, seq: Int, ptsUs: Long, payload: ByteArray): ByteArray {
        val b = ByteBuffer.allocate(14 + payload.size)
        b.put(codec.toByte()).put(flags.toByte()).putInt(seq).putLong(ptsUs).put(payload)
        return b.array()
    }

    /** `len u32 BE | seal(type | payload)`. */
    fun frame(type: Int, payload: ByteArray, cipher: CipherState): ByteArray {
        val pt = ByteArray(payload.size + 1)
        pt[0] = type.toByte()
        System.arraycopy(payload, 0, pt, 1, payload.size)
        val ct = cipher.seal(pt)
        return ByteBuffer.allocate(4 + ct.size).putInt(ct.size).put(ct).array()
    }
}

class SessionKeys(val c2s: ByteArray, val s2c: ByteArray) {
    companion object {
        fun derive(psk: ByteArray, nonceC: ByteArray, nonceS: ByteArray): SessionKeys {
            val salt = nonceC + nonceS
            return SessionKeys(
                Hkdf.sha256(psk, salt, "lbp1 c2s".toByteArray(), 32),
                Hkdf.sha256(psk, salt, "lbp1 s2c".toByteArray(), 32),
            )
        }
    }
}

/** One direction of ChaCha20-Poly1305 with nonce = 0u32 || counter u64 BE. */
class CipherState(key: ByteArray) {
    private val aead = ChaCha20Poly1305(key)
    private var counter = 0L
    private val empty = ByteArray(0)

    private fun nonce(): ByteArray {
        val n = ByteBuffer.allocate(12).putInt(0).putLong(counter).array()
        counter++
        return n
    }

    fun seal(plaintext: ByteArray): ByteArray = aead.seal(nonce(), plaintext, empty)

    /** Null when authentication fails. */
    fun open(sealed: ByteArray): ByteArray? = aead.open(nonce(), sealed, empty)
}

data class PairingUri(val key: ByteArray, val pcName: String) {
    companion object {
        /** `lensbridge://pair?v=1&k=<base64url(K)>&n=<name>` */
        fun parse(s: String): PairingUri? {
            val rest = s.trim().removePrefix("lensbridge://pair?")
            if (rest.length == s.trim().length) return null
            val params = rest.split('&').associate {
                val i = it.indexOf('=')
                if (i < 0) it to "" else it.substring(0, i) to it.substring(i + 1)
            }
            if (params["v"] != "1") return null
            val key = try {
                Base64.getUrlDecoder().decode(params["k"] ?: return null)
            } catch (e: IllegalArgumentException) {
                return null
            }
            if (key.size != 32) return null
            val name = params["n"]?.let { URLDecoder.decode(it, "UTF-8") } ?: "ПК"
            return PairingUri(key, name)
        }
    }

    override fun equals(other: Any?) = other is PairingUri && key.contentEquals(other.key) && pcName == other.pcName
    override fun hashCode() = key.contentHashCode() * 31 + pcName.hashCode()
}

fun ByteArray.hex(): String = joinToString("") { "%02x".format(it) }

/** Monotonic server clock (µs) shared by video, audio and pong. */
fun serverNowUs(): Long = System.nanoTime() / 1000
