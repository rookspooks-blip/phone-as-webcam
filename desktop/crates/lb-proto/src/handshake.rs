//! Cleartext handshake (PROTOCOL.md §4).
//!
//! client → server: "LBP1" | key_id[8] | nonce_c[16]
//! server → client: "LBP1" | status u8 | nonce_s[16]

use std::io::{Read, Write};

use crate::crypto::SessionKeys;
use crate::pairing::PairingKey;
use crate::{random_bytes, Error, Result, MAGIC};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeStatus {
    Ok,
    UnknownKey,
    Busy,
    Unsupported,
    Other(u8),
}

impl HandshakeStatus {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Ok,
            1 => Self::UnknownKey,
            2 => Self::Busy,
            3 => Self::Unsupported,
            x => Self::Other(x),
        }
    }

    pub fn as_u8(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::UnknownKey => 1,
            Self::Busy => 2,
            Self::Unsupported => 3,
            Self::Other(x) => x,
        }
    }
}

pub const CLIENT_HELLO_LEN: usize = 28;
pub const SERVER_HELLO_LEN: usize = 21;

/// Run the client side; returns the session keys on success.
pub fn client_handshake<S: Read + Write>(stream: &mut S, key: &PairingKey) -> Result<SessionKeys> {
    let mut nonce_c = [0u8; 16];
    random_bytes(&mut nonce_c);
    let mut hello = [0u8; CLIENT_HELLO_LEN];
    hello[..4].copy_from_slice(MAGIC);
    hello[4..12].copy_from_slice(&key.key_id());
    hello[12..].copy_from_slice(&nonce_c);
    stream.write_all(&hello)?;
    stream.flush()?;

    let mut resp = [0u8; SERVER_HELLO_LEN];
    stream.read_exact(&mut resp)?;
    if &resp[..4] != MAGIC {
        return Err(Error::BadMagic);
    }
    let status = HandshakeStatus::from_u8(resp[4]);
    if status != HandshakeStatus::Ok {
        return Err(Error::Rejected(status));
    }
    let mut nonce_s = [0u8; 16];
    nonce_s.copy_from_slice(&resp[5..]);
    Ok(SessionKeys::derive(key.bytes(), &nonce_c, &nonce_s))
}

/// Run the server side. `lookup` maps a key id to a paired key; `busy`
/// rejects the client when another session is active.
pub fn server_handshake<S: Read + Write>(
    stream: &mut S,
    lookup: impl Fn(&[u8; 8]) -> Option<PairingKey>,
    busy: bool,
) -> Result<SessionKeys> {
    let mut hello = [0u8; CLIENT_HELLO_LEN];
    stream.read_exact(&mut hello)?;
    if &hello[..4] != MAGIC {
        reply(stream, HandshakeStatus::Unsupported, &[0u8; 16])?;
        return Err(Error::BadMagic);
    }
    let mut key_id = [0u8; 8];
    key_id.copy_from_slice(&hello[4..12]);
    let mut nonce_c = [0u8; 16];
    nonce_c.copy_from_slice(&hello[12..]);

    let Some(key) = lookup(&key_id) else {
        reply(stream, HandshakeStatus::UnknownKey, &[0u8; 16])?;
        return Err(Error::Rejected(HandshakeStatus::UnknownKey));
    };
    if busy {
        reply(stream, HandshakeStatus::Busy, &[0u8; 16])?;
        return Err(Error::Rejected(HandshakeStatus::Busy));
    }
    let mut nonce_s = [0u8; 16];
    random_bytes(&mut nonce_s);
    reply(stream, HandshakeStatus::Ok, &nonce_s)?;
    Ok(SessionKeys::derive(key.bytes(), &nonce_c, &nonce_s))
}

fn reply<S: Write>(stream: &mut S, status: HandshakeStatus, nonce_s: &[u8; 16]) -> Result<()> {
    let mut resp = [0u8; SERVER_HELLO_LEN];
    resp[..4].copy_from_slice(MAGIC);
    resp[4] = status.as_u8();
    resp[5..].copy_from_slice(nonce_s);
    stream.write_all(&resp)?;
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    #[test]
    fn handshake_over_tcp_agrees_on_keys() {
        let key = PairingKey::generate();
        let server_key = key.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            server_handshake(&mut s, |id| (id == &server_key.key_id()).then(|| server_key.clone()), false).unwrap()
        });
        let mut c = TcpStream::connect(addr).unwrap();
        let ck = client_handshake(&mut c, &key).unwrap();
        let sk = srv.join().unwrap();
        assert_eq!(ck.c2s, sk.c2s);
        assert_eq!(ck.s2c, sk.s2c);
    }

    #[test]
    fn unknown_key_is_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            server_handshake(&mut s, |_| None, false)
        });
        let mut c = TcpStream::connect(addr).unwrap();
        let err = client_handshake(&mut c, &PairingKey::generate()).unwrap_err();
        assert!(matches!(err, Error::Rejected(HandshakeStatus::UnknownKey)));
        assert!(srv.join().unwrap().is_err());
    }
}
