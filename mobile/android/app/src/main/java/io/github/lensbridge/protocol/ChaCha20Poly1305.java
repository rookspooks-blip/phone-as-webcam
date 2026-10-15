package io.github.lensbridge.protocol;

import java.security.MessageDigest;
import java.util.Arrays;

/**
 * ChaCha20-Poly1305 AEAD (RFC 8439), pure Java.
 *
 * <p>Android only ships a ChaCha20-Poly1305 Cipher from API 28; we support API 26,
 * so we carry our own small, test-vector-checked implementation.
 */
public final class ChaCha20Poly1305 {
    public static final int TAG_LEN = 16;
    private final int[] key = new int[8];

    public ChaCha20Poly1305(byte[] key32) {
        if (key32.length != 32) throw new IllegalArgumentException("key must be 32 bytes");
        for (int i = 0; i < 8; i++) key[i] = le32(key32, i * 4);
    }

    public byte[] seal(byte[] nonce12, byte[] plaintext, byte[] aad) {
        byte[] out = new byte[plaintext.length + TAG_LEN];
        xorStream(nonce12, 1, plaintext, 0, plaintext.length, out, 0);
        byte[] tag = tag(nonce12, aad, out, plaintext.length);
        System.arraycopy(tag, 0, out, plaintext.length, TAG_LEN);
        return out;
    }

    /** Returns the plaintext, or null when authentication fails. */
    public byte[] open(byte[] nonce12, byte[] sealed, byte[] aad) {
        if (sealed.length < TAG_LEN) return null;
        int n = sealed.length - TAG_LEN;
        byte[] expected = tag(nonce12, aad, sealed, n);
        if (!MessageDigest.isEqual(expected, Arrays.copyOfRange(sealed, n, sealed.length))) return null;
        byte[] out = new byte[n];
        xorStream(nonce12, 1, sealed, 0, n, out, 0);
        return out;
    }

    // ------------------------------------------------------------------ ChaCha20

    private static int le32(byte[] b, int o) {
        return (b[o] & 0xff) | (b[o + 1] & 0xff) << 8 | (b[o + 2] & 0xff) << 16 | (b[o + 3] & 0xff) << 24;
    }

    private static void quarter(int[] x, int a, int b, int c, int d) {
        x[a] += x[b]; x[d] = Integer.rotateLeft(x[d] ^ x[a], 16);
        x[c] += x[d]; x[b] = Integer.rotateLeft(x[b] ^ x[c], 12);
        x[a] += x[b]; x[d] = Integer.rotateLeft(x[d] ^ x[a], 8);
        x[c] += x[d]; x[b] = Integer.rotateLeft(x[b] ^ x[c], 7);
    }

    private void block(byte[] nonce, int counter, byte[] out64) {
        int[] s = new int[16];
        s[0] = 0x61707865; s[1] = 0x3320646e; s[2] = 0x79622d32; s[3] = 0x6b206574;
        System.arraycopy(key, 0, s, 4, 8);
        s[12] = counter;
        s[13] = le32(nonce, 0); s[14] = le32(nonce, 4); s[15] = le32(nonce, 8);
        int[] x = s.clone();
        for (int i = 0; i < 10; i++) {
            quarter(x, 0, 4, 8, 12); quarter(x, 1, 5, 9, 13); quarter(x, 2, 6, 10, 14); quarter(x, 3, 7, 11, 15);
            quarter(x, 0, 5, 10, 15); quarter(x, 1, 6, 11, 12); quarter(x, 2, 7, 8, 13); quarter(x, 3, 4, 9, 14);
        }
        for (int i = 0; i < 16; i++) {
            int v = x[i] + s[i];
            out64[i * 4] = (byte) v; out64[i * 4 + 1] = (byte) (v >>> 8);
            out64[i * 4 + 2] = (byte) (v >>> 16); out64[i * 4 + 3] = (byte) (v >>> 24);
        }
    }

    private void xorStream(byte[] nonce, int counter, byte[] in, int inOff, int len, byte[] out, int outOff) {
        byte[] ks = new byte[64];
        for (int pos = 0; pos < len; pos += 64) {
            block(nonce, counter++, ks);
            int n = Math.min(64, len - pos);
            for (int i = 0; i < n; i++) out[outOff + pos + i] = (byte) (in[inOff + pos + i] ^ ks[i]);
        }
    }

    // ------------------------------------------------------------------ Poly1305

    private byte[] tag(byte[] nonce, byte[] aad, byte[] ct, int ctLen) {
        byte[] block0 = new byte[64];
        block(nonce, 0, block0);
        Poly1305 p = new Poly1305(Arrays.copyOf(block0, 32));
        p.update(aad, 0, aad.length);
        p.pad();
        p.update(ct, 0, ctLen);
        p.pad();
        byte[] lens = new byte[16];
        putLe64(lens, 0, aad.length);
        putLe64(lens, 8, ctLen);
        p.update(lens, 0, 16);
        return p.finish();
    }

    private static void putLe64(byte[] b, int o, long v) {
        for (int i = 0; i < 8; i++) b[o + i] = (byte) (v >>> (8 * i));
    }

    /** poly1305-donna, 26-bit limbs in longs. */
    static final class Poly1305 {
        private static final long M = 0x3ffffff;
        private final long r0, r1, r2, r3, r4, s1, s2, s3, s4;
        private final long pad0, pad1, pad2, pad3;
        private long h0, h1, h2, h3, h4;
        private final byte[] buf = new byte[16];
        private int bufLen;

