//! Pre-shared pairing key and the QR pairing URI (PROTOCOL.md §3):
//! `lensbridge://pair?v=1&k=<base64url(K)>&n=<urlencoded PC name>`.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};

use crate::{random_bytes, Error, Result};

#[derive(Clone, PartialEq, Eq)]
pub struct PairingKey([u8; 32]);

impl std::fmt::Debug for PairingKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print key material.
        write!(f, "PairingKey(id={})", self.key_id_hex())
    }
}

impl PairingKey {
    pub fn generate() -> Self {
        let mut k = [0u8; 32];
        random_bytes(&mut k);
        Self(k)
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// First 8 bytes of SHA-256(K): identifies the key without revealing it.
    pub fn key_id(&self) -> [u8; 8] {
        let digest = Sha256::digest(self.0);
        let mut id = [0u8; 8];
        id.copy_from_slice(&digest[..8]);
        id
    }

    pub fn key_id_hex(&self) -> String {
        self.key_id().iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn to_base64(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    pub fn from_base64(s: &str) -> Result<Self> {
        let raw = URL_SAFE_NO_PAD.decode(s.trim()).map_err(|_| Error::BadPairingUri("key is not base64url"))?;
        let bytes: [u8; 32] = raw.try_into().map_err(|_| Error::BadPairingUri("key must be 32 bytes"))?;
        Ok(Self(bytes))
    }

    pub fn to_uri(&self, pc_name: &str) -> String {
        format!("lensbridge://pair?v=1&k={}&n={}", self.to_base64(), percent_encode(pc_name))
    }

    /// Parse a pairing URI; returns the key and the PC name.
    pub fn parse_uri(uri: &str) -> Result<(Self, String)> {
        let rest = uri.trim().strip_prefix("lensbridge://pair?").ok_or(Error::BadPairingUri("wrong scheme"))?;
        let (mut v, mut k, mut n) = (None, None, String::new());
        for part in rest.split('&') {
            let (key, val) = part.split_once('=').unwrap_or((part, ""));
            match key {
                "v" => v = Some(val),
                "k" => k = Some(val),
                "n" => n = percent_decode(val)?,
                _ => {}
            }
        }
        if v != Some("1") {
            return Err(Error::BadPairingUri("unsupported version"));
        }
        let key = Self::from_base64(k.ok_or(Error::BadPairingUri("missing key"))?)?;
        Ok((key, n))
    }
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(s: &str) -> Result<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).map_err(|_| Error::BadPairingUri("bad escape"))?;
                out.push(u8::from_str_radix(hex, 16).map_err(|_| Error::BadPairingUri("bad escape"))?);
                i += 3;
            }
            b'%' => return Err(Error::BadPairingUri("truncated escape")),
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| Error::BadPairingUri("name is not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_roundtrip_with_unicode_name() {
        let k = PairingKey::generate();
        let uri = k.to_uri("Мой ПК & co");
        let (k2, name) = PairingKey::parse_uri(&uri).unwrap();
        assert_eq!(k, k2);
        assert_eq!(name, "Мой ПК & co");
    }

    #[test]
    fn rejects_bad_uris() {
        assert!(PairingKey::parse_uri("http://x").is_err());
        assert!(PairingKey::parse_uri("lensbridge://pair?v=2&k=AAAA").is_err());
        assert!(PairingKey::parse_uri("lensbridge://pair?v=1&k=AAAA").is_err());
        assert!(PairingKey::parse_uri("lensbridge://pair?v=1&k=x&n=%4").is_err());
    }

    #[test]
    fn key_id_is_stable() {
        let k = PairingKey::from_bytes([0x11; 32]);
        assert_eq!(k.key_id_hex().len(), 16);
        assert_eq!(k.key_id(), PairingKey::from_bytes([0x11; 32]).key_id());
    }
}
