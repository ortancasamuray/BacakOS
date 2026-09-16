//! Colour-emoji rendering from CBDT bitmap strikes (Noto Color Emoji).
//!
//! `fontdue` rasterises glyph **outlines** only — it has no CBDT/CBLC/COLR/sbix
//! support — so colour-emoji fonts render as tofu through the normal label
//! path. The only emoji font installed here is *Noto Color Emoji*, which stores
//! each glyph as an embedded **PNG** in the `CBDT` table.
//!
//! This module reads those PNGs with `ttf-parser`
//! ([`Face::glyph_raster_image`]) and decodes them with `image`, yielding a
//! plain RGBA bitmap the renderer blits exactly like any other label buffer.
//!
//! Scope: **single-codepoint** emoji (faces, hearts, common symbols). ZWJ
//! sequences (👨‍👩‍👧), skin-tone modifiers (👍🏽) and flags (🇹🇷) need GSUB
//! ligature resolution and are a follow-up — they fall back to the monochrome
//! `fontdue` path (a tofu box) for now.

const EMOJI_FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/NotoColorEmoji.ttf",
    "/usr/share/fonts/google-noto-emoji/NotoColorEmoji.ttf",
];

/// A decoded emoji glyph: tightly-packed RGBA8 (`[R,G,B,A]` per pixel, matching
/// the `Abgr8888` buffers the rest of the UI uses), `w`×`h` physical pixels.
pub struct EmojiBitmap {
    pub rgba: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

/// A loaded colour-emoji font. Owns the raw bytes; a fresh `ttf_parser::Face`
/// is parsed per lookup (microseconds — avoids a self-referential lifetime),
/// and decoded bitmaps are cached by the renderer, so a lookup happens at most
/// once per (char, size).
pub struct EmojiFont {
    bytes: Vec<u8>,
}

impl EmojiFont {
    /// Load the first available colour-emoji font, or `None` if none is
    /// installed (the OSK then keeps the monochrome `fontdue` fallback).
    pub fn load() -> Option<Self> {
        for path in EMOJI_FONT_CANDIDATES {
            let Ok(bytes) = std::fs::read(path) else { continue };
            if ttf_parser::Face::parse(&bytes, 0).is_ok() {
                tracing::info!(font = path, "loaded colour-emoji font");
                return Some(Self { bytes });
            }
        }
        tracing::info!("no colour-emoji font found — OSK emoji stay monochrome");
        None
    }

    /// Decode the emoji `text` (a single scalar **or** a multi-codepoint
    /// sequence — ZWJ family, skin-tone modifier, etc.), scaled to `px`×`px`.
    /// `None` when the font has no matching glyph or it isn't a PNG raster.
    pub fn glyph(&self, text: &str, px: u32) -> Option<EmojiBitmap> {
        let px = px.clamp(8, 256);
        let face = ttf_parser::Face::parse(&self.bytes, 0).ok()?;
        let gid = self.resolve_glyph(&face, text)?;
        // Ask for the strike nearest `px`; Noto Color Emoji ships one 128px
        // strike, so we always get that and downscale below.
        let img = face.glyph_raster_image(gid, px as u16)?;
        if img.format != ttf_parser::RasterImageFormat::PNG {
            return None;
        }
        let decoded = image::load_from_memory(img.data).ok()?.to_rgba8();
        let resized = image::imageops::resize(
            &decoded,
            px,
            px,
            image::imageops::FilterType::Triangle,
        );
        Some(EmojiBitmap { rgba: resized.into_raw(), w: px, h: px })
    }

