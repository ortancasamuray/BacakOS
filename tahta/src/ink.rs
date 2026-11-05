//! Shared helpers for panels that stamp their drawing onto the page as
//! ordinary ink ("Tahtaya bas" in `graph.rs` and `solid.rs`): polyline
//! resampling, dashing, and a tiny single-line stroke font for numbers
//! and vertex letters (the real-font `font_atlas` only draws screen-space
//! UI text, never ink).

use glam::Vec2;

/// One polyline to stamp onto the page, in screen space.
pub struct InkLine {
    pub points: Vec<Vec2>,
    pub color: [f32; 4],
    pub width: f32,
}

/// Resamples a polyline so consecutive points are at most `spacing` apart
/// — `stroke.rs` smooths ink through segment midpoints, which would round
/// a sparse polyline's corners (an axis arrow, a digit's edges) away.
pub fn densify(points: &[Vec2], spacing: f32) -> Vec<Vec2> {
    let mut out = vec![points[0]];
    for w in points.windows(2) {
        let n = (w[0].distance(w[1]) / spacing).ceil().max(1.0) as usize;
        for k in 1..=n {
            out.push(w[0].lerp(w[1], k as f32 / n as f32));
        }
    }
    out
}

/// Cuts a polyline into dashes (`dash` long, `gap` apart) with the
/// pattern running continuously across its vertices — a hidden edge made
/// of many short segments (a cylinder's back rim) still reads as one
/// evenly dashed line.
pub fn dash_polyline(points: &[Vec2], dash: f32, gap: f32) -> Vec<Vec<Vec2>> {
    let mut out = Vec::new();
    let mut current: Vec<Vec2> = Vec::new();
    let period = dash + gap;
    let mut phase = 0.0f32; // position within the dash+gap period
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = a.distance(b);
        if len <= f32::EPSILON {
            continue;
        }
        let mut s = 0.0f32;
        while s < len {
            let in_dash = phase < dash;
            let left_in_state = if in_dash { dash - phase } else { period - phase };
            let step = left_in_state.min(len - s);
            let p0 = a.lerp(b, s / len);
            let p1 = a.lerp(b, (s + step) / len);
            if in_dash {
                if current.is_empty() {
                    current.push(p0);
                }
                current.push(p1);
            }
            s += step;
            phase += step;
            if phase >= period - 1e-4 {
                phase = 0.0;
            }
            if in_dash && phase >= dash - 1e-4 && current.len() >= 2 {
                out.push(std::mem::take(&mut current));
            }
        }
    }
    if current.len() >= 2 {
        out.push(current);
    }
    out
}

