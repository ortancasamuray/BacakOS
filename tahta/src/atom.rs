//! Draggable "Atom modeli" panel: the same atom (or ion) seen through the
//! models a chemistry course walks through, side by side on tabs:
//!
//! - Thomson — positive sphere with the electrons in it ("üzümlü kek");
//! - Rutherford — a tiny nucleus in mostly empty space, with the gold
//!   foil experiment's alpha particles flying past (most straight, a few
//!   bent, one thrown back);
//! - Bohr — electrons circling on their shells;
//! - Modern — the electron cloud and the shapes of the occupied s/p/d/f
//!   orbitals;
//! - orbital (box) diagram with Hund's rule arrows;
//! - Lewis dot structure, ions in brackets.
//!
//! The element follows the periodic table panel (or ◀ ▶); − / + make an
//! ion, and in the Bohr view the electron that leaves or arrives flies
//! out or in. Every view is built as a list of `Shape`s, drawn on screen
//! and, for "Tahtaya bas", turned into ink.

use glam::Vec2;

use crate::elements as el;
use crate::font_atlas::{FontAtlas, GlyphVertex};
use crate::ink::{densify, stroke_text, stroke_text_width, InkLine};
use crate::periodic::{bohr_rings, circle_points};
use crate::stroke::{push_circle, push_line, push_rect, Vertex};

const HEADER_H: f32 = 36.0;
const PADDING: f32 = 12.0;
const GAP: f32 = 6.0;
const TAB_H: f32 = 44.0;
const VIEW_W: f32 = 600.0;
const VIEW_H: f32 = 460.0;
const CAPTION_H: f32 = 64.0;
const SIDE_W: f32 = 300.0;
const BTN_H: f32 = 44.0;
/// Seconds an electron takes to fly out of / into the Bohr atom.
const FLIGHT_SECS: f64 = 1.0;
const STAMP_SCALE: f32 = 1.2;
const MAX_CHARGE: i32 = 7;
const MIN_CHARGE: i32 = -3;

const COLOR_PANEL_BG: [f32; 4] = [0.12, 0.13, 0.17, 0.96];
const COLOR_HEADER: [f32; 4] = [0.18, 0.19, 0.24, 1.0];
const COLOR_VIEW_BG: [f32; 4] = [0.06, 0.07, 0.09, 1.0];
const COLOR_TEXT: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_TEXT_DIM: [f32; 4] = [0.95, 0.95, 0.97, 0.6];
const COLOR_KEY_IDLE: [f32; 4] = [0.22, 0.24, 0.30, 1.0];
const COLOR_KEY_ACTIVE: [f32; 4] = [0.23, 0.51, 0.96, 1.0];
const COLOR_KEY_STAMP: [f32; 4] = [0.98, 0.55, 0.15, 1.0];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Thomson,
    Rutherford,
    Bohr,
    Modern,
    Orbitals,
    Lewis,
}

const VIEWS: [(View, &str); 6] = [
    (View::Thomson, "Thomson"),
    (View::Rutherford, "Rutherford"),
    (View::Bohr, "Bohr"),
    (View::Modern, "Modern"),
    (View::Orbitals, "Orbital diyagramı"),
    (View::Lewis, "Lewis"),
];

fn caption(view: View) -> &'static str {
    match view {
        View::Thomson => "Thomson (1897): Pozitif yük bir küreye dağılmıştır; elektronlar içinde üzümlü kekteki üzümler gibi durur.",
        View::Rutherford => "Rutherford (1911): Alfa taneciklerinin çoğu dümdüz geçer, azı sapar, çok azı geri döner. Pozitif yük ve kütle küçük bir çekirdektedir; atomun çoğu boşluktur.",
        View::Bohr => "Bohr (1913): Elektronlar çekirdek çevresinde belirli enerji düzeylerinde (katmanlarda) dolanır.",
        View::Modern => "Modern atom modeli: Elektronun yeri kesin bilinemez; bulunma olasılığı bulut (orbital) olarak gösterilir.",
        View::Orbitals => "Orbital diyagramı: Her kutu bir orbitaldir. Hund kuralı: elektronlar önce orbitallere tek tek, aynı yönde yerleşir.",
        View::Lewis => "Lewis gösterimi: Element sembolünün çevresine değerlik elektronları nokta olarak yazılır.",
    }
}

/// Which palette entry a shape uses on screen (ink is one color).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Tone {
    Main,
    Dim,
    Electron,
    Nucleus,
    Positive,
    Accent,
}

fn tone_color(tone: Tone, alpha: f32) -> [f32; 4] {
    let [r, g, b] = match tone {
        Tone::Main => [0.95, 0.95, 0.97],
        Tone::Dim => [0.75, 0.77, 0.82],
        Tone::Electron => [0.45, 0.75, 1.0],
        Tone::Nucleus => [0.85, 0.35, 0.30],
        Tone::Positive => [0.92, 0.45, 0.55],
        Tone::Accent => [0.98, 0.75, 0.25],
    };
    [r, g, b, alpha]
}

#[derive(Debug)]
enum Shape {
    Poly { points: Vec<Vec2>, width: f32, tone: Tone, alpha: f32 },
    /// Filled circle; ink draws its outline (or a dot when small).
    Disk { c: Vec2, r: f32, tone: Tone, alpha: f32 },
    /// Centered text; `ink` is what the stamp writes (`None`: screen only).
    Text { text: String, ink: Option<String>, center: Vec2, h: f32, tone: Tone },
}

fn poly(points: Vec<Vec2>, width: f32, tone: Tone) -> Shape {
    Shape::Poly { points, width, tone, alpha: 1.0 }
}

fn text(t: &str, center: Vec2, h: f32, tone: Tone, ink: bool) -> Shape {
    Shape::Text { text: t.to_string(), ink: ink.then(|| t.to_string()), center, h, tone }
}

