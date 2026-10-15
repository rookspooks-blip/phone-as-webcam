package io.github.lensbridge.protocol;

import java.security.GeneralSecurityException;

import javax.crypto.Mac;
import javax.crypto.spec.SecretKeySpec;

/** HKDF-SHA256 (RFC 5869), output up to 32 bytes (one block) as needed by lbp/1. */
public final class Hkdf {
    private Hkdf() {}

    public static byte[] sha256(byte[] ikm, byte[] salt, byte[] info, int length) {
        if (length > 32) throw new IllegalArgumentException("lbp/1 only needs one HKDF block");
        try {
            Mac mac = Mac.getInstance("HmacSHA256");
            mac.init(new SecretKeySpec(salt.length == 0 ? new byte[32] : salt, "HmacSHA256"));
            byte[] prk = mac.doFinal(ikm);
            mac.init(new SecretKeySpec(prk, "HmacSHA256"));
            mac.update(info);
            mac.update((byte) 1);
            byte[] t = mac.doFinal();
            byte[] out = new byte[length];
            System.arraycopy(t, 0, out, 0, length);
            return out;
        } catch (GeneralSecurityException e) {
            throw new IllegalStateException(e);
        }
    }
}
