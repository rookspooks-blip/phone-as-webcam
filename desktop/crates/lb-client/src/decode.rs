//! H.264 decoding (OpenH264, software, portable) and pixel conversions.

use openh264::decoder::Decoder;
use openh264::formats::YUVSource;

/// One decoded picture in tightly packed I420 (Y plane, then U, then V).
#[derive(Debug, Clone)]
pub struct VideoFrame {
    pub width: usize,
    pub height: usize,
    pub i420: Vec<u8>,
    /// Capture time in the phone clock.
    pub pts_us: u64,
    pub seq: u32,
    /// Local receive time (`crate::now_us`).
    pub recv_us: u64,
}

impl VideoFrame {
    pub fn chroma_size(&self) -> (usize, usize) {
        (self.width.div_ceil(2), self.height.div_ceil(2))
    }

    pub fn planes(&self) -> (&[u8], &[u8], &[u8]) {
        let ys = self.width * self.height;
        let (cw, ch) = self.chroma_size();
        let cs = cw * ch;
        (&self.i420[..ys], &self.i420[ys..ys + cs], &self.i420[ys + cs..ys + 2 * cs])
    }

    /// RGBA8 for the preview. BT.709 limited range (what phone encoders emit for HD).
    pub fn to_rgba(&self, mirror: bool) -> Vec<u8> {
        self.to_rgba_scaled(usize::MAX, mirror).2
    }

    /// RGBA8 downscaled (nearest neighbour) so the width is at most `max_width`;
    /// returns `(width, height, pixels)`. Keeps preview conversion cheap at 1080p60.
    pub fn to_rgba_scaled(&self, max_width: usize, mirror: bool) -> (usize, usize, Vec<u8>) {
        let (sw, sh) = (self.width, self.height);
        let step = sw.div_ceil(max_width.max(1)).max(1);
        let (w, h) = (sw / step, sh / step);
        let (y_p, u_p, v_p) = self.planes();
        let cw = self.chroma_size().0;
        let mut out = vec![0u8; w * h * 4];
        for row in 0..h {
            let sy = row * step;
            let crow = sy / 2;
            for col in 0..w {
                let sx = col * step;
                let src_col = if mirror { sw - 1 - sx } else { sx };
                let y = y_p[sy * sw + src_col] as i32;
                let u = u_p[crow * cw + src_col / 2] as i32;
                let v = v_p[crow * cw + src_col / 2] as i32;
                let (r, g, b) = yuv_to_rgb(y, u, v);
                let o = (row * w + col) * 4;
                out[o] = r;
                out[o + 1] = g;
                out[o + 2] = b;
                out[o + 3] = 255;
            }
        }
        (w, h, out)
    }
}

#[inline]
pub fn yuv_to_rgb(y: i32, u: i32, v: i32) -> (u8, u8, u8) {
    let c = 298 * (y - 16);
    let d = u - 128;
    let e = v - 128;
    let r = (c + 459 * e + 128) >> 8;
    let g = (c - 55 * d - 136 * e + 128) >> 8;
    let b = (c + 541 * d + 128) >> 8;
    (r.clamp(0, 255) as u8, g.clamp(0, 255) as u8, b.clamp(0, 255) as u8)
}

pub struct H264Decoder {
    inner: Decoder,
}

impl H264Decoder {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            inner: Decoder::new().map_err(|e| e.to_string())?,
        })
    }

    /// Decodes one Annex B access unit; returns `(width, height, i420)` when a
    /// picture is ready.
    pub fn decode(&mut self, au: &[u8]) -> Result<Option<(usize, usize, Vec<u8>)>, String> {
        let Some(yuv) = self.inner.decode(au).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        let (w, h) = yuv.dimensions();
        let (sy, su, sv) = yuv.strides();
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let mut out = Vec::with_capacity(w * h + 2 * cw * ch);
        for r in 0..h {
            out.extend_from_slice(&yuv.y()[r * sy..r * sy + w]);
        }
        for r in 0..ch {
            out.extend_from_slice(&yuv.u()[r * su..r * su + cw]);
        }
        for r in 0..ch {
            out.extend_from_slice(&yuv.v()[r * sv..r * sv + cw]);
        }
        Ok(Some((w, h, out)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grey_and_primaries() {
        assert_eq!(yuv_to_rgb(16, 128, 128), (0, 0, 0));
        assert_eq!(yuv_to_rgb(235, 128, 128), (255, 255, 255));
        let (r, g, b) = yuv_to_rgb(63, 102, 240); // BT.709 red
        assert!(r > 240 && g < 15 && b < 15, "{r} {g} {b}");
    }

    #[test]
    fn mirror_flips_columns() {
        let f = VideoFrame {
            width: 2,
            height: 2,
            i420: vec![16, 235, 16, 235, 128, 128],
            pts_us: 0,
            seq: 0,
            recv_us: 0,
        };
        let normal = f.to_rgba(false);
        let mirrored = f.to_rgba(true);
        assert_eq!(normal[0], 0);
        assert_eq!(mirrored[0], 255);
    }

    #[test]
    fn scaled_preview_halves_1080p() {
        let f = VideoFrame {
            width: 1920,
            height: 1080,
            i420: vec![128; 1920 * 1080 * 3 / 2],
            pts_us: 0,
            seq: 0,
            recv_us: 0,
        };
        let (w, h, px) = f.to_rgba_scaled(960, false);
        assert_eq!((w, h), (960, 540));
        assert_eq!(px.len(), 960 * 540 * 4);
        let (w2, _, _) = f.to_rgba_scaled(1280, false);
        assert_eq!(w2, 960); // integer step 2
    }
}
