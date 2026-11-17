//! Draggable "Kuvvet" panel (physics): free-body diagrams of a block on a
//! horizontal or inclined plane with static/kinetic friction, and vector
//! addition on a square grid.
//!
//! Planes: the teacher sets m, F (and its angle α, or the incline θ), μs
//! and μk; the panel draws G, N, F, friction (static while it holds, else
//! kinetic), optional components, the net force and acceleration, and can
//! let the block slide ("Başlat"). Vectors: up to four arrows whose tips
//! are dragged to grid points, added tip to tail or by the parallelogram
//! rule, with the resultant and its components. "Tahtaya bas" stamps the
//! figure as a movable, resizable ink drawing (see `marks.rs`).

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
const SIDE_W: f32 = 340.0;
const BTN_H: f32 = 44.0;
const RESULT_H: f32 = 136.0;
const LABEL_W: f32 = 150.0;
/// Longest force arrow (px).
const ARROW_MAX: f32 = 150.0;
/// Block size on the flat plane / the incline (px).
const BLOCK: Vec2 = Vec2::new(150.0, 100.0);
const BLOCK_INCLINE: Vec2 = Vec2::new(120.0, 80.0);
/// Sliding animation: px per meter, and how far it goes before restarting.
const PX_PER_M: f32 = 30.0;
const SLIDE_MAX: f32 = 190.0;
/// Vector grid square (px) and the largest component (squares).
const GRID: f32 = 40.0;
const VEC_MAX: i32 = 9;
const MAX_VECTORS: usize = 4;
/// How near a tip a finger grabs it (px).
const GRAB: f32 = 40.0;
const STAMP_W: f32 = 960.0;
const STAMP_H: f32 = 580.0;

const COLOR_PANEL_BG: [f32; 4] = [0.12, 0.13, 0.17, 0.96];
const COLOR_HEADER: [f32; 4] = [0.18, 0.19, 0.24, 1.0];
const COLOR_FIELD: [f32; 4] = [0.06, 0.07, 0.09, 1.0];
const COLOR_TEXT: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_TEXT_DIM: [f32; 4] = [0.95, 0.95, 0.97, 0.55];
const COLOR_GRID: [f32; 4] = [1.0, 1.0, 1.0, 0.08];
const COLOR_LINE: [f32; 4] = [0.85, 0.87, 0.92, 1.0];
const COLOR_G: [f32; 4] = [0.95, 0.40, 0.40, 1.0];
const COLOR_N: [f32; 4] = [0.35, 0.85, 0.45, 1.0];
const COLOR_F: [f32; 4] = [0.98, 0.62, 0.20, 1.0];
const COLOR_FRICTION: [f32; 4] = [0.35, 0.65, 1.0, 1.0];
const COLOR_NET: [f32; 4] = [0.80, 0.55, 1.0, 1.0];
const COLOR_VECS: [[f32; 4]; MAX_VECTORS] = [[0.98, 0.62, 0.20, 1.0], [0.35, 0.65, 1.0, 1.0], [0.35, 0.85, 0.45, 1.0], [0.95, 0.45, 0.70, 1.0]];
const COLOR_KEY_IDLE: [f32; 4] = [0.22, 0.24, 0.30, 1.0];
const COLOR_KEY_ACTIVE: [f32; 4] = [0.23, 0.51, 0.96, 1.0];
const COLOR_KEY_STAMP: [f32; 4] = [0.98, 0.55, 0.15, 1.0];
const COLOR_SLIDER: [f32; 4] = [0.45, 0.72, 1.0, 1.0];

const VEC_NAMES: [&str; MAX_VECTORS] = ["A", "B", "C", "D"];

fn inked(color: [f32; 4]) -> Style {
    Style { color, ink: true }
}

fn screen(color: [f32; 4]) -> Style {
    Style { color, ink: false }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Flat,
    Incline,
    Vectors,
}

const MODES: [(Mode, &str); 3] = [(Mode::Flat, "Yatay düzlem"), (Mode::Incline, "Eğik düzlem"), (Mode::Vectors, "Vektör toplama")];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Param {
    Mass,
    Force,
    Alpha,
    Incline,
    MuS,
    MuK,
}

impl Param {
    /// (min, max, step)
    fn range(self) -> (f32, f32, f32) {
        match self {
            Param::Mass => (1.0, 20.0, 1.0),
            Param::Force => (0.0, 100.0, 1.0),
            Param::Alpha => (0.0, 80.0, 1.0),
            Param::Incline => (0.0, 60.0, 1.0),
            Param::MuS | Param::MuK => (0.0, 1.0, 0.05),
        }
    }
}

/// One plane problem, solved (forces in newtons).
#[derive(Clone, Copy, Debug, Default)]
pub struct Solution {
    pub weight: f32,
    pub normal: f32,
    /// Friction magnitude, and whether it's the static kind (block at rest).
    pub friction: f32,
    pub at_rest: bool,
    /// Largest static friction μs·N.
    pub friction_max: f32,
    /// Net force and acceleration along the surface (+ = toward F_x /
    /// up the slope).
    pub net: f32,
    pub accel: f32,
    /// F's angle lifts the block off the floor (F_y > G).
    pub lifted: bool,
    /// Pull of gravity along the slope (incline only).
    pub g_along: f32,
}

/// Block on a horizontal floor pulled by `force` at `alpha` (degrees)
/// above the horizontal.
pub fn solve_flat(m: f32, force: f32, alpha: f32, mu_s: f32, mu_k: f32, g: f32) -> Solution {
    let weight = m * g;
    let (fx, fy) = (force * alpha.to_radians().cos(), force * alpha.to_radians().sin());
    let lifted = fy > weight + 1e-4;
    let normal = (weight - fy).max(0.0);
    let friction_max = mu_s * normal;
    let mut s = Solution { weight, normal, friction_max, lifted, ..Default::default() };
    if fx <= friction_max + 1e-4 {
        s.at_rest = true;
        s.friction = fx;
    } else {
        s.friction = mu_k * normal;
        s.net = fx - s.friction;
        s.accel = s.net / m;
    }
    s
}

