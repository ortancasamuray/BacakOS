//! Draggable "Atış hareketleri" panel (physics): free fall, vertical,
//! horizontal and oblique projectile motion without air resistance.
//!
//! The teacher picks a kind, sets v₀ / θ / h with sliders and a planet's
//! g, and gets the path drawn to scale (1 m is the same length on both
//! axes) with the ball's position at equal time steps, its velocity and
//! components at any moment (play, or drag a finger along the path), the
//! x-t, y-t, vₓ-t, v_y-t graphs, and the flight time / range / peak /
//! impact speed. "Tahtaya bas" stamps the path (or the graphs) as a
//! movable, resizable ink figure.
//!
//! Both the screen and the ink are drawn from one list of `Mark`s, so the
//! stamp is exactly what the panel shows (minus the grid and tints).

use glam::Vec2;

use crate::font_atlas::{FontAtlas, GlyphVertex};
use crate::ink::InkLine;
use crate::marks::{self, layout_text, Anchor, Mark, Style};
use crate::stroke::{push_circle, push_line, push_rect, Vertex};

const HEADER_H: f32 = 36.0;
const PADDING: f32 = 12.0;
const GAP: f32 = 6.0;
const VIEW_W: f32 = 820.0;
const SIDE_W: f32 = 340.0;
const BTN_H: f32 = 44.0;
const RESULT_H: f32 = 112.0;
/// Slider label column width.
const LABEL_W: f32 = 150.0;
/// A real flight longer than this plays faster (seconds).
const PLAY_SECONDS: f32 = 6.0;
/// Path samples (drawing, finger hit test).
const SAMPLES: usize = 160;
/// Longest velocity arrow (px).
const ARROW_MAX: f32 = 120.0;
/// Width of the drawn tower/cliff a launch from height stands on (px).
const TOWER_W: f32 = 28.0;
/// Sideways gap between the way up and the way down of a vertical throw (px).
const VERTICAL_SPLIT: f32 = 26.0;
/// Stamp size on the board (px), shrunk to fit the screen.
const STAMP_W: f32 = 1000.0;
const STAMP_H: f32 = 560.0;

const COLOR_PANEL_BG: [f32; 4] = [0.12, 0.13, 0.17, 0.96];
const COLOR_HEADER: [f32; 4] = [0.18, 0.19, 0.24, 1.0];
const COLOR_FIELD: [f32; 4] = [0.06, 0.07, 0.09, 1.0];
const COLOR_TEXT: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_TEXT_DIM: [f32; 4] = [0.95, 0.95, 0.97, 0.55];
const COLOR_GRID: [f32; 4] = [1.0, 1.0, 1.0, 0.08];
const COLOR_LINE: [f32; 4] = [0.85, 0.87, 0.92, 1.0];
const COLOR_PATH_AHEAD: [f32; 4] = [0.98, 0.80, 0.30, 0.35];
const COLOR_PATH: [f32; 4] = [0.98, 0.80, 0.30, 1.0];
const COLOR_BALL: [f32; 4] = [0.98, 0.45, 0.25, 1.0];
const COLOR_V: [f32; 4] = [0.98, 0.45, 0.25, 1.0];
const COLOR_VX: [f32; 4] = [0.35, 0.65, 1.0, 1.0];
const COLOR_VY: [f32; 4] = [0.35, 0.85, 0.45, 1.0];
const COLOR_AREA: [f32; 4] = [0.35, 0.65, 1.0, 0.25];
const COLOR_KEY_IDLE: [f32; 4] = [0.22, 0.24, 0.30, 1.0];
const COLOR_KEY_ACTIVE: [f32; 4] = [0.23, 0.51, 0.96, 1.0];
const COLOR_KEY_STAMP: [f32; 4] = [0.98, 0.55, 0.15, 1.0];
const COLOR_SLIDER: [f32; 4] = [0.45, 0.72, 1.0, 1.0];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    FreeFall,
    Vertical,
    Horizontal,
    Oblique,
}

const KINDS: [(Kind, &str); 4] =
    [(Kind::FreeFall, "Serbest düşme"), (Kind::Vertical, "Düşey atış"), (Kind::Horizontal, "Yatay atış"), (Kind::Oblique, "Eğik atış")];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Param {
    Speed,
    Angle,
    Height,
}

const PARAMS: [Param; 3] = [Param::Speed, Param::Angle, Param::Height];

impl Param {
    fn range(self) -> (f32, f32) {
        match self {
            Param::Speed => (0.0, 50.0),
            Param::Angle => (0.0, 90.0),
            Param::Height => (0.0, 100.0),
        }
    }
}

const PLANETS: [(&str, f32); 4] = [("Dünya", 9.8), ("Ay", 1.6), ("Mars", 3.7), ("Jüpiter", 24.8)];

#[derive(Clone, Copy, Debug)]
struct Params {
    v0: f32,
    angle: f32,
    h: f32,
}

/// One launch, solved: from (0, h) with velocity (vx, vy0), until it
/// lands on y = 0.
#[derive(Clone, Copy, Debug)]
pub struct Shot {
    pub vx: f32,
    pub vy0: f32,
    pub h: f32,
    pub g: f32,
    pub t_end: f32,
}

impl Shot {
    fn new(kind: Kind, p: Params, g: f32) -> Self {
        let (vx, vy0) = match kind {
            Kind::FreeFall => (0.0, 0.0),
            Kind::Vertical => (0.0, p.v0),
            Kind::Horizontal => (p.v0, 0.0),
            Kind::Oblique => {
                let a = p.angle.to_radians();
                (p.v0 * a.cos(), p.v0 * a.sin())
            }
        };
        let t_end = (vy0 + (vy0 * vy0 + 2.0 * g * p.h).sqrt()) / g;
        Self { vx, vy0, h: p.h, g, t_end }
    }

    pub fn pos(&self, t: f32) -> Vec2 {
        Vec2::new(self.vx * t, self.h + self.vy0 * t - 0.5 * self.g * t * t)
    }

    pub fn vel(&self, t: f32) -> Vec2 {
        Vec2::new(self.vx, self.vy0 - self.g * t)
    }

