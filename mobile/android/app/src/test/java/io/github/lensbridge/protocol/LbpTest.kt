package io.github.lensbridge.protocol

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import java.util.Base64

class LbpTest {
    @Test
    fun knownVectorThroughKotlinApi() {
        val keys = SessionKeys.derive(ByteArray(32) { 0x11 }, ByteArray(16) { 0x22 }, ByteArray(16) { 0x33 })
        assertEquals("02dc43b962c49ee48a3e6b7c9405d4b945db4bb5e8d4ef08335798fb12b33a74", keys.c2s.hex())
        val ct = CipherState(keys.c2s).seal(byteArrayOf(0x00, 0x7b, 0x7d))
        assertEquals("91a3a868663ddef2ed4d030bf67d25d7186a21", ct.hex())
    }

    @Test
    fun frameRoundtrip() {
        val keys = SessionKeys.derive(ByteArray(32) { 7 }, ByteArray(16) { 1 }, ByteArray(16) { 2 })
        val f = Lbp.frame(Lbp.TYPE_VIDEO, byteArrayOf(1, 2, 3), CipherState(keys.s2c))
        val len = ((f[0].toInt() and 0xff) shl 24) or ((f[1].toInt() and 0xff) shl 16) or ((f[2].toInt() and 0xff) shl 8) or (f[3].toInt() and 0xff)
        assertEquals(f.size - 4, len)
        val pt = CipherState(keys.s2c).open(f.copyOfRange(4, f.size))!!
        assertArrayEquals(byteArrayOf(1, 1, 2, 3), pt)
    }

    @Test
    fun mediaHeaderLayout() {
        val m = Lbp.media(Lbp.CODEC_H264, Lbp.FLAG_KEY or Lbp.FLAG_CONFIG, 0x01020304, 0x1122334455667788, byteArrayOf(9, 9))
        assertArrayEquals(byteArrayOf(2, 3, 1, 2, 3, 4, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88.toByte(), 9, 9), m)
    }

    @Test
    fun pairingUri() {
        val key = ByteArray(32) { it.toByte() }
        val k = Base64.getUrlEncoder().withoutPadding().encodeToString(key)
        val p = PairingUri.parse("lensbridge://pair?v=1&k=$k&n=%D0%9C%D0%BE%D0%B9%20%D0%9F%D0%9A")!!
        assertArrayEquals(key, p.key)
        assertEquals("Мой ПК", p.pcName)
        assertNull(PairingUri.parse("lensbridge://pair?v=2&k=$k"))
        assertNull(PairingUri.parse("https://example.com"))
        assertNull(PairingUri.parse("lensbridge://pair?v=1&k=AAAA"))
    }
}
