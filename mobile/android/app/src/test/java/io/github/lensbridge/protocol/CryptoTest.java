package io.github.lensbridge.protocol;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;

import java.util.Random;

import javax.crypto.Cipher;
import javax.crypto.spec.IvParameterSpec;
import javax.crypto.spec.SecretKeySpec;

import org.junit.Test;

public class CryptoTest {
    static byte[] hex(String s) {
        s = s.replaceAll("[^0-9a-fA-F]", "");
        byte[] b = new byte[s.length() / 2];
        for (int i = 0; i < b.length; i++) b[i] = (byte) Integer.parseInt(s.substring(i * 2, i * 2 + 2), 16);
        return b;
    }

    static String toHex(byte[] b) {
        StringBuilder sb = new StringBuilder();
        for (byte x : b) sb.append(String.format("%02x", x));
        return sb.toString();
    }

    static byte[] fill(int v, int n) {
        byte[] b = new byte[n];
        java.util.Arrays.fill(b, (byte) v);
        return b;
    }

    /** RFC 8439 §2.8.2 AEAD test vector. */
    @Test
    public void rfc8439Aead() {
        byte[] key = hex("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f");
        byte[] nonce = hex("070000004041424344454647");
        byte[] aad = hex("50515253c0c1c2c3c4c5c6c7");
        byte[] pt = ("Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.")
                .getBytes(java.nio.charset.StandardCharsets.US_ASCII);
        byte[] sealed = new ChaCha20Poly1305(key).seal(nonce, pt, aad);
        String h = toHex(sealed);
        assertEquals("d31a8d34648e60db7b86afbc53ef7ec2", h.substring(0, 32));
        assertEquals("1ae10b594f09e26a7e902ecbd0600691", h.substring(h.length() - 32));
        assertArrayEquals(pt, new ChaCha20Poly1305(key).open(nonce, sealed, aad));
    }

    /** Same vector as desktop/crates/lb-proto (crypto.rs) and the iOS tests. */
    @Test
    public void lbpKnownVector() {
        byte[] salt = new byte[32];
        System.arraycopy(fill(0x22, 16), 0, salt, 0, 16);
        System.arraycopy(fill(0x33, 16), 0, salt, 16, 16);
        byte[] c2s = Hkdf.sha256(fill(0x11, 32), salt, "lbp1 c2s".getBytes(), 32);
        byte[] s2c = Hkdf.sha256(fill(0x11, 32), salt, "lbp1 s2c".getBytes(), 32);
        assertEquals("02dc43b962c49ee48a3e6b7c9405d4b945db4bb5e8d4ef08335798fb12b33a74", toHex(c2s));
        assertEquals("29516e9412d6237340adba5144441e0f168b470a002b45da13899d9d1daffc1e", toHex(s2c));
        byte[] ct = new ChaCha20Poly1305(c2s).seal(new byte[12], new byte[] {0x00, 0x7b, 0x7d}, new byte[0]);
        assertEquals("91a3a868663ddef2ed4d030bf67d25d7186a21", toHex(ct));
    }

    /** Cross-check against the JDK implementation for random sizes (incl. multi-block, partial blocks). */
    @Test
    public void matchesJdk() throws Exception {
        Random rnd = new Random(42);
        for (int len : new int[] {0, 1, 15, 16, 17, 63, 64, 65, 1000, 70000}) {
            byte[] key = new byte[32], nonce = new byte[12], pt = new byte[len], aad = new byte[len % 7];
            rnd.nextBytes(key); rnd.nextBytes(nonce); rnd.nextBytes(pt); rnd.nextBytes(aad);
            Cipher c = Cipher.getInstance("ChaCha20-Poly1305");
            c.init(Cipher.ENCRYPT_MODE, new SecretKeySpec(key, "ChaCha20"), new IvParameterSpec(nonce));
            c.updateAAD(aad);
            byte[] jdk = c.doFinal(pt);
            assertArrayEquals("len " + len, jdk, new ChaCha20Poly1305(key).seal(nonce, pt, aad));
        }
    }

    @Test
    public void tamperedIsRejected() {
        byte[] key = fill(5, 32);
        byte[] sealed = new ChaCha20Poly1305(key).seal(new byte[12], "hello".getBytes(), new byte[0]);
        sealed[0] ^= 1;
        assertNull(new ChaCha20Poly1305(key).open(new byte[12], sealed, new byte[0]));
    }
}
