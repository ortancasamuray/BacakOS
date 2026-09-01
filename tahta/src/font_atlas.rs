//! CPU-rasterized real-font atlas — used by `textbox.rs` in place of the
//! hand-rolled 5x7 dot-matrix font (`font5x7.rs` stays as-is for the
//! toast/address-bar labels, which are out of scope here).
//!
//! Built once at startup (`FontAtlas::new`): rasterizes every character
//! the text box's virtual keyboard can produce via `fontdue` into a
//! single R8 coverage bitmap, shelf-packed into one atlas. `renderer.rs`
//! uploads that bitmap to the GPU once and never touches it again;
//! `push_text` (CPU-side, no GPU access) is called every frame to lay
//! out glyph quads, mirroring `font5x7::push_text`'s call shape so
//! textbox.rs barely changes.

use std::collections::HashMap;

use glam::Vec2;

/// Embedded, not loaded from the system at runtime — tahta ships this in
/// its own binary/.deb rather than depending on `fonts-noto-core` being
/// installed. SIL Open Font License 1.1, see `assets/fonts/NOTO-LICENSE.txt`.
const FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/NotoSans-Regular.ttf");

/// Pixel size glyphs are rasterized at — text is always drawn scaled
/// down from this, so it stays comfortably above the text box's biggest
/// on-screen use (the ~30px display line) to look crisp.
const RASTER_PX: f32 = 64.0;

/// Every character `textbox.rs`'s virtual keyboard can produce (see its
/// `ROWS` and `font5x7::to_lower_tr`).
const CHARSET: &str = " 0123456789ABCÇDEFGĞHIİJKLMNOÖPQRSŞTUÜVWXYZabcçdefgğhıijklmnoöpqrsştuüvwxyz";

const ATLAS_WIDTH: u32 = 512;

/// fontdue's rasterized coverage is linear — blended straight over a
/// dark UI, thin stems and small text read washed-out/patchy. Same fix
/// `bacak-compositor/src/text.rs` uses: a gamma LUT below 1 lifts low/mid
/// coverage so edges stay crisp. Built once, not per-frame.
const COVERAGE_GAMMA: f32 = 0.72;

struct Glyph {
    uv_min: Vec2,
    uv_max: Vec2,
    /// Rasterized size and baseline-relative top-left bearing, in
    /// `RASTER_PX`-space pixels (fontdue's `xmin`/`ymin`).
    size: Vec2,
    bearing: Vec2,
    advance: f32,
}