    /// Map an emoji string to a single glyph: directly for one scalar, or via a
    /// **GSUB ligature** (lookup type 4) for ZWJ sequences / skin-tone
    /// modifiers (👨‍👩‍👧, 👍🏽). The font composes these multi-codepoint
    /// emoji into one ligated glyph; we replicate the substitution (proper
    /// shaping would use harfbuzz, but a full-sequence ligature match covers
    /// every emoji key, which is always exactly one cluster). A `U+FE0F`
    /// presentation selector carries no glyph and is dropped. Falls back to the
    /// first scalar's glyph (the base emoji) when no ligature matches.
    fn resolve_glyph(&self, face: &ttf_parser::Face, text: &str) -> Option<ttf_parser::GlyphId> {
        let mut glyphs: Vec<ttf_parser::GlyphId> = Vec::new();
        for ch in text.chars() {
            if ch == '\u{FE0F}' {
                continue;
            }
            if let Some(g) = face.glyph_index(ch) {
                glyphs.push(g);
            }
        }
        let first = *glyphs.first()?;
        if glyphs.len() >= 2 {
            if let Some(lig) = ligature_glyph(face, &glyphs) {
                return Some(lig);
            }
        }
        Some(first)
    }
}

/// Walk the font's GSUB ligature subtables (lookup type 4) for one whose first
/// glyph + components exactly equal `glyphs` (full-cluster consumption), and
/// return its ligated glyph. `None` if nothing matches.
fn ligature_glyph(
    face: &ttf_parser::Face,
    glyphs: &[ttf_parser::GlyphId],
) -> Option<ttf_parser::GlyphId> {
    use ttf_parser::gsub::SubstitutionSubtable;
    if glyphs.len() < 2 {
        return None;
    }
    let lookups = face.tables().gsub?.lookups;
    for li in 0..lookups.len() {
        let Some(lookup) = lookups.get(li) else { continue };
        let subs = lookup.subtables;
        for si in 0..subs.len() {
            let Some(SubstitutionSubtable::Ligature(lig)) =
                subs.get::<SubstitutionSubtable>(si)
            else {
                continue;
            };
            let Some(cov) = lig.coverage.get(glyphs[0]) else { continue };
            let Some(set) = lig.ligature_sets.get(cov) else { continue };
            for ki in 0..set.len() {
                let Some(l) = set.get(ki) else { continue };
                let comps = l.components;
                if comps.len() as usize == glyphs.len() - 1
                    && (0..comps.len()).all(|i| comps.get(i) == Some(glyphs[1 + i as usize]))
                {
                    return Some(l.glyph);
                }
            }
        }
    }
    None
}

/// If `s` begins with a codepoint we should render through the colour-emoji
/// bitmap path (rather than `fontdue`), return that scalar. Ranges cover the
/// emoji blocks plus Misc-Symbols / Dingbats / star supplements — but
/// deliberately **exclude** the `U+21xx`/`U+23xx` arrow & keyboard glyphs
/// (⇧ ↵ ⌫ ↹ …) the control keys use, which must stay on `fontdue`/DejaVu.
/// A leading `U+FE0F` variation selector is irrelevant (we test the base char).
pub fn leading_emoji(s: &str) -> Option<char> {
    let c = s.chars().next()?;
    let cp = c as u32;
    let is = (0x1F000..=0x1FAFF).contains(&cp)
        || (0x2600..=0x26FF).contains(&cp) // Miscellaneous Symbols
        || (0x2700..=0x27BF).contains(&cp) // Dingbats
        || (0x2B00..=0x2BFF).contains(&cp); // arrows/stars supplement (⭐ ⭕)
    is.then_some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_emoji_routes_only_emoji() {
        assert!(leading_emoji("😀").is_some());
        assert!(leading_emoji("⭐").is_some());
        assert!(leading_emoji("☀").is_some());
        // Control / text glyphs must stay on the fontdue path.
        assert!(leading_emoji("⌫").is_none());
        assert!(leading_emoji("↵").is_none());
        assert!(leading_emoji("←").is_none());
        assert!(leading_emoji("q").is_none());
        assert!(leading_emoji("ş").is_none());
        assert!(leading_emoji("").is_none());
    }

    #[test]
    fn decodes_colour_emoji_when_font_present() {
        let Some(font) = EmojiFont::load() else {
            eprintln!("no colour-emoji font installed — skipping decode test");
            return;
        };
        let bmp = font.glyph("😀", 48).expect("😀 should decode from Noto Color Emoji");
        assert_eq!((bmp.w, bmp.h), (48, 48));
        assert_eq!(bmp.rgba.len(), 48 * 48 * 4);
        assert!(
            bmp.rgba.chunks_exact(4).any(|p| p[3] > 0),
            "decoded emoji has no opaque pixels"
        );
        // A face emoji is mostly yellow → expect some strongly-red+green pixels.
        assert!(
            bmp.rgba.chunks_exact(4).any(|p| p[3] > 0 && p[0] > 150 && p[1] > 120),
            "decoded face emoji isn't coloured as expected"
        );
    }

    #[test]
    fn skin_tone_modifier_changes_the_glyph() {
        let Some(font) = EmojiFont::load() else { return };
        // Base 👍 vs. dark-skin-tone 👍🏿 must resolve to *different* glyphs
        // (proves the GSUB ligature for the modifier was applied).
        let (Some(base), Some(toned)) = (font.glyph("👍", 48), font.glyph("👍🏿", 48)) else {
            eprintln!("font lacks these glyphs — skipping");
            return;
        };
        assert_eq!(toned.rgba.len(), 48 * 48 * 4);
        assert_ne!(base.rgba, toned.rgba, "skin-tone ligature was not applied");
    }

    #[test]
    fn zwj_sequence_resolves_to_one_glyph() {
        let Some(font) = EmojiFont::load() else { return };
        // A ZWJ family + a profession sequence should both produce a glyph.
        assert!(font.glyph("👨‍👩‍👧", 48).is_some(), "family ZWJ didn't resolve");
        assert!(font.glyph("👩‍💻", 48).is_some(), "technologist ZWJ didn't resolve");
    }

    #[test]
    fn flags_resolve_to_distinct_coloured_glyphs() {
        let Some(font) = EmojiFont::load() else { return };
        // Flags are regional-indicator pairs ligated into one glyph.
        let (Some(tr), Some(us)) = (font.glyph("🇹🇷", 48), font.glyph("🇺🇸", 48)) else {
            eprintln!("font lacks flag glyphs — skipping");
            return;
        };
        assert_eq!(tr.rgba.len(), 48 * 48 * 4);
        assert!(tr.rgba.chunks_exact(4).any(|p| p[3] > 0), "flag has no opaque pixels");
        assert_ne!(tr.rgba, us.rgba, "TR and US flags resolved to the same glyph");
    }
}
