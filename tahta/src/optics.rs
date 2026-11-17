//! Draggable "Optik" panel (physics): ray diagrams for plane, concave and
//! convex mirrors and converging/diverging thin lenses, and refraction at
//! a boundary (Snell's law, critical angle, total internal reflection).
//!
//! Mirrors and lenses: the object (an upright arrow) is set by sliders or
//! dragged by its stem (distance) or tip (distance and height); the
//! special rays are drawn to where they meet — the image — solid for a
//! real image, dashed back-extensions for a virtual one, with d′, h′, the
//! magnification and the image's nature. Every outgoing ray is aimed
//! through (or away from) the computed image point, so the rays always
//! meet exactly where the thin-lens/mirror equation puts the image.
//! Refraction: drag the incoming ray or set θ₁; the media are picked
//! from air, water, glass and diamond. "Tahtaya bas" stamps the diagram.

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
/// Drawing scale limits (px per cm); the scale fits the figure in view.
const PX_MIN: f32 = 3.0;
const PX_MAX: f32 = 14.0;
/// Outgoing rays end this far past a real image, or this long (px).
const RAY_PAST: f32 = 150.0;
const RAY_VIRTUAL: f32 = 300.0;
/// Half height of a drawn mirror / lens (px) and a curved mirror's depth.
const ELEMENT_H: f32 = 190.0;
const SAG: f32 = 14.0;
/// Refraction: length of the drawn rays (px).
const RAY_LEN: f32 = 240.0;
const STAMP_W: f32 = 900.0;
const STAMP_H: f32 = 560.0;

const COLOR_PANEL_BG: [f32; 4] = [0.12, 0.13, 0.17, 0.96];
const COLOR_HEADER: [f32; 4] = [0.18, 0.19, 0.24, 1.0];
const COLOR_FIELD: [f32; 4] = [0.06, 0.07, 0.09, 1.0];
const COLOR_TEXT: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_TEXT_DIM: [f32; 4] = [0.95, 0.95, 0.97, 0.55];
const COLOR_LINE: [f32; 4] = [0.85, 0.87, 0.92, 1.0];
const COLOR_AXIS: [f32; 4] = [0.85, 0.87, 0.92, 0.6];
const COLOR_OBJECT: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_IMAGE: [f32; 4] = [1.0, 0.80, 0.30, 1.0];
const COLOR_RAYS: [[f32; 4]; 4] = [[0.98, 0.55, 0.25, 1.0], [0.40, 0.85, 0.50, 1.0], [0.40, 0.68, 1.0, 1.0], [0.95, 0.50, 0.75, 1.0]];
const COLOR_GLASS: [f32; 4] = [0.55, 0.80, 1.0, 1.0];
const COLOR_WARN: [f32; 4] = [1.0, 0.55, 0.40, 1.0];
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
    Plane,
    Concave,
    Convex,
    Converging,
    Diverging,
    Refraction,
}

const MODES: [(Mode, &str); 6] = [
    (Mode::Plane, "Düz ayna"),
    (Mode::Concave, "Çukur ayna"),
    (Mode::Convex, "Tümsek ayna"),
    (Mode::Converging, "İnce kenarlı mercek"),
    (Mode::Diverging, "Kalın kenarlı mercek"),
    (Mode::Refraction, "Kırılma"),
];

impl Mode {
    fn is_mirror(self) -> bool {
        matches!(self, Mode::Plane | Mode::Concave | Mode::Convex)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Param {
    Distance,
    Height,
    Focal,
    Incidence,
}

impl Param {
    fn range(self) -> (f32, f32) {
        match self {
            Param::Distance => (2.0, 60.0),
            Param::Height => (2.0, 15.0),
            Param::Focal => (4.0, 30.0),
            Param::Incidence => (0.0, 89.0),
        }
    }
}

const MEDIA: [(&str, f32); 4] = [("Hava", 1.00), ("Su", 1.33), ("Cam", 1.50), ("Elmas", 2.42)];

/// Where a mirror or lens puts the image (cm; real-is-positive).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Image {
    /// Image distance d′: + real (in front of a mirror / behind a lens),
    /// − virtual. Infinite when the object sits at the focus.
    pub distance: f32,
    /// Image height h′: + upright, − inverted.
    pub height: f32,
}

impl Image {
    pub fn at_infinity(&self) -> bool {
        !self.distance.is_finite()
    }

    pub fn magnification(&self, h: f32) -> f32 {
        self.height / h
    }

    pub fn is_real(&self) -> bool {
        self.distance > 0.0
    }

