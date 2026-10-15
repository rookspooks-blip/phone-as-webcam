//! 14-byte media header: `codec u8 | flags u8 | seq u32 BE | pts_us u64 BE`.

use crate::{Error, Result};

pub const CODEC_MJPEG: u8 = 1;
pub const CODEC_H264: u8 = 2;
pub const CODEC_PCM_S16LE: u8 = 16;

pub const FLAG_KEY: u8 = 1 << 0;
pub const FLAG_CONFIG: u8 = 1 << 1;
pub const FLAG_DISCONT: u8 = 1 << 2;

pub const MEDIA_HEADER_LEN: usize = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaHeader {
    pub codec: u8,
    pub flags: u8,
    pub seq: u32,
    pub pts_us: u64,
}

impl MediaHeader {
    pub fn encode_with(&self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(MEDIA_HEADER_LEN + data.len());
        out.push(self.codec);
        out.push(self.flags);
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.pts_us.to_be_bytes());
        out.extend_from_slice(data);
        out
    }

    pub fn parse(payload: &[u8]) -> Result<(MediaHeader, &[u8])> {
        if payload.len() < MEDIA_HEADER_LEN {
            return Err(Error::Malformed("media payload shorter than header"));
        }
        let h = MediaHeader {
            codec: payload[0],
            flags: payload[1],
            seq: u32::from_be_bytes(payload[2..6].try_into().unwrap()),
            pts_us: u64::from_be_bytes(payload[6..14].try_into().unwrap()),
        };
        Ok((h, &payload[MEDIA_HEADER_LEN..]))
    }

    pub fn is_key(&self) -> bool {
        self.flags & FLAG_KEY != 0
    }

    pub fn is_discont(&self) -> bool {
        self.flags & FLAG_DISCONT != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let h = MediaHeader { codec: CODEC_H264, flags: FLAG_KEY | FLAG_CONFIG, seq: 0x01020304, pts_us: 0x1122334455667788 };
        let bytes = h.encode_with(&[9, 9]);
        assert_eq!(&bytes[..14], &[2, 3, 1, 2, 3, 4, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
        let (p, data) = MediaHeader::parse(&bytes).unwrap();
        assert_eq!(p, h);
        assert_eq!(data, &[9, 9]);
    }
}