/// Deterministic pseudo-random number in [0, 1).
fn rnd(seed: u32, i: u32) -> f32 {
    let mut x = seed.wrapping_mul(0x9E37_79B9) ^ i.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    x = x.wrapping_mul(0x297A_2D39);
    x ^= x >> 15;
    (x >> 8) as f32 / (1u32 << 24) as f32
}

/// Golden-angle spiral point `i` of `n` inside a disk of radius `r`.
fn spiral(center: Vec2, r: f32, i: u32, n: u32, phase: f32) -> Vec2 {
    let k = i as f32 + 0.5;
    let rad = r * (k / n.max(1) as f32).sqrt();
    let a = k * 2.399_963 + phase;
    center + Vec2::new(a.cos(), a.sin()) * rad
}

fn ellipse(center: Vec2, a: f32, b: f32, rot: f32, segments: usize) -> Vec<Vec2> {
    let (s, c) = rot.sin_cos();
    (0..=segments)
        .map(|i| {
            let t = i as f32 / segments as f32 * std::f32::consts::TAU;
            let p = Vec2::new(a * t.cos(), b * t.sin());
            center + Vec2::new(p.x * c - p.y * s, p.x * s + p.y * c)
        })
        .collect()
}

/// Keeps the figure inside its view: polylines are cut where they leave
/// it, disks outside it dropped (alpha tracks run off the edges).
fn clip_to((pos, size): (Vec2, Vec2), shapes: Vec<Shape>) -> Vec<Shape> {
    let inside = |p: Vec2| p.x >= pos.x && p.y >= pos.y && p.x <= pos.x + size.x && p.y <= pos.y + size.y;
    let mut out = Vec::new();
    for shape in shapes {
        match shape {
            Shape::Poly { points, width, tone, alpha } => {
                let mut run: Vec<Vec2> = Vec::new();
                for p in points {
                    if inside(p) {
                        run.push(p);
                    } else if run.len() >= 2 {
                        out.push(Shape::Poly { points: std::mem::take(&mut run), width, tone, alpha });
                    } else {
                        run.clear();
                    }
                }
                if run.len() >= 2 {
                    out.push(Shape::Poly { points: run, width, tone, alpha });
                }
            }
            Shape::Disk { c, .. } if !inside(c) => {}
            other => out.push(other),
        }
    }
    out
}

fn arrow(from: Vec2, to: Vec2, head: f32, width: f32, tone: Tone) -> [Shape; 2] {
    let d = (to - from).normalize_or_zero() * head;
    [poly(vec![from, to], width, tone), poly(vec![to - d + d.perp() * 0.6, to, to - d - d.perp() * 0.6], width, tone)]
}

/// What a touch on the panel turned into.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum AtomPress {
    Move,
    Close,
    Stamp,
    Other,
}

/// An electron leaving (`gain == false`) or joining the Bohr atom.
#[derive(Clone, Copy, Debug)]
struct Flight {
    start: f64,
    angle: f32,
    gain: bool,
    /// The charge drawn while it flies (the ion before a gain).
    shown_charge: i32,
}

pub struct Atom {
    pub position: Vec2,
    z: u32,
    charge: i32,
    view: View,
    moving: bool,
    /// Animation time while `moving` is off.
    frozen_t: f64,
    flight: Option<Flight>,
}

impl Atom {
    pub fn new(screen_size: Vec2, z: u32) -> Self {
        let mut a = Self { position: Vec2::ZERO, z, charge: 0, view: View::Bohr, moving: true, frozen_t: 0.0, flight: None };
        let size = Vec2::new(a.panel_width(), a.panel_height());
        a.position = Vec2::new((screen_size.x - size.x) / 2.0, ((screen_size.y - 120.0 - size.y) / 2.0).max(12.0));
        a
    }

    pub fn set_element(&mut self, z: u32) {
        if z != self.z {
            self.z = z;
            self.charge = 0;
            self.flight = None;
        }
    }

    fn panel_width(&self) -> f32 {
        PADDING * 3.0 + VIEW_W + SIDE_W
    }

    fn panel_height(&self) -> f32 {
        HEADER_H + PADDING + TAB_H + 8.0 + VIEW_H + PADDING
    }

    fn header_rect(&self) -> (Vec2, Vec2) {
        (self.position, Vec2::new(self.panel_width(), HEADER_H))
    }

    fn close_rect(&self) -> (Vec2, Vec2) {
        (self.position + Vec2::new(self.panel_width() - HEADER_H, 0.0), Vec2::splat(HEADER_H))
    }

    fn tab_rect(&self, i: usize) -> (Vec2, Vec2) {
        let inner = self.panel_width() - PADDING * 2.0;
        let n = VIEWS.len() as f32;
        let w = (inner - (n - 1.0) * GAP) / n;
        (self.position + Vec2::new(PADDING + i as f32 * (w + GAP), HEADER_H + PADDING), Vec2::new(w, TAB_H))
    }

    fn view_rect(&self) -> (Vec2, Vec2) {
        (self.position + Vec2::new(PADDING, HEADER_H + PADDING + TAB_H + 8.0), Vec2::new(VIEW_W, VIEW_H))
    }

    /// The drawing area inside the view, above the caption.
    fn figure_rect(&self) -> (Vec2, Vec2) {
        let (pos, size) = self.view_rect();
        (pos, Vec2::new(size.x, size.y - CAPTION_H))
    }

    fn side_x(&self) -> f32 {
        self.position.x + PADDING * 2.0 + VIEW_W
    }

    fn side_top(&self) -> f32 {
        self.view_rect().0.y
    }

    fn prev_rect(&self) -> (Vec2, Vec2) {
        (Vec2::new(self.side_x(), self.side_top()), Vec2::new(52.0, 64.0))
    }

    fn next_rect(&self) -> (Vec2, Vec2) {
        (Vec2::new(self.side_x() + SIDE_W - 52.0, self.side_top()), Vec2::new(52.0, 64.0))
    }

