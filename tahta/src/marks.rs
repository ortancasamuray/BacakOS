//! A small display list shared by the physics panels (`motion.rs`,
//! `force.rs`): a panel builds its figure once as `Mark`s, and the same
//! list is drawn on screen (`draw`) and turned into ink for "Tahtaya bas"
//! (`to_ink`), so the stamp is exactly what the panel shows minus the
//! screen-only parts (grid, tints, hints).
//!
//! Text supports `_x` / `_{max}` subscripts and `^2` superscripts, in both
//! the real font and the single-line ink font.

use glam::Vec2;

use crate::font_atlas::{FontAtlas, GlyphVertex};
use crate::ink::{dash_polyline, densify, stroke_text, stroke_text_advance, InkLine};
use crate::stroke::{push_circle, push_line, push_triangle, Vertex};

/// Screen color, and whether the mark goes onto the board when stamped.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Style {
    pub color: [f32; 4],
    pub ink: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Anchor {
    Left,
    Center,
    Right,
}

pub enum Mark {
    Line { pts: Vec<Vec2>, w: f32, style: Style, dash: bool },
    Arrow { a: Vec2, b: Vec2, w: f32, style: Style, dash: bool },
    Dot { c: Vec2, r: f32, style: Style, filled: bool },
    /// `pos.y` is the cap top.
    Text { s: String, pos: Vec2, h: f32, style: Style, anchor: Anchor },
    /// Screen-only fill.
    Tri { a: Vec2, b: Vec2, c: Vec2, style: Style },
}

impl Mark {
    pub fn line(pts: Vec<Vec2>, w: f32, style: impl Into<Style>) -> Self {
        Mark::Line { pts, w, style: style.into(), dash: false }
    }

    pub fn dashed(pts: Vec<Vec2>, w: f32, style: impl Into<Style>) -> Self {
        Mark::Line { pts, w, style: style.into(), dash: true }
    }

    pub fn arrow(a: Vec2, b: Vec2, w: f32, style: impl Into<Style>) -> Self {
        Mark::Arrow { a, b, w, style: style.into(), dash: false }
    }

    pub fn text(s: impl Into<String>, pos: Vec2, h: f32, style: impl Into<Style>, anchor: Anchor) -> Self {
        Mark::Text { s: s.into(), pos, h, style: style.into(), anchor }
    }

    pub fn style(&self) -> Style {
        match self {
            Mark::Line { style, .. } | Mark::Arrow { style, .. } | Mark::Dot { style, .. } | Mark::Text { style, .. } | Mark::Tri { style, .. } => *style,
        }
    }
}

