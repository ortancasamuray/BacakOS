//! Minimal 5x7 dot-matrix font — the digit-only `digits.rs` renderer can't
//! spell words, so the text box needs actual letters. Same philosophy as
//! `digits.rs`: not a real font/glyph atlas, just enough blocky pixels to
//! be legible on a big touch panel, drawn as small filled squares with the
//! existing `push_rect` primitive (no new dependency, no font file).
//!
//! Covers space, `0`-`9`, `A`-`Z`/`a`-`z`. The six Turkish letters not in
//! plain ASCII (Ç Ğ İ Ö Ş Ü and their lowercase forms) reuse their
//! undotted/uncedilla'd base letter's grid plus a small diacritic mark
//! drawn on top — a 5x7 cell is too small to also fit an accent legibly
//! baked into the grid itself. Lowercase letters get their own bitmaps
//! (no descenders below the 7-row cell — same "just enough blocky pixels"
//! trade-off as the rest of this font) rather than reusing the uppercase
//! grid at a smaller scale, so `b`/`d`/`p`/`q` etc. stay visually distinct.

use glam::Vec2;

use crate::stroke::{push_circle, push_line, push_rect, Vertex};

/// 7 rows, top to bottom; each row's lower 5 bits are columns, MSB
/// (bit 4) = leftmost pixel.
fn glyph_rows(ch: char) -> Option<[u8; 7]> {
    Some(match ch {
        ' ' => [0, 0, 0, 0, 0, 0, 0],
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        '3' => [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
        'D' => [0b11100, 0b10010, 0b10001, 0b10001, 0b10001, 0b10010, 0b11100],
        'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111],
        'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'J' => [0b00001, 0b00001, 0b00001, 0b00001, 0b10001, 0b10001, 0b01110],
        'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001],
        'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        'Q' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101],
        'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010],
        'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
        'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
        'Z' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111],
        // Added for the browser address bar (urlbar.rs) — domains/URLs need
        // a dot and hyphen; same "single glyph, minimal ink" philosophy as
        // the rest of this font.
        '.' => [0, 0, 0, 0, 0, 0, 0b00100],
        '-' => [0, 0, 0, 0b11111, 0, 0, 0],
        // Turkish letters: base glyph (diacritic added separately in push_char).
        'Ç' => glyph_rows('C')?,
        'Ğ' => glyph_rows('G')?,
        'İ' => glyph_rows('I')?,
        'Ö' => glyph_rows('O')?,
        'Ş' => glyph_rows('S')?,
        'Ü' => glyph_rows('U')?,
        // Lowercase — own bitmaps (fit inside the 7-row cell, no true
        // descenders for g/j/p/q/y, kept legible rather than authentic).
        'a' => [0b00000, 0b01110, 0b00001, 0b01111, 0b10001, 0b01111, 0b00000],
        'b' => [0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b10001, 0b11110],
        'c' => [0b00000, 0b00000, 0b01111, 0b10000, 0b10000, 0b10000, 0b01111],
        'd' => [0b00001, 0b00001, 0b01111, 0b10001, 0b10001, 0b10001, 0b01111],
        'e' => [0b00000, 0b01110, 0b10001, 0b11111, 0b10000, 0b10001, 0b01110],
        'f' => [0b00110, 0b01001, 0b01000, 0b11110, 0b01000, 0b01000, 0b01000],
        'g' => [0b00000, 0b01111, 0b10001, 0b10001, 0b01111, 0b00001, 0b01110],
        'h' => [0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b10001, 0b10001],
        'i' => [0b00100, 0b00000, 0b01100, 0b00100, 0b00100, 0b00100, 0b01110],
        'ı' => [0b00000, 0b00000, 0b01100, 0b00100, 0b00100, 0b00100, 0b01110],
        'j' => [0b00010, 0b00000, 0b00110, 0b00010, 0b00010, 0b10010, 0b01100],
        'k' => [0b10000, 0b10000, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010],
        'l' => [0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'm' => [0b00000, 0b00000, 0b11010, 0b10101, 0b10101, 0b10101, 0b10101],
        'n' => [0b00000, 0b00000, 0b10110, 0b11001, 0b10001, 0b10001, 0b10001],
        'o' => [0b00000, 0b00000, 0b01110, 0b10001, 0b10001, 0b10001, 0b01110],
        'p' => [0b00000, 0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000],
        'q' => [0b00000, 0b01111, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001],
        'r' => [0b00000, 0b00000, 0b10110, 0b11001, 0b10000, 0b10000, 0b10000],
        's' => [0b00000, 0b00000, 0b01111, 0b10000, 0b01110, 0b00001, 0b11110],
        't' => [0b01000, 0b01000, 0b11110, 0b01000, 0b01000, 0b01001, 0b00110],
        'u' => [0b00000, 0b00000, 0b10001, 0b10001, 0b10001, 0b10011, 0b01101],
        'v' => [0b00000, 0b00000, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'w' => [0b00000, 0b00000, 0b10001, 0b10001, 0b10101, 0b10101, 0b01010],
        'x' => [0b00000, 0b00000, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001],
        'y' => [0b00000, 0b10001, 0b10001, 0b10001, 0b01111, 0b00001, 0b01110],
        'z' => [0b00000, 0b00000, 0b11111, 0b00010, 0b00100, 0b01000, 0b11111],
        // Turkish lowercase: base glyph (diacritic added separately in push_char).
        'ç' => glyph_rows('c')?,
        'ğ' => glyph_rows('g')?,
        'ö' => glyph_rows('o')?,
        'ş' => glyph_rows('s')?,
        'ü' => glyph_rows('u')?,
        _ => return None,
    })
}

/// Turkish-aware lowercasing for the characters this font supports —
/// `char::to_lowercase` gets `I`/`İ` wrong for Turkish (maps both to a
/// dotted `i`), so the text box's Shift key uses this instead of the
/// standard library conversion when switching case.
pub fn to_lower_tr(ch: char) -> char {
    match ch {
        'I' => 'ı',
        'İ' => 'i',
        'Ç' => 'ç',
        'Ğ' => 'ğ',
        'Ö' => 'ö',
        'Ş' => 'ş',
        'Ü' => 'ü',
        c => c.to_ascii_lowercase(),
    }
}

/// Draws one uppercase character cell at `top_left` sized `cell_size`
/// (5 columns x 7 rows of `pixel` squares fill that box), returning the
/// horizontal advance to the next cell. Unsupported characters (anything
/// not covered by [`glyph_rows`]) render as a blank space-width gap.
pub fn push_char(ch: char, top_left: Vec2, cell_size: Vec2, color: [f32; 4], out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) -> f32 {
    let pixel = Vec2::new(cell_size.x / 5.0, cell_size.y / 7.0);

    if let Some(rows) = glyph_rows(ch) {
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) != 0 {
                    let p = top_left + Vec2::new(col as f32 * pixel.x, row as f32 * pixel.y);
                    push_rect(p, pixel * 1.05, color, out_vertices, out_indices); // slight overlap hides seams
                }
            }
        }
    }

    // Lowercase i/ı bake their dot (or lack of one) directly into the
    // grid above, so only the capital dotted İ needs an overlay here.
    match ch {
        'İ' => {
            push_circle(top_left + Vec2::new(cell_size.x / 2.0, -pixel.y * 0.6), pixel.x * 0.55, color, 8, out_vertices, out_indices);
        }
        'Ğ' | 'ğ' => {
            let c = top_left + Vec2::new(cell_size.x / 2.0, -pixel.y * 0.5);
            let r = cell_size.x * 0.22;
            for i in 0..8 {
                let t0 = std::f32::consts::PI * (0.15 + 0.7 * i as f32 / 8.0);
                let t1 = std::f32::consts::PI * (0.15 + 0.7 * (i + 1) as f32 / 8.0);
                push_line(c + Vec2::new(t0.cos(), -t0.sin()) * r, c + Vec2::new(t1.cos(), -t1.sin()) * r, pixel.y * 0.4, color, out_vertices, out_indices);
            }
        }
        'Ö' | 'Ü' | 'ö' | 'ü' => {
            let y = top_left.y - pixel.y * 0.6;
            push_circle(top_left + Vec2::new(cell_size.x * 0.3, y), pixel.x * 0.35, color, 8, out_vertices, out_indices);
            push_circle(top_left + Vec2::new(cell_size.x * 0.7, y), pixel.x * 0.35, color, 8, out_vertices, out_indices);
        }
        'Ç' | 'Ş' | 'ç' | 'ş' => {
            let base = top_left + Vec2::new(cell_size.x * 0.55, cell_size.y + pixel.y * 0.15);
            push_line(base, base + Vec2::new(pixel.x * 0.7, pixel.y * 0.8), pixel.y * 0.4, color, out_vertices, out_indices);
        }
        _ => {}
    }

    cell_size.x + pixel.x * 1.5
}

/// Draws `text` left-to-right starting at `top_left`; returns total width.
pub fn push_text(text: &str, top_left: Vec2, cell_size: Vec2, color: [f32; 4], out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) -> f32 {
    let mut cursor = top_left;
    let mut total = 0.0;
    for ch in text.chars() {
        let advance = push_char(ch, cursor, cell_size, color, out_vertices, out_indices);
        cursor.x += advance;
        total += advance;
    }
    total
}