    fn charge_row_y(&self) -> f32 {
        self.side_top() + 64.0 + 12.0
    }

    fn minus_rect(&self) -> (Vec2, Vec2) {
        (Vec2::new(self.side_x() + SIDE_W - 52.0 * 2.0 - GAP, self.charge_row_y()), Vec2::new(52.0, BTN_H))
    }

    fn plus_rect(&self) -> (Vec2, Vec2) {
        (Vec2::new(self.side_x() + SIDE_W - 52.0, self.charge_row_y()), Vec2::new(52.0, BTN_H))
    }

    fn motion_rect(&self) -> (Vec2, Vec2) {
        let (vpos, vsize) = self.view_rect();
        (Vec2::new(self.side_x(), vpos.y + vsize.y - 2.0 * BTN_H - GAP), Vec2::new(SIDE_W, BTN_H))
    }

    fn stamp_rect(&self) -> (Vec2, Vec2) {
        let (vpos, vsize) = self.view_rect();
        (Vec2::new(self.side_x(), vpos.y + vsize.y - BTN_H), Vec2::new(SIDE_W, BTN_H))
    }

    fn animated(&self) -> bool {
        matches!(self.view, View::Rutherford | View::Bohr)
    }

    pub fn contains(&self, p: Vec2) -> bool {
        rect_contains((self.position, Vec2::new(self.panel_width(), self.panel_height())), p)
    }

    pub fn drag_to(&mut self, p: Vec2, grab_offset: Vec2) {
        self.position = p - grab_offset;
    }

    /// Whether frames must keep coming (electrons moving, a flight).
    pub fn is_animating(&self, now: f64) -> bool {
        (self.moving && self.animated()) || self.flight.is_some_and(|f| now - f.start < FLIGHT_SECS)
    }

    fn anim_t(&self, now: f64) -> f64 {
        if self.moving { now } else { self.frozen_t }
    }

    pub fn press_at(&mut self, p: Vec2, now: f64) -> AtomPress {
        if rect_contains(self.close_rect(), p) {
            return AtomPress::Close;
        }
        if rect_contains(self.header_rect(), p) {
            return AtomPress::Move;
        }
        if rect_contains(self.stamp_rect(), p) {
            return AtomPress::Stamp;
        }
        for (i, &(view, _)) in VIEWS.iter().enumerate() {
            if rect_contains(self.tab_rect(i), p) {
                self.view = view;
                self.flight = None;
                return AtomPress::Other;
            }
        }
        if rect_contains(self.prev_rect(), p) && self.z > 1 {
            self.set_element(self.z - 1);
        } else if rect_contains(self.next_rect(), p) && self.z < 118 {
            self.set_element(self.z + 1);
        } else if rect_contains(self.minus_rect(), p) {
            // "−" adds an electron: the charge goes down.
            self.change_charge(-1, now);
        } else if rect_contains(self.plus_rect(), p) {
            self.change_charge(1, now);
        } else if rect_contains(self.motion_rect(), p) && self.animated() {
            if self.moving {
                self.frozen_t = now;
            }
            self.moving = !self.moving;
        }
        AtomPress::Other
    }

    fn change_charge(&mut self, delta: i32, now: f64) {
        let new = self.charge + delta;
        if !(MIN_CHARGE..=MAX_CHARGE.min(self.z as i32)).contains(&new) {
            return;
        }
        let old = self.charge;
        self.charge = new;
        if self.view == View::Bohr {
            // The electron leaves from / arrives at the outermost shell.
            let gain = delta < 0;
            let t = self.anim_t(now) as f32;
            let angle = -0.6 + t * 0.4;
            self.flight = Some(Flight { start: now, angle, gain, shown_charge: if gain { old } else { new } });
        }
    }

    fn shown_charge(&self, now: f64) -> i32 {
        match self.flight {
            Some(f) if now - f.start < FLIGHT_SECS => f.shown_charge,
            _ => self.charge,
        }
    }

    fn formula(&self) -> String {
        format!("{}{}", el::symbol(self.z), el::charge_superscript(self.charge))
    }

    // --- The figures -----------------------------------------------------

