//! Draggable "Dalgalar ve titreşim" panel (physics), six animated modes:
//! a periodic transverse wave (λ, f, A, v = λf, a marked particle that
//! only moves up and down), a pulse reflecting from a fixed or free end,
//! two-source water-wave interference (crests, nodal and antinodal lines),
//! a spring–mass oscillator and a simple pendulum (period formulas, x–t
//! trace, energy bars), and a standing wave on a string (nodes and
//! antinodes for harmonic n). "Tahtaya bas" stamps the current frame
//! (see `marks.rs`).

use std::f32::consts::{PI, TAU};

use glam::Vec2;

use crate::font_atlas::{FontAtlas, GlyphVertex};
use crate::ink::InkLine;
use crate::marks::{self, push_rich, Anchor, Mark, Style};
use crate::motion::num;
use crate::stroke::{push_circle, push_line, push_rect, Vertex};

const HEADER_H: f32 = 36.0;
const PADDING: f32 = 12.0;
const GAP: f32 = 6.0;
const VIEW_W: f32 = 820.0;
const VIEW_H: f32 = 586.0;
const SIDE_W: f32 = 340.0;
const BTN_H: f32 = 44.0;
const LABEL_W: f32 = 150.0;
/// String modes: drawn length of the rope (m) and its scale (px per m).
const ROPE_M: f32 = 8.0;
const ROPE_PX: f32 = 92.0;
/// Interference: px per cm and the largest crest drawn (cm).
const TANK_PX: f32 = 26.0;
const TANK_R: f32 = 16.0;
/// Standing wave: string length (m), drawn amplitude (px).
const STRING_L: f32 = 2.0;
const STANDING_PX: f32 = 60.0;
/// Spring: px per cm of stretch.
const SPRING_PX: f32 = 12.0;
/// x–t traces show this many seconds.
const TRACE_S: f32 = 4.0;
const SAMPLES: usize = 240;
const STAMP_W: f32 = 900.0;
const STAMP_H: f32 = 560.0;

const COLOR_PANEL_BG: [f32; 4] = [0.12, 0.13, 0.17, 0.96];
const COLOR_HEADER: [f32; 4] = [0.18, 0.19, 0.24, 1.0];
const COLOR_FIELD: [f32; 4] = [0.06, 0.07, 0.09, 1.0];
const COLOR_TEXT: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_TEXT_DIM: [f32; 4] = [0.95, 0.95, 0.97, 0.55];
const COLOR_LINE: [f32; 4] = [0.85, 0.87, 0.92, 1.0];
const COLOR_AXIS: [f32; 4] = [0.85, 0.87, 0.92, 0.45];
const COLOR_WAVE: [f32; 4] = [0.40, 0.70, 1.0, 1.0];
const COLOR_WAVE2: [f32; 4] = [0.98, 0.62, 0.25, 1.0];
const COLOR_DOT: [f32; 4] = [0.98, 0.40, 0.35, 1.0];
const COLOR_NODE: [f32; 4] = [0.98, 0.45, 0.45, 1.0];
const COLOR_ANTI: [f32; 4] = [0.45, 0.85, 0.55, 1.0];
const COLOR_CREST: [f32; 4] = [0.40, 0.70, 1.0, 0.55];
const COLOR_KEY_IDLE: [f32; 4] = [0.22, 0.24, 0.30, 1.0];
const COLOR_KEY_ACTIVE: [f32; 4] = [0.23, 0.51, 0.96, 1.0];
const COLOR_KEY_STAMP: [f32; 4] = [0.98, 0.55, 0.15, 1.0];
const COLOR_SLIDER: [f32; 4] = [0.45, 0.72, 1.0, 1.0];

fn inked(color: [f32; 4]) -> Style {
    Style { color, ink: true }
}

fn screen(color: [f32; 4]) -> Style {
    Style { color, ink: false }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Periodic,
    Pulse,
    Interference,
    Spring,
    Pendulum,
    Standing,
}

const MODES: [(Mode, &str); 6] = [
    (Mode::Periodic, "Periyodik dalga"),
    (Mode::Pulse, "Atma ve yansıma"),
    (Mode::Interference, "Su dalgası girişimi"),
    (Mode::Spring, "Yay–kütle"),
    (Mode::Pendulum, "Basit sarkaç"),
    (Mode::Standing, "Duran dalga"),
];

/// One slider: label prefix, unit, (min, max, step), decimals shown.
#[derive(Clone, Copy, Debug)]
struct Spec {
    label: &'static str,
    unit: &'static str,
    min: f32,
    max: f32,
    step: f32,
    decimals: usize,
}

const fn spec(label: &'static str, unit: &'static str, min: f32, max: f32, step: f32, decimals: usize) -> Spec {
    Spec { label, unit, min, max, step, decimals }
}

fn specs(mode: Mode) -> [Spec; 3] {
    match mode {
        Mode::Periodic => [spec("λ", "m", 0.5, 4.0, 0.25, 2), spec("f", "Hz", 0.25, 2.0, 0.05, 2), spec("A", "m", 0.1, 0.6, 0.05, 2)],
        Mode::Pulse => [spec("A", "m", 0.2, 0.8, 0.05, 2), spec("w", "m", 0.5, 3.0, 0.25, 2), spec("v", "m/s", 1.0, 6.0, 0.5, 1)],
        Mode::Interference => [spec("λ", "cm", 1.0, 4.0, 0.25, 2), spec("d", "cm", 2.0, 10.0, 0.5, 1), spec("f", "Hz", 0.5, 2.0, 0.25, 2)],
        Mode::Spring => [spec("m", "kg", 0.1, 2.0, 0.1, 1), spec("k", "N/m", 5.0, 100.0, 5.0, 0), spec("A", "cm", 2.0, 12.0, 1.0, 0)],
        Mode::Pendulum => [spec("L", "m", 0.2, 2.0, 0.1, 1), spec("θ_0", "°", 5.0, 30.0, 1.0, 0), spec("m", "kg", 0.1, 2.0, 0.1, 1)],
        Mode::Standing => [spec("n", "", 1.0, 6.0, 1.0, 0), spec("v", "m/s", 20.0, 200.0, 10.0, 0), spec("A", "cm", 1.0, 5.0, 0.5, 1)],
    }
}

fn defaults(mode: Mode) -> [f32; 3] {
    match mode {
        Mode::Periodic => [2.0, 0.5, 0.4],
        Mode::Pulse => [0.5, 1.5, 3.0],
        Mode::Interference => [2.0, 6.0, 1.0],
        Mode::Spring => [0.5, 20.0, 8.0],
        Mode::Pendulum => [1.0, 15.0, 0.5],
        Mode::Standing => [3.0, 80.0, 2.0],
    }
}

