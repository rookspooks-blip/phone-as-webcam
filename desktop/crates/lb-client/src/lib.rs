//! Lensbridge desktop client core: connects to a phone (Wi-Fi, iOS USB via
//! usbmuxd, Android USB via adb), runs the `lbp/1` session with automatic
//! reconnection, decodes H.264 and hands out the newest frame and audio.

pub mod adb;
#[cfg(feature = "audio")]
pub mod audio_out;
pub mod clock;
pub mod decode;
pub mod frame_slot;
pub mod session;
pub mod transport;
pub mod usbmux;

pub use decode::VideoFrame;
pub use frame_slot::LatestFrame;
pub use session::{spawn, AudioChunk, ClientConfig, ClientHandle, ClientStats, ConnState, Event};
pub use transport::Target;

use std::sync::OnceLock;
use std::time::Instant;

/// Monotonic client clock in microseconds (process-relative).
pub fn now_us() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_micros() as u64
}