    fn shapes(&self, rect: (Vec2, Vec2), now: f64, for_ink: bool) -> Vec<Shape> {
        let mut out = Vec::new();
        let (pos, size) = rect;
        let u = size.y / (VIEW_H - CAPTION_H);
        let center = pos + size / 2.0;
        let charge = self.shown_charge(now);
        let config = el::ion_configuration(self.z, charge);
        let shells = el::shells_of(&config);
        let electrons = (self.z as i32 - charge).max(0) as u32;
        let t = self.anim_t(now);
        let radius = size.y * 0.42;
        match self.view {
            View::Thomson => {
                let r = radius * 0.9;
                out.push(Shape::Disk { c: center, r, tone: Tone::Positive, alpha: 0.28 });
                out.push(poly(circle_points(center, r, 72), 2.0 * u, Tone::Positive));
                let plus = self.z.min(40);
                for i in 0..plus {
                    let c = spiral(center, r * 0.92, i, plus, 1.3);
                    let s = 6.0 * u;
                    out.push(poly(vec![c - Vec2::new(s, 0.0), c + Vec2::new(s, 0.0)], 2.0 * u, Tone::Positive));
                    out.push(poly(vec![c - Vec2::new(0.0, s), c + Vec2::new(0.0, s)], 2.0 * u, Tone::Positive));
                }
                let shown = electrons.min(40);
                for i in 0..shown {
                    let c = spiral(center, r * 0.88, i, shown, 0.0) + Vec2::new(9.0, 7.0) * u;
                    out.push(Shape::Disk { c, r: 6.0 * u, tone: Tone::Electron, alpha: 1.0 });
                }
            }
            View::Rutherford => {
                // Alpha particles through the atom, bent by the nucleus.
                let k = radius * 0.018 * (self.z as f32).sqrt().clamp(1.0, 8.0);
                let left = pos.x + 8.0 * u;
                for (i, b) in [-0.85f32, -0.6, -0.35, -0.15, 0.02, 0.15, 0.35, 0.6, 0.85].iter().enumerate() {
                    let b = b * radius;
                    let theta = 2.0 * (k / b.abs().max(1e-3)).atan();
                    let p = Vec2::new(center.x - 12.0 * u * (theta / std::f32::consts::PI), center.y + b);
                    let dir = Vec2::new(theta.cos(), theta.sin() * b.signum());
                    let s = Vec2::new(left, center.y + b);
                    let e = p + dir * (size.x * 0.5);
                    let track: Vec<Vec2> = (0..=40)
                        .map(|j| {
                            let q = j as f32 / 40.0;
                            s * (1.0 - q) * (1.0 - q) + p * 2.0 * q * (1.0 - q) + e * q * q
                        })
                        .collect();
                    out.push(Shape::Poly { points: track.clone(), width: 1.2 * u, tone: Tone::Accent, alpha: 0.35 });
                    if !for_ink {
                        let q = ((t / 3.0 + i as f64 * 0.137) % 1.0) as f32;
                        let idx = ((q * 40.0) as usize).min(40);
                        out.push(Shape::Disk { c: track[idx], r: 4.5 * u, tone: Tone::Accent, alpha: 1.0 });
                    }
                }
                let shown = electrons.min(6);
                for i in 0..shown {
                    let a = radius * (0.35 + 0.6 * (i + 1) as f32 / shown.max(1) as f32);
                    let rot = i as f32 * 0.9;
                    out.push(Shape::Poly { points: ellipse(center, a, a * 0.32, rot, 64), width: 1.0 * u, tone: Tone::Dim, alpha: 0.22 });
                    let ang = (t * (1.2 - 0.05 * i as f64)) as f32 + i as f32 * 1.7;
                    let (s2, c2) = rot.sin_cos();
                    let local = Vec2::new(a * ang.cos(), a * 0.32 * ang.sin());
                    let c = center + Vec2::new(local.x * c2 - local.y * s2, local.x * s2 + local.y * c2);
                    out.push(Shape::Disk { c, r: 4.0 * u, tone: Tone::Electron, alpha: 1.0 });
                }
                out.push(Shape::Disk { c: center, r: 5.0 * u, tone: Tone::Nucleus, alpha: 1.0 });
            }
            View::Bohr => {
                let (nucleus, rings) = bohr_rings(radius, shells.len().max(1));
                out.push(Shape::Disk { c: center, r: nucleus, tone: Tone::Nucleus, alpha: 1.0 });
                let th = (nucleus * 0.5).clamp(9.0, 20.0);
                let neutrons = el::mass_number(self.z) - self.z;
                out.push(text(&format!("{}p", self.z), center - Vec2::new(0.0, th * 0.6), th, Tone::Main, true));
                out.push(text(&format!("{neutrons}n"), center + Vec2::new(0.0, th * 0.6), th, Tone::Main, true));
                for (i, &count) in shells.iter().enumerate() {
                    let r = rings[i];
                    out.push(Shape::Poly { points: circle_points(center, r, 72), width: 1.4 * u, tone: Tone::Dim, alpha: 0.6 });
                    let omega = if self.moving || for_ink { 0.6 / (i as f64 + 1.0) } else { 0.0 };
                    let start = -std::f32::consts::FRAC_PI_2 + i as f32 * 0.35 + (t * omega) as f32;
                    let dot = if count > 18 { 2.6 } else { 4.5 } * u;
                    for k in 0..count {
                        let a = start + k as f32 / count as f32 * std::f32::consts::TAU;
                        out.push(Shape::Disk { c: center + Vec2::new(a.cos(), a.sin()) * r, r: dot, tone: Tone::Electron, alpha: 1.0 });
                    }
                    if !for_ink {
                        out.push(text(el::SHELL_NAMES[i], center + Vec2::new(r * 0.72 + 6.0, -r * 0.72 - 8.0), 12.0 * u, Tone::Dim, false));
                    }
                }
                if let Some(f) = self.flight.filter(|f| now - f.start < FLIGHT_SECS && !for_ink) {
                    let q = ((now - f.start) / FLIGHT_SECS) as f32;
                    let outer = rings.last().copied().unwrap_or(radius * 0.5);
                    let far = radius * 1.25;
                    let r = if f.gain { far + (outer - far) * q } else { outer + (far - outer) * q };
                    let alpha = if f.gain { q.min(1.0) } else { 1.0 - q };
                    let c = center + Vec2::new(f.angle.cos(), f.angle.sin()) * r;
                    out.push(Shape::Disk { c, r: 5.0 * u, tone: Tone::Accent, alpha: alpha.max(0.15) });
                }
            }
            View::Modern => {
                let shapes_w = 150.0 * u;
                let cloud_c = Vec2::new(pos.x + (size.x - shapes_w) / 2.0, center.y);
                let (_, rings) = bohr_rings(radius, shells.len().max(1));
                let step = if rings.len() > 1 { rings[1] - rings[0] } else { rings[0] * 0.6 };
                let mut i = 0u32;
                for (s, &count) in shells.iter().enumerate() {
                    let n = if for_ink { (count * 6).min(90) } else { (count * 45).min(700) };
                    for _ in 0..n {
                        let a = rnd(self.z * 31 + s as u32, i) * std::f32::consts::TAU;
                        // Box-Muller from two uniforms: a soft shell.
                        let g = (-2.0 * rnd(self.z * 17 + 3, i).max(1e-4).ln()).sqrt() * (rnd(self.z * 13 + 7, i) * std::f32::consts::TAU).cos();
                        let r = (rings[s] + g * step * 0.28).max(2.0);
                        out.push(Shape::Disk { c: cloud_c + Vec2::new(a.cos(), a.sin()) * r, r: 1.7 * u, tone: Tone::Electron, alpha: 0.4 });
                        i += 1;
                    }
                }
                out.push(Shape::Disk { c: cloud_c, r: 4.0 * u, tone: Tone::Nucleus, alpha: 1.0 });
                // The occupied orbital types, as their classic shapes.
                let mut ls: Vec<u32> = config.iter().map(|e| e.1).collect();
                ls.sort();
                ls.dedup();
                let x = pos.x + size.x - shapes_w / 2.0 - 4.0 * u;
                let box_h = size.y / 4.0;
                for (k, &l) in ls.iter().enumerate() {
                    let c = Vec2::new(x, pos.y + box_h * (k as f32 + 0.45));
                    let r = box_h * 0.3;
                    match l {
                        0 => {
                            out.push(Shape::Disk { c, r, tone: Tone::Electron, alpha: 0.3 });
                            out.push(poly(circle_points(c, r, 40), 1.5 * u, Tone::Electron));
                        }
                        1 => {
                            for d in [Vec2::X, -Vec2::X] {
                                out.push(poly(ellipse(c + d * r * 0.55, r * 0.55, r * 0.3, 0.0, 32), 1.5 * u, Tone::Electron));
                            }
                        }
                        2 => {
                            for k2 in 0..4 {
                                let a = std::f32::consts::FRAC_PI_4 + k2 as f32 * std::f32::consts::FRAC_PI_2;
                                let d = Vec2::new(a.cos(), a.sin());
                                out.push(poly(ellipse(c + d * r * 0.5, r * 0.5, r * 0.24, a, 32), 1.5 * u, Tone::Electron));
                            }
                        }
                        _ => {
                            for k2 in 0..6 {
                                let a = k2 as f32 * std::f32::consts::FRAC_PI_3;
                                let d = Vec2::new(a.cos(), a.sin());
                                out.push(poly(ellipse(c + d * r * 0.5, r * 0.45, r * 0.18, a, 28), 1.4 * u, Tone::Electron));
                            }
                        }
                    }
                    let letter = el::subshell_letter(l).to_string();
                    out.push(text(&letter, c + Vec2::new(-r - 14.0 * u, -r * 0.8), 18.0 * u, Tone::Main, true));
                    let count = ["1 orbital", "3 orbital", "5 orbital", "7 orbital"][l as usize];
                    out.push(text(count, c + Vec2::new(0.0, r + 14.0 * u), 12.0 * u, Tone::Dim, false));
                }
            }
            View::Orbitals => self.orbital_diagram(&config, electrons, rect, u, &mut out),
            View::Lewis => self.lewis(&config, charge, center, u, &mut out),
        }
        clip_to(rect, out)
    }

