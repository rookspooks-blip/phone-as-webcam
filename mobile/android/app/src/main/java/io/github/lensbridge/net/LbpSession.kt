package io.github.lensbridge.net

import io.github.lensbridge.protocol.CipherState
import io.github.lensbridge.protocol.Lbp
import io.github.lensbridge.protocol.SessionKeys
import io.github.lensbridge.protocol.hex
import org.json.JSONObject
import java.io.BufferedOutputStream
import java.io.DataInputStream
import java.io.IOException
import java.net.Socket
import java.security.SecureRandom
import java.util.UUID
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import kotlin.concurrent.thread

/** One client connection: handshake, encrypted frames, send backpressure. */
class LbpSession(
    private val socket: Socket,
    private val lookup: (ByteArray) -> ByteArray?,
    private val isBusy: (ByteArray, LbpSession) -> Boolean,
    private val listener: Listener,
) {
    interface Listener {
        fun onHello(session: LbpSession, hello: JSONObject)
        fun onControl(session: LbpSession, msg: JSONObject)
        fun onClosed(session: LbpSession, reason: String)
    }

    val id: UUID = UUID.randomUUID()
    @Volatile var clientName = "ПК"
        private set
    @Volatile var keyId = ByteArray(0)
        private set
    val peer: String get() = socket.inetAddress?.hostAddress ?: "?"

    private val closed = AtomicBoolean(false)
    private val sender = Executors.newSingleThreadExecutor { r -> Thread(r, "lbp-send").apply { isDaemon = true } }
    private val pending = AtomicLong(0)
    private val sentWindow = AtomicLong(0)
    @Volatile private var sendCipher: CipherState? = null
    private val out = BufferedOutputStream(socket.getOutputStream(), 64 * 1024)

    fun start() {
        thread(name = "lbp-session", isDaemon = true) {
            val reason = try {
                socket.tcpNoDelay = true
                socket.soTimeout = 6000
                socket.sendBufferSize = 256 * 1024
                run()
                "соединение закрыто"
            } catch (e: IOException) {
                e.message ?: "сеть"
            } catch (e: Exception) {
                "ошибка: $e"
            }
            close(reason)
        }
    }

    private fun run() {
        val input = DataInputStream(socket.getInputStream().buffered(64 * 1024))
        val hello = ByteArray(Lbp.CLIENT_HELLO_LEN)
        input.readFully(hello)
        if (!hello.copyOfRange(0, 4).contentEquals(Lbp.MAGIC)) {
            writeRaw(Lbp.serverHello(Lbp.STATUS_UNSUPPORTED, null))
            throw IOException("неизвестный протокол")
        }
        val kid = hello.copyOfRange(4, 12)
        val nonceC = hello.copyOfRange(12, 28)
        keyId = kid
        val psk = lookup(kid)
        if (psk == null) {
            writeRaw(Lbp.serverHello(Lbp.STATUS_UNKNOWN_KEY, null))
            throw IOException("ПК не сопряжён (ключ ${kid.hex()})")
        }
        if (isBusy(kid, this)) {
            writeRaw(Lbp.serverHello(Lbp.STATUS_BUSY, null))
            throw IOException("занято другим ПК")
        }
        val nonceS = ByteArray(16).also { SecureRandom().nextBytes(it) }
        writeRaw(Lbp.serverHello(Lbp.STATUS_OK, nonceS))
        val keys = SessionKeys.derive(psk, nonceC, nonceS)
        sendCipher = CipherState(keys.s2c)
        val recv = CipherState(keys.c2s)

        var gotHello = false
        while (!closed.get()) {
            val len = input.readInt()
            if (len < 17 || len > Lbp.MAX_FRAME) throw IOException("неверная длина кадра")
            val body = ByteArray(len)
            input.readFully(body)
            val pt = recv.open(body) ?: throw IOException("ошибка расшифровки")
            if (pt[0].toInt() != Lbp.TYPE_CONTROL) continue
            val msg = try {
                JSONObject(String(pt, 1, pt.size - 1, Charsets.UTF_8))
            } catch (e: Exception) {
                continue
            }
            if (!gotHello) {
                if (msg.optString("t") != "hello") throw IOException("первое сообщение не hello")
                gotHello = true
                clientName = msg.optString("name", "ПК")
                listener.onHello(this, msg)
            } else {
                listener.onControl(this, msg)
            }
        }
    }

    private fun writeRaw(b: ByteArray) {
        synchronized(out) {
            out.write(b)
            out.flush()
        }
    }

    val queuedBytes: Long get() = pending.get()

    fun takeSentBytes(): Long = sentWindow.getAndSet(0)

    fun sendJson(obj: JSONObject) = enqueue(Lbp.TYPE_CONTROL, obj.toString().toByteArray(Charsets.UTF_8))

    /** False (nothing sent) when droppable and more than [limit] bytes are already queued. */
    fun sendMedia(type: Int, payload: ByteArray, droppable: Boolean, limit: Long): Boolean {
        if (droppable && pending.get() > limit) return false
        enqueue(type, payload)
        return true
    }

    private fun enqueue(type: Int, payload: ByteArray) {
        if (closed.get()) return
        pending.addAndGet(payload.size.toLong())
        try {
            sender.execute {
                try {
                    val cipher = sendCipher ?: return@execute
                    val frame = Lbp.frame(type, payload, cipher)
                    synchronized(out) {
                        out.write(frame)
                        out.flush()
                    }
                    sentWindow.addAndGet(frame.size.toLong())
                } catch (e: IOException) {
                    close("ошибка отправки: ${e.message}")
                } finally {
                    pending.addAndGet(-payload.size.toLong())
                }
            }
        } catch (e: java.util.concurrent.RejectedExecutionException) {
            pending.addAndGet(-payload.size.toLong())
        }
    }

    fun close(reason: String) {
        if (!closed.compareAndSet(false, true)) return
        try {
            socket.close()
        } catch (_: IOException) {
        }
        sender.shutdownNow()
        listener.onClosed(this, reason)
    }
}