/// Block on a slope of `theta` degrees, pushed up the slope by `force`.
pub fn solve_incline(m: f32, force: f32, theta: f32, mu_s: f32, mu_k: f32, g: f32) -> Solution {
    let weight = m * g;
    let (sin, cos) = theta.to_radians().sin_cos();
    let g_along = weight * sin;
    let normal = weight * cos;
    let drive = force - g_along;
    let friction_max = mu_s * normal;
    let mut s = Solution { weight, normal, friction_max, g_along, ..Default::default() };
    if drive.abs() <= friction_max + 1e-4 {
        s.at_rest = true;
        s.friction = drive.abs();
    } else {
        s.friction = mu_k * normal;
        s.net = drive.signum() * (drive.abs() - s.friction);
        s.accel = s.net / m;
    }
    s
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ForcePress {
    Move,
    Close,
    Slider(Param),
    /// Finger on a vector's tip: drags it (`drag_vector`).
    Drag(usize),
    Stamp,
    Other,
}

pub struct Force {
    pub position: Vec2,
    mode: Mode,
    mass: f32,
    force: f32,
    alpha: f32,
    incline: f32,
    mu_s: f32,
    mu_k: f32,
    /// g = 10 (textbook problems) or 9,8.
    g_ten: bool,
    components: bool,
    /// Sliding since (input clock).
    playing: Option<f64>,
    /// Grid vectors (squares, y up).
    vectors: Vec<(i32, i32)>,
    parallelogram: bool,
}

impl Force {
    pub fn new(screen_size: Vec2) -> Self {
        let mut f = Self {
            position: Vec2::ZERO,
            mode: Mode::Flat,
            mass: 5.0,
            force: 30.0,
            alpha: 0.0,
            incline: 30.0,
            mu_s: 0.4,
            mu_k: 0.3,
            g_ten: true,
            components: true,
            playing: None,
            vectors: vec![(4, 1), (1, 3)],
            parallelogram: false,
        };
        let size = Vec2::new(f.panel_width(), f.panel_height());
        f.position = Vec2::new((screen_size.x - size.x) / 2.0, ((screen_size.y - 120.0 - size.y) / 2.0).max(12.0));
        f
    }

    fn g(&self) -> f32 {
        if self.g_ten {
            10.0
        } else {
            9.8
        }
    }

    pub fn solution(&self) -> Solution {
        match self.mode {
            Mode::Incline => solve_incline(self.mass, self.force, self.incline, self.mu_s, self.mu_k, self.g()),
            _ => solve_flat(self.mass, self.force, self.alpha, self.mu_s, self.mu_k, self.g()),
        }
    }

    fn params(&self) -> [Param; 5] {
        match self.mode {
            Mode::Incline => [Param::Mass, Param::Incline, Param::Force, Param::MuS, Param::MuK],
            _ => [Param::Mass, Param::Force, Param::Alpha, Param::MuS, Param::MuK],
        }
    }

    fn value(&self, param: Param) -> f32 {
        match param {
            Param::Mass => self.mass,
            Param::Force => self.force,
            Param::Alpha => self.alpha,
            Param::Incline => self.incline,
            Param::MuS => self.mu_s,
            Param::MuK => self.mu_k,
        }
    }

    fn slider_label(&self, param: Param) -> String {
        let v = self.value(param);
        match param {
            Param::Mass => format!("m = {} kg", num(v, 0)),
            Param::Force => format!("F = {} N", num(v, 0)),
            Param::Alpha => format!("α = {}°", num(v, 0)),
            Param::Incline => format!("θ = {}°", num(v, 0)),
            Param::MuS => format!("μ_s = {}", num(v, 2)),
            Param::MuK => format!("μ_k = {}", num(v, 2)),
        }
    }

    /// The vectors' drawn starts (tip to tail, or all from the origin).
    fn vector_starts(&self) -> Vec<(i32, i32)> {
        let mut starts = Vec::new();
        let mut at = (0, 0);
        for v in &self.vectors {
            starts.push(if self.uses_parallelogram() { (0, 0) } else { at });
            at = (at.0 + v.0, at.1 + v.1);
        }
        starts
    }

    fn uses_parallelogram(&self) -> bool {
        self.parallelogram && self.vectors.len() == 2
    }

    fn resultant(&self) -> (i32, i32) {
        self.vectors.iter().fold((0, 0), |a, v| (a.0 + v.0, a.1 + v.1))
    }

    // --- Animation -------------------------------------------------------

    fn slide_period(&self) -> f32 {
        let a = self.solution().accel.abs();
        if a < 1e-4 {
            return 0.0;
        }
        (2.0 * SLIDE_MAX / (a * PX_PER_M)).sqrt().min(3.0)
    }

    /// Seconds into the current slide (0 when not sliding).
    fn slide_time(&self, now: f64) -> f32 {
        let period = self.slide_period();
        match self.playing {
            Some(start) if period > 0.0 && self.mode != Mode::Vectors => ((now - start).max(0.0) as f32) % (period + 0.6),
            _ => 0.0,
        }
        .min(period)
    }

    /// The block's offset along the surface (px, + = toward the net force's
    /// positive direction).
    fn slide_offset(&self, now: f64) -> f32 {
        let t = self.slide_time(now);
        0.5 * self.solution().accel * t * t * PX_PER_M
    }

    pub fn is_animating(&self, _now: f64) -> bool {
        self.playing.is_some() && self.mode != Mode::Vectors && self.slide_period() > 0.0
    }

    // --- Layout ----------------------------------------------------------

    fn slot_heights() -> [f32; 9] {
        [2.0 * BTN_H + GAP, BTN_H, BTN_H, BTN_H, BTN_H, BTN_H, RESULT_H, BTN_H, BTN_H]
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

    /// Mode tabs in the first slot's 2×2 grid; the fourth cell is g.
    fn cell_rect(&self, k: usize) -> (Vec2, Vec2) {
        let (pos, size) = self.slot_rect(0);
        let w = (size.x - GAP) / 2.0;
        (pos + Vec2::new((k % 2) as f32 * (w + GAP), (k / 2) as f32 * (BTN_H + GAP)), Vec2::new(w, BTN_H))
    }

    fn g_rect(&self) -> (Vec2, Vec2) {
        self.cell_rect(3)
    }

    fn slider_track(&self, slot: usize) -> (f32, f32, f32) {
        let (pos, size) = self.slot_rect(slot);
        (pos.x + LABEL_W, pos.x + size.x - 20.0, pos.y + size.y / 2.0)
    }

    fn add_rect(&self) -> (Vec2, Vec2) {
        self.half(1, 0)
    }

    fn remove_rect(&self) -> (Vec2, Vec2) {
        self.half(1, 1)
    }

    fn tip_to_tail_rect(&self) -> (Vec2, Vec2) {
        self.half(2, 0)
    }

    fn parallelogram_rect(&self) -> (Vec2, Vec2) {
        self.half(2, 1)
    }

    fn components_rect(&self) -> (Vec2, Vec2) {
        self.half(7, 0)
    }

    fn play_rect(&self) -> (Vec2, Vec2) {
        self.half(7, 1)
    }

    fn stamp_rect(&self) -> (Vec2, Vec2) {
        self.slot_rect(8)
    }

    /// Vector grid origin on screen: on grid lines, room up and right.
    fn grid_origin(&self) -> Vec2 {
        let (pos, size) = self.view_rect();
        pos + Vec2::new((size.x * 0.32 / GRID).round() * GRID, (size.y * 0.66 / GRID).round() * GRID)
    }

    // --- Input -----------------------------------------------------------

    pub fn contains(&self, p: Vec2) -> bool {
        rect_contains((self.position, Vec2::new(self.panel_width(), self.panel_height())), p)
    }

    pub fn drag_to(&mut self, p: Vec2, grab_offset: Vec2) {
        self.position = p - grab_offset;
    }

    pub fn press_at(&mut self, p: Vec2, now: f64) -> ForcePress {
        if rect_contains(self.close_rect(), p) {
            return ForcePress::Close;
        }
        if rect_contains(self.header_rect(), p) {
            return ForcePress::Move;
        }
        if rect_contains(self.stamp_rect(), p) {
            return ForcePress::Stamp;
        }
        if rect_contains(self.view_rect(), p) {
            if self.mode == Mode::Vectors {
                let origin = self.grid_origin();
                let tips: Vec<Vec2> = self.vector_starts().iter().zip(&self.vectors).map(|(s, v)| grid_pt(origin, (s.0 + v.0, s.1 + v.1))).collect();
                if let Some((i, d)) = tips.iter().enumerate().map(|(i, t)| (i, t.distance(p))).min_by(|a, b| a.1.total_cmp(&b.1)) {
                    if d <= GRAB {
                        return ForcePress::Drag(i);
                    }
                }
            }
            return ForcePress::Other;
        }
        if let Some(k) = (0..MODES.len()).find(|&k| rect_contains(self.cell_rect(k), p)) {
            self.mode = MODES[k].0;
            self.playing = None;
            return ForcePress::Other;
        }
        if rect_contains(self.g_rect(), p) {
            if self.mode != Mode::Vectors {
                self.g_ten = !self.g_ten;
            }
            return ForcePress::Other;
        }
        if self.mode == Mode::Vectors {
            if rect_contains(self.add_rect(), p) && self.vectors.len() < MAX_VECTORS {
                let next = [(4, 1), (1, 3), (-3, 2), (2, -3)][self.vectors.len()];
                self.vectors.push(next);
            } else if rect_contains(self.remove_rect(), p) && self.vectors.len() > 1 {
                self.vectors.pop();
            } else if rect_contains(self.tip_to_tail_rect(), p) {
                self.parallelogram = false;
            } else if rect_contains(self.parallelogram_rect(), p) {
                self.parallelogram = true;
            } else if rect_contains(self.components_rect(), p) {
                self.components = !self.components;
            }
            return ForcePress::Other;
        }
        for (k, param) in self.params().into_iter().enumerate() {
            if rect_contains(self.slot_rect(1 + k), p) {
                self.set_slider(param, p.x);
                return ForcePress::Slider(param);
            }
        }
        if rect_contains(self.components_rect(), p) {
            self.components = !self.components;
        } else if rect_contains(self.play_rect(), p) {
            self.playing = match self.playing {
                Some(_) => None,
                None => Some(now),
            };
        }
        ForcePress::Other
    }

    pub fn set_slider(&mut self, param: Param, x: f32) {
        let Some(k) = self.params().iter().position(|p| *p == param) else { return };
        let (x0, x1, _) = self.slider_track(1 + k);
        let (lo, hi, step) = param.range();
        let v = lo + ((hi - lo) * ((x - x0) / (x1 - x0)).clamp(0.0, 1.0) / step).round() * step;
        match param {
            Param::Mass => self.mass = v,
            Param::Force => self.force = v,
            Param::Alpha => self.alpha = v,
            Param::Incline => self.incline = v,
            // Kinetic friction never exceeds static.
            Param::MuS => {
                self.mu_s = v;
                self.mu_k = self.mu_k.min(v);
            }
            Param::MuK => {
                self.mu_k = v;
                self.mu_s = self.mu_s.max(v);
            }
        }
    }

    /// Moves vector `i`'s tip to the grid point nearest the finger.
    pub fn drag_vector(&mut self, i: usize, p: Vec2) {
        let Some(&start) = self.vector_starts().get(i) else { return };
        let origin = self.grid_origin();
        let s = grid_pt(origin, start);
        let v = ((((p.x - s.x) / GRID).round() as i32).clamp(-VEC_MAX, VEC_MAX), (((s.y - p.y) / GRID).round() as i32).clamp(-VEC_MAX, VEC_MAX));
        if v != (0, 0) {
            self.vectors[i] = v;
        }
    }

    // --- Scenes ----------------------------------------------------------

    fn scene(&self, rect: (Vec2, Vec2), now: f64, ink: bool) -> Vec<Mark> {
        match self.mode {
            Mode::Flat => self.flat_scene(rect, if ink { 0.0 } else { self.slide_offset(now) }, ink),
            Mode::Incline => self.incline_scene(rect, if ink { 0.0 } else { self.slide_offset(now) }, ink),
            Mode::Vectors => self.vector_scene(rect, ink),
        }
    }

    /// Given-values line at the top left of a plane figure.
    fn givens(&self) -> String {
        let mut s = format!("m = {} kg   μ_s = {}   μ_k = {}", num(self.mass, 0), num(self.mu_s, 2), num(self.mu_k, 2));
        s += &format!("   g = {} m/s^2", num(self.g(), 1).trim_end_matches(",0"));
        s
    }

    /// Px per newton for this problem's arrows.
    fn scale(&self, s: &Solution) -> f32 {
        let biggest = [s.weight, s.normal, self.force, s.friction].into_iter().fold(1.0f32, f32::max);
        ARROW_MAX / biggest
    }

    fn flat_scene(&self, rect: (Vec2, Vec2), offset: f32, ink: bool) -> Vec<Mark> {
        let s = self.solution();
        let k = self.scale(&s);
        let mut m = Vec::new();
        let ground = rect.0.y + rect.1.y - 190.0;
        let cx = rect.0.x + rect.1.x * 0.45 + offset;
        let c = Vec2::new(cx, ground - BLOCK.y / 2.0);
        m.push(Mark::text(self.givens(), rect.0 + Vec2::new(16.0, 46.0), 15.0, ink_line(), Anchor::Left));

        // Floor and block.
        let (gx0, gx1) = if ink { (c.x - 300.0, c.x + 300.0) } else { (rect.0.x + 20.0, rect.0.x + rect.1.x - 20.0) };
        surface(&mut m, Vec2::new(gx0, ground), Vec2::new(gx1, ground));
        let (tl, br) = (c - BLOCK / 2.0, c + BLOCK / 2.0);
        m.push(Mark::line(vec![tl, Vec2::new(br.x, tl.y), br, Vec2::new(tl.x, br.y), tl], 2.5, ink_line()));
        m.push(Mark::text(format!("{} kg", num(self.mass, 0)), Vec2::new(c.x + BLOCK.x / 2.0 - 10.0, br.y - 26.0), 14.0, ink_line(), Anchor::Right));

        // Forces from the center (screen: y down).
        let down = Vec2::new(0.0, 1.0);
        force_arrow(&mut m, c, down * s.weight * k, &format!("G = {} N", num(s.weight, 1)), COLOR_G);
        if s.normal > 1e-3 {
            force_arrow(&mut m, c, -down * s.normal * k, &format!("N = {} N", num(s.normal, 1)), COLOR_N);
        }
        let a = self.alpha.to_radians();
        let f_dir = Vec2::new(a.cos(), -a.sin());
        if self.force > 1e-3 {
            force_arrow(&mut m, c, f_dir * self.force * k, &format!("F = {} N", num(self.force, 0)), COLOR_F);
            if self.alpha > 0.5 {
                angle_arc(&mut m, c, 0.0, a, 52.0, &format!("α = {}°", num(self.alpha, 0)));
                if self.components {
                    let (fx, fy) = (self.force * a.cos(), self.force * a.sin());
                    dashed_arrow(&mut m, c, Vec2::new(fx * k, 0.0), &format!("F_x = {} N", num(fx, 1)), COLOR_F, Vec2::new(0.0, 22.0));
                    dashed_arrow(&mut m, c, Vec2::new(0.0, -fy * k), &format!("F_y = {} N", num(fy, 1)), COLOR_F, Vec2::new(-14.0, 0.0));
                }
            }
        }
        if s.friction > 1e-3 {
            let name = if s.at_rest { "f_s" } else { "f_k" };
            // From the trailing edge, along the floor, label above it.
            let at = Vec2::new(c.x - BLOCK.x / 2.0 - 2.0, ground - 10.0);
            force_arrow_nudged(&mut m, at, Vec2::new(-s.friction * k, 0.0), &format!("{name} = {} N", num(s.friction, 1)), COLOR_FRICTION, Vec2::new(0.0, -12.0));
        }
        if s.net.abs() > 1e-3 {
            let start = Vec2::new(c.x + s.net.signum() * (BLOCK.x / 2.0 + 14.0), c.y - BLOCK.y / 2.0 - 4.0);
            net_arrow(&mut m, start, Vec2::new(s.net.signum(), 0.0), &s);
        }
        m
    }

    fn incline_scene(&self, rect: (Vec2, Vec2), offset: f32, _ink: bool) -> Vec<Mark> {
        let s = self.solution();
        let k = self.scale(&s);
        let mut m = Vec::new();
        m.push(Mark::text(self.givens(), rect.0 + Vec2::new(16.0, 46.0), 15.0, ink_line(), Anchor::Left));
        let th = self.incline.to_radians();
        let (sin, cos) = th.sin_cos();
        let base_y = rect.0.y + rect.1.y - 70.0;
        let b = Vec2::new(rect.0.x + rect.1.x * 0.86, base_y);
        let mut len = 640.0f32.min((b.x - rect.0.x - 40.0) / cos.max(1e-3));
        if sin > 1e-3 {
            len = len.min((rect.1.y - 170.0) / sin);
        }
        let up = Vec2::new(-cos, -sin);
        // Away from the slope (straight up when flat).
        let out = Vec2::new(sin, -cos);
        let top = b + up * len;
        let corner = Vec2::new(top.x, b.y);
        // Slope, floor, back wall.
        m.push(Mark::line(vec![corner, b, top, corner], 2.5, ink_line()));
        let mut x = corner.x + 8.0;
        while x < b.x + 30.0 {
            m.push(Mark::line(vec![Vec2::new(x, base_y), Vec2::new(x - 10.0, base_y + 10.0)], 1.4, ink_line()));
            x += 22.0;
        }
        if self.incline > 0.5 {
            angle_arc(&mut m, b, std::f32::consts::PI, std::f32::consts::PI - th, 64.0, &format!("θ = {}°", num(self.incline, 0)));
        }

        // Block on the slope (offset along the slope toward the net force).
        let along = up * s.net.signum() * offset.abs();
        let c = b + up * (len * 0.5) + out * (BLOCK_INCLINE.y / 2.0) + along;
        let (hu, hn) = (up * BLOCK_INCLINE.x / 2.0, out * BLOCK_INCLINE.y / 2.0);
        m.push(Mark::line(vec![c - hu - hn, c + hu - hn, c + hu + hn, c - hu + hn, c - hu - hn], 2.5, ink_line()));

        let down = Vec2::new(0.0, 1.0);
        force_arrow(&mut m, c, down * s.weight * k, &format!("G = {} N", num(s.weight, 1)), COLOR_G);
        force_arrow(&mut m, c, out * s.normal * k, &format!("N = {} N", num(s.normal, 1)), COLOR_N);
        if self.components && self.incline > 0.5 {
            dashed_arrow(&mut m, c, -up * s.g_along * k, &format!("G sinθ = {} N", num(s.g_along, 1)), COLOR_G, Vec2::ZERO);
            dashed_arrow(&mut m, c, -out * s.normal * k, &format!("G cosθ = {} N", num(s.normal, 1)), COLOR_G, Vec2::ZERO);
        }
        if self.force > 1e-3 {
            force_arrow(&mut m, c + up * (BLOCK_INCLINE.x / 2.0), up * self.force * k, &format!("F = {} N", num(self.force, 0)), COLOR_F);
        }
        if s.friction > 1e-3 {
            // Opposes the way it slides, or would slide.
            let tendency = (self.force - s.g_along).signum();
            let name = if s.at_rest { "f_s" } else { "f_k" };
            // Along the contact face, from its trailing corner; label on
            // the slope's side so it clears G's components.
            let at = c - out * (BLOCK_INCLINE.y / 2.0 - 8.0) - up * tendency * (BLOCK_INCLINE.x / 2.0 + 2.0);
            force_arrow_nudged(&mut m, at, -up * tendency * s.friction * k, &format!("{name} = {} N", num(s.friction, 1)), COLOR_FRICTION, -out * 30.0);
        }
        if s.net.abs() > 1e-3 {
            let dir = up * s.net.signum();
            let start = c + dir * (BLOCK_INCLINE.x / 2.0 + 14.0) + out * (BLOCK_INCLINE.y / 2.0 + 10.0);
            net_arrow(&mut m, start, dir, &s);
        }
        m
    }

    fn vector_scene(&self, rect: (Vec2, Vec2), ink: bool) -> Vec<Mark> {
        let mut m = Vec::new();
        let starts = self.vector_starts();
        let r = self.resultant();
        // All the points the figure reaches (squares).
        let mut pts = vec![(0, 0), r];
        for (s, v) in starts.iter().zip(&self.vectors) {
            pts.push(*s);
            pts.push((s.0 + v.0, s.1 + v.1));
        }
        if self.uses_parallelogram() {
            pts.push(r);
        }
        let origin = if ink {
            let (lo_x, hi_x) = (pts.iter().map(|p| p.0).min().unwrap(), pts.iter().map(|p| p.0).max().unwrap());
            let (lo_y, hi_y) = (pts.iter().map(|p| p.1).min().unwrap(), pts.iter().map(|p| p.1).max().unwrap());
            let center = rect.0 + rect.1 / 2.0;
            center - Vec2::new((lo_x + hi_x) as f32 / 2.0 * GRID, -(lo_y + hi_y) as f32 / 2.0 * GRID)
        } else {
            self.grid_origin()
        };
        let (a0, a1) = (rect.0, rect.0 + rect.1);
        if !ink {
            let mut x = origin.x - ((origin.x - a0.x) / GRID).floor() * GRID;
            while x <= a1.x {
                m.push(Mark::line(vec![Vec2::new(x, a0.y + 30.0), Vec2::new(x, a1.y)], 1.0, screen(COLOR_GRID)));
                x += GRID;
            }
            let mut y = origin.y - ((origin.y - a0.y - 30.0) / GRID).floor() * GRID;
            while y <= a1.y {
                m.push(Mark::line(vec![Vec2::new(a0.x, y), Vec2::new(a1.x, y)], 1.0, screen(COLOR_GRID)));
                y += GRID;
            }
            m.push(Mark::arrow(Vec2::new(a0.x + 10.0, origin.y), Vec2::new(a1.x - 10.0, origin.y), 1.6, screen(COLOR_TEXT_DIM)));
            m.push(Mark::arrow(Vec2::new(origin.x, a1.y - 10.0), Vec2::new(origin.x, a0.y + 40.0), 1.6, screen(COLOR_TEXT_DIM)));
            m.push(Mark::text("1 kare = 1 birim", Vec2::new(a1.x - 16.0, a0.y + 12.0), 14.0, screen(COLOR_TEXT_DIM), Anchor::Right));
        } else {
            // Short axes through the origin.
            m.push(Mark::arrow(origin - Vec2::new(GRID, 0.0), origin + Vec2::new(GRID * 1.5, 0.0), 1.6, ink_line()));
            m.push(Mark::arrow(origin + Vec2::new(0.0, GRID), origin - Vec2::new(0.0, GRID * 1.5), 1.6, ink_line()));
            m.push(Mark::text("x", origin + Vec2::new(GRID * 1.5 + 6.0, -8.0), 14.0, ink_line(), Anchor::Left));
            m.push(Mark::text("y", origin + Vec2::new(8.0, -GRID * 1.5 - 8.0), 14.0, ink_line(), Anchor::Left));
        }

        if self.uses_parallelogram() {
            // Each vector again from the other's tip, dashed.
            let (a, b) = (self.vectors[0], self.vectors[1]);
            for (from, v, c) in [(a, b, COLOR_VECS[1]), (b, a, COLOR_VECS[0])] {
                m.push(Mark::dashed(vec![grid_pt(origin, from), grid_pt(origin, (from.0 + v.0, from.1 + v.1))], 1.6, inked(c)));
            }
        }
        for (i, (s, v)) in starts.iter().zip(&self.vectors).enumerate() {
            let (p0, p1) = (grid_pt(origin, *s), grid_pt(origin, (s.0 + v.0, s.1 + v.1)));
            m.push(Mark::arrow(p0, p1, 3.0, inked(COLOR_VECS[i])));
            let mid = (p0 + p1) / 2.0;
            let side = (p1 - p0).normalize_or_zero().perp() * 16.0;
            let at = mid - side;
            m.push(Mark::text(VEC_NAMES[i], at - Vec2::new(0.0, 8.0), 17.0, inked(COLOR_VECS[i]), Anchor::Center));
            if !ink {
                m.push(Mark::Dot { c: p1, r: 9.0, style: screen(COLOR_VECS[i]), filled: true });
            }
        }
        if r != (0, 0) {
            let tip = grid_pt(origin, r);
            if self.components && r.0 != 0 && r.1 != 0 {
                let corner = grid_pt(origin, (r.0, 0));
                m.push(Mark::Arrow { a: origin, b: corner, w: 1.8, style: inked(COLOR_NET), dash: true });
                m.push(Mark::Arrow { a: corner, b: tip, w: 1.8, style: inked(COLOR_NET), dash: true });
                let below = if r.1 > 0 { 10.0 } else { -26.0 };
                m.push(Mark::text(format!("R_x = {}", r.0), (origin + corner) / 2.0 + Vec2::new(0.0, below), 15.0, inked(COLOR_NET), Anchor::Center));
                let side = if r.0 > 0 { Anchor::Left } else { Anchor::Right };
                let dx = if r.0 > 0 { 10.0 } else { -10.0 };
                m.push(Mark::text(format!("R_y = {}", r.1), (corner + tip) / 2.0 + Vec2::new(dx, -8.0), 15.0, inked(COLOR_NET), side));
            }
            m.push(Mark::arrow(origin, tip, 3.6, inked(COLOR_NET)));
            let len = ((r.0 * r.0 + r.1 * r.1) as f32).sqrt();
            let d = (tip - origin).normalize_or_zero();
            let anchor = if d.x >= 0.0 { Anchor::Left } else { Anchor::Right };
            m.push(Mark::text(format!("R = {}", num(len, 2)), tip + d * 14.0 + Vec2::new(0.0, -8.0), 17.0, inked(COLOR_NET), anchor));
        } else if !ink {
            m.push(Mark::text("Bileşke sıfır: vektörler birbirini dengeliyor", Vec2::new(a0.x + rect.1.x / 2.0, a1.y - 34.0), 16.0, screen(COLOR_TEXT), Anchor::Center));
        }
        m
    }

    // --- Ink -------------------------------------------------------------

    pub fn stamp_lines(&self, screen_size: Vec2, ink: [f32; 4], now: f64) -> Vec<InkLine> {
        let size = Vec2::new(STAMP_W.min(screen_size.x - 160.0), STAMP_H.min(screen_size.y - 240.0));
        let top_left = Vec2::new((screen_size.x - size.x) / 2.0, ((screen_size.y - 130.0 - size.y) / 2.0).max(20.0));
        marks::to_ink(&self.scene((top_left, size), now, true), ink)
    }

    pub fn mode_name(&self) -> &'static str {
        MODES.iter().find(|(m, _)| *m == self.mode).map(|(_, n)| *n).unwrap_or("")
    }

    // --- Drawing ---------------------------------------------------------

    fn result_lines(&self) -> Vec<(String, [f32; 4])> {
        if self.mode == Mode::Vectors {
            let mut out = Vec::new();
            for pair in self.vectors.chunks(2).enumerate() {
                let (row, vs) = pair;
                let line = vs
                    .iter()
                    .enumerate()
                    .map(|(j, v)| format!("{} = {}", VEC_NAMES[row * 2 + j], num(((v.0 * v.0 + v.1 * v.1) as f32).sqrt(), 2)))
                    .collect::<Vec<_>>()
                    .join("     ");
                out.push((line, COLOR_TEXT));
            }
            let r = self.resultant();
            let len = ((r.0 * r.0 + r.1 * r.1) as f32).sqrt();
            out.push((format!("Bileşke R = {} birim", num(len, 2)), COLOR_NET));
            let angle = if r == (0, 0) { String::new() } else { format!("   açı = {}°", num((r.1 as f32).atan2(r.0 as f32).to_degrees(), 1)) };
            out.push((format!("R_x = {}   R_y = {}{angle}", r.0, r.1), COLOR_NET));
            return out;
        }
        let s = self.solution();
        let state = if s.lifted {
            "Cisim yerden kalkar (F_y > G)".to_string()
        } else if s.at_rest {
            format!("Cisim duruyor (f_{{s,en çok}} = {} N)", num(s.friction_max, 1))
        } else {
            "Cisim hızlanıyor".to_string()
        };
        vec![
            (state, if s.at_rest { COLOR_TEXT } else { COLOR_NET }),
            (format!("Net kuvvet: {} N", num(s.net.abs(), 1)), COLOR_TEXT),
            (format!("İvme: {} m/s^2", num(s.accel.abs(), 2)), COLOR_TEXT),
            (format!("Sürtünme: {} N ({})", num(s.friction, 1), if s.at_rest { "statik" } else { "kinetik" }), COLOR_FRICTION),
            (format!("Tepki kuvveti: {} N", num(s.normal, 1)), COLOR_N),
        ]
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
        font_atlas.push_text("Kuvvet ve vektörler", header_pos + Vec2::new(PADDING, 10.0), 16.0, COLOR_TEXT, gv, gi);
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
        let marks = self.scene(view, now, false);
        marks::draw(&marks, font_atlas, v, i, gv, gi);
        let hint = match self.mode {
            Mode::Vectors => "Okların uçlarını parmakla sürükleyin".to_string(),
            _ if self.is_animating(now) => {
                let t = self.slide_time(now);
                let a = self.solution().accel.abs();
                format!("t = {} s   v = {} m/s   x = {} m", num(t, 2), num(a * t, 2), num(0.5 * a * t * t, 2))
            }
            _ => String::new(),
        };
        font_atlas.push_text(&hint, view.0 + Vec2::new(16.0, 14.0), 15.0, COLOR_TEXT_DIM, gv, gi);

        // Side column.
        for (k, (mode, name)) in MODES.iter().enumerate() {
            button(self.cell_rect(k), name, if *mode == self.mode { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE }, font_atlas, v, i, gv, gi);
        }
        let planes = self.mode != Mode::Vectors;
        let g_label = format!("g = {} m/s²", if self.g_ten { "10" } else { "9,8" });
        button(self.g_rect(), &g_label, COLOR_KEY_IDLE, font_atlas, v, i, gv, gi);
        if !planes {
            push_rect(self.g_rect().0, self.g_rect().1, [0.12, 0.13, 0.17, 0.6], v, i);
        }
        if planes {
            for (k, param) in self.params().into_iter().enumerate() {
                let (pos, size) = self.slot_rect(1 + k);
                push_rect(pos, size, COLOR_FIELD, v, i);
                push_rich(font_atlas, &self.slider_label(param), pos + Vec2::new(12.0, 14.0), 16.0, COLOR_TEXT, Anchor::Left, gv, gi);
                let (x0, x1, y) = self.slider_track(1 + k);
                let (lo, hi, _) = param.range();
                let x = x0 + (x1 - x0) * (self.value(param) - lo) / (hi - lo);
                push_line(Vec2::new(x0, y), Vec2::new(x1, y), 4.0, COLOR_KEY_IDLE, v, i);
                push_line(Vec2::new(x0, y), Vec2::new(x, y), 4.0, COLOR_SLIDER, v, i);
                push_circle(Vec2::new(x, y), 11.0, COLOR_TEXT, 20, v, i);
            }
        } else {
            let full = self.vectors.len() >= MAX_VECTORS;
            button(self.add_rect(), "+ Vektör", if full { COLOR_FIELD } else { COLOR_KEY_IDLE }, font_atlas, v, i, gv, gi);
            button(self.remove_rect(), "− Vektör", if self.vectors.len() <= 1 { COLOR_FIELD } else { COLOR_KEY_IDLE }, font_atlas, v, i, gv, gi);
            button(self.tip_to_tail_rect(), "Uç uca", if self.parallelogram { COLOR_KEY_IDLE } else { COLOR_KEY_ACTIVE }, font_atlas, v, i, gv, gi);
            button(self.parallelogram_rect(), "Paralelkenar", if self.parallelogram { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE }, font_atlas, v, i, gv, gi);
            let (pos, _) = self.slot_rect(3);
            let notes: &[&str] = if self.parallelogram && self.vectors.len() != 2 {
                &["Paralelkenar yöntemi iki vektörle", "çizilir; şimdilik uç uca gösteriliyor."]
            } else {
                &["Bileşke, ilk vektörün başlangıcından", "son vektörün ucuna çizilen vektördür."]
            };
            for (k, note) in notes.iter().enumerate() {
                font_atlas.push_text(note, pos + Vec2::new(4.0, 10.0 + k as f32 * 22.0), 14.0, COLOR_TEXT_DIM, gv, gi);
            }
        }

        let (rpos, rsize) = self.slot_rect(6);
        push_rect(rpos, rsize, COLOR_FIELD, v, i);
        for (k, (line, color)) in self.result_lines().iter().enumerate() {
            push_rich(font_atlas, line, rpos + Vec2::new(12.0, 12.0 + k as f32 * 24.5), 15.0, *color, Anchor::Left, gv, gi);
        }

        button(self.components_rect(), "Bileşenler", if self.components { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE }, font_atlas, v, i, gv, gi);
        if planes {
            let running = self.playing.is_some();
            button(self.play_rect(), if running { "Durdur" } else { "Başlat" }, if running { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE }, font_atlas, v, i, gv, gi);
        }
        button(self.stamp_rect(), "Tahtaya bas", COLOR_KEY_STAMP, font_atlas, v, i, gv, gi);
    }
}

fn ink_line() -> Style {
    inked(COLOR_LINE)
}

fn grid_pt(origin: Vec2, (x, y): (i32, i32)) -> Vec2 {
    origin + Vec2::new(x as f32 * GRID, -(y as f32) * GRID)
}

/// A surface line with hatching below it.
fn surface(m: &mut Vec<Mark>, a: Vec2, b: Vec2) {
    m.push(Mark::line(vec![a, b], 2.5, ink_line()));
    let mut x = a.x + 10.0;
    while x < b.x {
        m.push(Mark::line(vec![Vec2::new(x, a.y), Vec2::new(x - 10.0, a.y + 10.0)], 1.4, ink_line()));
        x += 22.0;
    }
}

/// Where to put an arrow's label: past its tip, on the side it points to.
fn label_at(tip: Vec2, dir: Vec2) -> (Vec2, Anchor) {
    let anchor = if dir.x > 0.3 {
        Anchor::Left
    } else if dir.x < -0.3 {
        Anchor::Right
    } else {
        Anchor::Center
    };
    let pos = tip + dir * 12.0 + Vec2::new(if anchor == Anchor::Center { 0.0 } else { dir.x.signum() * 4.0 }, if dir.y > 0.5 { 4.0 } else if dir.y < -0.5 { -22.0 } else { -8.0 });
    (pos, anchor)
}

fn force_arrow(m: &mut Vec<Mark>, from: Vec2, vec: Vec2, label: &str, color: [f32; 4]) {
    force_arrow_nudged(m, from, vec, label, color, Vec2::ZERO);
}

fn force_arrow_nudged(m: &mut Vec<Mark>, from: Vec2, vec: Vec2, label: &str, color: [f32; 4], nudge: Vec2) {
    if vec.length() < 2.0 {
        return;
    }
    m.push(Mark::arrow(from, from + vec, 3.0, inked(color)));
    let (pos, anchor) = label_at(from + vec, vec.normalize());
    m.push(Mark::text(label, pos + nudge, 15.0, inked(color), anchor));
}

fn dashed_arrow(m: &mut Vec<Mark>, from: Vec2, vec: Vec2, label: &str, color: [f32; 4], nudge: Vec2) {
    if vec.length() < 2.0 {
        return;
    }
    m.push(Mark::Arrow { a: from, b: from + vec, w: 1.8, style: inked(color), dash: true });
    let (pos, anchor) = label_at(from + vec, vec.normalize());
    m.push(Mark::text(label, pos + nudge, 14.0, inked(color), anchor));
}

/// Arc at `center` from screen angle `a0` to `a1` (radians, math sense:
/// counter-clockwise on screen = up), labeled at its middle.
fn angle_arc(m: &mut Vec<Mark>, center: Vec2, a0: f32, a1: f32, r: f32, label: &str) {
    let pt = |a: f32| center + Vec2::new(a.cos(), -a.sin()) * r;
    m.push(Mark::line((0..=18).map(|i| pt(a0 + (a1 - a0) * i as f32 / 18.0)).collect(), 1.4, ink_line()));
    let mid = (a0 + a1) / 2.0;
    let d = Vec2::new(mid.cos(), -mid.sin());
    let at = center + d * (r + 12.0);
    m.push(Mark::text(label, at + Vec2::new(0.0, -7.0), 14.0, ink_line(), if d.x >= 0.0 { Anchor::Left } else { Anchor::Right }));
}

/// The net force / acceleration arrow next to the block.
fn net_arrow(m: &mut Vec<Mark>, start: Vec2, dir: Vec2, s: &Solution) {
    let end = start + dir * 90.0;
    m.push(Mark::arrow(start, end, 3.0, inked(COLOR_NET)));
    let anchor = if dir.x >= 0.0 { Anchor::Left } else { Anchor::Right };
    let pos = end + dir * 10.0 + Vec2::new(0.0, -26.0);
    m.push(Mark::text(format!("F_{{net}} = {} N", num(s.net.abs(), 1)), pos, 15.0, inked(COLOR_NET), anchor));
    m.push(Mark::text(format!("a = {} m/s^2", num(s.accel.abs(), 2)), pos + Vec2::new(0.0, 22.0), 15.0, inked(COLOR_NET), anchor));
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

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn flat_plane_answers() {
        // 5 kg, g = 10: N = 50, μs·N = 20. 15 N doesn't move it (fs = 15).
        let s = solve_flat(5.0, 15.0, 0.0, 0.4, 0.3, 10.0);
        assert!(s.at_rest && close(s.friction, 15.0) && close(s.accel, 0.0) && close(s.friction_max, 20.0));
        // 30 N does: fk = 15, net 15, a = 3.
        let s = solve_flat(5.0, 30.0, 0.0, 0.4, 0.3, 10.0);
        assert!(!s.at_rest && close(s.friction, 15.0) && close(s.net, 15.0) && close(s.accel, 3.0));
        // At 37° (cos 0,8 / sin 0,6 ≈) 50 N: Fy = 30 lightens N to 20.
        let s = solve_flat(5.0, 50.0, 36.8699, 0.4, 0.3, 10.0);
        assert!((s.normal - 20.0).abs() < 1e-2 && (s.net - 34.0).abs() < 1e-2, "{s:?}");
        // Pulled straight up harder than its weight.
        assert!(solve_flat(1.0, 50.0, 80.0, 0.4, 0.3, 10.0).lifted);
    }

    #[test]
    fn incline_answers() {
        // 30°, 2 kg, g = 10, frictionless: slides down at g·sin30 = 5.
        let s = solve_incline(2.0, 0.0, 30.0, 0.0, 0.0, 10.0);
        assert!(!s.at_rest && close(s.accel, -5.0) && close(s.g_along, 10.0) && (s.normal - 17.3205).abs() < 1e-3);
        // μs = 0,6 > tan30: stays, static friction = G sinθ up the slope.
        let s = solve_incline(2.0, 0.0, 30.0, 0.6, 0.5, 10.0);
        assert!(s.at_rest && close(s.friction, 10.0));
        // Pushed up hard: moves up, friction down.
        let s = solve_incline(2.0, 30.0, 30.0, 0.2, 0.2, 10.0);
        assert!(s.net > 0.0 && (s.accel - (20.0 - 0.2 * 17.3205) / 2.0).abs() < 1e-3);
    }

    #[test]
    fn panel_sliders_modes_and_vectors() {
        let mut f = Force::new(SCREEN);
        assert!(f.position.y + f.panel_height() <= SCREEN.y - 75.0, "{}", f.panel_height());
        let c = |r: (Vec2, Vec2)| r.0 + r.1 / 2.0;
        // μk above μs drags μs up with it.
        let (x0, x1, y) = f.slider_track(5);
        assert_eq!(f.press_at(Vec2::new(x0 + (x1 - x0) * 0.6, y), 0.0), ForcePress::Slider(Param::MuK));
        assert!(close(f.mu_k, 0.6) && close(f.mu_s, 0.6));
        let (x0, x1, _) = f.slider_track(4);
        f.set_slider(Param::MuS, x0 + (x1 - x0) * 0.2);
        assert!(close(f.mu_s, 0.2) && close(f.mu_k, 0.2));
        // Mass slider never reaches 0.
        f.set_slider(Param::Mass, -100.0);
        assert_eq!(f.mass, 1.0);
        // Play: slides and keeps animating while it accelerates.
        f.press_at(c(f.play_rect()), 5.0);
        assert!(f.is_animating(5.5) && f.slide_offset(5.5) > 0.0);
        // Vectors: (4,1)+(1,3) = (5,4); drag B's tip to (3,-1) from A's tip.
        f.press_at(c(f.cell_rect(2)), 0.0);
        assert_eq!(f.mode, Mode::Vectors);
        assert!(!f.is_animating(5.5));
        assert_eq!(f.resultant(), (5, 4));
        let origin = f.grid_origin();
        let tip_b = grid_pt(origin, (5, 4));
        assert_eq!(f.press_at(tip_b + Vec2::new(5.0, -5.0), 0.0), ForcePress::Drag(1));
        f.drag_vector(1, grid_pt(origin, (4 + 3, 1 - 1)) + Vec2::new(8.0, 6.0));
        assert_eq!(f.vectors[1], (3, -1));
        assert_eq!(f.resultant(), (7, 0));
        // Parallelogram: both from the origin; with three it falls back.
        f.press_at(c(f.parallelogram_rect()), 0.0);
        assert_eq!(f.vector_starts(), vec![(0, 0), (0, 0)]);
        f.press_at(c(f.add_rect()), 0.0);
        assert_eq!(f.vectors.len(), 3);
        assert_eq!(f.vector_starts()[2], (7, 0));
    }

    #[test]
    fn stamps_fit_and_have_glyphs() {
        let mut f = Force::new(SCREEN);
        for mode in [Mode::Flat, Mode::Incline, Mode::Vectors] {
            for (alpha, incline, force) in [(0.0, 30.0, 30.0), (40.0, 60.0, 0.0), (80.0, 0.0, 100.0)] {
                f.mode = mode;
                f.alpha = alpha;
                f.incline = incline;
                f.force = force;
                let marks = f.scene((Vec2::new(480.0, 120.0), Vec2::new(STAMP_W, STAMP_H)), 0.0, true);
                assert_eq!(marks::missing_glyphs(&marks), vec![]);
                let lines = f.stamp_lines(SCREEN, [1.0; 4], 0.0);
                assert!(lines.len() > 5);
                for l in &lines {
                    for p in &l.points {
                        assert!(p.x >= 0.0 && p.x <= SCREEN.x && p.y >= 0.0 && p.y <= SCREEN.y - 70.0, "{mode:?} {alpha} {incline}: {p:?}");
                    }
                }
            }
        }
    }
}