    /// "Gerçek, ters, büyük" etc.
    pub fn nature(&self, h: f32) -> String {
        if self.at_infinity() {
            return "Görüntü oluşmaz (ışınlar paralel)".into();
        }
        let m = self.magnification(h).abs();
        let size = if (m - 1.0).abs() < 0.005 { "cisimle aynı boyda" } else if m > 1.0 { "cisimden büyük" } else { "cisimden küçük" };
        format!("{}, {}, {size}", if self.is_real() { "Gerçek" } else { "Sanal" }, if self.height < 0.0 { "ters" } else { "düz" })
    }
}

/// 1/f = 1/d + 1/d′ and h′ = −d′/d · h. `f` is signed (concave mirror and
/// converging lens +, convex and diverging −); `None` for a plane mirror.
pub fn image(f: Option<f32>, d: f32, h: f32) -> Image {
    let Some(f) = f else { return Image { distance: -d, height: h } };
    if (d - f).abs() < 1e-4 {
        return Image { distance: f32::INFINITY, height: f32::INFINITY };
    }
    let di = d * f / (d - f);
    Image { distance: di, height: -di / d * h }
}

/// Refraction: θ₂ in degrees, or `None` for total internal reflection.
pub fn refract(n1: f32, n2: f32, theta1: f32) -> Option<f32> {
    let s = n1 / n2 * theta1.to_radians().sin();
    (s <= 1.0).then(|| s.asin().to_degrees())
}

pub fn critical_angle(n1: f32, n2: f32) -> Option<f32> {
    (n1 > n2).then(|| (n2 / n1).asin().to_degrees())
}

/// Clips segment a→b to the rectangle.
fn clip(a: Vec2, b: Vec2, (pos, size): (Vec2, Vec2)) -> Option<(Vec2, Vec2)> {
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    let d = b - a;
    let lo = pos;
    let hi = pos + size;
    for (p, q) in [(-d.x, a.x - lo.x), (d.x, hi.x - a.x), (-d.y, a.y - lo.y), (d.y, hi.y - a.y)] {
        if p.abs() < 1e-9 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
        }
    }
    (t0 <= t1).then(|| (a + d * t0, a + d * t1))
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum OpticsPress {
    Move,
    Close,
    Slider(Param),
    /// Finger on the object (or the incoming ray): drags it.
    Drag,
    Stamp,
    Other,
}

pub struct Optics {
    pub position: Vec2,
    mode: Mode,
    distance: f32,
    height: f32,
    focal: f32,
    incidence: f32,
    medium1: usize,
    medium2: usize,
    rays: bool,
    labels: bool,
    /// Dragging the object by its tip (height follows too).
    drag_tip: bool,
    /// Scale held while a finger drags or slides, so the figure doesn't
    /// rescale under it.
    frozen_px: Option<f32>,
}

/// Geometry of the current mirror/lens diagram in some rectangle.
struct Layout {
    x0: f32,
    axis: f32,
    /// px per cm.
    px: f32,
}

impl Optics {
    pub fn new(screen_size: Vec2) -> Self {
        let mut o = Self {
            position: Vec2::ZERO,
            mode: Mode::Concave,
            distance: 30.0,
            height: 8.0,
            focal: 10.0,
            incidence: 40.0,
            medium1: 0,
            medium2: 2,
            rays: true,
            labels: true,
            drag_tip: false,
            frozen_px: None,
        };
        let size = Vec2::new(o.panel_width(), o.panel_height());
        o.position = Vec2::new((screen_size.x - size.x) / 2.0, ((screen_size.y - 120.0 - size.y) / 2.0).max(12.0));
        o
    }

    /// Signed focal length (cm), `None` for the plane mirror.
    fn signed_focal(&self) -> Option<f32> {
        match self.mode {
            Mode::Plane | Mode::Refraction => None,
            Mode::Concave | Mode::Converging => Some(self.focal),
            Mode::Convex | Mode::Diverging => Some(-self.focal),
        }
    }

    pub fn image(&self) -> Image {
        image(self.signed_focal(), self.distance, self.height)
    }

    fn params(&self) -> [Option<Param>; 3] {
        match self.mode {
            Mode::Refraction => [Some(Param::Incidence), None, None],
            Mode::Plane => [Some(Param::Distance), Some(Param::Height), None],
            _ => [Some(Param::Distance), Some(Param::Height), Some(Param::Focal)],
        }
    }

    fn value(&self, p: Param) -> f32 {
        match p {
            Param::Distance => self.distance,
            Param::Height => self.height,
            Param::Focal => self.focal,
            Param::Incidence => self.incidence,
        }
    }