/// T = 2π√(m/k)
pub fn spring_period(m: f32, k: f32) -> f32 {
    TAU * (m / k).sqrt()
}

/// T = 2π√(L/g) (small angles)
pub fn pendulum_period(l: f32, g: f32) -> f32 {
    TAU * (l / g).sqrt()
}

/// Two-source interference with source gap `d` and wavelength `l`:
/// (nodal lines, antinodal lines incl. the center one).
pub fn line_counts(d: f32, l: f32) -> (usize, usize) {
    let mut nodal = 0;
    while (nodal as f32 + 0.5) * l < d - 1e-4 {
        nodal += 1;
    }
    let mut anti = 0;
    while (anti as f32 + 1.0) * l < d - 1e-4 {
        anti += 1;
    }
    (2 * nodal, 2 * anti + 1)
}

/// Half-sine bump of width `w` and height `a`, `z` from its front.
fn bump(z: f32, w: f32, a: f32) -> f32 {
    if (0.0..=w).contains(&z) {
        a * (PI * z / w).sin()
    } else {
        0.0
    }
}

/// Rope displacement for the pulse mode: incident pulse plus its
/// reflection (inverted from a fixed end).
pub fn pulse_y(x: f32, t: f32, a: f32, w: f32, v: f32, fixed: bool) -> f32 {
    let s = if fixed { -1.0 } else { 1.0 };
    bump(v * t - x, w, a) + s * bump(v * t - (2.0 * ROPE_M - x), w, a)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum WavesPress {
    Move,
    Close,
    Slider(usize),
    Stamp,
    Other,
}

pub struct Waves {
    pub position: Vec2,
    mode: Mode,
    values: [[f32; 3]; 6],
    /// Mode-specific switch: particle mark / fixed end / nodal lines /
    /// energy bars / Moon gravity / envelope.
    option: [bool; 6],
    /// Simulation time while paused, and running since (input clock).
    paused_t: f64,
    running_since: Option<f64>,
}

impl Waves {
    pub fn new(screen_size: Vec2) -> Self {
        let mut w = Self {
            position: Vec2::ZERO,
            mode: Mode::Periodic,
            values: [defaults(Mode::Periodic), defaults(Mode::Pulse), defaults(Mode::Interference), defaults(Mode::Spring), defaults(Mode::Pendulum), defaults(Mode::Standing)],
            option: [true, true, true, true, false, true],
            paused_t: 0.0,
            running_since: None,
        };
        let size = Vec2::new(w.panel_width(), w.panel_height());
        w.position = Vec2::new((screen_size.x - size.x) / 2.0, ((screen_size.y - 120.0 - size.y) / 2.0).max(12.0));
        w
    }

    fn mi(&self) -> usize {
        MODES.iter().position(|(m, _)| *m == self.mode).unwrap_or(0)
    }

    fn v(&self, k: usize) -> f32 {
        self.values[self.mi()][k]
    }

    fn opt(&self) -> bool {
        self.option[self.mi()]
    }

    fn option_label(&self) -> &'static str {
        match self.mode {
            Mode::Periodic => "Parçacık",
            Mode::Pulse => {
                if self.opt() {
                    "Sabit uç"
                } else {
                    "Serbest uç"
                }
            }
            Mode::Interference => "Düğüm çizgileri",
            Mode::Spring => "Enerji",
            Mode::Pendulum => {
                if self.opt() {
                    "Ay (g = 1,6)"
                } else {
                    "Dünya (g = 9,8)"
                }
            }
            Mode::Standing => "Zarf",
        }
    }

    fn g(&self) -> f32 {
        if self.option[4] {
            1.6
        } else {
            9.8
        }
    }

    pub fn time(&self, now: f64) -> f32 {
        (self.paused_t + self.running_since.map_or(0.0, |s| (now - s).max(0.0))) as f32
    }

    pub fn is_animating(&self, _now: f64) -> bool {
        self.running_since.is_some()
    }

    /// Starts on first show.
    pub fn start(&mut self, now: f64) {
        if self.running_since.is_none() {
            self.running_since = Some(now);
        }
    }

    // --- Layout ----------------------------------------------------------

    fn slot_heights() -> [f32; 7] {
        let tabs = 3.0 * BTN_H + 2.0 * GAP;
        [tabs, BTN_H, BTN_H, BTN_H, VIEW_H - tabs - 5.0 * BTN_H - 6.0 * GAP, BTN_H, BTN_H]
    }

    fn panel_width(&self) -> f32 {
        PADDING * 3.0 + VIEW_W + SIDE_W
    }

    fn panel_height(&self) -> f32 {
        HEADER_H + PADDING + VIEW_H + PADDING
    }

    fn header_rect(&self) -> (Vec2, Vec2) {
        (self.position, Vec2::new(self.panel_width(), HEADER_H))
    }

    fn close_rect(&self) -> (Vec2, Vec2) {
        (self.position + Vec2::new(self.panel_width() - HEADER_H, 0.0), Vec2::splat(HEADER_H))
    }

    fn view_rect(&self) -> (Vec2, Vec2) {
        (self.position + Vec2::new(PADDING, HEADER_H + PADDING), Vec2::new(VIEW_W, VIEW_H))
    }

    fn slot_rect(&self, i: usize) -> (Vec2, Vec2) {
        let hs = Self::slot_heights();
        let y = self.position.y + HEADER_H + PADDING + hs[..i].iter().sum::<f32>() + GAP * i as f32;
        (Vec2::new(self.position.x + PADDING * 2.0 + VIEW_W, y), Vec2::new(SIDE_W, hs[i]))
    }

    fn mode_rect(&self, k: usize) -> (Vec2, Vec2) {
        let (pos, size) = self.slot_rect(0);
        let w = (size.x - GAP) / 2.0;
        (pos + Vec2::new((k % 2) as f32 * (w + GAP), (k / 2) as f32 * (BTN_H + GAP)), Vec2::new(w, BTN_H))
    }

    fn slider_track(&self, k: usize) -> (f32, f32, f32) {
        let (pos, size) = self.slot_rect(1 + k);
        (pos.x + LABEL_W, pos.x + size.x - 20.0, pos.y + size.y / 2.0)
    }

    fn half(&self, slot: usize, j: usize) -> (Vec2, Vec2) {
        let (pos, size) = self.slot_rect(slot);
        let w = (size.x - GAP) / 2.0;
        (pos + Vec2::new(j as f32 * (w + GAP), 0.0), Vec2::new(w, size.y))
    }

    fn option_rect(&self) -> (Vec2, Vec2) {
        self.half(5, 0)
    }

    fn play_rect(&self) -> (Vec2, Vec2) {
        self.half(5, 1)
    }

    fn stamp_rect(&self) -> (Vec2, Vec2) {
        self.slot_rect(6)
    }

    // --- Input -----------------------------------------------------------

    pub fn contains(&self, p: Vec2) -> bool {
        rect_contains((self.position, Vec2::new(self.panel_width(), self.panel_height())), p)
    }

    pub fn drag_to(&mut self, p: Vec2, grab_offset: Vec2) {
        self.position = p - grab_offset;
    }

    pub fn press_at(&mut self, p: Vec2, now: f64) -> WavesPress {
        if rect_contains(self.close_rect(), p) {
            return WavesPress::Close;
        }
        if rect_contains(self.header_rect(), p) {
            return WavesPress::Move;
        }
        if rect_contains(self.stamp_rect(), p) {
            return WavesPress::Stamp;
        }
        if let Some(k) = (0..MODES.len()).find(|&k| rect_contains(self.mode_rect(k), p)) {
            self.mode = MODES[k].0;
            return WavesPress::Other;
        }
        for k in 0..3 {
            if rect_contains(self.slot_rect(1 + k), p) {
                self.set_slider(k, p.x);
                return WavesPress::Slider(k);
            }
        }
        if rect_contains(self.option_rect(), p) {
            let i = self.mi();
            self.option[i] = !self.option[i];
        } else if rect_contains(self.play_rect(), p) {
            match self.running_since.take() {
                Some(s) => self.paused_t += (now - s).max(0.0),
                None => self.running_since = Some(now),
            }
        }
        WavesPress::Other
    }

    pub fn set_slider(&mut self, k: usize, x: f32) {
        let s = specs(self.mode)[k];
        let (x0, x1, _) = self.slider_track(k);
        let v = s.min + ((s.max - s.min) * ((x - x0) / (x1 - x0)).clamp(0.0, 1.0) / s.step).round() * s.step;
        let i = self.mi();
        self.values[i][k] = v;
    }

    // --- Scenes ----------------------------------------------------------

    fn scene(&self, rect: (Vec2, Vec2), t: f32, ink: bool) -> Vec<Mark> {
        match self.mode {
            Mode::Periodic => self.periodic_scene(rect, t),
            Mode::Pulse => self.pulse_scene(rect, t),
            Mode::Interference => self.interference_scene(rect, t, ink),
            Mode::Spring => self.spring_scene(rect, t),
            Mode::Pendulum => self.pendulum_scene(rect, t),
            Mode::Standing => self.standing_scene(rect, t),
        }
    }

    /// Rope frame: x (m) → screen, y (m) → screen, around `axis`.
    fn rope_x0(rect: (Vec2, Vec2)) -> f32 {
        rect.0.x + (rect.1.x - ROPE_M * ROPE_PX) / 2.0
    }

    fn periodic_scene(&self, rect: (Vec2, Vec2), t: f32) -> Vec<Mark> {
        let (lambda, f, a) = (self.v(0), self.v(1), self.v(2));
        let k = TAU / lambda;
        let w = TAU * f;
        let x0 = Self::rope_x0(rect);
        let axis = rect.0.y + rect.1.y * 0.52;
        let pt = |x: f32, y: f32| Vec2::new(x0 + x * ROPE_PX, axis - y * ROPE_PX);
        let mut m = Vec::new();
        m.push(Mark::dashed(vec![pt(0.0, 0.0), pt(ROPE_M, 0.0)], 1.2, inked(COLOR_AXIS)));
        let y = |x: f32| a * (k * x - w * t).sin() * -1.0;
        let pts: Vec<Vec2> = (0..=SAMPLES).map(|i| {
            let x = ROPE_M * i as f32 / SAMPLES as f32;
            pt(x, y(x))
        }).collect();
        m.push(Mark::line(pts, 3.0, inked(COLOR_WAVE)));
        // A crest: y = a where k x − ω t = −π/2 + 2πn.
        let first = ((-PI / 2.0 + w * t) / k).rem_euclid(lambda);
        let mut crest = first;
        while crest < 0.4 {
            crest += lambda;
        }
        if crest + lambda <= ROPE_M - 0.2 {
            let top = axis - a * ROPE_PX - 34.0;
            dim(&mut m, Vec2::new(pt(crest, 0.0).x, top), Vec2::new(pt(crest + lambda, 0.0).x, top), &format!("λ = {} m", num(lambda, 2)));
            for x in [crest, crest + lambda] {
                m.push(Mark::dashed(vec![pt(x, a), Vec2::new(pt(x, 0.0).x, top - 6.0)], 1.0, inked(COLOR_AXIS)));
            }
        }
        if crest <= ROPE_M - 0.2 {
            let (c0, c1) = (pt(crest, 0.0), pt(crest, a));
            m.push(Mark::arrow((c0 + c1) / 2.0, c1, 1.6, inked(COLOR_WAVE2)));
            m.push(Mark::arrow((c0 + c1) / 2.0, c0, 1.6, inked(COLOR_WAVE2)));
            m.push(Mark::text(format!("A = {} m", num(a, 2)), (c0 + c1) / 2.0 + Vec2::new(10.0, -8.0), 15.0, inked(COLOR_WAVE2), Anchor::Left));
        }
        // Direction of travel.
        let arrow_y = rect.0.y + 70.0;
        let right = rect.0.x + rect.1.x - 40.0;
        m.push(Mark::arrow(Vec2::new(right - 110.0, arrow_y), Vec2::new(right, arrow_y), 2.4, inked(COLOR_LINE)));
        m.push(Mark::text(format!("v = {} m/s", num(lambda * f, 2)), Vec2::new(right - 55.0, arrow_y - 30.0), 15.0, inked(COLOR_LINE), Anchor::Center));
        if self.opt() {
            // One particle of the rope: it only moves up and down.
            let xp = ROPE_M - 1.5;
            let p = pt(xp, y(xp));
            m.push(Mark::dashed(vec![pt(xp, a + 0.15), pt(xp, -a - 0.15)], 1.2, inked(COLOR_DOT)));
            m.push(Mark::Dot { c: p, r: 8.0, style: inked(COLOR_DOT), filled: true });
        }
        m
    }

    fn pulse_scene(&self, rect: (Vec2, Vec2), t: f32) -> Vec<Mark> {
        let (a, w, v) = (self.v(0), self.v(1), self.v(2));
        let fixed = self.opt();
        let period = (2.0 * ROPE_M + w) / v + 0.8;
        let t = t.rem_euclid(period);
        let x0 = Self::rope_x0(rect);
        let axis = rect.0.y + rect.1.y * 0.5;
        let pt = |x: f32, y: f32| Vec2::new(x0 + x * ROPE_PX, axis - y * ROPE_PX);
        let mut m = Vec::new();
        let pts: Vec<Vec2> = (0..=SAMPLES).map(|i| {
            let x = ROPE_M * i as f32 / SAMPLES as f32;
            pt(x, pulse_y(x, t, a, w, v, fixed))
        }).collect();
        m.push(Mark::line(pts, 3.0, inked(COLOR_WAVE)));
        // Hand at the left, the end at the right.
        m.push(Mark::Dot { c: pt(0.0, 0.0), r: 6.0, style: inked(COLOR_LINE), filled: false });
        let end = pt(ROPE_M, pulse_y(ROPE_M, t, a, w, v, fixed));
        let (top, bottom) = (pt(ROPE_M, 1.1), pt(ROPE_M, -1.1));
        if fixed {
            m.push(Mark::line(vec![top + Vec2::new(4.0, 0.0), bottom + Vec2::new(4.0, 0.0)], 3.0, inked(COLOR_LINE)));
            let mut y = top.y;
            while y < bottom.y {
                m.push(Mark::line(vec![Vec2::new(top.x + 4.0, y), Vec2::new(top.x + 16.0, y + 12.0)], 1.4, inked(COLOR_LINE)));
                y += 16.0;
            }
        } else {
            m.push(Mark::line(vec![top + Vec2::new(10.0, 0.0), bottom + Vec2::new(10.0, 0.0)], 2.0, inked(COLOR_LINE)));
            m.push(Mark::line(crate::periodic::circle_points(end + Vec2::new(10.0, 0.0), 9.0, 18), 2.0, inked(COLOR_LINE)));
        }
        // Which pulse is where.
        let inc_center = v * t - w / 2.0;
        let ref_center = 2.0 * ROPE_M - (v * t - w / 2.0);
        if inc_center > 0.3 && inc_center < ROPE_M - w / 2.0 {
            m.push(Mark::text("Gelen atma", pt(inc_center, a) + Vec2::new(0.0, -32.0), 15.0, inked(COLOR_LINE), Anchor::Center));
            m.push(Mark::arrow(pt(inc_center, a) + Vec2::new(-30.0, -44.0), pt(inc_center, a) + Vec2::new(30.0, -44.0), 1.8, inked(COLOR_LINE)));
        }
        if ref_center > w / 2.0 && ref_center < ROPE_M - w / 2.0 {
            let s = if fixed { -1.0 } else { 1.0 };
            let lab = pt(ref_center, s * a) + Vec2::new(0.0, if fixed { 18.0 } else { -32.0 });
            m.push(Mark::text("Yansıyan atma", lab, 15.0, inked(COLOR_WAVE2), Anchor::Center));
            let ay = lab.y + if fixed { 28.0 } else { -12.0 };
            m.push(Mark::arrow(Vec2::new(lab.x + 30.0, ay), Vec2::new(lab.x - 30.0, ay), 1.8, inked(COLOR_WAVE2)));
        }
        m.push(Mark::text(if fixed { "Sabit uç" } else { "Serbest uç" }, Vec2::new(top.x, top.y - 28.0), 15.0, inked(COLOR_LINE), Anchor::Center));
        m
    }

    fn interference_scene(&self, rect: (Vec2, Vec2), t: f32, ink: bool) -> Vec<Mark> {
        let (lambda, d, f) = (self.v(0), self.v(1), self.v(2));
        let center = rect.0 + rect.1 / 2.0 + Vec2::new(0.0, 10.0);
        let s1 = center - Vec2::new(d / 2.0 * TANK_PX, 0.0);
        let s2 = center + Vec2::new(d / 2.0 * TANK_PX, 0.0);
        let inside = |p: Vec2| rect_contains((rect.0 + Vec2::new(4.0, 34.0), rect.1 - Vec2::new(8.0, 38.0)), p);
        let mut m = Vec::new();
        // Crests spreading from both sources.
        let phase = (lambda * f * t).rem_euclid(lambda);
        for s in [s1, s2] {
            let mut r = phase;
            while r < TANK_R {
                if r > 0.15 {
                    let mut seg: Vec<Vec2> = Vec::new();
                    for i in 0..=96 {
                        let a = TAU * i as f32 / 96.0;
                        let p = s + Vec2::new(a.cos(), a.sin()) * r * TANK_PX;
                        if inside(p) {
                            seg.push(p);
                        } else if seg.len() >= 2 {
                            m.push(Mark::line(std::mem::take(&mut seg), 1.3, inked(COLOR_CREST)));
                        } else {
                            seg.clear();
                        }
                    }
                    if seg.len() >= 2 {
                        m.push(Mark::line(seg, 1.3, inked(COLOR_CREST)));
                    }
                }
                r += lambda;
            }
            m.push(Mark::Dot { c: s, r: 6.0, style: inked(COLOR_LINE), filled: !ink });
        }
        m.push(Mark::text("K_1", s1 + Vec2::new(0.0, 12.0), 14.0, inked(COLOR_LINE), Anchor::Center));
        m.push(Mark::text("K_2", s2 + Vec2::new(0.0, 12.0), 14.0, inked(COLOR_LINE), Anchor::Center));
        if self.opt() {
            // |r1 − r2| = Δ: hyperbola branches (foci at the sources).
            let c = d / 2.0;
            let mut curve = |delta: f32, color: [f32; 4], dash: bool, w: f32| {
                for side in [-1.0f32, 1.0] {
                    let a = delta / 2.0;
                    let pts: Vec<Vec2> = if a < 1e-4 {
                        (0..=40).map(|i| center + Vec2::new(0.0, (-TANK_R + 2.0 * TANK_R * i as f32 / 40.0) * TANK_PX)).collect()
                    } else {
                        let b = (c * c - a * a).max(0.0).sqrt();
                        (0..=80).map(|i| {
                            let u = -3.0 + 6.0 * i as f32 / 80.0;
                            center + Vec2::new(side * a * u.cosh(), b * u.sinh()) * TANK_PX
                        }).collect()
                    };
                    let kept: Vec<Vec2> = pts.into_iter().filter(|p| inside(*p)).collect();
                    if kept.len() >= 2 {
                        m.push(Mark::Line { pts: kept, w, style: inked(color), dash });
                    }
                    if a < 1e-4 {
                        break;
                    }
                }
            };
            let mut n = 0;
            while (n as f32 + 0.5) * lambda < d - 1e-4 {
                curve((n as f32 + 0.5) * lambda, COLOR_NODE, true, 2.2);
                n += 1;
            }
            let mut n = 0;
            while (n as f32) * lambda < d - 1e-4 {
                curve(n as f32 * lambda, COLOR_ANTI, false, 1.6);
                n += 1;
            }
            let (nodal, anti) = line_counts(d, lambda);
            m.push(Mark::text(format!("{nodal} düğüm çizgisi"), rect.0 + Vec2::new(16.0, 44.0), 15.0, inked(COLOR_NODE), Anchor::Left));
            m.push(Mark::text(format!("{anti} katar çizgisi"), rect.0 + Vec2::new(16.0, 66.0), 15.0, inked(COLOR_ANTI), Anchor::Left));
        }
        dim(&mut m, s1 + Vec2::new(0.0, 40.0), s2 + Vec2::new(0.0, 40.0), &format!("d = {} cm", num(d, 1)));
        m
    }

    /// A small x–t (or θ–t) trace of `x(t)` over the last `TRACE_S`.
    fn trace(m: &mut Vec<Mark>, area: (Vec2, Vec2), t: f32, amp: f32, x: impl Fn(f32) -> f32, label: &str) {
        let (p0, size) = area;
        let mid = p0.y + size.y / 2.0;
        m.push(Mark::arrow(Vec2::new(p0.x, p0.y + size.y), Vec2::new(p0.x, p0.y - 6.0), 1.4, inked(COLOR_AXIS)));
        m.push(Mark::arrow(Vec2::new(p0.x, mid), Vec2::new(p0.x + size.x + 10.0, mid), 1.4, inked(COLOR_AXIS)));
        m.push(Mark::text(label, Vec2::new(p0.x + 8.0, p0.y - 8.0), 13.0, inked(COLOR_LINE), Anchor::Left));
        m.push(Mark::text("t", Vec2::new(p0.x + size.x + 14.0, mid - 7.0), 13.0, inked(COLOR_LINE), Anchor::Left));
        let t0 = (t - TRACE_S).max(0.0);
        let span = t - t0;
        if span <= 1e-3 {
            return;
        }
        let pts: Vec<Vec2> = (0..=SAMPLES / 2)
            .map(|i| {
                let tt = t0 + span * i as f32 / (SAMPLES / 2) as f32;
                Vec2::new(p0.x + (tt - t0) / TRACE_S * size.x, mid - x(tt) / amp * size.y * 0.45)
            })
            .collect();
        let last = *pts.last().unwrap();
        m.push(Mark::line(pts, 2.2, inked(COLOR_WAVE)));
        m.push(Mark::Dot { c: last, r: 4.5, style: inked(COLOR_DOT), filled: true });
    }

    fn spring_scene(&self, rect: (Vec2, Vec2), t: f32) -> Vec<Mark> {
        let (mass, k, a) = (self.v(0), self.v(1), self.v(2));
        let w = (k / mass).sqrt();
        let x = a * (w * t).cos();
        let mut m = Vec::new();
        let floor = rect.0.y + rect.1.y * 0.42;
        let wall = rect.0.x + 60.0;
        let eq = wall + 380.0;
        let block = Vec2::new(90.0, 70.0);
        let face = eq + x * SPRING_PX - block.x / 2.0;
        // Floor and wall.
        m.push(Mark::line(vec![Vec2::new(wall, floor), Vec2::new(rect.0.x + rect.1.x - 40.0, floor)], 2.4, inked(COLOR_LINE)));
        m.push(Mark::line(vec![Vec2::new(wall, floor), Vec2::new(wall, floor - 120.0)], 3.0, inked(COLOR_LINE)));
        let mut y = floor - 120.0;
        while y < floor {
            m.push(Mark::line(vec![Vec2::new(wall, y), Vec2::new(wall - 12.0, y + 12.0)], 1.4, inked(COLOR_LINE)));
            y += 16.0;
        }
        // Spring.
        let sy = floor - block.y / 2.0;
        let coils = 14;
        let mut spring = vec![Vec2::new(wall, sy), Vec2::new(wall + 16.0, sy)];
        let (s0, s1) = (wall + 16.0, face - 12.0);
        for i in 0..coils {
            let tt = (i as f32 + 0.5) / coils as f32;
            spring.push(Vec2::new(s0 + (s1 - s0) * tt, sy + if i % 2 == 0 { -14.0 } else { 14.0 }));
        }
        spring.push(Vec2::new(s1, sy));
        spring.push(Vec2::new(face, sy));
        m.push(Mark::line(spring, 2.2, inked(COLOR_LINE)));
        // Block.
        let (tl, br) = (Vec2::new(face, floor - block.y), Vec2::new(face + block.x, floor));
        m.push(Mark::line(vec![tl, Vec2::new(br.x, tl.y), br, Vec2::new(tl.x, br.y), tl], 2.6, inked(COLOR_WAVE2)));
        m.push(Mark::text(format!("{} kg", num(mass, 1)), (tl + br) / 2.0 - Vec2::new(0.0, 8.0), 15.0, inked(COLOR_WAVE2), Anchor::Center));
        // Equilibrium and ±A marks.
        for (xx, name) in [(-a, "-A"), (0.0, "O"), (a, "+A")] {
            let px = eq + xx * SPRING_PX;
            m.push(Mark::dashed(vec![Vec2::new(px, floor - block.y - 30.0), Vec2::new(px, floor + 14.0)], 1.0, inked(COLOR_AXIS)));
            m.push(Mark::text(name, Vec2::new(px, floor + 18.0), 14.0, inked(COLOR_LINE), Anchor::Center));
        }
        // Velocity and restoring force on the block.
        let vel = -a * w * (w * t).sin();
        let top = Vec2::new(face + block.x / 2.0, floor - block.y - 16.0);
        let vmax = a * w;
        if vel.abs() > vmax * 0.04 {
            m.push(Mark::arrow(top, top + Vec2::new(vel / vmax * 70.0, 0.0), 2.6, inked(COLOR_ANTI)));
            m.push(Mark::text("v", top + Vec2::new(vel / vmax * 70.0 + vel.signum() * 10.0, -9.0), 15.0, inked(COLOR_ANTI), if vel > 0.0 { Anchor::Left } else { Anchor::Right }));
        }
        let fy = floor - block.y / 2.0;
        if x.abs() > a * 0.04 {
            let fx = -x / a * 70.0;
            let start = Vec2::new(if fx > 0.0 { br.x + 6.0 } else { tl.x - 6.0 }, fy);
            m.push(Mark::arrow(start, start + Vec2::new(fx, 0.0), 2.6, inked(COLOR_NODE)));
            m.push(Mark::text("F", start + Vec2::new(fx + fx.signum() * 10.0, -9.0), 15.0, inked(COLOR_NODE), if fx > 0.0 { Anchor::Left } else { Anchor::Right }));
        }
        // Energy bars.
        if self.opt() {
            let e = 0.5 * k * (a / 100.0).powi(2);
            let ep = 0.5 * k * (x / 100.0).powi(2);
            let ek = e - ep;
            let base = Vec2::new(rect.0.x + rect.1.x - 150.0, floor - 30.0);
            let full = 150.0;
            for (j, (val, name, color)) in [(ek, "E_k", COLOR_ANTI), (ep, "E_p", COLOR_NODE)].into_iter().enumerate() {
                let x0 = base.x + j as f32 * 56.0;
                let h = if e > 0.0 { val / e * full } else { 0.0 };
                let (bl, tr) = (Vec2::new(x0, base.y), Vec2::new(x0 + 34.0, base.y - h));
                m.push(Mark::line(vec![bl, Vec2::new(bl.x, base.y - full), Vec2::new(tr.x, base.y - full), Vec2::new(tr.x, bl.y), bl], 1.2, inked(COLOR_AXIS)));
                m.push(Mark::Tri { a: bl, b: Vec2::new(tr.x, bl.y), c: tr, style: screen(color) });
                m.push(Mark::Tri { a: bl, b: tr, c: Vec2::new(bl.x, tr.y), style: screen(color) });
                m.push(Mark::line(vec![Vec2::new(bl.x, tr.y), tr], 2.4, inked(color)));
                m.push(Mark::text(name, Vec2::new(x0 + 17.0, base.y + 8.0), 14.0, inked(color), Anchor::Center));
            }
        }
        let area = (Vec2::new(rect.0.x + 70.0, floor + 80.0), Vec2::new(rect.1.x - 150.0, rect.0.y + rect.1.y - floor - 120.0));
        Self::trace(&mut m, area, t, a, |tt| a * (w * tt).cos(), "x");
        m
    }

    fn pendulum_scene(&self, rect: (Vec2, Vec2), t: f32) -> Vec<Mark> {
        let (l, theta0, mass) = (self.v(0), self.v(1), self.v(2));
        let g = self.g();
        let w = (g / l).sqrt();
        let th = theta0.to_radians() * (w * t).cos();
        let mut m = Vec::new();
        let pivot = Vec2::new(rect.0.x + rect.1.x * 0.36, rect.0.y + 60.0);
        let px_per_m = 330.0 / 2.0;
        let len = l * px_per_m;
        // Ceiling.
        m.push(Mark::line(vec![pivot - Vec2::new(60.0, 0.0), pivot + Vec2::new(60.0, 0.0)], 3.0, inked(COLOR_LINE)));
        let mut x = pivot.x - 56.0;
        while x < pivot.x + 60.0 {
            m.push(Mark::line(vec![Vec2::new(x, pivot.y), Vec2::new(x + 10.0, pivot.y - 10.0)], 1.4, inked(COLOR_LINE)));
            x += 14.0;
        }
        m.push(Mark::dashed(vec![pivot, pivot + Vec2::new(0.0, len + 30.0)], 1.0, inked(COLOR_AXIS)));
        // Swing arc between the turning points.
        let t0 = theta0.to_radians();
        let arc: Vec<Vec2> = (0..=30).map(|i| {
            let a = -t0 + 2.0 * t0 * i as f32 / 30.0;
            pivot + Vec2::new(a.sin(), a.cos()) * len
        }).collect();
        m.push(Mark::dashed(arc, 1.2, inked(COLOR_AXIS)));
        let bob = pivot + Vec2::new(th.sin(), th.cos()) * len;
        m.push(Mark::line(vec![pivot, bob], 2.2, inked(COLOR_LINE)));
        let r = 10.0 + 8.0 * mass;
        m.push(Mark::Dot { c: bob, r, style: inked(COLOR_WAVE2), filled: true });
        m.push(Mark::line(crate::periodic::circle_points(bob, r, 20), 2.0, inked(COLOR_WAVE2)));
        // Amplitude angle at the pivot.
        let a_arc: Vec<Vec2> = (0..=12).map(|i| {
            let a = t0 * i as f32 / 12.0;
            pivot + Vec2::new(a.sin(), a.cos()) * 60.0
        }).collect();
        m.push(Mark::line(a_arc, 1.4, inked(COLOR_LINE)));
        m.push(Mark::text(format!("θ_0 = {}°", num(theta0, 0)), pivot + Vec2::new(18.0, 66.0), 14.0, inked(COLOR_LINE), Anchor::Left));
        m.push(Mark::text(format!("L = {} m", num(l, 1)), pivot + Vec2::new(-14.0, len * 0.5), 15.0, inked(COLOR_LINE), Anchor::Right));
        let area = (Vec2::new(rect.0.x + rect.1.x * 0.62, rect.0.y + 120.0), Vec2::new(rect.1.x * 0.32, 220.0));
        Self::trace(&mut m, area, t, t0, |tt| t0 * (w * tt).cos(), "θ");
        m
    }

    fn standing_scene(&self, rect: (Vec2, Vec2), t: f32) -> Vec<Mark> {
        let (n, _v, a_cm) = (self.v(0).round().max(1.0), self.v(1), self.v(2));
        let amp = STANDING_PX * a_cm / 5.0;
        let x0 = rect.0.x + 60.0;
        let len = rect.1.x - 120.0;
        let axis = rect.0.y + rect.1.y * 0.5;
        // Shown slowed down to one swing a second.
        let c = (TAU * 0.8 * t).cos();
        let mut m = Vec::new();
        for x in [x0, x0 + len] {
            m.push(Mark::line(vec![Vec2::new(x, axis - amp - 40.0), Vec2::new(x, axis + amp + 40.0)], 3.0, inked(COLOR_LINE)));
        }
        let shape = |s: f32, factor: f32| -> Vec<Vec2> {
            (0..=SAMPLES).map(|i| {
                let u = i as f32 / SAMPLES as f32;
                Vec2::new(x0 + u * len, axis - s * factor * amp * (n * PI * u).sin())
            }).collect()
        };
        if self.opt() {
            m.push(Mark::dashed(shape(1.0, 1.0), 1.2, inked(COLOR_AXIS)));
            m.push(Mark::dashed(shape(-1.0, 1.0), 1.2, inked(COLOR_AXIS)));
        }
        m.push(Mark::line(shape(1.0, c), 3.0, inked(COLOR_WAVE)));
        let nn = n as usize;
        for j in 0..=nn {
            let p = Vec2::new(x0 + len * j as f32 / n, axis);
            m.push(Mark::Dot { c: p, r: 5.0, style: inked(COLOR_NODE), filled: true });
            m.push(Mark::text("D", p + Vec2::new(0.0, amp + 18.0), 14.0, inked(COLOR_NODE), Anchor::Center));
        }
        for j in 0..nn {
            let p = Vec2::new(x0 + len * (j as f32 + 0.5) / n, axis);
            m.push(Mark::text("K", p + Vec2::new(0.0, -amp - 34.0), 14.0, inked(COLOR_ANTI), Anchor::Center));
        }
        // One wavelength = two loops.
        if nn >= 2 {
            let top = axis + amp + 78.0;
            dim(&mut m, Vec2::new(x0, top), Vec2::new(x0 + 2.0 * len / n, top), &format!("λ = {} m", num(2.0 * STRING_L / n, 2)));
        }
        let lab_y = axis + amp + 140.0;
        dim(&mut m, Vec2::new(x0, lab_y), Vec2::new(x0 + len, lab_y), &format!("L = {} m", num(STRING_L, 0)));
        m
    }

    // --- Ink -------------------------------------------------------------

    pub fn stamp_lines(&self, screen_size: Vec2, ink: [f32; 4], now: f64) -> Vec<InkLine> {
        let size = Vec2::new(STAMP_W.min(screen_size.x - 160.0), STAMP_H.min(screen_size.y - 240.0));
        let top_left = Vec2::new((screen_size.x - size.x) / 2.0, ((screen_size.y - 130.0 - size.y) / 2.0).max(20.0));
        marks::to_ink(&self.scene((top_left, size), self.time(now), true), ink)
    }

    pub fn mode_name(&self) -> &'static str {
        MODES[self.mi()].1
    }

    // --- Drawing ---------------------------------------------------------

    fn result_lines(&self) -> Vec<(String, [f32; 4])> {
        let (a, b, c) = (self.v(0), self.v(1), self.v(2));
        match self.mode {
            Mode::Periodic => vec![
                (format!("Hız: v = λ·f = {} m/s", num(a * b, 2)), COLOR_TEXT),
                (format!("Periyot: T = 1/f = {} s", num(1.0 / b, 2)), COLOR_TEXT),
                ("Enine dalga: parçacıklar yukarı aşağı".into(), COLOR_TEXT_DIM),
                ("titreşir, dalga sağa ilerler.".into(), COLOR_TEXT_DIM),
            ],
            Mode::Pulse => vec![
                (format!("Atmanın hızı: v = {} m/s", num(c, 1)), COLOR_TEXT),
                (if self.opt() { "Sabit uçtan yansıyan atma ters döner." } else { "Serbest uçtan yansıyan atma düz döner." }.into(), COLOR_WAVE2),
                ("Yansımada hız, genişlik ve genlik".into(), COLOR_TEXT_DIM),
                ("değişmez; üst üste binen atmalar toplanır.".into(), COLOR_TEXT_DIM),
            ],
            Mode::Interference => {
                let (nodal, anti) = line_counts(b, a);
                vec![
                    (format!("Dalga hızı: v = λ·f = {} cm/s", num(a * c, 2)), COLOR_TEXT),
                    (format!("Düğüm çizgisi: {nodal}   Katar çizgisi: {anti}"), COLOR_TEXT),
                    ("Düğüm: |K_1P − K_2P| = (n − 1/2)λ".into(), COLOR_NODE),
                    ("Katar: |K_1P − K_2P| = nλ".into(), COLOR_ANTI),
                ]
            }
            Mode::Spring => {
                let t = spring_period(a, b);
                let w = TAU / t;
                vec![
                    (format!("T = 2π√(m/k) = {} s", num(t, 2)), COLOR_TEXT),
                    (format!("Frekans: f = {} Hz", num(1.0 / t, 2)), COLOR_TEXT),
                    (format!("v_{{max}} = Aω = {} m/s", num(c / 100.0 * w, 2)), COLOR_ANTI),
                    (format!("Enerji: E = kA^2/2 = {} J", num(0.5 * b * (c / 100.0).powi(2), 3)), COLOR_TEXT),
                ]
            }
            Mode::Pendulum => {
                let t = pendulum_period(a, self.g());
                vec![
                    (format!("T = 2π√(L/g) = {} s", num(t, 2)), COLOR_TEXT),
                    (format!("Frekans: f = {} Hz", num(1.0 / t, 2)), COLOR_TEXT),
                    ("Periyot kütleye ve (küçük açılarda)".into(), COLOR_TEXT_DIM),
                    ("genliğe bağlı değildir.".into(), COLOR_TEXT_DIM),
                ]
            }
            Mode::Standing => {
                let n = a.round().max(1.0);
                vec![
                    (format!("λ = 2L/n = {} m", num(2.0 * STRING_L / n, 2)), COLOR_TEXT),
                    (format!("Frekans: f = n·v/2L = {} Hz", num(n * b / (2.0 * STRING_L), 1)), COLOR_TEXT),
                    (format!("Düğüm (D): {}   Karın (K): {}", n as usize + 1, n as usize), COLOR_TEXT),
                    ("Görüntü yavaşlatılmıştır.".into(), COLOR_TEXT_DIM),
                ]
            }
        }
    }

    pub fn render(
        &self,
        now: f64,
        font_atlas: &FontAtlas,
        out_vertices: &mut Vec<Vertex>,
        out_indices: &mut Vec<u32>,
        glyph_vertices: &mut Vec<GlyphVertex>,
        glyph_indices: &mut Vec<u32>,
    ) {
        let (v, i, gv, gi) = (out_vertices, out_indices, glyph_vertices, glyph_indices);
        push_rect(self.position, Vec2::new(self.panel_width(), self.panel_height()), COLOR_PANEL_BG, v, i);
        let (header_pos, header_size) = self.header_rect();
        push_rect(header_pos, header_size, COLOR_HEADER, v, i);
        font_atlas.push_text("Dalgalar ve titreşim", header_pos + Vec2::new(PADDING, 10.0), 16.0, COLOR_TEXT, gv, gi);
        let grip = header_pos + header_size / 2.0;
        for k in -1..=1 {
            push_circle(Vec2::new(grip.x + k as f32 * 10.0, grip.y), 2.0, COLOR_TEXT, 8, v, i);
        }
        let (close_pos, close_size) = self.close_rect();
        let c = close_pos + close_size / 2.0;
        let r = close_size.x * 0.2;
        push_line(c - Vec2::splat(r), c + Vec2::splat(r), 2.5, COLOR_TEXT, v, i);
        push_line(c + Vec2::new(-r, r), c + Vec2::new(r, -r), 2.5, COLOR_TEXT, v, i);

        let view = self.view_rect();
        push_rect(view.0, view.1, COLOR_FIELD, v, i);
        let t = self.time(now);
        marks::draw(&self.scene(view, t, false), font_atlas, v, i, gv, gi);
        font_atlas.push_text(&format!("t = {} s", num(t, 1)), view.0 + Vec2::new(16.0, 14.0), 14.0, COLOR_TEXT_DIM, gv, gi);

        for (k, (mode, name)) in MODES.iter().enumerate() {
            button(self.mode_rect(k), name, if *mode == self.mode { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE }, font_atlas, v, i, gv, gi);
        }
        for (k, s) in specs(self.mode).iter().enumerate() {
            let (pos, size) = self.slot_rect(1 + k);
            push_rect(pos, size, COLOR_FIELD, v, i);
            let val = self.v(k);
            let unit = if s.unit.is_empty() || s.unit == "°" { s.unit.to_string() } else { format!(" {}", s.unit) };
            push_rich(font_atlas, &format!("{} = {}{unit}", s.label, num(val, s.decimals)), pos + Vec2::new(12.0, 14.0), 16.0, COLOR_TEXT, Anchor::Left, gv, gi);
            let (x0, x1, y) = self.slider_track(k);
            let x = x0 + (x1 - x0) * (val - s.min) / (s.max - s.min);
            push_line(Vec2::new(x0, y), Vec2::new(x1, y), 4.0, COLOR_KEY_IDLE, v, i);
            push_line(Vec2::new(x0, y), Vec2::new(x, y), 4.0, COLOR_SLIDER, v, i);
            push_circle(Vec2::new(x, y), 11.0, COLOR_TEXT, 20, v, i);
        }
        let (rpos, rsize) = self.slot_rect(4);
        push_rect(rpos, rsize, COLOR_FIELD, v, i);
        for (k, (line, color)) in self.result_lines().iter().enumerate() {
            push_rich(font_atlas, line, rpos + Vec2::new(12.0, 12.0 + k as f32 * 26.0), 15.0, *color, Anchor::Left, gv, gi);
        }
        let toggles = matches!(self.mode, Mode::Periodic | Mode::Interference | Mode::Spring | Mode::Standing);
        button(self.option_rect(), self.option_label(), if toggles && self.opt() { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE }, font_atlas, v, i, gv, gi);
        let running = self.running_since.is_some();
        button(self.play_rect(), if running { "Durdur" } else { "Başlat" }, if running { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE }, font_atlas, v, i, gv, gi);
        button(self.stamp_rect(), "Tahtaya bas", COLOR_KEY_STAMP, font_atlas, v, i, gv, gi);
    }
}

