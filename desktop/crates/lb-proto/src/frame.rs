//! Encrypted length-prefixed frames: `len u32 BE | ChaCha20-Poly1305(type u8 | payload)`.

use std::io::{Read, Write};

use crate::crypto::{CipherState, TAG_LEN};
use crate::{Error, Result, MAX_FRAME_LEN};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketType {
    Control,
    Video,
    Audio,
    Unknown(u8),
}

impl PacketType {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Control,
            1 => Self::Video,
            2 => Self::Audio,
            x => Self::Unknown(x),
        }
    }

    pub fn as_u8(self) -> u8 {
        match self {
            Self::Control => 0,
            Self::Video => 1,
            Self::Audio => 2,
            Self::Unknown(x) => x,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Packet {
    pub ty: PacketType,
    pub payload: Vec<u8>,
}

impl Packet {
    /// Parse a CONTROL payload as JSON.
    pub fn json(&self) -> Result<serde_json::Value> {
        serde_json::from_slice(&self.payload).map_err(|_| Error::Malformed("control payload is not JSON"))
    }
}

pub struct FrameWriter<W: Write> {
    inner: W,
    cipher: CipherState,
    buf: Vec<u8>,
}

impl<W: Write> FrameWriter<W> {
    pub fn new(inner: W, key: &[u8; 32]) -> Self {
        Self { inner, cipher: CipherState::new(key), buf: Vec::new() }
    }

    pub fn write_packet(&mut self, ty: PacketType, payload: &[u8]) -> Result<()> {
        self.buf.clear();
        self.buf.reserve(1 + payload.len());
        self.buf.push(ty.as_u8());
        self.buf.extend_from_slice(payload);
        let ct = self.cipher.seal(&self.buf);
        if ct.len() > MAX_FRAME_LEN {
            return Err(Error::FrameTooLarge(ct.len()));
        }
        // One write per frame: avoids a tiny header segment followed by the body.
        let mut out = Vec::with_capacity(4 + ct.len());
        out.extend_from_slice(&(ct.len() as u32).to_be_bytes());
        out.extend_from_slice(&ct);
        self.inner.write_all(&out)?;
        self.inner.flush()?;
        Ok(())
    }

    pub fn write_json(&mut self, value: &serde_json::Value) -> Result<()> {
        let bytes = serde_json::to_vec(value).expect("serializing a Value cannot fail");
        self.write_packet(PacketType::Control, &bytes)
    }

    pub fn get_ref(&self) -> &W {
        &self.inner
    }
}

pub struct FrameReader<R: Read> {
    inner: R,
    cipher: CipherState,
}

impl<R: Read> FrameReader<R> {
    pub fn new(inner: R, key: &[u8; 32]) -> Self {
        Self { inner, cipher: CipherState::new(key) }
    }

    pub fn read_packet(&mut self) -> Result<Packet> {
        let mut len = [0u8; 4];
        self.inner.read_exact(&mut len)?;
        let len = u32::from_be_bytes(len) as usize;
        if len > MAX_FRAME_LEN {
            return Err(Error::FrameTooLarge(len));
        }
        if len < 1 + TAG_LEN {
            return Err(Error::Malformed("frame shorter than type byte + tag"));
        }
        let mut ct = vec![0u8; len];
        self.inner.read_exact(&mut ct)?;
        let mut pt = self.cipher.open(&ct)?;
        let ty = PacketType::from_u8(pt[0]);
        pt.remove(0);
        Ok(Packet { ty, payload: pt })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionKeys;

    #[test]
    fn frames_roundtrip_over_buffer() {
        let keys = SessionKeys::derive(&[5u8; 32], &[6u8; 16], &[7u8; 16]);
        let mut w = FrameWriter::new(Vec::new(), &keys.s2c);
        w.write_json(&serde_json::json!({"t": "welcome", "proto": 1})).unwrap();
        w.write_packet(PacketType::Video, &[1, 2, 3]).unwrap();
        let bytes = w.get_ref().clone();

        let mut r = FrameReader::new(&bytes[..], &keys.s2c);
        let p = r.read_packet().unwrap();
        assert_eq!(p.ty, PacketType::Control);
        assert_eq!(p.json().unwrap()["t"], "welcome");
        let p = r.read_packet().unwrap();
        assert_eq!(p.ty, PacketType::Video);
        assert_eq!(p.payload, vec![1, 2, 3]);
    }

    #[test]
    fn tampered_frame_is_rejected() {
        let keys = SessionKeys::derive(&[5u8; 32], &[6u8; 16], &[7u8; 16]);
        let mut w = FrameWriter::new(Vec::new(), &keys.s2c);
        w.write_packet(PacketType::Audio, b"hello").unwrap();
        let mut bytes = w.get_ref().clone();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        let mut r = FrameReader::new(&bytes[..], &keys.s2c);
        assert!(matches!(r.read_packet(), Err(Error::Decrypt)));
    }
}
