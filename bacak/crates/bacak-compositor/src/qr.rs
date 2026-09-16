//! QR code rasterisation for the Uzakel pairing panel
//! (`plugins/uzakel.rs`) — encodes a pairing URI into an RGBA bitmap the
//! renderer can hand straight to `MemoryRenderBuffer::from_slice` the same
//! way `icons.rs` does for app icons (see its module doc: `Abgr8888` means
//! `[R, G, B, A]` byte order in this codebase).
//!
//! We rasterise the module grid ourselves rather than pulling in the
//! `qrcode` crate's own `image`-backed renderer: `bacak-compositor` already
//! depends on `image` for a different pixel format, and a QR code is just a
//! 2-colour grid — a dozen lines of our own scaling code avoids a second,
//! redundant image-encoding path.

#![cfg(feature = "runtime")]

use qrcode::{Color, EcLevel, QrCode};

/// Quiet zone width in modules on every side — the QR spec requires at
/// least 4 for reliable scanning; phone cameras in particular are picky
/// about this when the code is small on-screen.
const QUIET_ZONE: usize = 4;

/// Encodes `data` as a QR code and rasterises it to an RGBA8888 buffer
/// (`[R, G, B, A]` per pixel — see the module doc), each module drawn as a
/// `module_px`-pixel square, dark modules on a white background. Returns
/// `None` if `data` doesn't fit any QR version (practically never, for the
/// short pairing URIs this is used for).
pub fn generate_qr_rgba(data: &str, module_px: u32) -> Option<(Vec<u8>, u32, u32)> {
    // Medium error correction: enough to survive a scuffed phone-screen
    // photo of the desktop screen, without bloating the module count for
    // what's already a short URI.
    let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::M).ok()?;
    let modules = code.width();
    let colors = code.to_colors();
    debug_assert_eq!(colors.len(), modules * modules);

    let side_modules = modules + 2 * QUIET_ZONE;
    let side_px = side_modules as u32 * module_px;
    let mut rgba = vec![255u8; (side_px as usize) * (side_px as usize) * 4];

    for my in 0..modules {
        for mx in 0..modules {
            if colors[my * modules + mx] != Color::Dark {
                continue;
            }
            let px0 = (mx + QUIET_ZONE) as u32 * module_px;
            let py0 = (my + QUIET_ZONE) as u32 * module_px;
            for dy in 0..module_px {
                let row = ((py0 + dy) * side_px + px0) as usize * 4;
                for dx in 0..module_px as usize {
                    let i = row + dx * 4;
                    rgba[i] = 0;
                    rgba[i + 1] = 0;
                    rgba[i + 2] = 0;
                    // alpha (rgba[i + 3]) stays 255 — opaque black module.
                }
            }
        }
    }

    Some((rgba, side_px, side_px))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_pairing_uri_to_a_square_rgba_buffer() {
        let uri = "uzakel://pair?host=192.168.1.23&port=45922&pin=123456&name=BacakOS";
        let (rgba, w, h) = generate_qr_rgba(uri, 6).expect("short URI must encode");
        assert_eq!(w, h, "a QR code is always square");
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        // At least one opaque black pixel (a dark module) and one white
        // background pixel must exist, or this isn't a real QR bitmap.
        let mut saw_dark = false;
        let mut saw_light = false;
        for px in rgba.chunks_exact(4) {
            if px == [0, 0, 0, 255] {
                saw_dark = true;
            } else if px == [255, 255, 255, 255] {
                saw_light = true;
            }
        }
        assert!(saw_dark && saw_light);
    }
}
