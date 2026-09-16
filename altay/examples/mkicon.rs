// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Dev utility: square + downscale `assets/Altay.png` to a 256x256 themed icon.
//! Run with: `cargo run --example mkicon`

use image::imageops::FilterType;
use image::RgbaImage;

fn main() {
    let src = image::open("assets/Altay.png").expect("assets/Altay.png source logo").to_rgba8();
    let (w, h) = (src.width(), src.height());
    let scale = 256.0 / w.max(h) as f32;
    let (nw, nh) = ((w as f32 * scale).round() as u32, (h as f32 * scale).round() as u32);
    let resized = image::imageops::resize(&src, nw, nh, FilterType::Lanczos3);

    let mut canvas = RgbaImage::new(256, 256);
    image::imageops::overlay(&mut canvas, &resized, ((256 - nw) / 2) as i64, ((256 - nh) / 2) as i64);
    canvas.save("assets/altay.png").expect("write assets/altay.png");
    println!("wrote assets/altay.png (256x256, squared from {w}x{h})");
}
