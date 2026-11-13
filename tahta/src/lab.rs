//! Draggable "Laboratuvar" panel: line drawings of school lab equipment
//! and three ready setups, to stamp onto the board when drawing an
//! experiment.
//!
//! Every piece is a set of polylines in a box one unit tall (x centered
//! on 0, y down), plus, for vessels, the inner profile the liquid fills:
//! the "Sıvı düzeyi" slider sets how full, drawn as a surface line with
//! a tint (on screen) or hatching (in ink). Setups place several pieces
//! with their own offset and scale. The stamp is a movable, resizable
//! figure (`input_handler`'s selection handles), optionally with the
//! name written under it.

use glam::Vec2;

use crate::font_atlas::{FontAtlas, GlyphVertex};
use crate::ink::{densify, stroke_text, stroke_text_width, InkLine};
use crate::stroke::{push_circle, push_line, push_rect, push_triangle, Vertex};

const HEADER_H: f32 = 36.0;
const PADDING: f32 = 12.0;
const GAP: f32 = 6.0;
const COLS: usize = 5;
const CARD_W: f32 = 128.0;
const CARD_H: f32 = 124.0;
const PREVIEW_W: f32 = 380.0;
const BTN_H: f32 = 44.0;
/// Height of a stamped piece on the board (px).
const STAMP_H: f32 = 440.0;

const COLOR_PANEL_BG: [f32; 4] = [0.12, 0.13, 0.17, 0.96];
const COLOR_HEADER: [f32; 4] = [0.18, 0.19, 0.24, 1.0];
const COLOR_FIELD: [f32; 4] = [0.06, 0.07, 0.09, 1.0];
const COLOR_TEXT: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_TEXT_DIM: [f32; 4] = [0.95, 0.95, 0.97, 0.6];
const COLOR_GLASS: [f32; 4] = [0.85, 0.90, 0.97, 1.0];
const COLOR_LIQUID: [f32; 4] = [0.30, 0.60, 1.0, 0.45];
const COLOR_SURFACE: [f32; 4] = [0.45, 0.72, 1.0, 1.0];
const COLOR_KEY_IDLE: [f32; 4] = [0.22, 0.24, 0.30, 1.0];
const COLOR_KEY_ACTIVE: [f32; 4] = [0.23, 0.51, 0.96, 1.0];
const COLOR_KEY_STAMP: [f32; 4] = [0.98, 0.55, 0.15, 1.0];
const COLOR_CARD: [f32; 4] = [0.17, 0.18, 0.23, 1.0];
const COLOR_CARD_ACTIVE: [f32; 4] = [0.20, 0.32, 0.52, 1.0];

/// The fillable inside of a vessel: half-width at each depth (y down,
/// increasing), and the empty/full levels.
#[derive(Clone)]
struct Liquid {
    profile: Vec<(f32, f32)>,
    top: f32,
    bottom: f32,
    center_x: f32,
}

impl Liquid {
    fn half_width(&self, y: f32) -> f32 {
        let p = &self.profile;
        if y <= p[0].0 {
            return p[0].1;
        }
        for w in p.windows(2) {
            if y <= w[1].0 {
                let t = (y - w[0].0) / (w[1].0 - w[0].0).max(1e-6);
                return w[0].1 + (w[1].1 - w[0].1) * t;
            }
        }
        p[p.len() - 1].1
    }

    fn level(&self, fill: f32) -> f32 {
        self.bottom - (self.bottom - self.top) * fill.clamp(0.0, 1.0)
    }
}

#[derive(Clone, Default)]
struct Piece {
    lines: Vec<Vec<Vec2>>,
    liquids: Vec<Liquid>,
    /// Filled when there's any liquid (the thermometer's bulb).
    bulbs: Vec<(Vec2, f32)>,
}

impl Piece {
    fn add(&mut self, other: Piece, dx: f32, dy: f32, s: f32) {
        let t = |p: Vec2| Vec2::new(p.x * s + dx, p.y * s + dy);
        self.lines.extend(other.lines.into_iter().map(|l| l.into_iter().map(t).collect()));
        self.liquids.extend(other.liquids.into_iter().map(|l| Liquid {
            profile: l.profile.into_iter().map(|(y, w)| (y * s + dy, w * s)).collect(),
            top: l.top * s + dy,
            bottom: l.bottom * s + dy,
            center_x: l.center_x * s + dx,
        }));
        self.bulbs.extend(other.bulbs.into_iter().map(|(c, r)| (t(c), r * s)));
    }