pub struct FontAtlas {
    pub pixels: Vec<u8>, // R8, row-major, `width` x `height`.
    pub width: u32,
    pub height: u32,
    glyphs: HashMap<char, Glyph>,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GlyphVertex {
    pub position: [f32; 2],
    pub uv: [f32; 2],
    pub color: [f32; 4],
}

impl GlyphVertex {
    pub const ATTRIBS: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4];

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<GlyphVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

impl FontAtlas {
    pub fn new() -> Self {
        let font = fontdue::Font::from_bytes(FONT_BYTES, fontdue::FontSettings::default())
            .expect("bundled NotoSans-Regular.ttf failed to parse");

        let gamma_lut: [u8; 256] = {
            let mut lut = [0u8; 256];
            for (i, slot) in lut.iter_mut().enumerate() {
                let v = (i as f32 / 255.0).powf(COVERAGE_GAMMA);
                *slot = (v * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            lut
        };

        struct Rasterized {
            ch: char,
            metrics: fontdue::Metrics,
            bitmap: Vec<u8>,
        }
        let rasterized: Vec<Rasterized> = CHARSET
            .chars()
            .map(|ch| {
                let (metrics, bitmap) = font.rasterize(ch, RASTER_PX);
                Rasterized { ch, metrics, bitmap }
            })
            .collect();

        // Shelf-pack: left-to-right with a 1px gutter, wrapping to a new
        // row when a glyph wouldn't fit, tracking the tallest glyph seen
        // in the current row so the next row starts below it.
        let mut cursor_x = 0u32;
        let mut cursor_y = 0u32;
        let mut row_height = 0u32;
        let mut placements = Vec::with_capacity(rasterized.len());
        for r in &rasterized {
            let w = r.metrics.width as u32;
            let h = r.metrics.height as u32;
            if cursor_x + w > ATLAS_WIDTH {
                cursor_x = 0;
                cursor_y += row_height + 1;
                row_height = 0;
            }
            placements.push((cursor_x, cursor_y));
            cursor_x += w + 1;
            row_height = row_height.max(h);
        }
        let atlas_height = (cursor_y + row_height + 1).max(1);

        let mut pixels = vec![0u8; (ATLAS_WIDTH * atlas_height) as usize];
        let mut glyphs = HashMap::with_capacity(rasterized.len());
        for (r, &(px, py)) in rasterized.iter().zip(&placements) {
            let w = r.metrics.width;
            let h = r.metrics.height;
            for row in 0..h {
                let src = &r.bitmap[row * w..row * w + w];
                let dst_start = (py as usize + row) * ATLAS_WIDTH as usize + px as usize;
                for (i, &coverage) in src.iter().enumerate() {
                    pixels[dst_start + i] = gamma_lut[coverage as usize];
                }
            }
            glyphs.insert(
                r.ch,
                Glyph {
                    uv_min: Vec2::new(px as f32 / ATLAS_WIDTH as f32, py as f32 / atlas_height as f32),
                    uv_max: Vec2::new((px + w as u32) as f32 / ATLAS_WIDTH as f32, (py + h as u32) as f32 / atlas_height as f32),
                    size: Vec2::new(w as f32, h as f32),
                    bearing: Vec2::new(r.metrics.xmin as f32, r.metrics.ymin as f32),
                    advance: r.metrics.advance_width,
                },
            );
        }

        Self { pixels, width: ATLAS_WIDTH, height: atlas_height, glyphs }
    }

    /// Draws `text` left-to-right so its rasterized cap-height maps onto
    /// `pixel_height`, with `top_left.y + pixel_height` as the baseline —
    /// same `(text, top_left, size, color, out_vertices, out_indices) ->
    /// advance` call shape as `font5x7::push_text` so call sites read the
    /// same way. Unsupported characters (not in `CHARSET`) advance by
    /// half `pixel_height` as a blank gap, matching `font5x7`'s fallback.
    pub fn push_text(&self, text: &str, top_left: Vec2, pixel_height: f32, color: [f32; 4], out_vertices: &mut Vec<GlyphVertex>, out_indices: &mut Vec<u32>) -> f32 {
        let scale = pixel_height / RASTER_PX;
        let mut cursor_x = top_left.x;
        let baseline_y = top_left.y + pixel_height;
        for ch in text.chars() {
            let Some(glyph) = self.glyphs.get(&ch) else {
                cursor_x += pixel_height * 0.5;
                continue;
            };
            if glyph.size.x > 0.0 && glyph.size.y > 0.0 {
                let gx = cursor_x + glyph.bearing.x * scale;
                let gy = baseline_y - (glyph.bearing.y + glyph.size.y) * scale;
                let gw = glyph.size.x * scale;
                let gh = glyph.size.y * scale;
                let base = out_vertices.len() as u32;
                out_vertices.push(GlyphVertex { position: [gx, gy], uv: [glyph.uv_min.x, glyph.uv_min.y], color });
                out_vertices.push(GlyphVertex { position: [gx, gy + gh], uv: [glyph.uv_min.x, glyph.uv_max.y], color });
                out_vertices.push(GlyphVertex { position: [gx + gw, gy], uv: [glyph.uv_max.x, glyph.uv_min.y], color });
                out_vertices.push(GlyphVertex { position: [gx + gw, gy + gh], uv: [glyph.uv_max.x, glyph.uv_max.y], color });
                out_indices.extend_from_slice(&[base, base + 1, base + 2, base + 2, base + 1, base + 3]);
            }
            cursor_x += glyph.advance * scale;
        }
        cursor_x - top_left.x
    }

    /// Total horizontal advance `push_text` would use for `text`, without
    /// generating any geometry — for callers that need to know the width
    /// up front (e.g. centering).
    pub fn measure(&self, text: &str, pixel_height: f32) -> f32 {
        let scale = pixel_height / RASTER_PX;
        text.chars()
            .map(|ch| match self.glyphs.get(&ch) {
                Some(glyph) => glyph.advance * scale,
                None => pixel_height * 0.5,
            })
            .sum()
    }
}
