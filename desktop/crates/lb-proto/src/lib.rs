//! Lensbridge wire protocol `lbp/1` (see `docs/PROTOCOL.md`).
//!
//! Blocking, transport-agnostic: everything works over any `Read + Write`
//! stream (TCP socket, usbmux tunnel, in-memory pipe in tests).

pub mod crypto;
pub mod frame;
pub mod handshake;
pub mod media;
pub mod pairing;

pub use crypto::{Direction, SessionKeys};
pub use frame::{FrameReader, FrameWriter, Packet, PacketType};
pub use handshake::{client_handshake, server_handshake, HandshakeStatus};
pub use media::{MediaHeader, CODEC_H264, CODEC_MJPEG, CODEC_PCM_S16LE, FLAG_CONFIG, FLAG_DISCONT, FLAG_KEY};
pub use pairing::PairingKey;

/// Main protocol port (encrypted `lbp/1`).
pub const DEFAULT_PORT: u16 = 4748;
/// Optional plain HTTP port (`/video` MJPEG) on the phone.
pub const HTTP_PORT: u16 = 4747;
/// Bonjour / mDNS service type.
pub const SERVICE_TYPE: &str = "_lensbridge._tcp.local.";
/// Protocol version carried in `hello`/`welcome`.
pub const PROTO_VERSION: u32 = 1;
/// Handshake magic.
pub const MAGIC: &[u8; 4] = b"LBP1";
/// Upper bound for one encrypted frame (ciphertext incl. tag).
pub const MAX_FRAME_LEN: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("bad handshake magic")]
    BadMagic,
    #[error("server rejected handshake: {0:?}")]
    Rejected(HandshakeStatus),
    #[error("frame too large: {0} bytes")]
    FrameTooLarge(usize),
    #[error("decryption failed (wrong key or corrupted stream)")]
    Decrypt,
    #[error("malformed packet: {0}")]
    Malformed(&'static str),
    #[error("invalid pairing URI: {0}")]
    BadPairingUri(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Fill `buf` from the OS CSPRNG (thread-local ChaCha reseeded from the OS).
pub fn random_bytes(buf: &mut [u8]) {
    rand::fill(buf);
}