    pub fn range(&self) -> f32 {
        self.vx * self.t_end
    }

    pub fn peak(&self) -> f32 {
        self.h + self.vy0.max(0.0).powi(2) / (2.0 * self.g)
    }

    fn t_peak(&self) -> f32 {
        (self.vy0 / self.g).max(0.0)
    }

    pub fn impact_speed(&self) -> f32 {
        self.vel(self.t_end).length()
    }

    /// Fastest moment: |v|² is quadratic in t, so at an end.
    fn max_speed(&self) -> f32 {
        self.vel(0.0).length().max(self.impact_speed())
    }

    fn is_empty(&self) -> bool {
        self.t_end <= 1e-4
    }
}

/// "1, 2, 5 × 10ⁿ" step at least `raw`.
fn nice(raw: f32) -> f32 {
    let exp = 10f32.powf(raw.max(1e-6).log10().floor());
    let f = raw / exp;
    exp * if f <= 1.0 { 1.0 } else if f <= 2.0 { 2.0 } else if f <= 5.0 { 5.0 } else { 10.0 }
}

/// Time between the equal-interval positions: about 6-10 of them.
fn dot_step(t_end: f32) -> f32 {
    [0.1f32, 0.2, 0.25, 0.5, 1.0, 2.0, 2.5, 5.0, 10.0].into_iter().find(|&c| t_end / c <= 10.0).unwrap_or(20.0)
}

/// Turkish decimal comma, no "-0".
pub fn num(x: f32, decimals: usize) -> String {
    let s = format!("{x:.decimals$}").replace('.', ",");
    if s.starts_with('-') && s[1..].chars().all(|c| c == '0' || c == ',') {
        s[1..].to_string()
    } else {
        s
    }
}

/// Short number for axis labels: no trailing ",0".
fn tick(x: f32) -> String {
    let s = num(x, 2);
    let s = s.trim_end_matches('0').trim_end_matches(',');
    s.to_string()
}

// --- Marks (see `marks.rs`) ------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum Role {
    /// Screen only (grid, tints, the not-yet-flown path).
    Grid,
    GridText,
    Line,
    PathAhead,
    Path,
    Ball,
    V,
    Vx,
    Vy,
    Area,
}

impl From<Role> for Style {
    fn from(role: Role) -> Style {
        let (color, ink) = match role {
            Role::Grid => (COLOR_GRID, false),
            Role::GridText => (COLOR_TEXT_DIM, false),
            Role::Line => (COLOR_LINE, true),
            Role::PathAhead => (COLOR_PATH_AHEAD, false),
            Role::Path => (COLOR_PATH, true),
            Role::Ball => (COLOR_BALL, true),
            Role::V => (COLOR_V, true),
            Role::Vx => (COLOR_VX, true),
            Role::Vy => (COLOR_VY, true),
            Role::Area => (COLOR_AREA, false),
        };
        Style { color, ink }
    }
}

/// World (meters, y up) → screen, same scale on both axes.
struct Frame {
    origin: Vec2,
    scale: f32,
}

impl Frame {
    /// Fits the whole flight into `area`, ground on its bottom edge,
    /// centered across.
    fn fit(shot: &Shot, (pos, size): (Vec2, Vec2)) -> Self {
        let ext = Vec2::new(shot.range().max(1e-3), shot.peak().max(1.0));
        let scale = (size.x / ext.x).min(size.y / ext.y);
        let origin = Vec2::new(pos.x + (size.x - ext.x * scale) / 2.0, pos.y + size.y);
        Self { origin, scale }
    }