        Poly1305(byte[] key32) {
            long t0 = u32(key32, 0), t1 = u32(key32, 4), t2 = u32(key32, 8), t3 = u32(key32, 12);
            r0 = t0 & 0x3ffffff;
            r1 = ((t0 >>> 26) | (t1 << 6)) & 0x3ffff03;
            r2 = ((t1 >>> 20) | (t2 << 12)) & 0x3ffc0ff;
            r3 = ((t2 >>> 14) | (t3 << 18)) & 0x3f03fff;
            r4 = (t3 >>> 8) & 0x00fffff;
            s1 = r1 * 5; s2 = r2 * 5; s3 = r3 * 5; s4 = r4 * 5;
            pad0 = u32(key32, 16); pad1 = u32(key32, 20); pad2 = u32(key32, 24); pad3 = u32(key32, 28);
        }

        private static long u32(byte[] b, int o) {
            return le32(b, o) & 0xffffffffL;
        }

        private void blocks(byte[] m, int o, long hibit) {
            long t0 = u32(m, o), t1 = u32(m, o + 4), t2 = u32(m, o + 8), t3 = u32(m, o + 12);
            h0 += t0 & M;
            h1 += ((t0 >>> 26) | (t1 << 6)) & M;
            h2 += ((t1 >>> 20) | (t2 << 12)) & M;
            h3 += ((t2 >>> 14) | (t3 << 18)) & M;
            h4 += (t3 >>> 8) | hibit;
            long d0 = h0 * r0 + h1 * s4 + h2 * s3 + h3 * s2 + h4 * s1;
            long d1 = h0 * r1 + h1 * r0 + h2 * s4 + h3 * s3 + h4 * s2;
            long d2 = h0 * r2 + h1 * r1 + h2 * r0 + h3 * s4 + h4 * s3;
            long d3 = h0 * r3 + h1 * r2 + h2 * r1 + h3 * r0 + h4 * s4;
            long d4 = h0 * r4 + h1 * r3 + h2 * r2 + h3 * r1 + h4 * r0;
            long c = d0 >>> 26; h0 = d0 & M;
            d1 += c; c = d1 >>> 26; h1 = d1 & M;
            d2 += c; c = d2 >>> 26; h2 = d2 & M;
            d3 += c; c = d3 >>> 26; h3 = d3 & M;
            d4 += c; c = d4 >>> 26; h4 = d4 & M;
            h0 += c * 5; c = h0 >>> 26; h0 &= M;
            h1 += c;
        }

        void update(byte[] m, int off, int len) {
            int i = 0;
            if (bufLen > 0) {
                int n = Math.min(16 - bufLen, len);
                System.arraycopy(m, off, buf, bufLen, n);
                bufLen += n;
                i = n;
                if (bufLen < 16) return;
                blocks(buf, 0, 1L << 24);
                bufLen = 0;
            }
            for (; i + 16 <= len; i += 16) blocks(m, off + i, 1L << 24);
            if (i < len) {
                System.arraycopy(m, off + i, buf, 0, len - i);
                bufLen = len - i;
            }
        }

        /** Zero-pads the buffered input to a 16-byte boundary (AEAD construction). */
        void pad() {
            if (bufLen == 0) return;
            Arrays.fill(buf, bufLen, 16, (byte) 0);
            blocks(buf, 0, 1L << 24);
            bufLen = 0;
        }

        byte[] finish() {
            if (bufLen > 0) {
                buf[bufLen] = 1;
                Arrays.fill(buf, bufLen + 1, 16, (byte) 0);
                blocks(buf, 0, 0);
                bufLen = 0;
            }
            long c = h1 >>> 26; h1 &= M;
            h2 += c; c = h2 >>> 26; h2 &= M;
            h3 += c; c = h3 >>> 26; h3 &= M;
            h4 += c; c = h4 >>> 26; h4 &= M;
            h0 += c * 5; c = h0 >>> 26; h0 &= M;
            h1 += c;

            long g0 = h0 + 5; c = g0 >>> 26; g0 &= M;
            long g1 = h1 + c; c = g1 >>> 26; g1 &= M;
            long g2 = h2 + c; c = g2 >>> 26; g2 &= M;
            long g3 = h3 + c; c = g3 >>> 26; g3 &= M;
            long g4 = h4 + c - (1L << 26);
            if (g4 >= 0) { h0 = g0; h1 = g1; h2 = g2; h3 = g3; h4 = g4; }

            long f0 = (h0 | (h1 << 26)) & 0xffffffffL;
            long f1 = ((h1 >>> 6) | (h2 << 20)) & 0xffffffffL;
            long f2 = ((h2 >>> 12) | (h3 << 14)) & 0xffffffffL;
            long f3 = ((h3 >>> 18) | (h4 << 8)) & 0xffffffffL;
            long f = f0 + pad0; f0 = f & 0xffffffffL;
            f = f1 + pad1 + (f >>> 32); f1 = f & 0xffffffffL;
            f = f2 + pad2 + (f >>> 32); f2 = f & 0xffffffffL;
            f = f3 + pad3 + (f >>> 32); f3 = f & 0xffffffffL;
            byte[] out = new byte[16];
            long[] w = {f0, f1, f2, f3};
            for (int i = 0; i < 4; i++) {
                out[i * 4] = (byte) w[i]; out[i * 4 + 1] = (byte) (w[i] >>> 8);
                out[i * 4 + 2] = (byte) (w[i] >>> 16); out[i * 4 + 3] = (byte) (w[i] >>> 24);
            }
            return out;
        }
    }
}