/// Text runs: (text, level) with level 0 normal, -1 subscript, 1 superscript.
pub fn runs(s: &str) -> Vec<(String, i8)> {
    let mut out: Vec<(String, i8)> = Vec::new();
    let mut chars = s.chars().peekable();
    let mut cur = String::new();
    while let Some(c) = chars.next() {
        if c == '_' || c == '^' {
            let level = if c == '_' { -1 } else { 1 };
            let mut part = String::new();
            if chars.peek() == Some(&'{') {
                chars.next();
                for d in chars.by_ref() {
                    if d == '}' {
                        break;
                    }
                    part.push(d);
                }
            } else if let Some(d) = chars.next() {
                part.push(d);
            }
            if !cur.is_empty() {
                out.push((std::mem::take(&mut cur), 0));
            }
            out.push((part, level));
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        out.push((cur, 0));
    }
    out
}

/// Size and top offset of a run at `level` for text of cap height `h`.
fn run_metrics(level: i8, h: f32) -> (f32, f32) {
    match level {
        0 => (h, 0.0),
        -1 => (h * 0.68, h * 0.57),
        _ => (h * 0.65, -h * 0.25),
    }
}

/// Lays `s` out from `pos` with `anchor`: each run's (text, top-left, size).
pub fn layout_text(s: &str, pos: Vec2, h: f32, anchor: Anchor, advance: impl Fn(&str, f32) -> f32) -> Vec<(String, Vec2, f32)> {
    let rs = runs(s);
    let total: f32 = rs.iter().map(|(t, l)| advance(t, run_metrics(*l, h).0)).sum();
    let mut x = match anchor {
        Anchor::Left => pos.x,
        Anchor::Center => pos.x - total / 2.0,
        Anchor::Right => pos.x - total,
    };
    let mut out = Vec::new();
    for (t, l) in rs {
        let (size, dy) = run_metrics(l, h);
        let w = advance(&t, size);
        out.push((t, Vec2::new(x, pos.y + dy), size));
        x += w;
    }
    out
}

/// Draws rich text in the real font; returns its width.
#[allow(clippy::too_many_arguments)]
pub fn push_rich(font_atlas: &FontAtlas, s: &str, pos: Vec2, h: f32, color: [f32; 4], anchor: Anchor, gv: &mut Vec<GlyphVertex>, gi: &mut Vec<u32>) -> f32 {
    let mut right = pos.x;
    let mut left = f32::INFINITY;
    for (t, at, size) in layout_text(s, pos, h, anchor, |t, h| font_atlas.measure(t, h)) {
        left = left.min(at.x);
        right = right.max(at.x + font_atlas.push_text(&t, at, size, color, gv, gi));
    }
    right - left.min(pos.x)
}

/// The two back corners of an arrow head at `b`.
pub fn arrow_head(a: Vec2, b: Vec2, size: f32) -> Option<(Vec2, Vec2)> {
    let d = (b - a).normalize_or_zero();
    if d == Vec2::ZERO {
        return None;
    }
    let base = b - d * size;
    Some((base + d.perp() * size * 0.45, base - d.perp() * size * 0.45))
}

pub fn draw(marks: &[Mark], font_atlas: &FontAtlas, v: &mut Vec<Vertex>, i: &mut Vec<u32>, gv: &mut Vec<GlyphVertex>, gi: &mut Vec<u32>) {
    let segments = |pts: &[Vec2], dash: bool| -> Vec<Vec<Vec2>> { if dash { dash_polyline(pts, 7.0, 5.0) } else { vec![pts.to_vec()] } };
    for mark in marks {
        match mark {
            Mark::Line { pts, w, style, dash } => {
                for d in segments(pts, *dash) {
                    for s in d.windows(2) {
                        push_line(s[0], s[1], *w, style.color, v, i);
                    }
                }
            }
            Mark::Arrow { a, b, w, style, dash } => {
                if let Some((l, r)) = arrow_head(*a, *b, 13.0) {
                    for d in segments(&[*a, (l + r) / 2.0], *dash) {
                        for s in d.windows(2) {
                            push_line(s[0], s[1], *w, style.color, v, i);
                        }
                    }
                    push_triangle(l, *b, r, style.color, v, i);
                }
            }
            Mark::Dot { c, r, style, filled } => {
                if *filled {
                    push_circle(*c, *r, style.color, 16, v, i);
                } else {
                    let pts = crate::periodic::circle_points(*c, *r, 16);
                    for s in pts.windows(2) {
                        push_line(s[0], s[1], 2.0, style.color, v, i);
                    }
                }
            }
            Mark::Text { s, pos, h, style, anchor } => {
                push_rich(font_atlas, s, *pos, *h, style.color, *anchor, gv, gi);
            }
            Mark::Tri { a, b, c, style } => push_triangle(*a, *b, *c, style.color, v, i),
        }
    }
}

/// The inked marks as board strokes in color `ink`.
pub fn to_ink(marks: &[Mark], ink: [f32; 4]) -> Vec<InkLine> {
    let mut lines = Vec::new();
    let mut add = |points: Vec<Vec2>, width: f32| {
        if points.len() >= 2 {
            lines.push(InkLine { points: densify(&points, 3.0), color: ink, width });
        }
    };
    for mark in marks.iter().filter(|m| m.style().ink) {
        match mark {
            Mark::Tri { .. } => {}
            Mark::Line { pts, w, dash, .. } => {
                if *dash {
                    for d in dash_polyline(pts, 9.0, 6.0) {
                        add(d, *w);
                    }
                } else {
                    add(pts.clone(), *w);
                }
            }
            Mark::Arrow { a, b, w, dash, .. } => {
                if *dash {
                    for d in dash_polyline(&[*a, *b], 9.0, 6.0) {
                        add(d, *w);
                    }
                } else {
                    add(vec![*a, *b], *w);
                }
                if let Some((l, r)) = arrow_head(*a, *b, 14.0) {
                    add(vec![l, *b, r], *w);
                }
            }
            Mark::Dot { c, r, .. } => add(crate::periodic::circle_points(*c, *r, 16), 2.0),
            Mark::Text { s, pos, h, anchor, .. } => {
                for (t, at, size) in layout_text(s, *pos, *h, *anchor, stroke_text_advance) {
                    for g in stroke_text(&t, at, size) {
                        add(g, (size / 9.0).clamp(1.4, 2.2));
                    }
                }
            }
        }
    }
    lines
}

/// Every inked text character has an ink glyph (for panel tests).
#[cfg(test)]
pub fn missing_glyphs(marks: &[Mark]) -> Vec<(char, String)> {
    let mut out = Vec::new();
    for m in marks {
        if let Mark::Text { s, style, .. } = m {
            if style.ink {
                for (run, _) in runs(s) {
                    for ch in run.chars().filter(|c| !c.is_whitespace()) {
                        if crate::ink::stroke_glyph(ch).is_empty() {
                            out.push((ch, s.clone()));
                        }
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rich_text_runs() {
        assert_eq!(runs("h_{max} = 3 m/s^2"), vec![("h".into(), 0), ("max".into(), -1), (" = 3 m/s".into(), 0), ("2".into(), 1)]);
        assert_eq!(runs("F_x"), vec![("F".into(), 0), ("x".into(), -1)]);
    }

    #[test]
    fn screen_only_marks_stay_off_the_board() {
        let on = Style { color: [1.0; 4], ink: true };
        let off = Style { color: [1.0; 4], ink: false };
        let marks = vec![
            Mark::line(vec![Vec2::ZERO, Vec2::new(10.0, 0.0)], 2.0, on),
            Mark::line(vec![Vec2::ZERO, Vec2::new(0.0, 10.0)], 2.0, off),
            Mark::arrow(Vec2::ZERO, Vec2::new(50.0, 0.0), 2.0, on),
        ];
        // The line, the arrow's shaft and its head.
        assert_eq!(to_ink(&marks, [0.0; 4]).len(), 3);
    }
}