    fn orbital_diagram(&self, config: &[(u32, u32, u32)], electrons: u32, (pos, size): (Vec2, Vec2), u: f32, out: &mut Vec<Shape>) {
        // Past argon, only what's beyond the noble-gas core.
        let core = [18, 36, 54, 86].into_iter().rev().find(|&c| c < electrons && electrons > 18);
        let core_config = core.map(el::configuration).unwrap_or_default();
        let subshells: Vec<(u32, u32, u32)> = config
            .iter()
            .filter_map(|&(n, l, k)| {
                let inner = core_config.iter().find(|e| e.0 == n && e.1 == l).map_or(0, |e| e.2);
                (k > inner).then_some((n, l, k - inner))
            })
            .collect();
        let (bw, bh, inner_gap, group_gap, row_h) = (34.0 * u, 40.0 * u, 3.0 * u, 18.0 * u, 84.0 * u);
        let max_x = pos.x + size.x - 16.0 * u;
        // Lay out first (rows), then center the block vertically.
        let mut items: Vec<(Vec2, (u32, u32, u32))> = Vec::new();
        let mut x = pos.x + 16.0 * u;
        let mut row = 0;
        let mut core_shift = 0.0;
        if core.is_some() {
            x += 70.0 * u;
        }
        for &sub in &subshells {
            let w = (2 * sub.1 + 1) as f32 * (bw + inner_gap) - inner_gap;
            if x + w > max_x && x > pos.x + 16.0 * u + 1.0 {
                x = pos.x + 16.0 * u;
                row += 1;
            }
            items.push((Vec2::new(x, row as f32 * row_h), sub));
            x += w + group_gap;
        }
        let block_h = (row + 1) as f32 * row_h - (row_h - bh - 26.0 * u);
        let top = pos.y + ((size.y - block_h) / 2.0).max(8.0 * u);
        // Center each row horizontally (the core label rides with row 0).
        let left = pos.x + 16.0 * u;
        for r in 0..=row {
            let end = items
                .iter()
                .filter(|(at, _)| at.y == r as f32 * row_h)
                .map(|(at, sub)| at.x + (2 * sub.1 + 1) as f32 * (bw + inner_gap) - inner_gap)
                .fold(left, f32::max);
            let shift = ((max_x - end) / 2.0).max(0.0);
            for (at, _) in items.iter_mut().filter(|(at, _)| at.y == r as f32 * row_h) {
                at.x += shift;
            }
            if r == 0 {
                core_shift = shift;
            }
        }
        if let Some(c) = core {
            out.push(text(&format!("[{}]", el::symbol(c)), Vec2::new(pos.x + 16.0 * u + 30.0 * u + core_shift, top + bh / 2.0), 24.0 * u, Tone::Main, true));
        }
        for (at, (n, l, k)) in items {
            let boxes = 2 * l + 1;
            let origin = Vec2::new(at.x, top + at.y);
            for b in 0..boxes {
                let p = origin + Vec2::new(b as f32 * (bw + inner_gap), 0.0);
                let frame = vec![p, p + Vec2::new(bw, 0.0), p + Vec2::new(bw, bh), p + Vec2::new(0.0, bh), p];
                out.push(poly(frame, 1.8 * u, Tone::Main));
                // Hund: one up arrow in every box first, then the pairs.
                let up = k > b;
                let down = k > boxes + b;
                if up {
                    let x = p.x + bw * 0.36;
                    out.extend(arrow(Vec2::new(x, p.y + bh * 0.85), Vec2::new(x, p.y + bh * 0.15), 7.0 * u, 2.0 * u, Tone::Electron));
                }
                if down {
                    let x = p.x + bw * 0.64;
                    out.extend(arrow(Vec2::new(x, p.y + bh * 0.15), Vec2::new(x, p.y + bh * 0.85), 7.0 * u, 2.0 * u, Tone::Electron));
                }
            }
            let w = boxes as f32 * (bw + inner_gap) - inner_gap;
            let label = format!("{n}{}", el::subshell_letter(l));
            out.push(text(&label, origin + Vec2::new(w / 2.0, bh + 16.0 * u), 17.0 * u, Tone::Main, true));
        }
    }

