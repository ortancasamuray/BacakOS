//! Minimal single-line text rasteriser for compositor overlays.
//!
//! This is deliberately tiny: the only consumer today is the alt+tab
//! task-switcher, which needs one short, truncated label per tile. No
//! shaping, no bidi, no wrapping — just left-to-right glyph advance
//! with a hard truncate when the line runs past `max_w`.
//!
//! Output is a tightly-packed `Abgr8888` buffer (`[R, G, B, A]` bytes
//! per pixel — that's what `GlesRenderer`'s `ImportMem` maps
//! `Fourcc::Abgr8888` to). Glyph coverage drives the alpha channel; the
//! RGB stays the caller-chosen constant so the label tints uniformly.
//!
//! Compiled only with the `runtime` feature (it pulls in `fontdue` and
//! is only ever drawn by the Smithay renderer).

#![cfg(feature = "runtime")]

use fontdue::{Font, FontSettings};

/// System font search path. Liberation Sans is the most reliably
/// present on Debian/Ubuntu (it's a base metric-compatible font);
/// the rest cover Arch / Fedora / minimal images. First hit wins.
const FONT_CANDIDATES: &[&str] = &[
    // DejaVu Sans first: unlike Liberation Sans it covers the keyboard symbol
    // glyphs (⇧ ↵ ⌫ arrows …) *and* full Turkish, so the whole UI — including
    // the on-screen keyboard — renders them instead of tofu boxes.
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/freefont/FreeSans.ttf",
];

/// Fonts with full coverage of the keyboard glyphs (⇧ ↵ ⌫ ⌦ ↹ arrows ☺ ☰ ✕
/// … ₺ €). Liberation Sans — the default UI font — lacks these, so they'd
/// render as "tofu" boxes; the on-screen keyboard uses one of these instead.
/// DejaVu Sans covers every glyph we draw; Noto/FreeSans are fallbacks.
const SYMBOL_FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/freefont/FreeSans.ttf",
];

/// A loaded UI font plus the one-line rasterise entry point.
pub struct TextRenderer {
    font: Font,
}

impl TextRenderer {
    /// Try every path in [`FONT_CANDIDATES`]. Returns `None` if no
    /// usable font is found — callers must treat text as optional
    /// (the switcher still draws colour-coded tiles without labels).
    pub fn load() -> Option<Self> {
        for path in FONT_CANDIDATES {
            let Ok(bytes) = std::fs::read(path) else { continue };
            match Font::from_bytes(bytes, FontSettings::default()) {
                Ok(font) => {
                    tracing::info!(font = path, "loaded UI font");
                    return Some(Self { font });
                }
                Err(err) => {
                    tracing::debug!(font = path, ?err, "font parse failed, trying next");
                }
            }
        }
        tracing::warn!("no UI font found — overlay labels disabled");
        None
    }

    /// Load a font with full keyboard-glyph coverage (see
    /// [`SYMBOL_FONT_CANDIDATES`]) for the on-screen keyboard. Falls back to
    /// the general UI font when none of the symbol fonts is installed (the OSK
    /// then shows tofu for a few control keys, but still works).
    pub fn load_symbol() -> Option<Self> {
        for path in SYMBOL_FONT_CANDIDATES {
            let Ok(bytes) = std::fs::read(path) else { continue };
            if let Ok(font) = Font::from_bytes(bytes, FontSettings::default()) {
                tracing::info!(font = path, "loaded OSK symbol font");
                return Some(Self { font });
            }
        }
        tracing::warn!("no symbol font found — OSK falls back to the UI font");
        Self::load()
    }

    /// Raw bytes of the first available UI font (see [`FONT_CANDIDATES`]), so
    /// other subsystems (e.g. the cosmic-text selection engine) can share the
    /// exact same face the overlays use. `None` if no system font is present.
    pub fn ui_font_bytes() -> Option<Vec<u8>> {
        FONT_CANDIDATES.iter().find_map(|p| std::fs::read(p).ok())
    }

    /// Construct directly from font bytes. Used by tests so they don't
    /// depend on a system font being installed.
    pub fn from_bytes(bytes: Vec<u8>) -> Option<Self> {
        Font::from_bytes(bytes, FontSettings::default())
            .ok()
            .map(|font| Self { font })
    }