    fn set(&mut self, p: Param, v: f32) {
        let (lo, hi) = p.range();
        let v = v.round().clamp(lo, hi);
        match p {
            Param::Distance => self.distance = v,
            Param::Height => self.height = v,
            Param::Focal => self.focal = v,
            Param::Incidence => self.incidence = v,
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

    fn slider_track(&self, slot: usize) -> (f32, f32, f32) {
        let (pos, size) = self.slot_rect(slot);
        (pos.x + LABEL_W, pos.x + size.x - 20.0, pos.y + size.y / 2.0)
    }

    fn half(&self, slot: usize, j: usize) -> (Vec2, Vec2) {
        let (pos, size) = self.slot_rect(slot);
        let w = (size.x - GAP) / 2.0;
        (pos + Vec2::new(j as f32 * (w + GAP), 0.0), Vec2::new(w, size.y))
    }

    fn rays_rect(&self) -> (Vec2, Vec2) {
        self.half(5, 0)
    }

    fn labels_rect(&self) -> (Vec2, Vec2) {
        self.half(5, 1)
    }

    fn stamp_rect(&self) -> (Vec2, Vec2) {
        self.slot_rect(6)
    }

    /// Mirror/lens placement in `rect`: mirrors toward the right (object
    /// and real images in front), lenses centered.
    fn layout(&self, rect: (Vec2, Vec2)) -> Layout {
        let frac = if self.mode.is_mirror() { 0.62 } else { 0.5 };
        let (x0, axis) = (rect.0.x + rect.1.x * frac, rect.0.y + rect.1.y * 0.55);
        let px = self.frozen_px.unwrap_or_else(|| self.fit_scale(rect, x0, axis));
        Layout { x0, axis, px }
    }

    /// Largest scale that shows the object, the image and the focal /
    /// center points (a very far virtual image may still fall outside).
    fn fit_scale(&self, rect: (Vec2, Vec2), x0: f32, axis: f32) -> f32 {
        let img = self.image();
        let f = self.focal;
        let cap = 2.5 * self.distance.max(2.0 * f);
        let d_img = if img.at_infinity() { 0.0 } else { img.distance.abs().min(cap) };
        let (mut left, mut right) = (self.distance, 0.0f32);
        match self.mode {
            Mode::Plane => right = d_img,
            Mode::Concave => {
                left = left.max(2.0 * f);
                if img.is_real() { left = left.max(d_img) } else { right = d_img }
            }
            Mode::Convex => right = (2.0 * f).max(d_img),
            Mode::Converging | Mode::Diverging => {
                left = left.max(2.0 * f);
                right = 2.0 * f;
                if img.is_real() { right = right.max(d_img) } else { left = left.max(d_img) }
            }
            Mode::Refraction => {}
        }
        let mut px = (x0 - rect.0.x - 50.0) / left.max(1.0);
        if right > 0.0 {
            px = px.min((rect.0.x + rect.1.x - x0 - 50.0) / right);
        }
        let tall = self.height.max(if img.at_infinity() { 0.0 } else { img.height.abs().min(cap) });
        px = px.min((axis - rect.0.y - 50.0).min(rect.0.y + rect.1.y - axis - 90.0) / tall.max(1.0));
        px.clamp(PX_MIN, PX_MAX)
    }

    /// Finger lifted anywhere: let the figure fit itself again.
    pub fn release(&mut self) {
        self.frozen_px = None;
    }

    /// Object foot and tip on screen.
    fn object_pts(&self, l: &Layout) -> (Vec2, Vec2) {
        let x = l.x0 - self.distance * l.px;
        (Vec2::new(x, l.axis), Vec2::new(x, l.axis - self.height * l.px))
    }

    // --- Input -----------------------------------------------------------

    pub fn contains(&self, p: Vec2) -> bool {
        rect_contains((self.position, Vec2::new(self.panel_width(), self.panel_height())), p)
    }

    pub fn drag_to(&mut self, p: Vec2, grab_offset: Vec2) {
        self.position = p - grab_offset;
    }

    pub fn press_at(&mut self, p: Vec2) -> OpticsPress {
        let press = self.press_inner(p);
        if matches!(press, OpticsPress::Drag | OpticsPress::Slider(_)) && self.mode != Mode::Refraction {
            // Hold the scale the finger started with.
            let view = self.view_rect();
            self.frozen_px = None;
            let px = self.layout(view).px;
            self.frozen_px = Some(px);
        }
        press
    }

    fn press_inner(&mut self, p: Vec2) -> OpticsPress {
        if rect_contains(self.close_rect(), p) {
            return OpticsPress::Close;
        }
        if rect_contains(self.header_rect(), p) {
            return OpticsPress::Move;
        }
        if rect_contains(self.stamp_rect(), p) {
            return OpticsPress::Stamp;
        }
        if rect_contains(self.view_rect(), p) {
            if self.mode == Mode::Refraction {
                let o = self.refraction_origin(self.view_rect());
                if p.y < o.y {
                    self.drag_object(p);
                    return OpticsPress::Drag;
                }
                return OpticsPress::Other;
            }
            let l = self.layout(self.view_rect());
            let (foot, tip) = self.object_pts(&l);
            if p.distance(tip) <= 34.0 {
                self.drag_tip = true;
                return OpticsPress::Drag;
            }
            if (p.x - foot.x).abs() <= 30.0 && p.y <= foot.y + 20.0 && p.y >= tip.y - 20.0 {
                self.drag_tip = false;
                return OpticsPress::Drag;
            }
            return OpticsPress::Other;
        }
        if let Some(k) = (0..MODES.len()).find(|&k| rect_contains(self.mode_rect(k), p)) {
            self.mode = MODES[k].0;
            return OpticsPress::Other;
        }
        for (k, param) in self.params().into_iter().enumerate() {
            if !rect_contains(self.slot_rect(1 + k), p) {
                continue;
            }
            if let Some(param) = param {
                self.set_slider(param, p.x);
                return OpticsPress::Slider(param);
            }
            if self.mode == Mode::Refraction {
                if k == 1 {
                    self.medium1 = (self.medium1 + 1) % MEDIA.len();
                } else {
                    self.medium2 = (self.medium2 + 1) % MEDIA.len();
                }
            }
            return OpticsPress::Other;
        }
        if rect_contains(self.rays_rect(), p) {
            self.rays = !self.rays;
        } else if rect_contains(self.labels_rect(), p) {
            self.labels = !self.labels;
        }
        OpticsPress::Other
    }

    pub fn set_slider(&mut self, param: Param, x: f32) {
        let Some(k) = self.params().iter().position(|q| *q == Some(param)) else { return };
        let (x0, x1, _) = self.slider_track(1 + k);
        let (lo, hi) = param.range();
        self.set(param, lo + (hi - lo) * ((x - x0) / (x1 - x0)).clamp(0.0, 1.0));
    }

    /// Finger moving on the object / incoming ray.
    pub fn drag_object(&mut self, p: Vec2) {
        if self.mode == Mode::Refraction {
            let o = self.refraction_origin(self.view_rect());
            let v = p - o;
            self.set(Param::Incidence, v.x.abs().atan2((-v.y).max(1e-3)).to_degrees());
            return;
        }
        let l = self.layout(self.view_rect());
        self.set(Param::Distance, (l.x0 - p.x) / l.px);
        if self.drag_tip {
            self.set(Param::Height, (l.axis - p.y) / l.px);
        }
    }

    // --- Scenes ----------------------------------------------------------

    fn scene(&self, rect: (Vec2, Vec2), ink: bool) -> Vec<Mark> {
        if self.mode == Mode::Refraction {
            self.refraction_scene(rect, ink)
        } else {
            self.element_scene(rect, ink)
        }
    }

    /// x of the drawn mirror surface at height `y` (curved mirrors are a
    /// shallow arc: concave opens toward the object).
    fn mirror_x(&self, l: &Layout, y: f32) -> f32 {
        let t = ((y - l.axis) / ELEMENT_H).clamp(-1.0, 1.0);
        match self.mode {
            Mode::Concave => l.x0 + SAG * t * t,
            Mode::Convex => l.x0 - SAG * t * t,
            _ => l.x0,
        }
    }

    fn element_scene(&self, rect: (Vec2, Vec2), ink: bool) -> Vec<Mark> {
        let l = self.layout(rect);
        let mut m = Vec::new();
        let (r0, r1) = (rect.0, rect.0 + rect.1);
        // Principal axis.
        m.push(Mark::line(vec![Vec2::new(r0.x + 10.0, l.axis), Vec2::new(r1.x - 10.0, l.axis)], 1.4, inked(COLOR_AXIS)));

        // The element.
        let ys: Vec<f32> = (0..=24).map(|i| l.axis - ELEMENT_H + 2.0 * ELEMENT_H * i as f32 / 24.0).collect();
        match self.mode {
            Mode::Plane | Mode::Concave | Mode::Convex => {
                let pts: Vec<Vec2> = ys.iter().map(|&y| Vec2::new(self.mirror_x(&l, y), y)).collect();
                m.push(Mark::line(pts.clone(), 3.0, inked(COLOR_GLASS)));
                // Hatching on the back.
                for w in pts.iter().step_by(2) {
                    m.push(Mark::line(vec![*w, *w + Vec2::new(12.0, 10.0)], 1.4, inked(COLOR_GLASS)));
                }
            }
            Mode::Converging | Mode::Diverging => {
                // Half thickness: thick middle (converging) or thick edges.
                let converging = self.mode == Mode::Converging;
                let side = |s: f32| -> Vec<Vec2> {
                    ys.iter()
                        .map(|&y| {
                            let t = (y - l.axis) / ELEMENT_H;
                            let half = if converging { 2.0 + 16.0 * (1.0 - t * t) } else { 4.0 + 12.0 * t * t };
                            Vec2::new(l.x0 + s * half, y)
                        })
                        .collect()
                };
                let (left, right) = (side(-1.0), side(1.0));
                m.push(Mark::line(left.clone(), 2.2, inked(COLOR_GLASS)));
                m.push(Mark::line(right.clone(), 2.2, inked(COLOR_GLASS)));
                m.push(Mark::line(vec![left[0], right[0]], 2.2, inked(COLOR_GLASS)));
                m.push(Mark::line(vec![left[24], right[24]], 2.2, inked(COLOR_GLASS)));
            }
            Mode::Refraction => {}
        }

        // Focal and center points.
        if let Some(f) = self.signed_focal() {
            let mark = |m: &mut Vec<Mark>, x: f32, name: &str| {
                if x > r0.x + 8.0 && x < r1.x - 8.0 {
                    m.push(Mark::line(vec![Vec2::new(x, l.axis - 6.0), Vec2::new(x, l.axis + 6.0)], 2.0, inked(COLOR_LINE)));
                    m.push(Mark::text(name, Vec2::new(x, l.axis + 12.0), 15.0, inked(COLOR_LINE), Anchor::Center));
                }
            };
            if self.mode.is_mirror() {
                mark(&mut m, l.x0 - f * l.px, "F");
                mark(&mut m, l.x0 - 2.0 * f * l.px, "M");
            } else {
                let a = f.abs() * l.px;
                mark(&mut m, l.x0 - a, "F");
                mark(&mut m, l.x0 + a, "F");
                mark(&mut m, l.x0 - 2.0 * a, "2F");
                mark(&mut m, l.x0 + 2.0 * a, "2F");
            }
        }

        // Object.
        let (foot, tip) = self.object_pts(&l);
        m.push(Mark::arrow(foot, tip, 3.0, inked(COLOR_OBJECT)));

        // Image.
        let img = self.image();
        // Object at the focus: aim at a far point so the rays come out
        // (almost) parallel.
        let aim = if img.at_infinity() { image(self.signed_focal(), self.distance + 0.02, self.height) } else { img };
        let toward = |d: f32| -> f32 {
            if self.mode.is_mirror() {
                l.x0 - d * l.px
            } else {
                l.x0 + d * l.px
            }
        };
        let i_tip = Vec2::new(toward(aim.distance), l.axis - aim.height * l.px);
        let i_foot = Vec2::new(i_tip.x, l.axis);
        if !img.at_infinity() {
            if let Some((a, b)) = clip(i_foot, i_tip, rect) {
                if a.distance(i_foot) < 1.0 && b.distance(i_tip) < 1.0 {
                    m.push(Mark::Arrow { a: i_foot, b: i_tip, w: 3.0, style: inked(COLOR_IMAGE), dash: !img.is_real() });
                }
            }
        }

        // Special rays: incoming from the object tip to a hit point on the
        // element; outgoing through the image tip (real) or away from it
        // (virtual, dashed back to it).
        if self.rays {
            let hit_x = |y: f32| if self.mode.is_mirror() { self.mirror_x(&l, y) } else { l.x0 };
            let through = |p: Vec2| -> Option<f32> {
                // Where the line tip→p (extended) meets the element.
                let dx = p.x - tip.x;
                if dx.abs() < 1e-3 {
                    return None;
                }
                let y = tip.y + (p.y - tip.y) * (l.x0 - tip.x) / dx;
                ((y - l.axis).abs() <= ELEMENT_H * 1.3).then_some(y)
            };
            let mut hits: Vec<(f32, Option<Vec2>)> = Vec::new();
            // 1: parallel to the axis.
            hits.push((tip.y, None));
            match (self.mode, self.signed_focal()) {
                (Mode::Plane, _) => hits.push((l.axis, None)),
                (_, Some(f)) => {
                    // 2: through (or toward) the focus on the object's side.
                    let focus = Vec2::new(l.x0 - f * l.px, l.axis);
                    if let Some(y) = through(focus) {
                        hits.push((y, Some(focus)));
                    }
                    if self.mode.is_mirror() {
                        // 3: through the center of curvature; 4: to the vertex.
                        let center = Vec2::new(l.x0 - 2.0 * f * l.px, l.axis);
                        if let Some(y) = through(center) {
                            hits.push((y, Some(center)));
                        }
                    }
                    // Through the optical center / to the vertex.
                    hits.push((l.axis, None));
                }
                _ => {}
            }
            for (k, (y, via)) in hits.into_iter().enumerate() {
                let color = COLOR_RAYS[k % COLOR_RAYS.len()];
                let h = Vec2::new(hit_x(y), y);
                m.push(Mark::arrow(tip, (tip + h) / 2.0, 2.0, inked(color)));
                m.push(Mark::line(vec![tip, h], 2.0, inked(color)));
                // A ray aimed at a point it doesn't pass before the element
                // (focus behind the object): dashed guide to it.
                if let Some(v) = via {
                    let before = (v.x - tip.x) * (h.x - tip.x) > 0.0 && (v.x - tip.x).abs() < (h.x - tip.x).abs();
                    if !before {
                        if let Some((a, b)) = clip(v, tip, rect) {
                            m.push(Mark::dashed(vec![a, b], 1.4, inked(color)));
                        }
                    }
                }
                let dir = if img.is_real() || img.at_infinity() && aim.is_real() { (i_tip - h).normalize_or_zero() } else { (h - i_tip).normalize_or_zero() };
                let reach = if img.is_real() {
                    h.distance(i_tip) + RAY_PAST
                } else if img.at_infinity() {
                    2.0 * RAY_VIRTUAL
                } else {
                    RAY_VIRTUAL
                };
                if let Some((a, b)) = clip(h, h + dir * reach, rect) {
                    m.push(Mark::line(vec![a, b], 2.0, inked(color)));
                    m.push(Mark::arrow(a, a + dir * 70.0, 2.0, inked(color)));
                }
                if !img.is_real() && !img.at_infinity() {
                    if let Some((a, b)) = clip(h, i_tip, rect) {
                        m.push(Mark::dashed(vec![a, b], 1.4, inked(color)));
                    }
                }
            }
        }

        // Distances.
        if self.labels {
            // Dimension lines along the bottom, with thin guides up to
            // the axis.
            let y = r1.y - 74.0;
            let dim = |m: &mut Vec<Mark>, x_a: f32, x_b: f32, text: String, y: f32| {
                let (xa, xb) = (x_a.clamp(r0.x + 4.0, r1.x - 4.0), x_b.clamp(r0.x + 4.0, r1.x - 4.0));
                if (xb - xa).abs() < 12.0 {
                    return;
                }
                for x in [xa, xb] {
                    m.push(Mark::line(vec![Vec2::new(x, y - 8.0), Vec2::new(x, y + 8.0)], 1.2, inked(COLOR_AXIS)));
                }
                let (a, b) = (Vec2::new(xa, y), Vec2::new(xb, y));
                m.push(Mark::arrow((a + b) / 2.0, a, 1.4, inked(COLOR_LINE)));
                m.push(Mark::arrow((a + b) / 2.0, b, 1.4, inked(COLOR_LINE)));
                m.push(Mark::text(text, Vec2::new((xa + xb) / 2.0, y + 6.0), 14.0, inked(COLOR_LINE), Anchor::Center));
            };
            dim(&mut m, foot.x, l.x0, format!("d = {} cm", num(self.distance, 0)), y);
            if !img.at_infinity() {
                dim(&mut m, i_foot.x, l.x0, format!("d' = {} cm", num(img.distance.abs(), 1)), y + 34.0);
            }
            m.push(Mark::text(format!("h = {} cm", num(self.height, 0)), tip + Vec2::new(0.0, -26.0), 14.0, inked(COLOR_OBJECT), Anchor::Center));
            if !img.at_infinity() && i_tip.y > r0.y && i_tip.y < r1.y && i_tip.x > r0.x && i_tip.x < r1.x {
                let below = img.height < 0.0;
                m.push(Mark::text(format!("h' = {} cm", num(img.height.abs(), 1)), i_tip + Vec2::new(0.0, if below { 10.0 } else { -26.0 }), 14.0, inked(COLOR_IMAGE), Anchor::Center));
            }
        }
        if !ink && !img.at_infinity() && !rect_contains(rect, i_tip) {
            m.push(Mark::text("Görüntü çerçevenin dışında", Vec2::new(r1.x - 16.0, r0.y + 14.0), 14.0, screen(COLOR_WARN), Anchor::Right));
        }
        m
    }

    fn refraction_origin(&self, rect: (Vec2, Vec2)) -> Vec2 {
        rect.0 + Vec2::new(rect.1.x * 0.5, rect.1.y * 0.52)
    }

    fn refraction_scene(&self, rect: (Vec2, Vec2), ink: bool) -> Vec<Mark> {
        let o = self.refraction_origin(rect);
        let (r0, r1) = (rect.0, rect.0 + rect.1);
        let (n1, n2) = (MEDIA[self.medium1].1, MEDIA[self.medium2].1);
        let mut m = Vec::new();
        if !ink {
            let tint = |n: f32| [0.45, 0.70, 1.0, 0.04 + 0.1 * (n - 1.0)];
            m.push(Mark::Tri { a: Vec2::new(r0.x, o.y), b: Vec2::new(r1.x, o.y), c: Vec2::new(r1.x, r1.y), style: screen(tint(n2)) });
            m.push(Mark::Tri { a: Vec2::new(r0.x, o.y), b: Vec2::new(r1.x, r1.y), c: Vec2::new(r0.x, r1.y), style: screen(tint(n2)) });
            m.push(Mark::Tri { a: Vec2::new(r0.x, r0.y + 34.0), b: Vec2::new(r1.x, r0.y + 34.0), c: Vec2::new(r1.x, o.y), style: screen(tint(n1)) });
            m.push(Mark::Tri { a: Vec2::new(r0.x, r0.y + 34.0), b: Vec2::new(r1.x, o.y), c: Vec2::new(r0.x, o.y), style: screen(tint(n1)) });
        }
        let half = if ink { 330.0 } else { rect.1.x / 2.0 - 10.0 };
        m.push(Mark::line(vec![Vec2::new(o.x - half, o.y), Vec2::new(o.x + half, o.y)], 2.4, inked(COLOR_LINE)));
        m.push(Mark::dashed(vec![Vec2::new(o.x, o.y - RAY_LEN - 10.0), Vec2::new(o.x, o.y + RAY_LEN + 10.0)], 1.4, inked(COLOR_AXIS)));
        m.push(Mark::text("normal", Vec2::new(o.x + 6.0, o.y - RAY_LEN - 14.0), 13.0, inked(COLOR_AXIS), Anchor::Left));
        let label = |k: usize, n: f32| format!("{} (n = {})", MEDIA[k].0, num(n, 2));
        m.push(Mark::text(label(self.medium1, n1), Vec2::new(o.x - half + 6.0, o.y - 30.0), 16.0, inked(COLOR_LINE), Anchor::Left));
        m.push(Mark::text(label(self.medium2, n2), Vec2::new(o.x - half + 6.0, o.y + 14.0), 16.0, inked(COLOR_LINE), Anchor::Left));

        let t1 = self.incidence.to_radians();
        let start = o + Vec2::new(-t1.sin(), -t1.cos()) * RAY_LEN;
        let ray = COLOR_RAYS[0];
        m.push(Mark::line(vec![start, o], 2.6, inked(ray)));
        m.push(Mark::arrow(start, start.lerp(o, 0.55), 2.6, inked(ray)));
        let refracted = refract(n1, n2, self.incidence);
        let reflected_end = o + Vec2::new(t1.sin(), -t1.cos()) * RAY_LEN;
        let refl_color = if refracted.is_none() { ray } else { [ray[0], ray[1], ray[2], 0.4] };
        m.push(Mark::line(vec![o, reflected_end], if refracted.is_none() { 2.6 } else { 1.4 }, inked(refl_color)));
        m.push(Mark::arrow(o, o.lerp(reflected_end, 0.55), if refracted.is_none() { 2.6 } else { 1.4 }, inked(refl_color)));
        let arc = |m: &mut Vec<Mark>, from: f32, to: f32, r: f32, text: String, color: [f32; 4]| {
            let pts: Vec<Vec2> = (0..=16).map(|i| {
                let a = from + (to - from) * i as f32 / 16.0;
                o + Vec2::new(a.cos(), -a.sin()) * r
            }).collect();
            m.push(Mark::line(pts, 1.4, inked(color)));
            let mid = (from + to) / 2.0;
            let at = o + Vec2::new(mid.cos(), -mid.sin()) * (r + 16.0);
            m.push(Mark::text(text, at - Vec2::new(0.0, 8.0), 15.0, inked(color), if mid.cos() < 0.0 { Anchor::Right } else { Anchor::Left }));
        };
        let up = std::f32::consts::FRAC_PI_2;
        if self.incidence > 0.5 {
            arc(&mut m, up, up + t1, 60.0, format!("θ_1 = {}°", num(self.incidence, 0)), ray);
        }
        match refracted {
            Some(t2) => {
                let t2r = t2.to_radians();
                let end = o + Vec2::new(t2r.sin(), t2r.cos()) * RAY_LEN;
                let c = COLOR_RAYS[2];
                m.push(Mark::line(vec![o, end], 2.6, inked(c)));
                m.push(Mark::arrow(o, o.lerp(end, 0.55), 2.6, inked(c)));
                if t2 > 0.5 {
                    arc(&mut m, -up, -up + t2r, 60.0, format!("θ_2 = {}°", num(t2, 1)), c);
                }
            }
            None => {
                m.push(Mark::text("Tam yansıma", o + Vec2::new(0.0, 40.0), 18.0, inked(COLOR_WARN), Anchor::Center));
            }
        }
        m
    }

    // --- Ink -------------------------------------------------------------

    pub fn stamp_lines(&self, screen_size: Vec2, ink: [f32; 4]) -> Vec<InkLine> {
        let size = Vec2::new(STAMP_W.min(screen_size.x - 160.0), STAMP_H.min(screen_size.y - 240.0));
        let top_left = Vec2::new((screen_size.x - size.x) / 2.0, ((screen_size.y - 130.0 - size.y) / 2.0).max(20.0));
        marks::to_ink(&self.scene((top_left, size), true), ink)
    }

    pub fn mode_name(&self) -> &'static str {
        MODES.iter().find(|(m, _)| *m == self.mode).map(|(_, n)| *n).unwrap_or("")
    }

    // --- Drawing ---------------------------------------------------------

    fn result_lines(&self) -> Vec<(String, [f32; 4])> {
        if self.mode == Mode::Refraction {
            let (n1, n2) = (MEDIA[self.medium1].1, MEDIA[self.medium2].1);
            let mut out = vec![("n_1 sinθ_1 = n_2 sinθ_2".to_string(), COLOR_TEXT_DIM)];
            match refract(n1, n2, self.incidence) {
                Some(t2) => {
                    out.push((format!("Kırılma açısı: θ_2 = {}°", num(t2, 1)), COLOR_RAYS[2]));
                    let how = if n2 > n1 {
                        "Çok yoğun ortama geçiyor: normale yaklaşır"
                    } else if n2 < n1 {
                        "Az yoğun ortama geçiyor: normalden uzaklaşır"
                    } else {
                        "Aynı ortam: kırılmadan geçer"
                    };
                    out.push((how.to_string(), COLOR_TEXT));
                }
                None => out.push(("Tam yansıma: ışın kırılmadan geri döner".to_string(), COLOR_WARN)),
            }
            match critical_angle(n1, n2) {
                Some(c) => out.push((format!("Sınır açısı: θ_s = {}°", num(c, 1)), COLOR_TEXT)),
                None => out.push(("Sınır açısı yok (ışık çok yoğun ortama geçiyor)".to_string(), COLOR_TEXT_DIM)),
            }
            return out;
        }
        let img = self.image();
        let mut out = vec![("1/f = 1/d + 1/d'".to_string(), COLOR_TEXT_DIM)];
        if img.at_infinity() {
            out.push(("Cisim odakta: görüntü oluşmaz,".to_string(), COLOR_WARN));
            out.push(("yansıyan/kırılan ışınlar paraleldir.".to_string(), COLOR_WARN));
            return out;
        }
        out.push((format!("Görüntü uzaklığı: d' = {} cm", num(img.distance.abs(), 1)), COLOR_IMAGE));
        out.push((format!("Görüntü boyu: h' = {} cm", num(img.height.abs(), 1)), COLOR_IMAGE));
        out.push((format!("Büyütme: {}", num(img.magnification(self.height).abs(), 2)), COLOR_TEXT));
        out.push((img.nature(self.height), COLOR_TEXT));
        out
    }

    pub fn render(
        &self,
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
        font_atlas.push_text("Optik", header_pos + Vec2::new(PADDING, 10.0), 16.0, COLOR_TEXT, gv, gi);
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
        marks::draw(&self.scene(view, false), font_atlas, v, i, gv, gi);
        let hint = if self.mode == Mode::Refraction { "Gelen ışını parmakla döndürün" } else { "Cismi gövdesinden ya da ucundan sürükleyin" };
        font_atlas.push_text(hint, view.0 + Vec2::new(16.0, 14.0), 15.0, COLOR_TEXT_DIM, gv, gi);

        for (k, (mode, name)) in MODES.iter().enumerate() {
            button(self.mode_rect(k), name, if *mode == self.mode { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE }, font_atlas, v, i, gv, gi);
        }
        for (k, param) in self.params().into_iter().enumerate() {
            let slot = 1 + k;
            let (pos, size) = self.slot_rect(slot);
            match param {
                Some(param) => {
                    push_rect(pos, size, COLOR_FIELD, v, i);
                    let label = match param {
                        Param::Distance => format!("d = {} cm", num(self.distance, 0)),
                        Param::Height => format!("h = {} cm", num(self.height, 0)),
                        Param::Focal => format!("f = {} cm", num(self.focal, 0)),
                        Param::Incidence => format!("θ_1 = {}°", num(self.incidence, 0)),
                    };
                    push_rich(font_atlas, &label, pos + Vec2::new(12.0, 14.0), 16.0, COLOR_TEXT, Anchor::Left, gv, gi);
                    let (x0, x1, y) = self.slider_track(slot);
                    let (lo, hi) = param.range();
                    let x = x0 + (x1 - x0) * (self.value(param) - lo) / (hi - lo);
                    push_line(Vec2::new(x0, y), Vec2::new(x1, y), 4.0, COLOR_KEY_IDLE, v, i);
                    push_line(Vec2::new(x0, y), Vec2::new(x, y), 4.0, COLOR_SLIDER, v, i);
                    push_circle(Vec2::new(x, y), 11.0, COLOR_TEXT, 20, v, i);
                }
                None if self.mode == Mode::Refraction => {
                    let (k_medium, which) = if k == 1 { (self.medium1, "Üst ortam") } else { (self.medium2, "Alt ortam") };
                    let (name, n) = MEDIA[k_medium];
                    button((pos, size), &format!("{which}: {name} (n = {})", num(n, 2)), COLOR_KEY_IDLE, font_atlas, v, i, gv, gi);
                }
                None => {
                    push_rect(pos, size, COLOR_FIELD, v, i);
                    font_atlas.push_text("Düz aynada odak yoktur", pos + Vec2::new(12.0, 15.0), 14.0, COLOR_TEXT_DIM, gv, gi);
                }
            }
        }
        let (rpos, rsize) = self.slot_rect(4);
        push_rect(rpos, rsize, COLOR_FIELD, v, i);
        for (k, (line, color)) in self.result_lines().iter().enumerate() {
            push_rich(font_atlas, line, rpos + Vec2::new(12.0, 12.0 + k as f32 * 26.0), 15.0, *color, Anchor::Left, gv, gi);
        }
        button(self.rays_rect(), "Işınlar", if self.rays { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE }, font_atlas, v, i, gv, gi);
        button(self.labels_rect(), "Değerler", if self.labels { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE }, font_atlas, v, i, gv, gi);
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

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn mirror_and_lens_equation() {
        // Concave f = 10: object at 30 → real, inverted, half size at 15.
        let i = image(Some(10.0), 30.0, 8.0);
        assert!(close(i.distance, 15.0) && close(i.height, -4.0) && i.is_real());
        assert_eq!(i.nature(8.0), "Gerçek, ters, cisimden küçük");
        // At M (2f): same size.
        assert_eq!(image(Some(10.0), 20.0, 8.0).nature(8.0), "Gerçek, ters, cisimle aynı boyda");
        // Inside F: virtual, upright, bigger (magnifier).
        let i = image(Some(10.0), 5.0, 8.0);
        assert!(close(i.distance, -10.0) && close(i.height, 16.0));
        assert_eq!(i.nature(8.0), "Sanal, düz, cisimden büyük");
        // At F: no image.
        assert!(image(Some(10.0), 10.0, 8.0).at_infinity());
        // Convex / diverging: always virtual, upright, smaller.
        let i = image(Some(-10.0), 30.0, 8.0);
        assert!(close(i.distance, -7.5) && close(i.height, 2.0));
        // Plane mirror: as far behind, same size.
        let i = image(None, 25.0, 8.0);
        assert!(close(i.distance, -25.0) && close(i.height, 8.0));
    }

    #[test]
    fn snell_and_critical_angle() {
        // Air → glass at 30°: sin θ2 = 0,5/1,5.
        assert!((refract(1.0, 1.5, 30.0).unwrap() - 19.4712).abs() < 1e-3);
        // Glass → air: critical angle 41,8°; beyond it total reflection.
        assert!((critical_angle(1.5, 1.0).unwrap() - 41.8103).abs() < 1e-3);
        assert!(refract(1.5, 1.0, 45.0).is_none());
        assert!(refract(1.5, 1.0, 40.0).is_some());
        assert!(critical_angle(1.0, 1.33).is_none());
    }

    #[test]
    fn rays_meet_at_the_image() {
        // Every outgoing ray (or its back-extension) passes through the
        // image tip.
        for mode in [Mode::Concave, Mode::Convex, Mode::Converging, Mode::Diverging, Mode::Plane] {
            for d in [5.0, 15.0, 30.0] {
                let mut o = Optics::new(SCREEN);
                o.mode = mode;
                o.distance = d;
                o.labels = false;
                let rect = (Vec2::new(0.0, 0.0), Vec2::new(VIEW_W, VIEW_H));
                let l = o.layout(rect);
                let img = o.image();
                let x = if mode.is_mirror() { l.x0 - img.distance * l.px } else { l.x0 + img.distance * l.px };
                let i_tip = Vec2::new(x, l.axis - img.height * l.px);
                if !rect_contains(rect, i_tip) {
                    continue;
                }
                // Outgoing ray lines (solid, from the element onward).
                let mut lines = 0;
                for mark in o.scene(rect, false) {
                    if let Mark::Line { pts, dash: false, style, .. } = mark {
                        if pts.len() == 2 && COLOR_RAYS.contains(&style.color) && (pts[0].x - l.x0).abs() < SAG + 1.0 && (pts[1].x - l.x0).abs() > 30.0 {
                            let (a, b) = (pts[0], pts[1]);
                            let dist = ((b - a).perp_dot(i_tip - a) / (b - a).length()).abs();
                            assert!(dist < 1.0, "{mode:?} d={d}: ray misses the image by {dist}");
                            lines += 1;
                        }
                    }
                }
                assert!(lines >= 2, "{mode:?} d={d}: {lines} rays");
            }
        }
    }

    #[test]
    fn panel_drags_modes_and_stamps() {
        let mut o = Optics::new(SCREEN);
        assert!(o.position.y + o.panel_height() <= SCREEN.y - 75.0);
        assert!(Optics::slot_heights()[4] > 130.0);
        let l = o.layout(o.view_rect());
        let (_, tip) = o.object_pts(&l);
        // Grab the tip, move it to 20 cm, 10 cm high.
        assert_eq!(o.press_at(tip + Vec2::new(3.0, 3.0)), OpticsPress::Drag);
        o.drag_object(Vec2::new(l.x0 - 20.0 * l.px, l.axis - 10.0 * l.px));
        assert!(o.distance == 20.0 && o.height == 10.0);
        // The stem only moves it along.
        let (foot, _) = o.object_pts(&l);
        assert_eq!(o.press_at(foot + Vec2::new(2.0, -10.0)), OpticsPress::Drag);
        o.drag_object(Vec2::new(l.x0 - 40.0 * l.px, l.axis - 3.0 * l.px));
        assert!(o.distance == 40.0 && o.height == 10.0);
        // Refraction: media cycle, finger sets the angle.
        let c = |r: (Vec2, Vec2)| r.0 + r.1 / 2.0;
        o.press_at(c(o.mode_rect(5)));
        assert_eq!(o.mode, Mode::Refraction);
        o.press_at(c(o.slot_rect(2)));
        assert_eq!(o.medium1, 1);
        let origin = o.refraction_origin(o.view_rect());
        assert_eq!(o.press_at(origin + Vec2::new(-100.0, -100.0)), OpticsPress::Drag);
        assert_eq!(o.incidence, 45.0);
        // Stamps fit on screen, every label has ink glyphs.
        for k in 0..MODES.len() {
            o.mode = MODES[k].0;
            for d in [5.0, 10.0, 30.0] {
                o.distance = d;
                let lines = o.stamp_lines(SCREEN, [1.0; 4]);
                assert!(lines.len() > 5);
                for ln in &lines {
                    for p in &ln.points {
                        assert!(p.x >= 0.0 && p.x <= SCREEN.x && p.y >= 0.0 && p.y <= SCREEN.y - 70.0, "{:?} d={d}: {p:?}", o.mode);
                    }
                }
                assert_eq!(marks::missing_glyphs(&o.scene((Vec2::new(500.0, 200.0), Vec2::new(STAMP_W, STAMP_H)), true)), vec![]);
            }
        }
    }
}
