//! Minimal 7-segment-style numeric renderer — not a font, just enough to
//! draw measurements (ruler length, protractor angle, a future calculator
//! display) without a glyph atlas. Supports digits, '.', ':', '-', and a
//! small circle for '°'.

use glam::Vec2;

use crate::stroke::{push_circle, push_line, Vertex};

/// Which of the 7 segments (standard a-g layout) each digit lights up.
const SEGMENTS: [&[u8]; 10] = [
    b"abcdef",  // 0
    b"bc",      // 1
    b"abged",   // 2
    b"abgcd",   // 3
    b"fgbc",    // 4
    b"afgcd",   // 5
    b"afgecd",  // 6
    b"abc",     // 7
    b"abcdefg", // 8
    b"abcdfg",  // 9
];

/// Draws one character cell at `top_left` sized `size`, `thickness`-wide
/// strokes. Returns the horizontal advance to the next cell.
fn push_char(ch: char, top_left: Vec2, size: Vec2, thickness: f32, color: [f32; 4], out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) -> f32 {
    match ch {
        '0'..='9' => {
            let lit = SEGMENTS[ch as usize - '0' as usize];
            let w = size.x;
            let h = size.y;
            let mid = h / 2.0;
            let seg = |name: u8| -> (Vec2, Vec2) {
                match name {
                    b'a' => (top_left, top_left + Vec2::new(w, 0.0)),
                    b'b' => (top_left + Vec2::new(w, 0.0), top_left + Vec2::new(w, mid)),
                    b'c' => (top_left + Vec2::new(w, mid), top_left + Vec2::new(w, h)),
                    b'd' => (top_left + Vec2::new(0.0, h), top_left + Vec2::new(w, h)),
                    b'e' => (top_left + Vec2::new(0.0, mid), top_left + Vec2::new(0.0, h)),
                    b'f' => (top_left, top_left + Vec2::new(0.0, mid)),
                    _ => (top_left + Vec2::new(0.0, mid), top_left + Vec2::new(w, mid)), // g
                }
            };
            for &name in lit {
                let (a, b) = seg(name);
                push_line(a, b, thickness, color, out_vertices, out_indices);
            }
            size.x + size.x * 0.35
        }
        '.' => {
            push_circle(top_left + Vec2::new(0.0, size.y), thickness * 0.7, color, 8, out_vertices, out_indices);
            size.x * 0.4
        }
        ':' => {
            push_circle(top_left + Vec2::new(0.0, size.y * 0.3), thickness * 0.7, color, 8, out_vertices, out_indices);
            push_circle(top_left + Vec2::new(0.0, size.y * 0.7), thickness * 0.7, color, 8, out_vertices, out_indices);
            size.x * 0.4
        }
        '-' => {
            let mid = size.y / 2.0;
            push_line(top_left + Vec2::new(0.0, mid), top_left + Vec2::new(size.x, mid), thickness, color, out_vertices, out_indices);
            size.x + size.x * 0.35
        }
        '°' => {
            push_circle(top_left + Vec2::new(size.x * 0.25, size.y * 0.15), size.x * 0.22, color, 10, out_vertices, out_indices);
            size.x * 0.6
        }
        _ => size.x * 0.5,
    }
}

/// Draws `text` left-to-right starting at `top_left`; each character cell
/// is `digit_size` with `thickness`-wide strokes. Returns total width.
pub fn push_number(
    text: &str,
    top_left: Vec2,
    digit_size: Vec2,
    thickness: f32,
    color: [f32; 4],
    out_vertices: &mut Vec<Vertex>,
    out_indices: &mut Vec<u32>,
) -> f32 {
    let mut cursor = top_left;
    let mut total = 0.0;
    for ch in text.chars() {
        let advance = push_char(ch, cursor, digit_size, thickness, color, out_vertices, out_indices);
        cursor.x += advance;
        total += advance;
    }
    total
}