    /// Rasterise one line at `px` height into an `Abgr8888` buffer.
    ///
    /// * `color` is the straight-alpha RGB the glyphs tint to.
    /// * `max_w` hard-caps the width; glyphs that would cross it are
    ///   dropped (no ellipsis — keeps the math trivial).
    ///
    /// Returns `(rgba, width, height)`, or `None` for empty input or a
    /// degenerate font with no line metrics.
    pub fn rasterize_line(
        &self,
        text: &str,
        px: f32,
        color: [u8; 3],
        max_w: usize,
    ) -> Option<(Vec<u8>, usize, usize)> {
        let trimmed = text.trim();
        if trimmed.is_empty() || max_w == 0 {
            return None;
        }

        let lm = self.font.horizontal_line_metrics(px)?;
        let ascent = lm.ascent.ceil() as i32;
        // `descent` is negative (below baseline). Total rows span the
        // full ascent + |descent| so descenders aren't clipped.
        let height = (lm.ascent - lm.descent).ceil().max(1.0) as usize;
        let baseline = ascent;

        // Decide the glyph sequence: the whole string if it fits, else
        // as many leading chars as fit *with room for an ellipsis*,
        // followed by '…'. Layout uses pen advance (matches the blit
        // loop below) so the two stay consistent.
        const ELLIPSIS: char = '\u{2026}';
        let chars: Vec<char> = trimmed.chars().collect();
        let advance = |c: char| self.font.metrics(c, px).advance_width;
        let full_adv: f32 = chars.iter().map(|&c| advance(c)).sum();

        let render_seq: Vec<char> = if full_adv <= max_w as f32 {
            chars
        } else {
            // Greedily fit leading chars, reserving the ellipsis width.
            let budget = (max_w as f32 - advance(ELLIPSIS)).max(0.0);
            let mut acc = 0.0_f32;
            let mut kept: Vec<char> = Vec::new();
            for &c in &chars {
                let a = advance(c);
                if acc + a > budget {
                    break;
                }
                acc += a;
                kept.push(c);
            }
            kept.push(ELLIPSIS);
            kept
        };

        // First pass: lay out, find the cropped width. The per-glyph
        // `right > max_w` break is a safety net for the rare glyph
        // whose ink box overruns its advance; normal truncation is
        // already handled above.
        struct Placed {
            bmp: Vec<u8>,
            gw: usize,
            gh: usize,
            ox: i32,
            oy: i32,
        }
        let mut placed: Vec<Placed> = Vec::new();
        let mut pen_x = 0.0_f32;
        for ch in render_seq {
            let (m, bmp) = self.font.rasterize(ch, px);
            let glyph_left = pen_x.round() as i32 + m.xmin;
            // Right edge this glyph would occupy.
            let right = glyph_left + m.width as i32;
            if right > max_w as i32 {
                break;
            }
            // Buffer-space top: baseline, minus how far the glyph rises
            // (ymin is the baseline→bottom distance; height adds the
            // body above that).
            let top = baseline - m.ymin - m.height as i32;
            placed.push(Placed {
                bmp,
                gw: m.width,
                gh: m.height,
                ox: glyph_left,
                oy: top,
            });
            pen_x += m.advance_width;
        }

        if placed.is_empty() {
            return None;
        }
        let width = placed
            .iter()
            .map(|p| p.ox + p.gw as i32)
            .max()
            .unwrap_or(0)
            .clamp(0, max_w as i32) as usize;
        if width == 0 {
            return None;
        }

        // Coverage gamma LUT. fontdue emits *linear* coverage; blended
        // straight over the dark UI, thin stems and small text come out
        // washed-out and patchy (the classic "grey mush" look). A gamma
        // < 1 lifts the low/mid coverage so glyph edges read crisp and
        // the text gains perceived weight without going blocky. Built
        // once per label (rasterisation is off the per-frame path).
        const COVERAGE_GAMMA: f32 = 0.72;
        let lut: [u8; 256] = {
            let mut l = [0u8; 256];
            for (i, slot) in l.iter_mut().enumerate() {
                let v = (i as f32 / 255.0).powf(COVERAGE_GAMMA);
                *slot = (v * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            l
        };

        // Second pass: blit coverage into the RGBA buffer.
        let mut buf = vec![0u8; width * height * 4];
        for p in &placed {
            for row in 0..p.gh {
                let by = p.oy + row as i32;
                if by < 0 || by as usize >= height {
                    continue;
                }
                for col in 0..p.gw {
                    let bx = p.ox + col as i32;
                    if bx < 0 || bx as usize >= width {
                        continue;
                    }
                    let cov = lut[p.bmp[row * p.gw + col] as usize];
                    if cov == 0 {
                        continue;
                    }
                    let idx = ((by as usize) * width + bx as usize) * 4;
                    buf[idx] = color[0];
                    buf[idx + 1] = color[1];
                    buf[idx + 2] = color[2];
                    // Max so overlapping glyph edges don't punch holes.
                    buf[idx + 3] = buf[idx + 3].max(cov);
                }
            }
        }
        Some((buf, width, height))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Best-effort: load whatever system font exists. Skipped (returns
    /// early, test passes vacuously) on a font-less CI image rather
    /// than failing — the production path already treats text as
    /// optional.
    fn font_or_skip() -> Option<TextRenderer> {
        TextRenderer::load()
    }

    #[test]
    fn empty_input_yields_none() {
        let Some(tr) = font_or_skip() else { return };
        assert!(tr.rasterize_line("", 18.0, [255, 255, 255], 200).is_none());
        assert!(tr.rasterize_line("   ", 18.0, [255, 255, 255], 200).is_none());
        assert!(tr.rasterize_line("x", 18.0, [255, 255, 255], 0).is_none());
    }

    #[test]
    fn produces_rgba_buffer_of_expected_size() {
        let Some(tr) = font_or_skip() else { return };
        let (buf, w, h) = tr
            .rasterize_line("Firefox", 18.0, [230, 230, 240], 400)
            .expect("non-empty label should rasterize");
        assert_eq!(buf.len(), w * h * 4);
        assert!(w > 0 && h > 0);
        // At least one pixel must have non-zero coverage, otherwise the
        // glyph blit silently produced nothing.
        assert!(buf.chunks_exact(4).any(|px| px[3] > 0));
        // RGB is the constant we asked for wherever alpha is set.
        for px in buf.chunks_exact(4) {
            if px[3] > 0 {
                assert_eq!([px[0], px[1], px[2]], [230, 230, 240]);
            }
        }
    }

    #[test]
    fn width_never_exceeds_max() {
        let Some(tr) = font_or_skip() else { return };
        let (_, w, _) = tr
            .rasterize_line(
                "a very long window title that should get truncated hard",
                18.0,
                [255, 255, 255],
                120,
            )
            .expect("some glyphs should fit");
        assert!(w <= 120, "width {w} exceeded max 120");
    }

    #[test]
    fn short_text_is_not_truncated() {
        let Some(tr) = font_or_skip() else { return };
        // Rendered with a generous cap → must match the natural width
        // when capped just above it (no ellipsis path taken).
        let (_, w_big, _) = tr
            .rasterize_line("Term", 18.0, [255, 255, 255], 4000)
            .expect("fits");
        let (_, w_fit, _) = tr
            .rasterize_line("Term", 18.0, [255, 255, 255], w_big + 8)
            .expect("fits");
        assert_eq!(w_big, w_fit, "short text should not be reflowed");
    }

    #[test]
    fn overflowing_text_truncates_below_natural_width() {
        let Some(tr) = font_or_skip() else { return };
        let long = "an extremely long window title that will not fit at all";
        let (_, natural, _) = tr
            .rasterize_line(long, 18.0, [255, 255, 255], 100_000)
            .expect("fits at huge cap");
        let (_, capped, _) = tr
            .rasterize_line(long, 18.0, [255, 255, 255], 150)
            .expect("ellipsis path still renders");
        assert!(capped <= 150, "capped width {capped} exceeded 150");
        assert!(
            capped < natural,
            "expected truncation: capped {capped} >= natural {natural}"
        );
    }
}