    fn bounds(&self) -> (Vec2, Vec2) {
        let mut lo = Vec2::splat(f32::INFINITY);
        let mut hi = Vec2::splat(f32::NEG_INFINITY);
        for p in self.lines.iter().flatten() {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        (lo, hi)
    }
}

fn v(x: f32, y: f32) -> Vec2 {
    Vec2::new(x, y)
}

/// A symmetric outline from its left half (top to bottom): down the
/// left, back up the right.
fn sym(left: &[(f32, f32)]) -> Vec<Vec2> {
    let mut out: Vec<Vec2> = left.iter().map(|&(x, y)| v(x, y)).collect();
    let skip = usize::from(left.last().is_some_and(|p| p.0 == 0.0));
    out.extend(left.iter().rev().skip(skip).map(|&(x, y)| v(-x, y)));
    out
}

/// Left half and its mirror as two separate lines (open at the bottom).
fn sym_open(left: &[(f32, f32)]) -> Vec<Vec<Vec2>> {
    vec![left.iter().map(|&(x, y)| v(x, y)).collect(), left.iter().map(|&(x, y)| v(-x, y)).collect()]
}

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<Vec2> {
    vec![v(x0, y0), v(x1, y0), v(x1, y1), v(x0, y1), v(x0, y0)]
}

fn ellipse(c: Vec2, a: f32, b: f32, t0: f32, t1: f32, n: usize) -> Vec<Vec2> {
    (0..=n).map(|i| {
        let t = t0 + (t1 - t0) * i as f32 / n as f32;
        c + v(a * t.cos(), b * t.sin())
    }).collect()
}

fn liquid(profile: &[(f32, f32)], top: f32, bottom: f32) -> Liquid {
    Liquid { profile: profile.to_vec(), top, bottom, center_x: 0.0 }
}

/// A round flask body: neck of half-width `neck` down to where it meets
/// a sphere of radius `r` centered at depth `cy`, then to the flat
/// bottom at depth `bottom` (or the sphere's lowest point).
fn flask_left(neck: f32, neck_top: f32, cy: f32, r: f32, bottom: f32) -> (Vec<(f32, f32)>, Vec<(f32, f32)>) {
    let phi0 = (neck / r).asin();
    let phi1 = if bottom < cy + r { ((cy - bottom) / r).acos() } else { std::f32::consts::PI };
    let mut left = vec![(-neck - 0.02, neck_top - 0.01), (-neck, neck_top + 0.02)];
    let mut profile = vec![(neck_top + 0.02, neck - 0.01)];
    for i in 0..=16 {
        let phi = phi0 + (phi1 - phi0) * i as f32 / 16.0;
        let (x, y) = (-r * phi.sin(), cy - r * phi.cos());
        left.push((x, y));
        profile.push((y, (-x - 0.01).max(0.0)));
    }
    if left.last().unwrap().0 != 0.0 {
        left.push((0.0, left.last().unwrap().1));
    }
    (left, profile)
}

fn piece(index: usize) -> Piece {
    let mut p = Piece::default();
    match index {
        // Deney tüpü
        0 => {
            let mut left = vec![(-0.11, -0.01), (-0.08, 0.03), (-0.08, 0.9)];
            for i in 1..=6 {
                let t = i as f32 / 6.0 * std::f32::consts::FRAC_PI_2;
                left.push((-0.08 * t.cos(), 0.9 + 0.08 * t.sin()));
            }
            p.lines.push(sym(&left));
            p.liquids.push(liquid(&[(0.04, 0.072), (0.9, 0.072), (0.95, 0.052), (0.975, 0.0)], 0.1, 0.975));
        }
        // Beher
        1 => {
            p.lines.push(sym(&[(-0.47, -0.03), (-0.42, 0.02), (-0.42, 0.97), (-0.38, 1.0), (0.0, 1.0)]));
            for (k, y) in [0.3f32, 0.42, 0.54, 0.66, 0.78].iter().enumerate() {
                let len = if k % 2 == 0 { 0.12 } else { 0.07 };
                p.lines.push(vec![v(-0.42, *y), v(-0.42 + len, *y)]);
            }
            p.liquids.push(liquid(&[(0.03, 0.41), (0.97, 0.41), (0.99, 0.37)], 0.12, 0.99));
        }
        // Erlenmayer
        2 => {
            p.lines.push(sym(&[(-0.11, -0.01), (-0.09, 0.02), (-0.09, 0.33), (-0.42, 0.93), (-0.40, 0.99), (-0.35, 1.0), (0.0, 1.0)]));
            p.liquids.push(liquid(&[(0.33, 0.08), (0.93, 0.41), (0.99, 0.35)], 0.36, 0.99));
        }
        // Balon joje
        3 => {
            let (left, profile) = flask_left(0.05, 0.0, 0.73, 0.27, 0.98);
            p.lines.push(sym(&left));
            p.lines.push(vec![v(-0.075, 0.22), v(0.075, 0.22)]);
            p.liquids.push(liquid(&profile, 0.22, 0.97));
        }
        // Dereceli silindir
        4 => {
            p.lines.push(sym(&[(-0.13, -0.02), (-0.1, 0.02), (-0.1, 0.92), (0.0, 0.92)]));
            p.lines.push(rect(-0.22, 0.92, 0.22, 0.98));
            for k in 0..11 {
                let y = 0.15 + k as f32 * 0.07;
                let len = if k % 2 == 0 { 0.08 } else { 0.045 };
                p.lines.push(vec![v(0.1, y), v(0.1 - len, y)]);
            }
            p.liquids.push(liquid(&[(0.02, 0.092), (0.91, 0.092)], 0.1, 0.91));
        }
        // Büret
        5 => {
            p.lines.push(sym(&[(-0.045, 0.0), (-0.045, 0.78), (-0.02, 0.82), (0.0, 0.82)]));
            p.lines.push(rect(-0.13, 0.82, 0.13, 0.86));
            p.lines.extend(sym_open(&[(-0.02, 0.86), (-0.006, 1.0)]));
            p.lines.push(vec![v(-0.006, 1.0), v(0.006, 1.0)]);
            for k in 0..15 {
                let y = 0.05 + k as f32 * 0.05;
                let len = if k % 5 == 0 { 0.05 } else { 0.025 };
                p.lines.push(vec![v(-0.045, y), v(-0.045 + len, y)]);
            }
            p.liquids.push(liquid(&[(0.0, 0.04), (0.78, 0.04), (0.82, 0.015)], 0.04, 0.82));
        }
        // Pipet
        6 => {
            p.lines.push(sym(&[(-0.018, 0.0), (-0.018, 0.3), (-0.05, 0.34), (-0.07, 0.4), (-0.07, 0.53), (-0.05, 0.59), (-0.018, 0.63), (-0.018, 0.9), (-0.005, 1.0), (0.0, 1.0)]));
            p.lines.push(vec![v(-0.04, 0.15), v(0.04, 0.15)]);
            p.liquids.push(liquid(&[(0.0, 0.014), (0.3, 0.014), (0.34, 0.045), (0.4, 0.065), (0.53, 0.065), (0.59, 0.045), (0.63, 0.014), (0.9, 0.014), (1.0, 0.003)], 0.15, 1.0));
        }
        // Huni
        7 => {
            p.lines.extend(sym_open(&[(-0.44, 0.0), (-0.05, 0.45), (-0.05, 1.0)]));
            p.lines.push(ellipse(v(0.0, 0.0), 0.44, 0.04, 0.0, std::f32::consts::TAU, 40));
        }
        // Ayırma hunisi
        8 => {
            p.lines.push(rect(-0.05, 0.0, 0.05, 0.07));
            p.lines.extend(sym_open(&[(-0.06, 0.07), (-0.06, 0.12), (-0.3, 0.42), (-0.3, 0.48), (-0.05, 0.74), (-0.05, 0.78)]));
            p.lines.push(rect(-0.14, 0.78, 0.14, 0.82));
            p.lines.extend(sym_open(&[(-0.025, 0.82), (-0.018, 1.0)]));
            p.liquids.push(liquid(&[(0.12, 0.055), (0.42, 0.29), (0.48, 0.29), (0.74, 0.045), (0.78, 0.04)], 0.15, 0.78));
        }
        // Bunsen beki
        9 => {
            p.lines.push(vec![v(-0.32, 1.0), v(-0.26, 0.92), v(0.26, 0.92), v(0.32, 1.0), v(-0.32, 1.0)]);
            p.lines.push(vec![v(-0.07, 0.92), v(-0.07, 0.45), v(0.07, 0.45), v(0.07, 0.92)]);
            p.lines.push(ellipse(v(0.0, 0.8), 0.03, 0.03, 0.0, std::f32::consts::TAU, 16));
            p.lines.push(vec![v(0.07, 0.85), v(0.22, 0.85)]);
            p.lines.push(vec![v(0.07, 0.89), v(0.22, 0.89)]);
            p.lines.push(sym(&[(-0.07, 0.44), (-0.11, 0.33), (-0.1, 0.2), (-0.05, 0.08), (0.0, 0.02)]));
            p.lines.push(sym(&[(-0.04, 0.44), (-0.055, 0.36), (-0.03, 0.26), (0.0, 0.21)]));
        }
        // Üç ayak ve tel kafes
        10 => {
            p.lines.push(rect(-0.46, 0.1, 0.46, 0.15));
            for k in 0..15 {
                let x = -0.42 + k as f32 * 0.06;
                p.lines.push(vec![v(x, 0.1), v(x, 0.15)]);
            }
            p.lines.push(vec![v(-0.34, 0.15), v(-0.44, 1.0)]);
            p.lines.push(vec![v(0.34, 0.15), v(0.44, 1.0)]);
            p.lines.push(vec![v(0.0, 0.15), v(0.02, 0.9)]);
        }
        // Termometre
        11 => {
            p.lines.extend(sym_open(&[(0.0, 0.0), (-0.025, 0.006), (-0.035, 0.03), (-0.035, 0.845)]));
            p.lines.push(ellipse(v(0.0, 0.9), 0.065, 0.065, -1.0, std::f32::consts::PI + 1.0, 32));
            for k in 0..13 {
                let y = 0.1 + k as f32 * 0.06;
                let len = if k % 2 == 0 { 0.07 } else { 0.04 };
                p.lines.push(vec![v(0.05, y), v(0.05 + len, y)]);
            }
            p.liquids.push(liquid(&[(0.03, 0.011), (0.86, 0.011)], 0.06, 0.86));
            p.bulbs.push((v(0.0, 0.9), 0.05));
        }
        // Damlalık
        12 => {
            p.lines.push(ellipse(v(0.0, 0.15), 0.09, 0.15, 0.0, std::f32::consts::TAU, 32));
            p.lines.extend(sym_open(&[(-0.04, 0.3), (-0.04, 0.82), (-0.012, 1.0)]));
            p.lines.push(vec![v(-0.012, 1.0), v(0.012, 1.0)]);
            p.liquids.push(liquid(&[(0.3, 0.035), (0.82, 0.035), (1.0, 0.008)], 0.4, 0.99));
        }
        // Petri kabı
        13 => {
            p.lines.push(ellipse(v(0.0, 0.4), 0.48, 0.1, 0.0, std::f32::consts::TAU, 48));
            p.lines.push(vec![v(-0.48, 0.4), v(-0.48, 0.52)]);
            p.lines.push(vec![v(0.48, 0.4), v(0.48, 0.52)]);
            p.lines.push(ellipse(v(0.0, 0.52), 0.48, 0.1, 0.0, std::f32::consts::PI, 24));
        }
        // Havan ve havaneli
        14 => {
            p.lines.push(ellipse(v(0.0, 0.42), 0.45, 0.5, 0.0, std::f32::consts::PI, 32));
            p.lines.push(ellipse(v(0.0, 0.42), 0.45, 0.07, 0.0, std::f32::consts::TAU, 40));
            p.lines.push(vec![v(-0.15, 0.89), v(-0.2, 0.98), v(0.2, 0.98), v(0.15, 0.89)]);
            let (a, b) = (v(0.05, 0.55), v(0.4, -0.05));
            let n = (b - a).normalize().perp() * 0.035;
            p.lines.push(vec![a + n, b + n * 1.4, b - n * 1.4, a - n, a + n]);
        }
        // Spor (destek)
        15 => {
            p.lines.push(rect(-0.45, 0.94, 0.25, 1.0));
            p.lines.push(vec![v(-0.312, 0.94), v(-0.312, 0.0), v(-0.288, 0.0), v(-0.288, 0.94)]);
            p.lines.push(rect(-0.33, 0.28, -0.27, 0.34));
            p.lines.push(vec![v(-0.27, 0.31), v(0.11, 0.31)]);
            p.lines.push(vec![v(0.2, 0.27), v(0.11, 0.27), v(0.11, 0.35), v(0.2, 0.35)]);
        }
        // Dibi yuvarlak balon
        16 => {
            let (left, profile) = flask_left(0.07, 0.0, 0.66, 0.32, 2.0);
            p.lines.push(sym(&left));
            p.liquids.push(liquid(&profile, 0.4, 0.97));
        }
        // Titrasyon düzeneği
        17 => {
            p.add(piece(15), 0.0, 0.0, 1.0);
            p.add(piece(5), 0.155, 0.0, 0.8);
            p.add(piece(2), 0.155, 0.78, 0.16);
        }
        // Isıtma düzeneği
        18 => {
            p.add(piece(10), 0.0, 0.4, 0.6);
            p.add(piece(1), 0.0, 0.14, 0.32);
            p.add(piece(9), 0.0, 0.5, 0.5);
        }
        // Süzme düzeneği
        _ => {
            p.add(piece(15), 0.0, 0.0, 1.0);
            p.add(piece(7), 0.155, 0.2, 0.35);
            let filter = Piece { lines: sym_open(&[(-0.4, 0.02), (0.0, 0.42)]), ..Default::default() };
            p.add(filter, 0.155, 0.2, 0.35);
            p.add(piece(1), 0.155, 0.62, 0.32);
        }
    }
    p
}

const NAMES: [&str; 20] = [
    "Deney tüpü",
    "Beher",
    "Erlenmayer",
    "Balon joje",
    "Dereceli silindir",
    "Büret",
    "Pipet",
    "Huni",
    "Ayırma hunisi",
    "Bunsen beki",
    "Üç ayak ve tel kafes",
    "Termometre",
    "Damlalık",
    "Petri kabı",
    "Havan ve havaneli",
    "Spor (destek)",
    "Dibi yuvarlak balon",
    "Titrasyon düzeneği",
    "Isıtma düzeneği",
    "Süzme düzeneği",
];

/// Places `piece` (unit box) into `rect`, keeping its aspect.
struct Fit {
    origin: Vec2,
    scale: f32,
}

impl Fit {
    fn new(piece: &Piece, (pos, size): (Vec2, Vec2)) -> Self {
        let (lo, hi) = piece.bounds();
        let span = (hi - lo).max(Vec2::splat(1e-3));
        let scale = (size.x / span.x).min(size.y / span.y);
        let used = span * scale;
        let origin = pos + (size - used) / 2.0 - lo * scale;
        Self { origin, scale }
    }