    fn lewis(&self, config: &[(u32, u32, u32)], charge: i32, center: Vec2, u: f32, out: &mut Vec<Shape>) {
        let sym = el::symbol(self.z);
        let h = 110.0 * u;
        let block = el::block(self.z);
        if charge == 0 && matches!(block, 'd' | 'f') {
            out.push(text(sym, center, h, Tone::Main, true));
            out.push(text("Lewis gösterimi baş grup (A) elementleri için kullanılır", center + Vec2::new(0.0, h * 0.9), 15.0 * u, Tone::Dim, false));
            return;
        }
        out.push(text(sym, center, h, Tone::Main, true));
        let outer = config.iter().map(|e| e.0).max().unwrap_or(1);
        let valence: u32 = if charge > 0 { 0 } else { config.iter().filter(|e| e.0 == outer && e.1 <= 1).map(|e| e.2).sum() };
        // Half extents of the symbol, for placing dots and brackets.
        let half = Vec2::new(h * 0.3 * sym.chars().count() as f32 + 6.0 * u, h * 0.5);
        let gap = 18.0 * u;
        let sides = [
            (center - Vec2::new(0.0, half.y + gap), Vec2::X),
            (center + Vec2::new(half.x + gap, 0.0), Vec2::Y),
            (center + Vec2::new(0.0, half.y + gap), Vec2::X),
            (center - Vec2::new(half.x + gap, 0.0), Vec2::Y),
        ];
        let mut per_side = [0u32; 4];
        if self.z == 2 && charge == 0 {
            per_side[0] = 2;
        } else {
            for i in 0..valence.min(8) {
                per_side[(i % 4) as usize] += 1;
            }
        }
        for (k, &(c, along)) in sides.iter().enumerate() {
            match per_side[k] {
                1 => out.push(Shape::Disk { c, r: 6.0 * u, tone: Tone::Electron, alpha: 1.0 }),
                2 => {
                    for s in [-1.0, 1.0] {
                        out.push(Shape::Disk { c: c + along * s * 11.0 * u, r: 6.0 * u, tone: Tone::Electron, alpha: 1.0 });
                    }
                }
                _ => {}
            }
        }
        if charge != 0 {
            let pad = half + Vec2::splat(gap + 22.0 * u);
            let (l, r, t, b) = (center.x - pad.x, center.x + pad.x, center.y - pad.y, center.y + pad.y);
            let arm = 14.0 * u;
            out.push(poly(vec![Vec2::new(l + arm, t), Vec2::new(l, t), Vec2::new(l, b), Vec2::new(l + arm, b)], 2.5 * u, Tone::Main));
            out.push(poly(vec![Vec2::new(r - arm, t), Vec2::new(r, t), Vec2::new(r, b), Vec2::new(r - arm, b)], 2.5 * u, Tone::Main));
            let ink = format!("{}{}", if charge.abs() > 1 { charge.abs().to_string() } else { String::new() }, if charge > 0 { "+" } else { "-" });
            out.push(Shape::Text { text: el::charge_superscript(charge), ink: Some(ink), center: Vec2::new(r + 22.0 * u, t + 2.0 * u), h: 30.0 * u, tone: Tone::Main });
        }
    }

    // --- Ink -------------------------------------------------------------

