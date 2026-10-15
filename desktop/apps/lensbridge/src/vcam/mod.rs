//! Virtual camera output. Windows: shared memory read by the "Lensbridge
//! Camera" DirectShow filter (drivers/windows/dshow-filter).

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::{filter_installed, install_filter, VcamWriter};

#[cfg(not(windows))]
mod unsupported;
#[cfg(not(windows))]
pub use self::unsupported::{filter_installed, install_filter, VcamWriter};

/// Shared-memory layout constants (keep in sync with drivers/windows/common/lb_vcam_shm.h).
#[allow(dead_code)]
pub mod layout {
    pub const MAGIC: u32 = 0x4356_424C;
    pub const VERSION: u32 = 1;
    pub const MAX_WIDTH: usize = 1920;
    pub const MAX_HEIGHT: usize = 1080;
    pub const FORMAT_I420: u32 = 1;
    pub const HEADER_SIZE: usize = 64;
    pub const SLOT_HEADER_SIZE: usize = 64;
    pub const SLOT_SIZE: usize = SLOT_HEADER_SIZE + MAX_WIDTH * MAX_HEIGHT * 3 / 2;
    pub const TOTAL_SIZE: usize = HEADER_SIZE + 2 * SLOT_SIZE;

    // Header field offsets.
    pub const OFF_MAGIC: usize = 0;
    pub const OFF_VERSION: usize = 4;
    pub const OFF_MAX_WIDTH: usize = 8;
    pub const OFF_MAX_HEIGHT: usize = 12;
    pub const OFF_SLOT_SIZE: usize = 16;
    pub const OFF_FORMAT: usize = 20;
    pub const OFF_LATEST_SLOT: usize = 24;
    pub const OFF_HEARTBEAT: usize = 32;
    // Slot field offsets (relative to the slot start).
    pub const SLOT_OFF_SEQ: usize = 0;
    pub const SLOT_OFF_WIDTH: usize = 8;
    pub const SLOT_OFF_HEIGHT: usize = 12;
    pub const SLOT_OFF_FRAME_ID: usize = 16;
    pub const SLOT_OFF_PTS: usize = 24;
}

use lb_client::VideoFrame;

/// Produces the I420 picture written to the virtual camera: optional mirror
/// and nearest-neighbour downscale to fit 1920x1080.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn prepare_i420(frame: &VideoFrame, mirror: bool) -> (usize, usize, Vec<u8>) {
    let (sw, sh) = (frame.width, frame.height);
    let scale = f64::max(sw as f64 / layout::MAX_WIDTH as f64, sh as f64 / layout::MAX_HEIGHT as f64).max(1.0);
    let dw = ((sw as f64 / scale) as usize) & !1;
    let dh = ((sh as f64 / scale) as usize) & !1;
    if !mirror && dw == sw && dh == sh && sw % 2 == 0 && sh % 2 == 0 {
        return (sw, sh, frame.i420.clone());
    }
    let (y, u, v) = frame.planes();
    let scw = sw.div_ceil(2);
    let (dcw, dch) = (dw / 2, dh / 2);
    let mut out = vec![0u8; dw * dh + 2 * dcw * dch];
    let map_x = |x: usize, dwid: usize, swid: usize| {
        let sx = x * swid / dwid;
        if mirror {
            swid - 1 - sx
        } else {
            sx
        }
    };
    for row in 0..dh {
        let sy = row * sh / dh;
        for col in 0..dw {
            out[row * dw + col] = y[sy * sw + map_x(col, dw, sw)];
        }
    }
    let (uo, vo) = (dw * dh, dw * dh + dcw * dch);
    for row in 0..dch {
        let sy = (row * 2 * sh / dh) / 2;
        for col in 0..dcw {
            let sx = map_x(col * 2, dw, sw) / 2;
            out[uo + row * dcw + col] = u[sy * scw + sx];
            out[vo + row * dcw + col] = v[sy * scw + sx];
        }
    }
    (dw, dh, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize) -> VideoFrame {
        let mut i420 = vec![0u8; w * h + 2 * (w / 2) * (h / 2)];
        for (i, p) in i420.iter_mut().enumerate().take(w * h) {
            *p = (i % w) as u8;
        }
        VideoFrame { width: w, height: h, i420, pts_us: 0, seq: 0, recv_us: 0 }
    }

    #[test]
    fn passthrough_and_mirror() {
        let f = frame(8, 4);
        let (w, h, d) = prepare_i420(&f, false);
        assert_eq!((w, h), (8, 4));
        assert_eq!(d, f.i420);
        let (_, _, m) = prepare_i420(&f, true);
        assert_eq!(&m[..8], &[7, 6, 5, 4, 3, 2, 1, 0]);
    }

    #[test]
    fn downscales_4k_to_fit() {
        let f = frame(3840, 2160);
        let (w, h, d) = prepare_i420(&f, false);
        assert_eq!((w, h), (1920, 1080));
        assert_eq!(d.len(), 1920 * 1080 * 3 / 2);
    }
}