/// Single-line stroke glyphs, in a box 0.6 wide × 1 tall (y down). Each
/// glyph is one or more polylines.
pub fn stroke_glyph(c: char) -> &'static [&'static [(f32, f32)]] {
    match c {
        '0' => &[&[(0.3, 0.0), (0.55, 0.12), (0.6, 0.5), (0.55, 0.88), (0.3, 1.0), (0.05, 0.88), (0.0, 0.5), (0.05, 0.12), (0.3, 0.0)]],
        '1' => &[&[(0.1, 0.2), (0.35, 0.0), (0.35, 1.0)]],
        '2' => &[&[(0.02, 0.18), (0.25, 0.0), (0.5, 0.05), (0.6, 0.25), (0.5, 0.48), (0.0, 1.0), (0.6, 1.0)]],
        '3' => &[&[(0.02, 0.1), (0.3, 0.0), (0.55, 0.12), (0.55, 0.38), (0.25, 0.48), (0.58, 0.6), (0.58, 0.88), (0.3, 1.0), (0.0, 0.9)]],
        '4' => &[&[(0.45, 1.0), (0.45, 0.0), (0.0, 0.68), (0.6, 0.68)]],
        '5' => &[&[(0.58, 0.0), (0.05, 0.0), (0.02, 0.45), (0.3, 0.4), (0.55, 0.52), (0.6, 0.78), (0.45, 0.97), (0.2, 1.0), (0.0, 0.9)]],
        '6' => &[&[(0.5, 0.02), (0.2, 0.12), (0.02, 0.5), (0.05, 0.88), (0.3, 1.0), (0.55, 0.88), (0.58, 0.62), (0.32, 0.5), (0.05, 0.6)]],
        '7' => &[&[(0.0, 0.0), (0.6, 0.0), (0.2, 1.0)]],
        '8' => &[&[(0.3, 0.48), (0.08, 0.36), (0.08, 0.1), (0.3, 0.0), (0.52, 0.1), (0.52, 0.36), (0.3, 0.48), (0.02, 0.62), (0.05, 0.9), (0.3, 1.0), (0.55, 0.9), (0.58, 0.62), (0.3, 0.48)]],
        '9' => &[&[(0.55, 0.4), (0.28, 0.5), (0.03, 0.38), (0.03, 0.12), (0.28, 0.0), (0.55, 0.12), (0.58, 0.5), (0.4, 0.88), (0.1, 0.98)]],
        '-' => &[&[(0.05, 0.55), (0.45, 0.55)]],
        ',' => &[&[(0.15, 0.85), (0.15, 1.0), (0.08, 1.12)]],
        '°' => &[&[(0.15, 0.0), (0.27, 0.05), (0.3, 0.15), (0.27, 0.25), (0.15, 0.3), (0.03, 0.25), (0.0, 0.15), (0.03, 0.05), (0.15, 0.0)]],
        'x' => &[&[(0.0, 0.35), (0.55, 1.0)], &[(0.55, 0.35), (0.0, 1.0)]],
        'y' => &[&[(0.0, 0.35), (0.3, 0.8)], &[(0.6, 0.35), (0.15, 1.25)]],
        'A' => &[&[(0.0, 1.0), (0.3, 0.0), (0.6, 1.0)], &[(0.12, 0.62), (0.48, 0.62)]],
        'B' => &[&[(0.0, 0.5), (0.0, 0.0), (0.4, 0.0), (0.55, 0.1), (0.55, 0.38), (0.4, 0.48), (0.0, 0.48), (0.42, 0.48), (0.6, 0.6), (0.6, 0.88), (0.45, 1.0), (0.0, 1.0), (0.0, 0.5)]],
        'C' => &[&[(0.6, 0.12), (0.45, 0.0), (0.2, 0.0), (0.03, 0.15), (0.0, 0.5), (0.03, 0.85), (0.2, 1.0), (0.45, 1.0), (0.6, 0.88)]],
        'D' => &[&[(0.0, 0.0), (0.0, 1.0), (0.3, 1.0), (0.55, 0.82), (0.6, 0.5), (0.55, 0.18), (0.3, 0.0), (0.0, 0.0)]],
        'E' => &[&[(0.6, 0.0), (0.0, 0.0), (0.0, 1.0), (0.6, 1.0)], &[(0.0, 0.5), (0.45, 0.5)]],
        'F' => &[&[(0.6, 0.0), (0.0, 0.0), (0.0, 1.0)], &[(0.0, 0.5), (0.45, 0.5)]],
        'G' => &[&[(0.6, 0.12), (0.45, 0.0), (0.2, 0.0), (0.03, 0.15), (0.0, 0.5), (0.03, 0.85), (0.2, 1.0), (0.45, 1.0), (0.6, 0.85), (0.6, 0.55), (0.35, 0.55)]],
        'H' => &[&[(0.0, 0.0), (0.0, 1.0)], &[(0.6, 0.0), (0.6, 1.0)], &[(0.0, 0.5), (0.6, 0.5)]],
        'I' => &[&[(0.3, 0.0), (0.3, 1.0)]],
        'J' => &[&[(0.5, 0.0), (0.5, 0.8), (0.35, 1.0), (0.12, 0.95), (0.02, 0.8)]],
        'K' => &[&[(0.0, 0.0), (0.0, 1.0)], &[(0.6, 0.0), (0.0, 0.6)], &[(0.2, 0.45), (0.6, 1.0)]],
        'L' => &[&[(0.0, 0.0), (0.0, 1.0), (0.6, 1.0)]],
        'T' => &[&[(0.0, 0.0), (0.6, 0.0)], &[(0.3, 0.0), (0.3, 1.0)]],
        _ => &[],
    }
}

/// Pen advance after a stroke glyph, in glyph heights — the minus sign
/// and comma are narrow, so a fixed advance would leave "- 8" gaps.
fn stroke_advance(c: char) -> f32 {
    match c {
        '-' => 0.55,
        ',' => 0.35,
        '°' => 0.4,
        'I' => 0.5,
        _ => 0.8,
    }
}

pub fn stroke_text_width(text: &str, height: f32) -> f32 {
    // The last glyph's ink ends at 0.6, not at its advance.
    let advance: f32 = text.chars().map(stroke_advance).sum();
    match text.chars().last() {
        Some(last) => (advance - stroke_advance(last) + 0.6) * height,
        None => 0.0,
    }
}

pub fn stroke_text(text: &str, top_left: Vec2, height: f32) -> Vec<Vec<Vec2>> {
    let mut out = Vec::new();
    let mut x = 0.0;
    for c in text.chars() {
        let origin = top_left + Vec2::new(x * height, 0.0);
        for poly in stroke_glyph(c) {
            out.push(poly.iter().map(|&(gx, gy)| origin + Vec2::new(gx, gy) * height).collect());
        }
        x += stroke_advance(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashes_run_across_vertices() {
        // 10 + 10 long L-shape, dash 4 gap 2: dashes at 0-4, 6-10 (corner),
        // 12-16, 18-20.
        let pts = [Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0)];
        let dashes = dash_polyline(&pts, 4.0, 2.0);
        assert_eq!(dashes.len(), 4);
        // The dash over the corner keeps the corner point.
        assert!(dashes[1].contains(&Vec2::new(10.0, 0.0)));
        let total: f32 = dashes.iter().flat_map(|d| d.windows(2).map(|w| w[0].distance(w[1]))).sum();
        assert!((total - 14.0).abs() < 1e-3, "{total}");
    }

    #[test]
    fn every_label_char_has_a_glyph() {
        for c in "0123456789-,°xyABCDEFGHIJKLT".chars() {
            assert!(!stroke_glyph(c).is_empty(), "{c}");
        }
    }
}