    /// The current view as ink, `STAMP_SCALE`× the panel figure, centered
    /// above the toolbar, with the element/ion written under it.
    pub fn stamp_lines(&self, screen_size: Vec2, ink: [f32; 4], now: f64) -> Vec<InkLine> {
        let (_, fsize) = self.figure_rect();
        let size = fsize * STAMP_SCALE;
        let top_left = ((screen_size - Vec2::new(0.0, 120.0) - size) / 2.0).max(Vec2::splat(10.0));
        let mut lines = Vec::new();
        for shape in self.shapes((top_left, size), now, true) {
            match shape {
                Shape::Poly { points, width, .. } => {
                    if points.len() >= 2 {
                        lines.push(InkLine { points: densify(&points, 3.0), color: ink, width: width.max(1.2) });
                    }
                }
                Shape::Disk { c, r, .. } => {
                    let (pr, w) = if r > 8.0 { (r, 2.0) } else { (r * 0.6, r * 1.1) };
                    lines.push(InkLine { points: circle_points(c, pr, if r > 8.0 { 48 } else { 10 }), color: ink, width: w.max(1.5) });
                }
                Shape::Text { ink: Some(t), center, h, .. } => {
                    let w = stroke_text_width(&t, h);
                    for g in stroke_text(&t, center - Vec2::new(w / 2.0, h / 2.0), h) {
                        if g.len() >= 2 {
                            lines.push(InkLine { points: densify(&g, 3.0), color: ink, width: (h / 14.0).clamp(1.6, 3.5) });
                        }
                    }
                }
                Shape::Text { ink: None, .. } => {}
            }
        }
        lines
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
        push_rect(self.position, Vec2::new(self.panel_width(), self.panel_height()), COLOR_PANEL_BG, out_vertices, out_indices);
        let (header_pos, header_size) = self.header_rect();
        push_rect(header_pos, header_size, COLOR_HEADER, out_vertices, out_indices);
        font_atlas.push_text("Atom modeli", header_pos + Vec2::new(PADDING, 10.0), 16.0, COLOR_TEXT, glyph_vertices, glyph_indices);
        let grip = header_pos + header_size / 2.0;
        for i in -1..=1 {
            push_circle(Vec2::new(grip.x + i as f32 * 10.0, grip.y), 2.0, COLOR_TEXT, 8, out_vertices, out_indices);
        }
        let (close_pos, close_size) = self.close_rect();
        let c = close_pos + close_size / 2.0;
        let r = close_size.x * 0.2;
        push_line(c - Vec2::splat(r), c + Vec2::splat(r), 2.5, COLOR_TEXT, out_vertices, out_indices);
        push_line(c + Vec2::new(-r, r), c + Vec2::new(r, -r), 2.5, COLOR_TEXT, out_vertices, out_indices);

        for (i, &(view, label)) in VIEWS.iter().enumerate() {
            let color = if view == self.view { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE };
            button(self.tab_rect(i), label, color, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
        }

        let (vpos, vsize) = self.view_rect();
        push_rect(vpos, vsize, COLOR_VIEW_BG, out_vertices, out_indices);
        for shape in self.shapes(self.figure_rect(), now, false) {
            match shape {
                Shape::Poly { points, width, tone, alpha } => {
                    let color = tone_color(tone, alpha);
                    for w in points.windows(2) {
                        push_line(w[0], w[1], width, color, out_vertices, out_indices);
                    }
                }
                Shape::Disk { c, r, tone, alpha } => {
                    push_circle(c, r, tone_color(tone, alpha), if r > 10.0 { 40 } else { 10 }, out_vertices, out_indices);
                }
                Shape::Text { text, center, h, tone, .. } => {
                    let w = font_atlas.measure(&text, h);
                    font_atlas.push_text(&text, center - Vec2::new(w / 2.0, h * 0.62), h, tone_color(tone, 1.0), glyph_vertices, glyph_indices);
                }
            }
        }
        let mut y = vpos.y + vsize.y - CAPTION_H + 6.0;
        for line in wrap(caption(self.view), vsize.x - 24.0, |t| font_atlas.measure(t, 14.0)) {
            font_atlas.push_text(&line, Vec2::new(vpos.x + 12.0, y), 14.0, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);
            y += 19.0;
        }

        self.render_side(font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
    }

    fn render_side(
        &self,
        font_atlas: &FontAtlas,
        out_vertices: &mut Vec<Vertex>,
        out_indices: &mut Vec<u32>,
        glyph_vertices: &mut Vec<GlyphVertex>,
        glyph_indices: &mut Vec<u32>,
    ) {
        let x = self.side_x();
        let dim_if = |on: bool| if on { COLOR_KEY_IDLE } else { [0.18, 0.19, 0.23, 1.0] };
        button(self.prev_rect(), "", dim_if(self.z > 1), font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
        button(self.next_rect(), "", dim_if(self.z < 118), font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
        // Drawn chevrons (no arrow glyphs in the font atlas).
        for (rect, dir) in [(self.prev_rect(), -1.0f32), (self.next_rect(), 1.0)] {
            let c = rect.0 + rect.1 / 2.0;
            let tip = c + Vec2::new(dir * 9.0, 0.0);
            push_line(tip, c + Vec2::new(-dir * 7.0, -11.0), 3.0, COLOR_TEXT, out_vertices, out_indices);
            push_line(tip, c + Vec2::new(-dir * 7.0, 11.0), 3.0, COLOR_TEXT, out_vertices, out_indices);
        }
        let mid = x + SIDE_W / 2.0;
        let formula = self.formula();
        let w = font_atlas.measure(&formula, 34.0);
        font_atlas.push_text(&formula, Vec2::new(mid - w / 2.0, self.side_top() + 2.0), 34.0, COLOR_TEXT, glyph_vertices, glyph_indices);
        let name = if self.charge == 0 { el::name(self.z).to_string() } else { format!("{} iyonu", el::name(self.z)) };
        let w = font_atlas.measure(&name, 15.0);
        font_atlas.push_text(&name, Vec2::new(mid - w / 2.0, self.side_top() + 44.0), 15.0, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);

        let y = self.charge_row_y();
        let charge_text = match self.charge {
            0 => "Yük: 0 (nötr)".to_string(),
            c => format!("Yük: {}{}", if c > 0 { "+" } else { "−" }, c.abs()),
        };
        font_atlas.push_text(&charge_text, Vec2::new(x, y + 13.0), 16.0, COLOR_TEXT, glyph_vertices, glyph_indices);
        button(self.minus_rect(), "−", COLOR_KEY_IDLE, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
        button(self.plus_rect(), "+", COLOR_KEY_IDLE, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);

        let electrons = (self.z as i32 - self.charge).max(0) as u32;
        let config = el::ion_configuration(self.z, self.charge);
        let mut ty = y + BTN_H + 14.0;
        let h = 15.0;
        for (label, value) in [
            ("Proton", self.z.to_string()),
            ("Nötron", (el::mass_number(self.z) - self.z).to_string()),
            ("Elektron", electrons.to_string()),
        ] {
            font_atlas.push_text(label, Vec2::new(x, ty), h, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);
            font_atlas.push_text(&value, Vec2::new(x + 100.0, ty), h, COLOR_TEXT, glyph_vertices, glyph_indices);
            ty += 22.0;
        }
        ty += 6.0;
        for line in wrap(&el::format_configuration(&config), SIDE_W, |t| font_atlas.measure(t, h)) {
            font_atlas.push_text(&line, Vec2::new(x, ty), h, COLOR_TEXT, glyph_vertices, glyph_indices);
            ty += 20.0;
        }
        let shells = el::shells_of(&config).iter().map(|c| format!("{c})")).collect::<Vec<_>>().join(" ");
        font_atlas.push_text(&format!("Katmanlar: {shells}"), Vec2::new(x, ty + 4.0), h, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);

        if self.animated() {
            let label = if self.moving { "Hareketi durdur" } else { "Hareket ettir" };
            button(self.motion_rect(), label, COLOR_KEY_IDLE, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
        }
        button(self.stamp_rect(), "Tahtaya bas", COLOR_KEY_STAMP, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
    }
}

fn wrap(text: &str, max_w: f32, measure: impl Fn(&str) -> f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split(' ') {
        let candidate = if current.is_empty() { word.to_string() } else { format!("{current} {word}") };
        if measure(&candidate) > max_w && !current.is_empty() {
            lines.push(std::mem::replace(&mut current, word.to_string()));
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
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

    fn center((pos, size): (Vec2, Vec2)) -> Vec2 {
        pos + size / 2.0
    }

    fn count_disks(shapes: &[Shape], tone: Tone) -> usize {
        shapes.iter().filter(|s| matches!(s, Shape::Disk { tone: t, .. } if *t == tone)).count()
    }

    #[test]
    fn panel_fits_and_buttons_work() {
        let mut a = Atom::new(SCREEN, 11);
        assert!(a.position.y + a.panel_height() <= SCREEN.y - 75.0);
        a.press_at(center(a.plus_rect()), 1.0);
        assert_eq!((a.charge, a.formula()), (1, "Na⁺".to_string()));
        assert!(a.is_animating(1.5), "the leaving electron flies");
        a.press_at(center(a.next_rect()), 2.0);
        assert_eq!((a.z, a.charge), (12, 0), "a new element starts neutral");
        a.press_at(center(a.prev_rect()), 2.0);
        a.press_at(center(a.prev_rect()), 2.0);
        assert_eq!(a.z, 10);
        // Charge limits: H can lose only one electron.
        a.set_element(1);
        a.press_at(center(a.plus_rect()), 3.0);
        a.press_at(center(a.plus_rect()), 3.0);
        assert_eq!(a.charge, 1);
        a.press_at(center(a.tab_rect(5)), 3.0);
        assert_eq!(a.view, View::Lewis);
        assert!(!a.is_animating(10.0), "Lewis is still");
        assert_eq!(a.press_at(center(a.stamp_rect()), 3.0), AtomPress::Stamp);
    }

    #[test]
    fn bohr_and_thomson_show_every_electron_of_the_ion() {
        let mut a = Atom::new(SCREEN, 17);
        let rect = a.figure_rect();
        assert_eq!(count_disks(&a.shapes(rect, 0.0, false), Tone::Electron), 17);
        a.press_at(center(a.minus_rect()), 0.0); // Cl⁻
        let shapes = a.shapes(rect, 5.0, false);
        assert_eq!(count_disks(&shapes, Tone::Electron), 18);
        a.view = View::Thomson;
        assert_eq!(count_disks(&a.shapes(rect, 5.0, false), Tone::Electron), 18);
    }

    #[test]
    fn orbital_diagram_follows_hund() {
        // Nitrogen 2p³: three up arrows, no down arrow in 2p.
        let mut a = Atom::new(SCREEN, 7);
        a.view = View::Orbitals;
        let shapes = a.shapes(a.figure_rect(), 0.0, false);
        // Each arrow is two polys (shaft + head); boxes are one poly each.
        let boxes = 1 + 1 + 3;
        let arrows = 7; // 1s² 2s² 2p³
        let polys = shapes.iter().filter(|s| matches!(s, Shape::Poly { .. })).count();
        assert_eq!(polys, boxes + 2 * arrows);
        // Iron, past argon: the core is written, then 4s and 3d.
        a.set_element(26);
        let labels: Vec<String> = a
            .shapes(a.figure_rect(), 0.0, false)
            .into_iter()
            .filter_map(|s| match s {
                Shape::Text { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(labels, vec!["[Ar]", "4s", "3d"]);
    }

    #[test]
    fn lewis_dots_and_brackets() {
        let mut a = Atom::new(SCREEN, 8);
        a.view = View::Lewis;
        let rect = a.figure_rect();
        assert_eq!(count_disks(&a.shapes(rect, 0.0, false), Tone::Electron), 6);
        a.press_at(center(a.minus_rect()), 0.0);
        a.press_at(center(a.minus_rect()), 0.0); // O²⁻
        let shapes = a.shapes(rect, 0.0, false);
        assert_eq!(count_disks(&shapes, Tone::Electron), 8);
        assert!(shapes.iter().any(|s| matches!(s, Shape::Text { text, .. } if text == "²⁻")));
        a.set_element(11);
        a.press_at(center(a.plus_rect()), 0.0); // Na⁺: brackets, no dots
        assert_eq!(count_disks(&a.shapes(rect, 0.0, false), Tone::Electron), 0);
    }

    #[test]
    fn every_view_stamps_onto_the_screen() {
        let mut a = Atom::new(SCREEN, 26);
        for (view, _) in VIEWS {
            a.view = view;
            let lines = a.stamp_lines(SCREEN, [1.0; 4], 2.0);
            assert!(!lines.is_empty(), "{view:?}");
            for l in &lines {
                for p in &l.points {
                    assert!(p.x >= -2.0 && p.x <= SCREEN.x + 2.0 && p.y >= -2.0 && p.y <= SCREEN.y - 70.0, "{view:?} {p:?}");
                }
            }
        }
    }
}