    fn pt(&self, w: Vec2) -> Vec2 {
        Vec2::new(self.origin.x + w.x * self.scale, self.origin.y - w.y * self.scale)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MotionPress {
    Move,
    Close,
    Slider(Param),
    /// Finger on the path: the ball follows it (`scrub_to`).
    Scrub,
    Stamp,
    Other,
}

pub struct Motion {
    pub position: Vec2,
    kind: Kind,
    params: [Params; 4],
    planet: usize,
    show_vectors: bool,
    show_dots: bool,
    graphs: bool,
    /// Shown time while paused (s).
    t: f32,
    /// Playing since (input clock, sim time then).
    playing: Option<(f64, f32)>,
}

impl Motion {
    pub fn new(screen_size: Vec2) -> Self {
        let mut m = Self {
            position: Vec2::ZERO,
            kind: Kind::Oblique,
            params: [
                Params { v0: 0.0, angle: 0.0, h: 45.0 },
                Params { v0: 20.0, angle: 90.0, h: 0.0 },
                Params { v0: 15.0, angle: 0.0, h: 45.0 },
                Params { v0: 20.0, angle: 45.0, h: 0.0 },
            ],
            planet: 0,
            show_vectors: true,
            show_dots: true,
            graphs: false,
            t: 0.0,
            playing: None,
        };
        let size = Vec2::new(m.panel_width(), m.panel_height());
        m.position = Vec2::new((screen_size.x - size.x) / 2.0, ((screen_size.y - 120.0 - size.y) / 2.0).max(12.0));
        m
    }

    fn kind_index(&self) -> usize {
        KINDS.iter().position(|(k, _)| *k == self.kind).unwrap_or(0)
    }

    fn p(&self) -> Params {
        self.params[self.kind_index()]
    }

    pub fn shot(&self) -> Shot {
        Shot::new(self.kind, self.p(), PLANETS[self.planet].1)
    }

    fn value(&self, param: Param) -> f32 {
        let p = self.p();
        match param {
            Param::Speed => p.v0,
            Param::Angle => p.angle,
            Param::Height => p.h,
        }
    }

    fn param_active(&self, param: Param) -> bool {
        match param {
            Param::Speed => self.kind != Kind::FreeFall,
            Param::Angle => self.kind == Kind::Oblique,
            Param::Height => true,
        }
    }

    fn speedup(&self) -> f32 {
        (self.shot().t_end / PLAY_SECONDS).max(1.0)
    }

    /// The moment shown at `now`.
    pub fn time(&self, now: f64) -> f32 {
        let end = self.shot().t_end;
        match self.playing {
            Some((start, t0)) => (t0 + (now - start).max(0.0) as f32 * self.speedup()).min(end),
            None => self.t.min(end),
        }
    }

    fn running(&self, now: f64) -> bool {
        self.playing.is_some() && self.time(now) < self.shot().t_end
    }

    pub fn is_animating(&self, now: f64) -> bool {
        self.running(now)
    }

    fn reset_time(&mut self) {
        self.t = 0.0;
        self.playing = None;
    }

    // --- Layout ----------------------------------------------------------

    /// Side column slots: kinds, 3 sliders, planet, results, toggles,
    /// graphs/play, stamp.
    fn slot_heights() -> [f32; 9] {
        [2.0 * BTN_H + GAP, BTN_H, BTN_H, BTN_H, BTN_H, RESULT_H, BTN_H, BTN_H, BTN_H]
    }

    fn view_h() -> f32 {
        Self::slot_heights().iter().sum::<f32>() + GAP * 8.0
    }

    fn panel_width(&self) -> f32 {
        PADDING * 3.0 + VIEW_W + SIDE_W
    }

    fn panel_height(&self) -> f32 {
        HEADER_H + PADDING + Self::view_h() + PADDING
    }

    fn header_rect(&self) -> (Vec2, Vec2) {
        (self.position, Vec2::new(self.panel_width(), HEADER_H))
    }

    fn close_rect(&self) -> (Vec2, Vec2) {
        (self.position + Vec2::new(self.panel_width() - HEADER_H, 0.0), Vec2::splat(HEADER_H))
    }

    fn view_rect(&self) -> (Vec2, Vec2) {
        (self.position + Vec2::new(PADDING, HEADER_H + PADDING), Vec2::new(VIEW_W, Self::view_h()))
    }

    fn slot_rect(&self, i: usize) -> (Vec2, Vec2) {
        let hs = Self::slot_heights();
        let y = self.position.y + HEADER_H + PADDING + hs[..i].iter().sum::<f32>() + GAP * i as f32;
        (Vec2::new(self.position.x + PADDING * 2.0 + VIEW_W, y), Vec2::new(SIDE_W, hs[i]))
    }

    fn half(&self, slot: usize, j: usize) -> (Vec2, Vec2) {
        let (pos, size) = self.slot_rect(slot);
        let w = (size.x - GAP) / 2.0;
        (pos + Vec2::new(j as f32 * (w + GAP), 0.0), Vec2::new(w, size.y))
    }

    fn kind_rect(&self, k: usize) -> (Vec2, Vec2) {
        let (pos, size) = self.slot_rect(0);
        let w = (size.x - GAP) / 2.0;
        (pos + Vec2::new((k % 2) as f32 * (w + GAP), (k / 2) as f32 * (BTN_H + GAP)), Vec2::new(w, BTN_H))
    }

    fn slider_rect(&self, param: Param) -> (Vec2, Vec2) {
        self.slot_rect(1 + PARAMS.iter().position(|p| *p == param).unwrap())
    }

    fn slider_track(&self, param: Param) -> (f32, f32, f32) {
        let (pos, size) = self.slider_rect(param);
        (pos.x + LABEL_W, pos.x + size.x - 20.0, pos.y + size.y / 2.0)
    }

    fn planet_rect(&self) -> (Vec2, Vec2) {
        self.slot_rect(4)
    }

    fn vectors_rect(&self) -> (Vec2, Vec2) {
        self.half(6, 0)
    }

    fn dots_rect(&self) -> (Vec2, Vec2) {
        self.half(6, 1)
    }

    fn graphs_rect(&self) -> (Vec2, Vec2) {
        self.half(7, 0)
    }

    fn play_rect(&self) -> (Vec2, Vec2) {
        self.half(7, 1)
    }

    fn stamp_rect(&self) -> (Vec2, Vec2) {
        self.slot_rect(8)
    }

    // --- Input -----------------------------------------------------------

    pub fn contains(&self, p: Vec2) -> bool {
        rect_contains((self.position, Vec2::new(self.panel_width(), self.panel_height())), p)
    }

    pub fn drag_to(&mut self, p: Vec2, grab_offset: Vec2) {
        self.position = p - grab_offset;
    }

    pub fn press_at(&mut self, p: Vec2, now: f64) -> MotionPress {
        if rect_contains(self.close_rect(), p) {
            return MotionPress::Close;
        }
        if rect_contains(self.header_rect(), p) {
            return MotionPress::Move;
        }
        if rect_contains(self.view_rect(), p) {
            if self.graphs {
                return MotionPress::Other;
            }
            self.scrub_to(p);
            return MotionPress::Scrub;
        }
        if rect_contains(self.stamp_rect(), p) {
            return MotionPress::Stamp;
        }
        if let Some(k) = (0..KINDS.len()).find(|&k| rect_contains(self.kind_rect(k), p)) {
            self.kind = KINDS[k].0;
            self.reset_time();
            return MotionPress::Other;
        }
        for param in PARAMS {
            if rect_contains(self.slider_rect(param), p) && self.param_active(param) {
                self.set_slider(param, p.x);
                return MotionPress::Slider(param);
            }
        }
        if rect_contains(self.planet_rect(), p) {
            self.planet = (self.planet + 1) % PLANETS.len();
            self.reset_time();
        } else if rect_contains(self.vectors_rect(), p) {
            self.show_vectors = !self.show_vectors;
        } else if rect_contains(self.dots_rect(), p) {
            self.show_dots = !self.show_dots;
        } else if rect_contains(self.graphs_rect(), p) {
            self.graphs = !self.graphs;
        } else if rect_contains(self.play_rect(), p) {
            if self.running(now) {
                self.t = self.time(now);
                self.playing = None;
            } else if !self.shot().is_empty() {
                let t = self.time(now);
                let t0 = if t >= self.shot().t_end - 1e-4 { 0.0 } else { t };
                self.playing = Some((now, t0));
            }
        }
        MotionPress::Other
    }

    pub fn set_slider(&mut self, param: Param, x: f32) {
        let (x0, x1, _) = self.slider_track(param);
        let (lo, hi) = param.range();
        let v = (lo + (hi - lo) * ((x - x0) / (x1 - x0)).clamp(0.0, 1.0)).round();
        let k = self.kind_index();
        let p = &mut self.params[k];
        match param {
            Param::Speed => p.v0 = v,
            Param::Angle => p.angle = v,
            Param::Height => p.h = v,
        }
        self.reset_time();
    }

    fn area(rect: (Vec2, Vec2)) -> (Vec2, Vec2) {
        (rect.0 + Vec2::new(64.0, 56.0), rect.1 - Vec2::new(64.0 + 44.0, 56.0 + 44.0))
    }

    /// Moves the ball to the point of the path nearest the finger.
    pub fn scrub_to(&mut self, p: Vec2) {
        self.playing = None;
        let shot = self.shot();
        if shot.is_empty() {
            return;
        }
        let f = Frame::fit(&shot, Self::area(self.view_rect()));
        let best = (0..=SAMPLES)
            .map(|i| i as f32 / SAMPLES as f32 * shot.t_end)
            .min_by(|&a, &b| Self::screen_pos(&shot, &f, a).distance_squared(p).total_cmp(&Self::screen_pos(&shot, &f, b).distance_squared(p)))
            .unwrap_or(0.0);
        self.t = best;
    }

    // --- Scene -----------------------------------------------------------

    /// Where the ball is drawn at `t`. A straight up-and-down flight comes
    /// back down a little to the side (as textbooks draw it), so the way up
    /// and the way down don't cover each other.
    fn screen_pos(shot: &Shot, f: &Frame, t: f32) -> Vec2 {
        let p = f.pt(shot.pos(t));
        if shot.vx == 0.0 && shot.vy0 > 0.0 {
            let w = (shot.t_end * 0.06).max(1e-3);
            let s = ((t - shot.t_peak() + w) / (2.0 * w)).clamp(0.0, 1.0);
            p + Vec2::new(VERTICAL_SPLIT * s * s * (3.0 - 2.0 * s), 0.0)
        } else {
            p
        }
    }

    fn path_scene(&self, rect: (Vec2, Vec2), t: f32, ink: bool) -> Vec<Mark> {
        let shot = self.shot();
        let area = Self::area(rect);
        let f = Frame::fit(&shot, area);
        let mut m = Vec::new();
        let (a0, a1) = (area.0, area.0 + area.1);
        let ground_y = f.origin.y;

        if !ink {
            let step = nice(70.0 / f.scale);
            let x_lo = (a0.x - 40.0 - f.origin.x) / f.scale;
            let x_hi = (a1.x + 30.0 - f.origin.x) / f.scale;
            let y_hi = (f.origin.y - a0.y + 20.0) / f.scale;
            let mut k = (x_lo / step).ceil() as i32;
            while k as f32 * step <= x_hi {
                let x = f.pt(Vec2::new(k as f32 * step, 0.0)).x;
                m.push(Mark::line(vec![Vec2::new(x, ground_y), Vec2::new(x, ground_y - y_hi * f.scale)], 1.0, Role::Grid));
                if k >= 0 && x < a1.x - 4.0 {
                    m.push(Mark::text(tick(k as f32 * step), Vec2::new(x, ground_y + 14.0), 13.0, Role::GridText, Anchor::Center));
                }
                k += 1;
            }
            let mut j = 1;
            while j as f32 * step <= y_hi {
                let y = f.pt(Vec2::new(0.0, j as f32 * step)).y;
                m.push(Mark::line(vec![Vec2::new(a0.x - 40.0, y), Vec2::new(a1.x + 30.0, y)], 1.0, Role::Grid));
                m.push(Mark::text(tick(j as f32 * step), Vec2::new(a0.x - 46.0, y - 6.0), 13.0, Role::GridText, Anchor::Right));
                j += 1;
            }
            m.push(Mark::text("x (m)", Vec2::new(a1.x + 40.0, ground_y + 14.0), 13.0, Role::GridText, Anchor::Right));
            m.push(Mark::text("y (m)", Vec2::new(rect.0.x + 12.0, rect.0.y + 38.0), 13.0, Role::GridText, Anchor::Left));
        }

        // Ground, with hatching below.
        let launch = f.pt(Vec2::new(0.0, shot.h));
        let (gx0, gx1) = if ink {
            (launch.x - TOWER_W - 40.0, f.pt(Vec2::new(shot.range(), 0.0)).x + 50.0)
        } else {
            (a0.x - 40.0, a1.x + 30.0)
        };
        m.push(Mark::line(vec![Vec2::new(gx0, ground_y), Vec2::new(gx1, ground_y)], 2.5, Role::Line));
        let mut x = gx0 + 10.0;
        while x < gx1 {
            m.push(Mark::line(vec![Vec2::new(x, ground_y), Vec2::new(x - 10.0, ground_y + 10.0)], 1.4, Role::Line));
            x += 22.0;
        }
        // The tower the ball starts from.
        if shot.h > 0.0 {
            m.push(Mark::line(
                vec![Vec2::new(launch.x - TOWER_W, ground_y), Vec2::new(launch.x - TOWER_W, launch.y), launch, Vec2::new(launch.x, ground_y)],
                2.0,
                Role::Line,
            ));
            m.push(Mark::text(format!("h = {} m", num(shot.h, 0)), Vec2::new(launch.x - TOWER_W - 10.0, (launch.y + ground_y) / 2.0 - 8.0), 16.0, Role::Line, Anchor::Right));
        }

        if shot.is_empty() {
            if !ink {
                m.push(Mark::text("Hız ya da yükseklik verin", Vec2::new(rect.0.x + rect.1.x / 2.0, rect.0.y + rect.1.y / 2.0 - 10.0), 18.0, Role::Line, Anchor::Center));
            }
            return m;
        }

        // The path: flown part bright, the rest faint (on screen).
        let sample = |t0: f32, t1: f32| -> Vec<Vec2> {
            (0..=SAMPLES).map(|i| Self::screen_pos(&shot, &f, t0 + (t1 - t0) * i as f32 / SAMPLES as f32)).collect()
        };
        if ink {
            m.push(Mark::line(sample(0.0, shot.t_end), 2.5, Role::Path));
        } else {
            m.push(Mark::line(sample(0.0, shot.t_end), 2.0, Role::PathAhead));
            if t > 0.0 {
                m.push(Mark::line(sample(0.0, t), 3.0, Role::Path));
            }
        }

        // Peak height.
        if shot.vy0 > 0.0 {
            let top = f.pt(shot.pos(shot.t_peak()));
            let label = format!("h_{{max}} = {} m", num(shot.peak(), 1));
            if shot.vx > 0.0 {
                m.push(Mark::Line { pts: vec![top, Vec2::new(top.x, ground_y)], w: 1.4, style: Role::Line.into(), dash: true });
                m.push(Mark::text(label, Vec2::new(top.x + 10.0, top.y + 8.0), 15.0, Role::Line, Anchor::Left));
            } else {
                m.push(Mark::text(label, Vec2::new(top.x - 16.0, top.y - 7.0), 15.0, Role::Line, Anchor::Right));
            }
        }

        // Equal time steps.
        if self.show_dots {
            let dt = dot_step(shot.t_end);
            let mut k = 0;
            while k as f32 * dt <= shot.t_end + 1e-4 {
                let p = Self::screen_pos(&shot, &f, k as f32 * dt);
                m.push(Mark::Dot { c: p, r: 4.5, style: Role::Path.into(), filled: !ink });
                // Their shadows on the ground and on the start's vertical:
                // equal steps across, growing steps down.
                if shot.vx > 0.0 {
                    m.push(Mark::line(vec![Vec2::new(p.x, ground_y - 6.0), Vec2::new(p.x, ground_y + 6.0)], 1.6, Role::Line));
                }
                m.push(Mark::line(vec![Vec2::new(launch.x - 6.0, p.y), Vec2::new(launch.x + 6.0, p.y)], 1.6, Role::Line));
                k += 1;
            }
            m.push(Mark::text(format!("Δt = {} s", tick(dt)), Vec2::new(a1.x, rect.0.y + 46.0), 15.0, Role::Line, Anchor::Right));
        }

        // Angle at launch.
        if self.kind == Kind::Oblique {
            let r = 46.0;
            let ang = self.p().angle.to_radians();
            let arc: Vec<Vec2> = (0..=16).map(|i| launch + Vec2::new((ang * i as f32 / 16.0).cos(), -(ang * i as f32 / 16.0).sin()) * r).collect();
            m.push(Mark::line(vec![launch, launch + Vec2::new(r + 24.0, 0.0)], 1.4, Role::Line));
            m.push(Mark::line(arc, 1.4, Role::Line));
            let mid = launch + Vec2::new((ang / 2.0).cos(), -(ang / 2.0).sin()) * (r + 14.0);
            m.push(Mark::text(format!("θ = {}°", num(self.p().angle, 0)), mid + Vec2::new(4.0, -6.0), 15.0, Role::Line, Anchor::Left));
        }

        // g, top right.
        let g_at = Vec2::new(a1.x + 20.0, rect.0.y + 72.0);
        m.push(Mark::Arrow { a: g_at, b: g_at + Vec2::new(0.0, 46.0), w: 2.0, style: Role::Line.into(), dash: false });
        m.push(Mark::text(format!("g = {} m/s^2", num(PLANETS[self.planet].1, 1)), g_at + Vec2::new(-10.0, 16.0), 15.0, Role::Line, Anchor::Right));

        // The ball and its velocity now.
        let p = Self::screen_pos(&shot, &f, t);
        if self.show_vectors {
            let k = ARROW_MAX / shot.max_speed().max(1e-3);
            let v = shot.vel(t);
            let tip = p + Vec2::new(v.x, -v.y) * k;
            // Components only when long enough to read (not at the top,
            // where v_y is a dot under its label).
            if v.x.abs() * k > 22.0 && v.y.abs() * k > 22.0 {
                let tx = p + Vec2::new(v.x * k, 0.0);
                let ty = p + Vec2::new(0.0, -v.y * k);
                m.push(Mark::Arrow { a: p, b: tx, w: 2.0, style: Role::Vx.into(), dash: ink });
                m.push(Mark::Arrow { a: p, b: ty, w: 2.0, style: Role::Vy.into(), dash: ink });
                m.push(Mark::Line { pts: vec![tx, tip, ty], w: 1.0, style: Role::Grid.into(), dash: true });
                m.push(Mark::text("v_x", tx + Vec2::new(6.0, 4.0), 15.0, Role::Vx, Anchor::Left));
                m.push(Mark::text("v_y", ty + Vec2::new(-8.0, if v.y > 0.0 { -6.0 } else { -12.0 }), 15.0, Role::Vy, Anchor::Right));
            }
            if v.length() > 1e-3 {
                m.push(Mark::Arrow { a: p, b: tip, w: 2.8, style: Role::V.into(), dash: false });
                let name = if t <= 1e-4 { "v_0" } else { "v" };
                let off = Vec2::new(v.x, -v.y).normalize() * 12.0 + Vec2::new(4.0, -8.0);
                m.push(Mark::text(format!("{name} = {} m/s", num(v.length(), 1)), tip + off, 15.0, Role::V, Anchor::Left));
            }
        }
        m.push(Mark::Dot { c: p, r: 9.0, style: Role::Ball.into(), filled: !ink });
        m
    }

    fn graphs_scene(&self, rect: (Vec2, Vec2), t: f32, ink: bool) -> Vec<Mark> {
        let shot = self.shot();
        let mut m = Vec::new();
        if shot.is_empty() {
            if !ink {
                m.push(Mark::text("Hız ya da yükseklik verin", rect.0 + rect.1 / 2.0 - Vec2::new(0.0, 10.0), 18.0, Role::Line, Anchor::Center));
            }
            return m;
        }
        type Curve = fn(&Shot, f32) -> f32;
        let plots: [(&str, Curve, Role); 4] = [
            ("x (m)", |s, t| s.pos(t).x, Role::Path),
            ("y (m)", |s, t| s.pos(t).y, Role::Path),
            ("v_x (m/s)", |s, t| s.vel(t).x, Role::Vx),
            ("v_y (m/s)", |s, t| s.vel(t).y, Role::Vy),
        ];
        let cell = Vec2::new((rect.1.x - 20.0) / 2.0, (rect.1.y - 20.0) / 2.0);
        for (i, (title, curve, role)) in plots.iter().enumerate() {
            let cpos = rect.0 + Vec2::new((i % 2) as f32 * (cell.x + 20.0), (i / 2) as f32 * (cell.y + 20.0));
            let (pa, pb) = (cpos + Vec2::new(62.0, 34.0), cpos + cell - Vec2::new(60.0, 30.0));
            let vals: Vec<f32> = (0..=SAMPLES).map(|k| curve(&shot, k as f32 / SAMPLES as f32 * shot.t_end)).collect();
            let mut lo = vals.iter().copied().fold(0.0f32, f32::min);
            let mut hi = vals.iter().copied().fold(0.0f32, f32::max);
            if hi - lo < 1e-3 {
                hi = lo + 1.0;
            }
            let pad = (hi - lo) * 0.1;
            if hi > 0.0 {
                hi += pad;
            }
            if lo < 0.0 {
                lo -= pad;
            }
            let at = |tt: f32, v: f32| Vec2::new(pa.x + (pb.x - pa.x) * tt / shot.t_end, pb.y - (pb.y - pa.y) * (v - lo) / (hi - lo));
            let zero = at(0.0, 0.0).y;
            // Axes: value up the left, time along zero.
            m.push(Mark::Arrow { a: Vec2::new(pa.x, pb.y + 4.0), b: Vec2::new(pa.x, pa.y - 14.0), w: 1.8, style: Role::Line.into(), dash: false });
            m.push(Mark::Arrow { a: Vec2::new(pa.x - 4.0, zero), b: Vec2::new(pb.x + 18.0, zero), w: 1.8, style: Role::Line.into(), dash: false });
            m.push(Mark::text(*title, Vec2::new(pa.x + 10.0, pa.y - 22.0), 15.0, Role::Line, Anchor::Left));
            m.push(Mark::text("t (s)", Vec2::new(pb.x + 24.0, zero - 6.0), 13.0, Role::Line, Anchor::Left));
            m.push(Mark::text("0", Vec2::new(pa.x - 8.0, zero - 6.0), 13.0, Role::Line, Anchor::Right));
            // Ticks at the ends: the largest value, the landing time.
            let vmax = vals.iter().copied().fold(f32::MIN, f32::max);
            let vmin = vals.iter().copied().fold(f32::MAX, f32::min);
            let ends = if vmax - vmin > 1e-3 { vec![vmax, vmin] } else { vec![vmax] };
            for v in ends {
                if v.abs() > 1e-3 {
                    let y = at(0.0, v).y;
                    m.push(Mark::line(vec![Vec2::new(pa.x - 5.0, y), Vec2::new(pa.x + 5.0, y)], 1.6, Role::Line));
                    m.push(Mark::text(num(v, 1), Vec2::new(pa.x - 8.0, y - 6.0), 13.0, Role::Line, Anchor::Right));
                }
            }
            let te = at(shot.t_end, 0.0);
            m.push(Mark::line(vec![te - Vec2::new(0.0, 5.0), te + Vec2::new(0.0, 5.0)], 1.6, Role::Line));
            m.push(Mark::text(num(shot.t_end, 2), te + Vec2::new(0.0, if lo < 0.0 { -22.0 } else { 9.0 }), 13.0, Role::Line, Anchor::Center));
            let pts: Vec<Vec2> = vals.iter().enumerate().map(|(k, &v)| at(k as f32 / SAMPLES as f32 * shot.t_end, v)).collect();
            if !ink && i >= 2 && t > 0.0 {
                // The area under v-t up to now is the displacement.
                let n = ((t / shot.t_end) * SAMPLES as f32).round() as usize;
                for w in pts[..=n.min(SAMPLES)].windows(2) {
                    let (a, b) = (w[0], w[1]);
                    m.push(Mark::Tri { a, b, c: Vec2::new(b.x, zero), style: Role::Area.into() });
                    m.push(Mark::Tri { a, b: Vec2::new(b.x, zero), c: Vec2::new(a.x, zero), style: Role::Area.into() });
                }
            }
            m.push(Mark::line(pts, 2.5, *role));
            if !ink {
                let now = at(t, curve(&shot, t));
                m.push(Mark::Line { pts: vec![Vec2::new(now.x, pa.y - 6.0), Vec2::new(now.x, pb.y)], w: 1.0, style: Role::Grid.into(), dash: true });
                m.push(Mark::Dot { c: now, r: 6.0, style: Role::Ball.into(), filled: true });
            }
        }
        m
    }

    fn scene(&self, rect: (Vec2, Vec2), t: f32, ink: bool) -> Vec<Mark> {
        if self.graphs {
            // On screen, below the live readout line.
            let rect = if ink { rect } else { (rect.0 + Vec2::new(0.0, 30.0), rect.1 - Vec2::new(0.0, 30.0)) };
            self.graphs_scene(rect, t, ink)
        } else {
            self.path_scene(rect, t, ink)
        }
    }

    // --- Ink -------------------------------------------------------------

    /// "Tahtaya bas": `None` if there's nothing to draw yet.
    pub fn stamp_lines(&self, screen_size: Vec2, ink: [f32; 4], now: f64) -> Option<Vec<InkLine>> {
        if self.shot().is_empty() {
            return None;
        }
        let size = Vec2::new(STAMP_W.min(screen_size.x - 160.0), STAMP_H.min(screen_size.y - 240.0));
        let top_left = Vec2::new((screen_size.x - size.x) / 2.0, ((screen_size.y - 130.0 - size.y) / 2.0).max(20.0));
        Some(marks::to_ink(&self.scene((top_left, size), self.time(now), true), ink))
    }

    // --- Drawing ---------------------------------------------------------

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
        font_atlas.push_text("Atış hareketleri", header_pos + Vec2::new(PADDING, 10.0), 16.0, COLOR_TEXT, gv, gi);
        let grip = header_pos + header_size / 2.0;
        for k in -1..=1 {
            push_circle(Vec2::new(grip.x + k as f32 * 10.0, grip.y), 2.0, COLOR_TEXT, 8, v, i);
        }
        let (close_pos, close_size) = self.close_rect();
        let c = close_pos + close_size / 2.0;
        let r = close_size.x * 0.2;
        push_line(c - Vec2::splat(r), c + Vec2::splat(r), 2.5, COLOR_TEXT, v, i);
        push_line(c + Vec2::new(-r, r), c + Vec2::new(r, -r), 2.5, COLOR_TEXT, v, i);

        // The view.
        let view = self.view_rect();
        push_rect(view.0, view.1, COLOR_FIELD, v, i);
        let t = self.time(now);
        let marks = self.scene(view, t, false);
        marks::draw(&marks, font_atlas, v, i, gv, gi);
        let shot = self.shot();
        if !shot.is_empty() {
            let pos = shot.pos(t);
            let vel = shot.vel(t);
            let mut x = view.0.x + 14.0;
            let y = view.0.y + 12.0;
            for (s, col) in [
                (format!("t = {} s", num(t, 2)), COLOR_TEXT),
                (format!("x = {} m", num(pos.x, 1)), COLOR_TEXT),
                (format!("y = {} m", num(pos.y.max(0.0), 1)), COLOR_TEXT),
                (format!("v_x = {} m/s", num(vel.x, 1)), COLOR_VX),
                (format!("v_y = {} m/s", num(vel.y, 1)), COLOR_VY),
            ] {
                for (t, at, size) in layout_text(&s, Vec2::new(x, y), 15.0, Anchor::Left, |t, h| font_atlas.measure(t, h)) {
                    x = x.max(at.x + font_atlas.push_text(&t, at, size, col, gv, gi));
                }
                x += 22.0;
            }
            if self.speedup() > 1.01 {
                font_atlas.push_text(&format!("×{} hızlı", num(self.speedup(), 0)), Vec2::new(x, y + 1.0), 13.0, COLOR_TEXT_DIM, gv, gi);
            }
        }

        // Side column.
        let k_on = self.kind_index();
        for (k, (_, name)) in KINDS.iter().enumerate() {
            button(self.kind_rect(k), name, if k == k_on { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE }, font_atlas, v, i, gv, gi);
        }
        for param in PARAMS {
            let (pos, size) = self.slider_rect(param);
            push_rect(pos, size, COLOR_FIELD, v, i);
            let on = self.param_active(param);
            let val = self.value(param);
            let label = match param {
                Param::Speed => format!("v_0 = {} m/s", num(if on { val } else { 0.0 }, 0)),
                Param::Angle => format!("θ = {}°", num(if on { val } else if self.kind == Kind::Vertical { 90.0 } else { 0.0 }, 0)),
                Param::Height => format!("h = {} m", num(val, 0)),
            };
            for (t, at, s) in layout_text(&label, pos + Vec2::new(12.0, 14.0), 16.0, Anchor::Left, |t, h| font_atlas.measure(t, h)) {
                font_atlas.push_text(&t, at, s, if on { COLOR_TEXT } else { COLOR_TEXT_DIM }, gv, gi);
            }
            let (x0, x1, y) = self.slider_track(param);
            push_line(Vec2::new(x0, y), Vec2::new(x1, y), 4.0, COLOR_KEY_IDLE, v, i);
            if on {
                let (lo, hi) = param.range();
                let x = x0 + (x1 - x0) * (val - lo) / (hi - lo);
                push_line(Vec2::new(x0, y), Vec2::new(x, y), 4.0, COLOR_SLIDER, v, i);
                push_circle(Vec2::new(x, y), 11.0, COLOR_TEXT, 20, v, i);
            }
        }
        let (name, g) = PLANETS[self.planet];
        button(self.planet_rect(), &format!("Gezegen: {name}  (g = {} m/s²)", num(g, 1)), COLOR_KEY_IDLE, font_atlas, v, i, gv, gi);

        let (rpos, rsize) = self.slot_rect(5);
        push_rect(rpos, rsize, COLOR_FIELD, v, i);
        let lines: Vec<String> = if shot.is_empty() {
            vec!["Hız ya da yükseklik verin".into()]
        } else {
            vec![
                format!("Uçuş süresi: {} s", num(shot.t_end, 2)),
                format!("Menzil: {} m", num(shot.range(), 1)),
                format!("En yüksek nokta: {} m", num(shot.peak(), 1)),
                format!("Yere çarpma hızı: {} m/s", num(shot.impact_speed(), 1)),
            ]
        };
        for (k, line) in lines.iter().enumerate() {
            font_atlas.push_text(line, rpos + Vec2::new(12.0, 12.0 + k as f32 * 25.0), 15.0, COLOR_TEXT, gv, gi);
        }

        button(self.vectors_rect(), "Hız vektörleri", if self.show_vectors { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE }, font_atlas, v, i, gv, gi);
        button(self.dots_rect(), "Eşit zaman", if self.show_dots { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE }, font_atlas, v, i, gv, gi);
        button(self.graphs_rect(), "Grafikler", if self.graphs { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE }, font_atlas, v, i, gv, gi);
        let running = self.running(now);
        button(self.play_rect(), if running { "Durdur" } else { "Başlat" }, if running { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE }, font_atlas, v, i, gv, gi);
        button(self.stamp_rect(), "Tahtaya bas", COLOR_KEY_STAMP, font_atlas, v, i, gv, gi);
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

    fn close(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn textbook_answers() {
        // Eğik atış, 20 m/s, 45°, g = 9,8: T = 2,886 s, R = 40,8 m, hmax = 10,2 m.
        let s = Shot::new(Kind::Oblique, Params { v0: 20.0, angle: 45.0, h: 0.0 }, 9.8);
        assert!(close(s.t_end, 2.886, 1e-3), "{}", s.t_end);
        assert!(close(s.range(), 40.82, 1e-2), "{}", s.range());
        assert!(close(s.peak(), 10.20, 1e-2), "{}", s.peak());
        assert!(close(s.impact_speed(), 20.0, 1e-3));
        // Yatay atış from 45 m at 15 m/s, g = 10: 3 s, 45 m.
        let s = Shot::new(Kind::Horizontal, Params { v0: 15.0, angle: 0.0, h: 45.0 }, 10.0);
        assert!(close(s.t_end, 3.0, 1e-4) && close(s.range(), 45.0, 1e-3));
        assert!(close(s.impact_speed(), (15f32 * 15.0 + 30.0 * 30.0).sqrt(), 1e-3));
        // Serbest düşme: v = √(2gh); the speed slider doesn't count.
        let s = Shot::new(Kind::FreeFall, Params { v0: 30.0, angle: 30.0, h: 45.0 }, 10.0);
        assert!(close(s.t_end, 3.0, 1e-4) && close(s.impact_speed(), 30.0, 1e-3) && s.range() == 0.0);
        // Düşey atış: up 20 m/s from the ground, back in 2·v/g.
        let s = Shot::new(Kind::Vertical, Params { v0: 20.0, angle: 0.0, h: 0.0 }, 10.0);
        assert!(close(s.t_end, 4.0, 1e-4) && close(s.peak(), 20.0, 1e-3) && s.range() == 0.0);
        assert!(close(s.pos(s.t_end).y, 0.0, 1e-3));
        // Nothing to fly.
        assert!(Shot::new(Kind::FreeFall, Params { v0: 0.0, angle: 0.0, h: 0.0 }, 9.8).is_empty());
    }

    #[test]
    fn helpers() {
        assert_eq!(nice(13.0), 20.0);
        assert_eq!(nice(0.3), 0.5);
        assert_eq!(dot_step(2.886), 0.5);
        assert_eq!(dot_step(0.9), 0.1);
        assert_eq!(num(-0.01, 1), "0,0");
        assert_eq!(num(40.82, 1), "40,8");
        assert_eq!(tick(2.5), "2,5");
        assert_eq!(tick(20.0), "20");
        // Vertical throw: same height on the way up and down, side by side.
        let s = Shot::new(Kind::Vertical, Params { v0: 20.0, angle: 0.0, h: 0.0 }, 10.0);
        let f = Frame { origin: Vec2::new(100.0, 500.0), scale: 10.0 };
        let (up, down) = (Motion::screen_pos(&s, &f, 1.0), Motion::screen_pos(&s, &f, 3.0));
        assert!(close(up.y, down.y, 1e-3) && close(down.x - up.x, VERTICAL_SPLIT, 1e-3));
    }

    #[test]
    fn panel_sets_plays_scrubs_and_stamps() {
        let mut m = Motion::new(SCREEN);
        assert!(m.position.y + m.panel_height() <= SCREEN.y - 75.0, "{}", m.panel_height());
        let c = |r: (Vec2, Vec2)| r.0 + r.1 / 2.0;
        // Speed slider to the end: 50 m/s.
        let (_, x1, y) = m.slider_track(Param::Speed);
        assert_eq!(m.press_at(Vec2::new(x1 + 5.0, y), 0.0), MotionPress::Slider(Param::Speed));
        assert_eq!(m.value(Param::Speed), 50.0);
        // Free fall: no speed, no angle.
        m.press_at(c(m.kind_rect(0)), 0.0);
        assert_eq!(m.kind, Kind::FreeFall);
        assert_eq!(m.press_at(Vec2::new(x1, y), 0.0), MotionPress::Other);
        let t_end = m.shot().t_end;
        assert!(close(t_end, (2.0 * 45.0 / 9.8f32).sqrt(), 1e-4));
        // Play: runs, then stops by itself at the landing.
        m.press_at(c(m.play_rect()), 10.0);
        assert!(m.is_animating(11.0));
        assert!(close(m.time(11.0), 1.0, 1e-4));
        assert!(!m.is_animating(10.0 + t_end as f64 + 0.1));
        assert!(close(m.time(100.0), t_end, 1e-5));
        // Tapping play again at the end starts over.
        m.press_at(c(m.play_rect()), 100.0);
        assert!(close(m.time(100.5), 0.5, 1e-4));
        // Finger on the ground under the path: the landing moment.
        m.press_at(c(m.kind_rect(3)), 0.0);
        let f = Frame::fit(&m.shot(), Motion::area(m.view_rect()));
        let landing = f.pt(Vec2::new(m.shot().range(), 0.0));
        assert_eq!(m.press_at(landing + Vec2::new(0.0, 3.0), 0.0), MotionPress::Scrub);
        assert!(close(m.time(0.0), m.shot().t_end, 0.05));
        // Moon: slower, longer.
        m.press_at(c(m.planet_rect()), 0.0);
        assert!(m.shot().t_end > 5.0 * 2.88);
        assert!(m.speedup() > 1.0);
        // Stamp, both views, all inside the screen above the toolbar.
        assert_eq!(m.press_at(c(m.stamp_rect()), 0.0), MotionPress::Stamp);
        for graphs in [false, true] {
            m.graphs = graphs;
            for kind in 0..4 {
                m.kind = KINDS[kind].0;
                let lines = m.stamp_lines(SCREEN, [1.0; 4], 0.0).unwrap();
                assert!(lines.len() > 10);
                for l in &lines {
                    for p in &l.points {
                        assert!(p.x >= 0.0 && p.x <= SCREEN.x && p.y >= 0.0 && p.y <= SCREEN.y - 70.0, "{kind} {graphs}: {p:?}");
                    }
                }
            }
        }
        m.params[0].h = 0.0;
        m.kind = Kind::FreeFall;
        assert!(m.stamp_lines(SCREEN, [1.0; 4], 0.0).is_none());
    }

    #[test]
    fn every_stamped_char_has_a_glyph() {
        for graphs in [false, true] {
            for kind in 0..4 {
                let mut m = Motion::new(SCREEN);
                m.graphs = graphs;
                m.kind = KINDS[kind].0;
                let marks = m.scene((Vec2::ZERO, Vec2::new(1000.0, 560.0)), 1.0, true);
                assert_eq!(marks::missing_glyphs(&marks), vec![]);
            }
        }
    }
}