    fn pt(&self, p: Vec2) -> Vec2 {
        self.origin + p * self.scale
    }
}

/// The liquid of a vessel at `fill`: the surface line and horizontal
/// hatch lines `spacing` px apart below it, in screen space.
fn liquid_lines(l: &Liquid, fill: f32, fit: &Fit, spacing: f32) -> Option<(Vec<Vec2>, Vec<Vec<Vec2>>)> {
    if fill <= 0.0 {
        return None;
    }
    let level = l.level(fill);
    let line_at = |y: f32| {
        let w = l.half_width(y);
        (w > 0.003).then(|| vec![fit.pt(v(l.center_x - w, y)), fit.pt(v(l.center_x + w, y))])
    };
    let surface = line_at(level)?;
    let step = spacing / fit.scale;
    let mut hatch = Vec::new();
    let mut y = level + step;
    while y < l.bottom - step * 0.3 {
        if let Some(h) = line_at(y) {
            hatch.push(h);
        }
        y += step;
    }
    Some((surface, hatch))
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LabPress {
    Move,
    Close,
    Slider,
    Stamp,
    Other,
}

pub struct Lab {
    pub position: Vec2,
    selected: usize,
    fill: f32,
    label: bool,
    pieces: Vec<Piece>,
}

impl Lab {
    pub fn new(screen_size: Vec2) -> Self {
        let mut l = Self { position: Vec2::ZERO, selected: 2, fill: 0.5, label: true, pieces: (0..NAMES.len()).map(piece).collect() };
        let size = Vec2::new(l.panel_width(), l.panel_height());
        l.position = Vec2::new((screen_size.x - size.x) / 2.0, ((screen_size.y - 120.0 - size.y) / 2.0).max(12.0));
        l
    }

    fn panel_width(&self) -> f32 {
        PADDING * 3.0 + COLS as f32 * CARD_W + (COLS as f32 - 1.0) * GAP + PREVIEW_W
    }

    fn rows(&self) -> usize {
        NAMES.len().div_ceil(COLS)
    }

    fn panel_height(&self) -> f32 {
        HEADER_H + PADDING + self.rows() as f32 * (CARD_H + GAP) - GAP + PADDING
    }

    fn header_rect(&self) -> (Vec2, Vec2) {
        (self.position, Vec2::new(self.panel_width(), HEADER_H))
    }

    fn close_rect(&self) -> (Vec2, Vec2) {
        (self.position + Vec2::new(self.panel_width() - HEADER_H, 0.0), Vec2::splat(HEADER_H))
    }

    fn card_rect(&self, i: usize) -> (Vec2, Vec2) {
        let (row, col) = (i / COLS, i % COLS);
        (self.position + Vec2::new(PADDING + col as f32 * (CARD_W + GAP), HEADER_H + PADDING + row as f32 * (CARD_H + GAP)), Vec2::new(CARD_W, CARD_H))
    }

    fn right_x(&self) -> f32 {
        self.position.x + self.panel_width() - PADDING - PREVIEW_W
    }

    fn bottom(&self) -> f32 {
        self.position.y + self.panel_height() - PADDING
    }

    fn stamp_rect(&self) -> (Vec2, Vec2) {
        (Vec2::new(self.right_x(), self.bottom() - BTN_H), Vec2::new(PREVIEW_W, BTN_H))
    }

    fn label_rect(&self) -> (Vec2, Vec2) {
        (Vec2::new(self.right_x(), self.bottom() - 2.0 * BTN_H - GAP), Vec2::new(PREVIEW_W, BTN_H))
    }

    fn slider_rect(&self) -> (Vec2, Vec2) {
        (Vec2::new(self.right_x(), self.bottom() - 3.0 * BTN_H - 2.0 * GAP), Vec2::new(PREVIEW_W, BTN_H))
    }

    fn slider_track(&self) -> (f32, f32, f32) {
        let (pos, size) = self.slider_rect();
        (pos.x + 120.0, pos.x + size.x - 22.0, pos.y + size.y / 2.0)
    }

    fn preview_rect(&self) -> (Vec2, Vec2) {
        let top = self.position.y + HEADER_H + PADDING;
        (Vec2::new(self.right_x(), top), Vec2::new(PREVIEW_W, self.slider_rect().0.y - GAP - top))
    }

    fn has_liquid(&self) -> bool {
        !self.pieces[self.selected].liquids.is_empty()
    }

    pub fn contains(&self, p: Vec2) -> bool {
        rect_contains((self.position, Vec2::new(self.panel_width(), self.panel_height())), p)
    }

    pub fn drag_to(&mut self, p: Vec2, grab_offset: Vec2) {
        self.position = p - grab_offset;
    }

    pub fn press_at(&mut self, p: Vec2) -> LabPress {
        if rect_contains(self.close_rect(), p) {
            return LabPress::Close;
        }
        if rect_contains(self.header_rect(), p) {
            return LabPress::Move;
        }
        if rect_contains(self.stamp_rect(), p) {
            return LabPress::Stamp;
        }
        if rect_contains(self.label_rect(), p) {
            self.label = !self.label;
            return LabPress::Other;
        }
        if rect_contains(self.slider_rect(), p) && self.has_liquid() {
            self.set_slider(p.x);
            return LabPress::Slider;
        }
        if let Some(i) = (0..NAMES.len()).find(|&i| rect_contains(self.card_rect(i), p)) {
            self.selected = i;
        }
        LabPress::Other
    }

    pub fn set_slider(&mut self, x: f32) {
        let (x0, x1, _) = self.slider_track();
        self.fill = ((x - x0) / (x1 - x0)).clamp(0.0, 1.0);
    }

    pub fn selected_name(&self) -> &'static str {
        NAMES[self.selected]
    }

    // --- Ink -------------------------------------------------------------

    pub fn stamp_lines(&self, screen_size: Vec2, ink: [f32; 4]) -> Vec<InkLine> {
        let piece = &self.pieces[self.selected];
        let (lo, hi) = piece.bounds();
        let span = hi - lo;
        let h = STAMP_H.min(screen_size.y - 260.0);
        let size = Vec2::new(span.x / span.y * h, h);
        let top_left = Vec2::new((screen_size.x - size.x) / 2.0, ((screen_size.y - 130.0 - size.y - 50.0) / 2.0).max(20.0));
        let fit = Fit::new(piece, (top_left, size));
        let mut lines = Vec::new();
        let mut add = |points: Vec<Vec2>, width: f32| {
            if points.len() >= 2 {
                lines.push(InkLine { points: densify(&points, 3.0), color: ink, width });
            }
        };
        for l in &piece.lines {
            add(l.iter().map(|&p| fit.pt(p)).collect(), 2.5);
        }
        for l in &piece.liquids {
            if let Some((surface, hatch)) = liquid_lines(l, self.fill, &fit, 12.0) {
                add(surface, 2.5);
                for hl in hatch {
                    add(hl, 1.2);
                }
            }
        }
        if self.fill > 0.0 {
            for &(c, r) in &piece.bulbs {
                let c = fit.pt(c);
                let r = r * fit.scale;
                for k in 0..3 {
                    add(crate::periodic::circle_points(c, r * (1.0 - k as f32 * 0.3), 24), 1.6);
                }
            }
        }
        if self.label {
            let name = NAMES[self.selected];
            let th = 30.0;
            let w = stroke_text_width(name, th);
            for g in stroke_text(name, Vec2::new(top_left.x + (size.x - w) / 2.0, top_left.y + size.y + 18.0), th) {
                add(g, 2.0);
            }
        }
        lines
    }

    // --- Drawing ---------------------------------------------------------

    fn draw_piece(&self, piece: &Piece, rect: (Vec2, Vec2), width: f32, out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) {
        let fit = Fit::new(piece, rect);
        for l in &piece.liquids {
            if self.fill <= 0.0 {
                continue;
            }
            let level = l.level(self.fill);
            // Tint: quads between consecutive depths under the surface.
            let mut ys: Vec<f32> = l.profile.iter().map(|p| p.0).filter(|&y| y > level && y < l.bottom).collect();
            ys.insert(0, level);
            ys.push(l.bottom);
            for w in ys.windows(2) {
                let (a, b) = (w[0], w[1]);
                let (wa, wb) = (l.half_width(a), l.half_width(b));
                let p = |x: f32, y: f32| fit.pt(v(l.center_x + x, y));
                push_triangle(p(-wa, a), p(wa, a), p(wb, b), COLOR_LIQUID, out_vertices, out_indices);
                push_triangle(p(-wa, a), p(wb, b), p(-wb, b), COLOR_LIQUID, out_vertices, out_indices);
            }
            if let Some((surface, _)) = liquid_lines(l, self.fill, &fit, 1000.0) {
                push_line(surface[0], surface[1], width + 0.5, COLOR_SURFACE, out_vertices, out_indices);
            }
        }
        if self.fill > 0.0 {
            for &(c, r) in &piece.bulbs {
                push_circle(fit.pt(c), r * fit.scale, COLOR_LIQUID, 20, out_vertices, out_indices);
            }
        }
        for l in &piece.lines {
            for w in l.windows(2) {
                push_line(fit.pt(w[0]), fit.pt(w[1]), width, COLOR_GLASS, out_vertices, out_indices);
            }
        }
    }

    pub fn render(
        &self,
        font_atlas: &FontAtlas,
        out_vertices: &mut Vec<Vertex>,
        out_indices: &mut Vec<u32>,
        glyph_vertices: &mut Vec<GlyphVertex>,
        glyph_indices: &mut Vec<u32>,
    ) {
        push_rect(self.position, Vec2::new(self.panel_width(), self.panel_height()), COLOR_PANEL_BG, out_vertices, out_indices);
        let (header_pos, header_size) = self.header_rect();
        push_rect(header_pos, header_size, COLOR_HEADER, out_vertices, out_indices);
        font_atlas.push_text("Laboratuvar malzemeleri", header_pos + Vec2::new(PADDING, 10.0), 16.0, COLOR_TEXT, glyph_vertices, glyph_indices);
        let grip = header_pos + header_size / 2.0;
        for i in -1..=1 {
            push_circle(Vec2::new(grip.x + i as f32 * 10.0, grip.y), 2.0, COLOR_TEXT, 8, out_vertices, out_indices);
        }
        let (close_pos, close_size) = self.close_rect();
        let c = close_pos + close_size / 2.0;
        let r = close_size.x * 0.2;
        push_line(c - Vec2::splat(r), c + Vec2::splat(r), 2.5, COLOR_TEXT, out_vertices, out_indices);
        push_line(c + Vec2::new(-r, r), c + Vec2::new(r, -r), 2.5, COLOR_TEXT, out_vertices, out_indices);

        for (i, name) in NAMES.iter().enumerate() {
            let (pos, size) = self.card_rect(i);
            push_rect(pos, size, if i == self.selected { COLOR_CARD_ACTIVE } else { COLOR_CARD }, out_vertices, out_indices);
            self.draw_piece(&self.pieces[i], (pos + Vec2::new(10.0, 8.0), Vec2::new(size.x - 20.0, size.y - 40.0)), 1.6, out_vertices, out_indices);
            let h = 12.5;
            let label = crate::font_atlas::fit_text(name, size.x - 8.0, false, |t| font_atlas.measure(t, h));
            let w = font_atlas.measure(&label, h);
            font_atlas.push_text(&label, Vec2::new(pos.x + (size.x - w) / 2.0, pos.y + size.y - 22.0), h, COLOR_TEXT, glyph_vertices, glyph_indices);
        }

        let (ppos, psize) = self.preview_rect();
        push_rect(ppos, psize, COLOR_FIELD, out_vertices, out_indices);
        self.draw_piece(&self.pieces[self.selected], (ppos + Vec2::new(30.0, 40.0), psize - Vec2::new(60.0, 60.0)), 2.5, out_vertices, out_indices);
        font_atlas.push_text(NAMES[self.selected], ppos + Vec2::new(12.0, 10.0), 18.0, COLOR_TEXT, glyph_vertices, glyph_indices);

        let (spos, ssize) = self.slider_rect();
        push_rect(spos, ssize, COLOR_FIELD, out_vertices, out_indices);
        let on = self.has_liquid();
        font_atlas.push_text("Sıvı düzeyi", spos + Vec2::new(12.0, 13.0), 16.0, if on { COLOR_TEXT } else { COLOR_TEXT_DIM }, glyph_vertices, glyph_indices);
        let (x0, x1, y) = self.slider_track();
        push_line(Vec2::new(x0, y), Vec2::new(x1, y), 4.0, COLOR_KEY_IDLE, out_vertices, out_indices);
        if on {
            push_line(Vec2::new(x0, y), Vec2::new(x0 + (x1 - x0) * self.fill, y), 4.0, COLOR_SURFACE, out_vertices, out_indices);
            push_circle(Vec2::new(x0 + (x1 - x0) * self.fill, y), 11.0, COLOR_TEXT, 20, out_vertices, out_indices);
        }
        button(self.label_rect(), "Adını altına yaz", if self.label { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE }, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
        button(self.stamp_rect(), "Tahtaya bas", COLOR_KEY_STAMP, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
    }
}

#[allow(clippy::too_many_arguments)]
fn button(
    (pos, size): (Vec2, Vec2),
    label: &str,
    color: [f32; 4],
    font_atlas: &FontAtlas,
    out_vertices: &mut Vec<Vertex>,
    out_indices: &mut Vec<u32>,
    glyph_vertices: &mut Vec<GlyphVertex>,
    glyph_indices: &mut Vec<u32>,
) {
    push_rect(pos, size, color, out_vertices, out_indices);
    let h = 15.0;
    let w = font_atlas.measure(label, h);
    font_atlas.push_text(label, pos + Vec2::new((size.x - w) / 2.0, (size.y - h) / 2.0), h, COLOR_TEXT, glyph_vertices, glyph_indices);
}

fn rect_contains((top_left, size): (Vec2, Vec2), point: Vec2) -> bool {
    point.x >= top_left.x && point.x <= top_left.x + size.x && point.y >= top_left.y && point.y <= top_left.y + size.y
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Vec2 = Vec2::new(1920.0, 1080.0);

    #[test]
    fn liquids_stay_inside_their_vessels() {
        for i in 0..NAMES.len() {
            let p = piece(i);
            let (lo, hi) = p.bounds();
            assert!(hi.x > lo.x && hi.y > lo.y, "{}", NAMES[i]);
            for l in &p.liquids {
                assert!(l.top < l.bottom, "{}", NAMES[i]);
                for w in l.profile.windows(2) {
                    assert!(w[0].0 <= w[1].0, "{}: profile must go down", NAMES[i]);
                }
                for f in [0.1f32, 0.5, 1.0] {
                    let y = l.level(f);
                    let w = l.half_width(y);
                    assert!(l.center_x - w >= lo.x - 1e-3 && l.center_x + w <= hi.x + 1e-3, "{} at {f}", NAMES[i]);
                }
            }
        }
        // The beaker: half full sits halfway between empty and full.
        let b = piece(1);
        let l = &b.liquids[0];
        assert!((l.level(0.5) - (l.top + l.bottom) / 2.0).abs() < 1e-5);
    }

    #[test]
    fn setups_combine_their_pieces() {
        assert_eq!(piece(17).liquids.len(), 2, "burette + flask");
        assert_eq!(piece(18).liquids.len(), 1, "the beaker on the gauze");
        assert!(piece(17).lines.len() > piece(15).lines.len() + piece(5).lines.len());
    }

    #[test]
    fn panel_picks_fills_and_stamps() {
        let mut lab = Lab::new(SCREEN);
        assert!(lab.position.y + lab.panel_height() <= SCREEN.y - 75.0);
        let c = |r: (Vec2, Vec2)| r.0 + r.1 / 2.0;
        lab.press_at(c(lab.card_rect(5)));
        assert_eq!(lab.selected_name(), "Büret");
        let (x0, x1, y) = lab.slider_track();
        assert_eq!(lab.press_at(Vec2::new(x0 + (x1 - x0) * 0.8, y)), LabPress::Slider);
        assert!((lab.fill - 0.8).abs() < 1e-3);
        // No liquid in a funnel: the slider doesn't take the touch.
        lab.press_at(c(lab.card_rect(7)));
        assert_eq!(lab.press_at(Vec2::new(x0, y)), LabPress::Other);
        assert_eq!(lab.press_at(c(lab.stamp_rect())), LabPress::Stamp);
        for i in 0..NAMES.len() {
            lab.selected = i;
            for ch in NAMES[i].chars().filter(|c| !c.is_whitespace()) {
                assert!(!crate::ink::stroke_glyph(ch).is_empty(), "{ch} in {}", NAMES[i]);
            }
            for l in lab.stamp_lines(SCREEN, [1.0; 4]) {
                for p in &l.points {
                    assert!(p.x >= 0.0 && p.x <= SCREEN.x && p.y >= 0.0 && p.y <= SCREEN.y - 70.0, "{}: {p:?}", NAMES[i]);
                }
            }
        }
    }
}