/// A dimension line a↔b with a centered label above.
fn dim(m: &mut Vec<Mark>, a: Vec2, b: Vec2, text: &str) {
    let mid = (a + b) / 2.0;
    m.push(Mark::arrow(mid, a, 1.4, inked(COLOR_LINE)));
    m.push(Mark::arrow(mid, b, 1.4, inked(COLOR_LINE)));
    m.push(Mark::text(text, mid + Vec2::new(0.0, -24.0), 15.0, inked(COLOR_LINE), Anchor::Center));
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
    let mut h = 15.0;
    while font_atlas.measure(label, h) > size.x - 10.0 && h > 11.0 {
        h -= 0.5;
    }
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
    fn formulas() {
        // m = 0,5 kg, k = 20 N/m: T = 2π·0,158 = 0,993 s.
        assert!((spring_period(0.5, 20.0) - 0.9935).abs() < 1e-3);
        // L = 1 m on Earth: 2,007 s; on the Moon much longer.
        assert!((pendulum_period(1.0, 9.8) - 2.0071).abs() < 1e-3);
        assert!(pendulum_period(1.0, 1.6) > 4.9);
        // d = 6, λ = 2: |Δ| = 1, 3, 5 → 6 nodal lines; 0, ±2, ±4 → 5 antinodal.
        assert_eq!(line_counts(6.0, 2.0), (6, 5));
        // d = λ exactly: one nodal pair, only the center antinode.
        assert_eq!(line_counts(2.0, 2.0), (2, 1));
    }

    #[test]
    fn pulse_reflects_inverted_from_a_fixed_end() {
        let (a, w, v) = (0.5, 1.0, 2.0);
        // On the way in: upright at x = 3 once its middle gets there.
        let t_in = (3.0 + w / 2.0) / v;
        assert!((pulse_y(3.0, t_in, a, w, v, true) - a).abs() < 1e-4);
        // On the way back to x = 3: inverted (fixed) or upright (free).
        let t_back = (2.0 * ROPE_M - 3.0 + w / 2.0) / v;
        assert!((pulse_y(3.0, t_back, a, w, v, true) + a).abs() < 1e-4);
        assert!((pulse_y(3.0, t_back, a, w, v, false) - a).abs() < 1e-4);
        // The fixed end never moves; the free end swings to twice the height.
        let t_end = (ROPE_M + w / 2.0) / v;
        assert!(pulse_y(ROPE_M, t_end, a, w, v, true).abs() < 1e-4);
        assert!((pulse_y(ROPE_M, t_end, a, w, v, false) - 2.0 * a).abs() < 1e-4);
    }

    #[test]
    fn panel_runs_pauses_and_stamps() {
        let mut wv = Waves::new(SCREEN);
        assert!(wv.position.y + wv.panel_height() <= SCREEN.y - 75.0);
        assert!(Waves::slot_heights()[4] > 110.0);
        wv.start(10.0);
        assert!(wv.is_animating(10.0));
        let c = |r: (Vec2, Vec2)| r.0 + r.1 / 2.0;
        wv.press_at(c(wv.play_rect()), 12.0);
        assert!(!wv.is_animating(12.0));
        assert!((wv.time(50.0) - 2.0).abs() < 1e-4, "paused at 2 s");
        wv.press_at(c(wv.play_rect()), 60.0);
        assert!((wv.time(61.0) - 3.0).abs() < 1e-4);
        // Sliders snap to their step.
        let (x0, x1, y) = wv.slider_track(0);
        assert_eq!(wv.press_at(Vec2::new(x0 + (x1 - x0) * 0.33, y), 0.0), WavesPress::Slider(0));
        assert_eq!(wv.v(0), 1.75);
        for k in 0..MODES.len() {
            wv.mode = MODES[k].0;
            for opt in [false, true] {
                wv.option[k] = opt;
                for t in [0.0, 0.7, 3.3] {
                    let marks = wv.scene((Vec2::new(510.0, 200.0), Vec2::new(STAMP_W, STAMP_H)), t, true);
                    assert_eq!(marks::missing_glyphs(&marks), vec![], "{:?}", wv.mode);
                }
                let lines = wv.stamp_lines(SCREEN, [1.0; 4], 61.0);
                assert!(lines.len() > 3, "{:?}", wv.mode);
                for l in &lines {
                    for p in &l.points {
                        assert!(p.x >= 0.0 && p.x <= SCREEN.x && p.y >= 0.0 && p.y <= SCREEN.y - 70.0, "{:?}: {p:?}", wv.mode);
                    }
                }
            }
        }
    }
}
