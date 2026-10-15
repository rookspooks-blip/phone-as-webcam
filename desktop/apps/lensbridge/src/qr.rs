//! Renders the pairing URI as a QR code image.

use eframe::egui::{Color32, ColorImage};
use qrcode::{Color, EcLevel, QrCode};

pub fn render(text: &str, module_px: usize) -> Option<ColorImage> {
    let code = QrCode::with_error_correction_level(text.as_bytes(), EcLevel::M).ok()?;
    let n = code.width();
    let quiet = 4;
    let size = (n + 2 * quiet) * module_px;
    let mut img = ColorImage::new([size, size], vec![Color32::WHITE; size * size]);
    for (i, c) in code.to_colors().iter().enumerate() {
        if *c == Color::Dark {
            let (mx, my) = (i % n + quiet, i / n + quiet);
            for dy in 0..module_px {
                let row = (my * module_px + dy) * size;
                for dx in 0..module_px {
                    img.pixels[row + mx * module_px + dx] = Color32::BLACK;
                }
            }
        }
    }
    Some(img)
}

#[cfg(test)]
mod tests {
    #[test]
    fn renders_pairing_uri() {
        let uri = lb_proto::PairingKey::generate().to_uri("Мой ПК");
        let img = super::render(&uri, 4).expect("qr");
        assert!(img.size[0] > 100);
        assert_eq!(img.size[0], img.size[1]);
    }
}
