//! Session key derivation (HKDF-SHA256) and per-direction ChaCha20-Poly1305.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;

use crate::{Error, Result};

pub const TAG_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    ClientToServer,
    ServerToClient,
}

impl Direction {
    fn info(self) -> &'static [u8] {
        match self {
            Direction::ClientToServer => b"lbp1 c2s",
            Direction::ServerToClient => b"lbp1 s2c",
        }
    }
}

/// Both directional keys of one session.
#[derive(Clone)]
pub struct SessionKeys {
    pub c2s: [u8; 32],
    pub s2c: [u8; 32],
}

impl std::fmt::Debug for SessionKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionKeys(..)")
    }
}

impl SessionKeys {
    pub fn derive(psk: &[u8; 32], nonce_c: &[u8; 16], nonce_s: &[u8; 16]) -> Self {
        let mut salt = [0u8; 32];
        salt[..16].copy_from_slice(nonce_c);
        salt[16..].copy_from_slice(nonce_s);
        let hk = Hkdf::<Sha256>::new(Some(&salt), psk);
        let mut c2s = [0u8; 32];
        let mut s2c = [0u8; 32];
        hk.expand(Direction::ClientToServer.info(), &mut c2s).expect("32 bytes is a valid HKDF length");
        hk.expand(Direction::ServerToClient.info(), &mut s2c).expect("32 bytes is a valid HKDF length");
        Self { c2s, s2c }
    }

    pub fn key(&self, dir: Direction) -> &[u8; 32] {
        match dir {
            Direction::ClientToServer => &self.c2s,
            Direction::ServerToClient => &self.s2c,
        }
    }
}

/// Stateful AEAD for one direction: nonce = 0u32 || counter u64 BE.
pub struct CipherState {
    cipher: ChaCha20Poly1305,
    counter: u64,
}

impl CipherState {
    pub fn new(key: &[u8; 32]) -> Self {
        Self { cipher: ChaCha20Poly1305::new_from_slice(key).expect("32-byte key"), counter: 0 }
    }

    fn next_nonce(&mut self) -> Nonce {
        let mut n = [0u8; 12];
        n[4..].copy_from_slice(&self.counter.to_be_bytes());
        self.counter = self.counter.checked_add(1).expect("nonce counter exhausted");
        Nonce::from(n)
    }

    pub fn seal(&mut self, plaintext: &[u8]) -> Vec<u8> {
        let nonce = self.next_nonce();
        self.cipher.encrypt(&nonce, plaintext).expect("encryption cannot fail for in-memory buffers")
    }

    pub fn open(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let nonce = self.next_nonce();
        self.cipher.decrypt(&nonce, ciphertext).map_err(|_| Error::Decrypt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_counter_binding() {
        let keys = SessionKeys::derive(&[7u8; 32], &[1u8; 16], &[2u8; 16]);
        assert_ne!(keys.c2s, keys.s2c);
        let mut tx = CipherState::new(&keys.c2s);
        let mut rx = CipherState::new(&keys.c2s);
        let a = tx.seal(b"first");
        let b = tx.seal(b"second");
        // Out-of-order delivery must fail (counter mismatch).
        let mut rx2 = CipherState::new(&keys.c2s);
        assert!(rx2.open(&b).is_err());
        assert_eq!(rx.open(&a).unwrap(), b"first");
        assert_eq!(rx.open(&b).unwrap(), b"second");
    }

    #[test]
    fn wrong_direction_key_fails() {
        let keys = SessionKeys::derive(&[9u8; 32], &[3u8; 16], &[4u8; 16]);
        let ct = CipherState::new(&keys.c2s).seal(b"x");
        assert!(CipherState::new(&keys.s2c).open(&ct).is_err());
    }

    /// Fixed vector shared with the Swift implementation (PROTOCOL.md §4/§5):
    /// K = 0x11×32, nonce_c = 0x22×16, nonce_s = 0x33×16, first c2s frame plaintext = 00 7b 7d.
    #[test]
    fn known_vector() {
        let keys = SessionKeys::derive(&[0x11; 32], &[0x22; 16], &[0x33; 16]);
        let ct = CipherState::new(&keys.c2s).seal(b"\x00{}");
        println!("c2s={} s2c={} ct={}", hex(&keys.c2s), hex(&keys.s2c), hex(&ct));
        assert_eq!(hex(&keys.c2s), C2S);
        assert_eq!(hex(&keys.s2c), S2C);
        assert_eq!(hex(&ct), CT);
    }

    const C2S: &str = "02dc43b962c49ee48a3e6b7c9405d4b945db4bb5e8d4ef08335798fb12b33a74";
    const S2C: &str = "29516e9412d6237340adba5144441e0f168b470a002b45da13899d9d1daffc1e";
    const CT: &str = "91a3a868663ddef2ed4d030bf67d25d7186a21";

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
}
