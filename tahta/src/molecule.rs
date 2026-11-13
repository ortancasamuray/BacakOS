//! Draggable "Molekül" panel: 3D molecules for the chemistry curriculum's
//! VSEPR / bonding units.
//!
//! - fourteen molecules, from H₂ to SF₆ and benzene, plus a NaCl crystal
//!   cut-out, built from real bond lengths and angles;
//! - ball-and-stick (double/triple bonds as parallel sticks) or
//!   space-filling, CPK colors, turned by dragging or spinning on its own;
//! - the bond angle as an arc with its value, and the central atom's lone
//!   pairs as lobes;
//! - a card with the geometry's Turkish name, VSEPR notation, bond angle,
//!   hybridization and polarity;
//! - "Tahtaya bas" stamps the molecule as seen, the angle and the formula
//!   as ink.
//!
//! Like `solid.rs` there's no 3D pipeline: atoms and bonds are painted
//! back to front.

use glam::{Mat3, Vec2, Vec3};

use crate::font_atlas::{FontAtlas, GlyphVertex};
use crate::ink::{densify, stroke_text, stroke_text_width, InkLine};
use crate::periodic::circle_points;
use crate::stroke::{push_circle, push_line, push_rect, Vertex};

const HEADER_H: f32 = 36.0;
const PADDING: f32 = 12.0;
const GAP: f32 = 6.0;
const VIEW_W: f32 = 620.0;
const VIEW_H: f32 = 480.0;
const SIDE_W: f32 = 300.0;
const BTN_H: f32 = 44.0;
const PER_ROW: usize = 7;
/// Radians per second while spinning.
const SPIN_SPEED: f32 = 0.5;

const COLOR_PANEL_BG: [f32; 4] = [0.12, 0.13, 0.17, 0.96];
const COLOR_HEADER: [f32; 4] = [0.18, 0.19, 0.24, 1.0];
const COLOR_VIEW_BG: [f32; 4] = [0.06, 0.07, 0.09, 1.0];
const COLOR_TEXT: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_TEXT_DIM: [f32; 4] = [0.95, 0.95, 0.97, 0.6];
const COLOR_KEY_IDLE: [f32; 4] = [0.22, 0.24, 0.30, 1.0];
const COLOR_KEY_ACTIVE: [f32; 4] = [0.23, 0.51, 0.96, 1.0];
const COLOR_KEY_STAMP: [f32; 4] = [0.98, 0.55, 0.15, 1.0];
const COLOR_BOND: [f32; 4] = [0.70, 0.72, 0.76, 1.0];
const COLOR_ANGLE: [f32; 4] = [0.98, 0.75, 0.25, 1.0];
const COLOR_LONE_PAIR: [f32; 4] = [0.55, 0.80, 1.0, 0.28];

/// (symbol, CPK color, ball radius Å, van der Waals radius Å).
fn element_style(symbol: &str) -> ([f32; 3], f32, f32) {
    match symbol {
        "H" => ([0.92, 0.92, 0.94], 0.25, 1.20),
        "C" => ([0.35, 0.36, 0.40], 0.38, 1.70),
        "N" => ([0.25, 0.42, 0.90], 0.36, 1.55),
        "O" => ([0.88, 0.22, 0.20], 0.35, 1.52),
        "F" => ([0.55, 0.88, 0.40], 0.32, 1.47),
        "Cl" => ([0.30, 0.82, 0.30], 0.45, 1.75),
        "S" => ([0.95, 0.85, 0.25], 0.46, 1.80),
        "P" => ([0.98, 0.55, 0.15], 0.46, 1.80),
        "B" => ([0.98, 0.70, 0.65], 0.38, 1.92),
        "Be" => ([0.70, 0.85, 0.40], 0.40, 1.53),
        // In the crystal: ions, Na⁺ small, Cl⁻ large (ionic radii).
        "Na" => ([0.65, 0.40, 0.90], 0.40, 1.02),
        _ => ([0.8, 0.8, 0.8], 0.35, 1.5),
    }
}

struct Atom3 {
    symbol: &'static str,
    pos: Vec3,
    /// Space-filling radius override (crystal ions); 0 = from the table.
    fill_r: f32,
}

struct Mol {
    atoms: Vec<Atom3>,
    /// (a, b, order); order 0 = lattice line, not a bond.
    bonds: Vec<(usize, usize, u8)>,
    /// (central atom, lone pair count) for the lobes.
    lone_pairs: Option<(usize, u32)>,
    /// (vertex, a, b) of the angle arc.
    angle: Option<(usize, usize, usize)>,
}

struct Info {
    formula: &'static str,
    /// Formula for ink (digits become subscripts when stamped).
    ink: &'static str,
    name: &'static str,
    geometry: &'static str,
    vsepr: &'static str,
    angle: &'static str,
    hybrid: &'static str,
    polarity: &'static str,
}

const MOLECULES: [Info; 14] = [
    Info { formula: "H₂", ink: "H2", name: "Hidrojen", geometry: "Doğrusal", vsepr: "—", angle: "—", hybrid: "—", polarity: "Apolar" },
    Info { formula: "HCl", ink: "HCl", name: "Hidrojen klorür", geometry: "Doğrusal", vsepr: "—", angle: "—", hybrid: "—", polarity: "Polar" },
    Info { formula: "H₂O", ink: "H2O", name: "Su", geometry: "Kırık doğru (açısal)", vsepr: "AX₂E₂", angle: "104,5°", hybrid: "sp³", polarity: "Polar" },
    Info { formula: "NH₃", ink: "NH3", name: "Amonyak", geometry: "Üçgen piramit", vsepr: "AX₃E", angle: "107°", hybrid: "sp³", polarity: "Polar" },
    Info { formula: "CH₄", ink: "CH4", name: "Metan", geometry: "Düzgün dörtyüzlü", vsepr: "AX₄", angle: "109,5°", hybrid: "sp³", polarity: "Apolar" },
    Info { formula: "CO₂", ink: "CO2", name: "Karbon dioksit", geometry: "Doğrusal", vsepr: "AX₂", angle: "180°", hybrid: "sp", polarity: "Apolar" },
    Info { formula: "BeCl₂", ink: "BeCl2", name: "Berilyum klorür", geometry: "Doğrusal", vsepr: "AX₂", angle: "180°", hybrid: "sp", polarity: "Apolar" },
    Info { formula: "BF₃", ink: "BF3", name: "Bor triflorür", geometry: "Düzlem üçgen", vsepr: "AX₃", angle: "120°", hybrid: "sp²", polarity: "Apolar" },
    Info { formula: "PCl₅", ink: "PCl5", name: "Fosfor pentaklorür", geometry: "Üçgen çift piramit", vsepr: "AX₅", angle: "90° ve 120°", hybrid: "sp³d", polarity: "Apolar" },
    Info { formula: "SF₆", ink: "SF6", name: "Kükürt hekzaflorür", geometry: "Düzgün sekizyüzlü", vsepr: "AX₆", angle: "90°", hybrid: "sp³d²", polarity: "Apolar" },
    Info { formula: "C₂H₄", ink: "C2H4", name: "Eten (etilen)", geometry: "Düzlem üçgen (her C)", vsepr: "AX₃", angle: "≈120°", hybrid: "sp²", polarity: "Apolar" },
    Info { formula: "C₂H₂", ink: "C2H2", name: "Etin (asetilen)", geometry: "Doğrusal", vsepr: "AX₂", angle: "180°", hybrid: "sp", polarity: "Apolar" },
    Info { formula: "C₆H₆", ink: "C6H6", name: "Benzen", geometry: "Düzlem altıgen", vsepr: "AX₃", angle: "120°", hybrid: "sp²", polarity: "Apolar" },
    Info { formula: "NaCl", ink: "NaCl", name: "Sodyum klorür kristali", geometry: "Kübik örgü", vsepr: "—", angle: "90°", hybrid: "—", polarity: "İyonik" },
];

fn atom(symbol: &'static str, pos: Vec3) -> Atom3 {
    Atom3 { symbol, pos, fill_r: 0.0 }
}

/// Unit vectors of `n` ligands around a pyramid apex pointing down (−y),
/// any two `angle_deg` apart (NH₃ 107°, CH₄'s lower three 109.5°).
fn pyramid(n: usize, angle_deg: f32) -> Vec<Vec3> {
    let cos_a = angle_deg.to_radians().cos();
    // cos α = cos²θ + sin²θ·cos(360°/n) for a regular pyramid.
    let c = (std::f32::consts::TAU / n as f32).cos();
    let sin2 = (1.0 - cos_a) / (1.0 - c);
    let (s, co) = (sin2.sqrt(), (1.0 - sin2).max(0.0).sqrt());
    (0..n)
        .map(|i| {
            let phi = i as f32 / n as f32 * std::f32::consts::TAU;
            Vec3::new(s * phi.cos(), -co, s * phi.sin())
        })
        .collect()
}

fn build(index: usize) -> Mol {
    let mut atoms = Vec::new();
    let mut bonds = Vec::new();
    let mut lone_pairs = None;
    let mut angle = None;
    let star = |atoms: &mut Vec<Atom3>, bonds: &mut Vec<(usize, usize, u8)>, center: &'static str, lig: &'static str, dirs: &[Vec3], len: f32, order: u8| {
        atoms.push(atom(center, Vec3::ZERO));
        for d in dirs {
            atoms.push(atom(lig, *d * len));
            bonds.push((0, atoms.len() - 1, order));
        }
    };
    match index {
        0 => {
            atoms.push(atom("H", Vec3::new(-0.37, 0.0, 0.0)));
            atoms.push(atom("H", Vec3::new(0.37, 0.0, 0.0)));
            bonds.push((0, 1, 1));
        }
        1 => {
            atoms.push(atom("H", Vec3::new(-0.9, 0.0, 0.0)));
            atoms.push(atom("Cl", Vec3::new(0.37, 0.0, 0.0)));
            bonds.push((0, 1, 1));
        }
        2 => {
            let h = 104.5f32.to_radians() / 2.0;
            let dirs = [Vec3::new(h.sin(), -h.cos(), 0.0), Vec3::new(-h.sin(), -h.cos(), 0.0)];
            star(&mut atoms, &mut bonds, "O", "H", &dirs, 0.96, 1);
            lone_pairs = Some((0, 2));
            angle = Some((0, 1, 2));
        }
        3 => {
            star(&mut atoms, &mut bonds, "N", "H", &pyramid(3, 107.0), 1.01, 1);
            lone_pairs = Some((0, 1));
            angle = Some((0, 1, 2));
        }
        4 => {
            let mut dirs = vec![Vec3::Y];
            dirs.extend(pyramid(3, 109.47));
            star(&mut atoms, &mut bonds, "C", "H", &dirs, 1.09, 1);
            angle = Some((0, 1, 2));
        }
        5 => {
            star(&mut atoms, &mut bonds, "C", "O", &[Vec3::X, -Vec3::X], 1.16, 2);
            angle = Some((0, 1, 2));
        }
        6 => {
            star(&mut atoms, &mut bonds, "Be", "Cl", &[Vec3::X, -Vec3::X], 1.75, 1);
            angle = Some((0, 1, 2));
        }
        7 => {
            let dirs: Vec<Vec3> = (0..3).map(|i| {
                let a = std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::TAU / 3.0;
                Vec3::new(a.cos(), a.sin(), 0.0)
            }).collect();
            star(&mut atoms, &mut bonds, "B", "F", &dirs, 1.31, 1);
            angle = Some((0, 1, 2));
        }
        8 => {
            atoms.push(atom("P", Vec3::ZERO));
            for d in [Vec3::Y, -Vec3::Y] {
                atoms.push(atom("Cl", d * 2.14));
                bonds.push((0, atoms.len() - 1, 1));
            }
            for i in 0..3 {
                let a = i as f32 * std::f32::consts::TAU / 3.0;
                atoms.push(atom("Cl", Vec3::new(a.cos(), 0.0, a.sin()) * 2.04));
                bonds.push((0, atoms.len() - 1, 1));
            }
            angle = Some((0, 1, 3));
        }
        9 => {
            star(&mut atoms, &mut bonds, "S", "F", &[Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z], 1.56, 1);
            angle = Some((0, 1, 3));
        }
        10 => {
            atoms.push(atom("C", Vec3::new(-0.67, 0.0, 0.0)));
            atoms.push(atom("C", Vec3::new(0.67, 0.0, 0.0)));
            bonds.push((0, 1, 2));
            for (c, sx) in [(0usize, -1.0f32), (1, 1.0)] {
                for sy in [1.0f32, -1.0] {
                    let a = 121.5f32.to_radians();
                    let d = Vec3::new(-sx * a.cos(), sy * a.sin(), 0.0);
                    let p = atoms[c].pos + d * 1.09;
                    atoms.push(atom("H", p));
                    bonds.push((c, atoms.len() - 1, 1));
                }
            }
            angle = Some((1, 0, 4));
        }
        11 => {
            atoms.push(atom("C", Vec3::new(-0.60, 0.0, 0.0)));
            atoms.push(atom("C", Vec3::new(0.60, 0.0, 0.0)));
            atoms.push(atom("H", Vec3::new(-1.66, 0.0, 0.0)));
            atoms.push(atom("H", Vec3::new(1.66, 0.0, 0.0)));
            bonds.extend([(0, 1, 3), (0, 2, 1), (1, 3, 1)]);
            angle = Some((0, 2, 1));
        }
        12 => {
            for i in 0..6 {
                let a = std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::FRAC_PI_3;
                atoms.push(atom("C", Vec3::new(a.cos(), a.sin(), 0.0) * 1.39));
            }
            for i in 0..6 {
                let a = std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::FRAC_PI_3;
                atoms.push(atom("H", Vec3::new(a.cos(), a.sin(), 0.0) * 2.48));
                bonds.push((i, i + 6, 1));
                bonds.push((i, (i + 1) % 6, if i % 2 == 0 { 2 } else { 1 }));
            }
            angle = Some((0, 5, 1));
        }
        _ => {
            // 3×3×3 cut of the rock-salt lattice.
            let d = 2.82 / 1.6;
            for x in -1..=1 {
                for y in -1..=1 {
                    for z in -1..=1 {
                        let na = (x + y + z) % 2 == 0;
                        let (sym, r) = if na { ("Na", 1.02 / 1.6) } else { ("Cl", 1.81 / 1.6) };
                        atoms.push(Atom3 { symbol: sym, pos: Vec3::new(x as f32, y as f32, z as f32) * d, fill_r: r });
                    }
                }
            }
            for i in 0..atoms.len() {
                for j in i + 1..atoms.len() {
                    if (atoms[i].pos.distance(atoms[j].pos) - d).abs() < 0.01 {
                        bonds.push((i, j, 0));
                    }
                }
            }
            angle = Some((13, 14, 16));
        }
    }
    Mol { atoms, bonds, lone_pairs, angle }
}

/// Lone-pair directions on `center`: opposite the bonds, spread like the
/// missing corners of a tetrahedron when there are two.
fn lone_pair_dirs(mol: &Mol) -> Vec<Vec3> {
    let Some((c, n)) = mol.lone_pairs else { return Vec::new() };
    let center = mol.atoms[c].pos;
    let bonded: Vec<Vec3> = mol
        .bonds
        .iter()
        .filter_map(|&(a, b, _)| if a == c { Some(b) } else if b == c { Some(a) } else { None })
        .map(|o| (mol.atoms[o].pos - center).normalize())
        .collect();
    let base = -bonded.iter().copied().sum::<Vec3>().normalize_or_zero();
    match n {
        1 => vec![base],
        _ => {
            let perp = bonded[0].cross(bonded[1]).normalize_or_zero();
            let t = 54.75f32.to_radians();
            vec![base * t.cos() + perp * t.sin(), base * t.cos() - perp * t.sin()]
        }
    }
}

/// What a touch on the panel turned into.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MoleculePress {
    Move,
    Close,
    /// The 3D view: dragging turns the molecule.
    View,
    Stamp,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Toggle {
    Fill,
    Angle,
    LonePairs,
    Spin,
}

const TOGGLES: [(Toggle, &str); 4] =
    [(Toggle::Fill, "Boşluk dolduran"), (Toggle::Angle, "Bağ açısı"), (Toggle::LonePairs, "Elektron çiftleri"), (Toggle::Spin, "Kendi kendine dön")];

pub struct Molecule {
    pub position: Vec2,
    index: usize,
    mol: Mol,
    yaw: f32,
    pitch: f32,
    fill: bool,
    show_angle: bool,
    show_lone_pairs: bool,
    /// Spinning since this time (input clock); `yaw` holds the rest.
    spin_since: Option<f64>,
}

/// One projected thing to paint, back to front.
enum Item {
    Atom { i: usize, z: f32 },
    Bond { b: usize, z: f32 },
}

impl Molecule {
    pub fn new(screen_size: Vec2) -> Self {
        let mut m = Self {
            position: Vec2::ZERO,
            index: 2,
            mol: build(2),
            yaw: 0.5,
            pitch: 0.3,
            fill: false,
            show_angle: true,
            show_lone_pairs: true,
            spin_since: None,
        };
        let size = Vec2::new(m.panel_width(), m.panel_height());
        m.position = Vec2::new((screen_size.x - size.x) / 2.0, ((screen_size.y - 120.0 - size.y) / 2.0).max(12.0));
        m
    }

    fn panel_width(&self) -> f32 {
        PADDING * 3.0 + VIEW_W + SIDE_W
    }

    fn panel_height(&self) -> f32 {
        HEADER_H + PADDING + VIEW_H + 8.0 + 2.0 * BTN_H + GAP + PADDING
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

    fn side_x(&self) -> f32 {
        self.position.x + PADDING * 2.0 + VIEW_W
    }

    fn mol_rect(&self, i: usize) -> (Vec2, Vec2) {
        let inner = self.panel_width() - PADDING * 2.0;
        let w = (inner - (PER_ROW as f32 - 1.0) * GAP) / PER_ROW as f32;
        let (row, col) = (i / PER_ROW, i % PER_ROW);
        let top = self.position.y + HEADER_H + PADDING + VIEW_H + 8.0;
        (Vec2::new(self.position.x + PADDING + col as f32 * (w + GAP), top + row as f32 * (BTN_H + GAP)), Vec2::new(w, BTN_H))
    }

    fn toggle_rect(&self, i: usize) -> (Vec2, Vec2) {
        let (vpos, vsize) = self.view_rect();
        let bottom = vpos.y + vsize.y - BTN_H - GAP;
        let y = bottom - (TOGGLES.len() - i) as f32 * (BTN_H + GAP);
        (Vec2::new(self.side_x(), y), Vec2::new(SIDE_W, BTN_H))
    }

    fn stamp_rect(&self) -> (Vec2, Vec2) {
        let (vpos, vsize) = self.view_rect();
        (Vec2::new(self.side_x(), vpos.y + vsize.y - BTN_H), Vec2::new(SIDE_W, BTN_H))
    }

    fn toggle_on(&self, t: Toggle) -> bool {
        match t {
            Toggle::Fill => self.fill,
            Toggle::Angle => self.show_angle,
            Toggle::LonePairs => self.show_lone_pairs,
            Toggle::Spin => self.spin_since.is_some(),
        }
    }

    pub fn contains(&self, p: Vec2) -> bool {
        rect_contains((self.position, Vec2::new(self.panel_width(), self.panel_height())), p)
    }

    pub fn drag_to(&mut self, p: Vec2, grab_offset: Vec2) {
        self.position = p - grab_offset;
    }

    pub fn is_animating(&self) -> bool {
        self.spin_since.is_some()
    }

    pub fn press_at(&mut self, p: Vec2, now: f64) -> MoleculePress {
        if rect_contains(self.close_rect(), p) {
            return MoleculePress::Close;
        }
        if rect_contains(self.header_rect(), p) {
            return MoleculePress::Move;
        }
        if rect_contains(self.view_rect(), p) {
            // Grabbing the molecule stops the spin where it is.
            self.stop_spin(now);
            return MoleculePress::View;
        }
        if rect_contains(self.stamp_rect(), p) {
            return MoleculePress::Stamp;
        }
        for (i, &(t, _)) in TOGGLES.iter().enumerate() {
            if rect_contains(self.toggle_rect(i), p) {
                match t {
                    Toggle::Fill => self.fill = !self.fill,
                    Toggle::Angle => self.show_angle = !self.show_angle,
                    Toggle::LonePairs => self.show_lone_pairs = !self.show_lone_pairs,
                    Toggle::Spin => {
                        if self.spin_since.is_some() {
                            self.stop_spin(now);
                        } else {
                            self.spin_since = Some(now);
                        }
                    }
                }
                return MoleculePress::Other;
            }
        }
        for i in 0..MOLECULES.len() {
            if rect_contains(self.mol_rect(i), p) {
                if i != self.index {
                    self.index = i;
                    self.mol = build(i);
                }
                return MoleculePress::Other;
            }
        }
        MoleculePress::Other
    }

    fn stop_spin(&mut self, now: f64) {
        if let Some(since) = self.spin_since.take() {
            self.yaw += (now - since) as f32 * SPIN_SPEED;
        }
    }

    pub fn rotate_by(&mut self, delta: Vec2) {
        self.yaw += delta.x * 0.01;
        self.pitch = (self.pitch + delta.y * 0.01).clamp(-1.5, 1.5);
    }

    fn rotation(&self, now: f64) -> Mat3 {
        let yaw = self.yaw + self.spin_since.map_or(0.0, |s| (now - s) as f32 * SPIN_SPEED);
        Mat3::from_rotation_x(self.pitch) * Mat3::from_rotation_y(yaw)
    }

    fn radius_of(&self, i: usize) -> f32 {
        let a = &self.mol.atoms[i];
        let (_, ball, vdw) = element_style(a.symbol);
        if self.fill {
            if a.fill_r > 0.0 { a.fill_r } else { vdw * 0.75 }
        } else if a.fill_r > 0.0 {
            a.fill_r * 0.45
        } else {
            ball
        }
    }

    /// Pixels per Å so the whole molecule fits `rect` in any orientation.
    fn scale_for(&self, rect: (Vec2, Vec2)) -> f32 {
        let extent = (0..self.mol.atoms.len()).map(|i| self.mol.atoms[i].pos.length() + self.radius_of(i)).fold(0.5f32, f32::max);
        rect.1.x.min(rect.1.y) * 0.44 / extent
    }

    /// Screen position and depth of every atom.
    fn project(&self, rect: (Vec2, Vec2), now: f64) -> (Vec<(Vec2, f32)>, f32, Mat3) {
        let rot = self.rotation(now);
        let scale = self.scale_for(rect);
        let center = rect.0 + rect.1 / 2.0;
        let pts = self.mol.atoms.iter().map(|a| {
            let p = rot * a.pos;
            (center + Vec2::new(p.x, -p.y) * scale, p.z)
        }).collect();
        (pts, scale, rot)
    }

    fn items(&self, pts: &[(Vec2, f32)]) -> Vec<Item> {
        let mut items: Vec<Item> = (0..pts.len()).map(|i| Item::Atom { i, z: pts[i].1 }).collect();
        if !self.fill {
            // A stick goes into both balls: paint it before either end.
            items.extend(self.mol.bonds.iter().enumerate().map(|(b, &(i, j, _))| Item::Bond { b, z: pts[i].1.min(pts[j].1) - 0.01 }));
        }
        items.sort_by(|a, b| {
            let za = match a { Item::Atom { z, .. } | Item::Bond { z, .. } => *z };
            let zb = match b { Item::Atom { z, .. } | Item::Bond { z, .. } => *z };
            za.total_cmp(&zb)
        });
        items
    }

    /// The angle arc (screen polyline) and where its label goes.
    fn angle_arc(&self, pts: &[(Vec2, f32)], scale: f32, rot: Mat3, rect: (Vec2, Vec2)) -> Option<(Vec<Vec2>, Vec2)> {
        let (v, a, b) = self.mol.angle?;
        if !self.show_angle || MOLECULES[self.index].angle == "—" {
            return None;
        }
        let center = rect.0 + rect.1 / 2.0;
        let origin = self.mol.atoms[v].pos;
        let da = (self.mol.atoms[a].pos - origin).normalize();
        let db = (self.mol.atoms[b].pos - origin).normalize();
        let mut axis = da.cross(db);
        if axis.length() < 1e-4 {
            // 180°: arc over the side facing the viewer.
            axis = da.cross(rot.transpose() * Vec3::Z);
            if axis.length() < 1e-4 {
                axis = da.cross(Vec3::Y);
            }
        }
        let axis = axis.normalize();
        let total = da.angle_between(db).max(if da.dot(db) < -0.999 { std::f32::consts::PI } else { 0.0 });
        let r = (self.radius_of(v) + 0.35).max(0.55);
        let to_screen = |p: Vec3| {
            let q = rot * p;
            center + Vec2::new(q.x, -q.y) * scale
        };
        let arc: Vec<Vec2> = (0..=24)
            .map(|k| {
                let t = total * k as f32 / 24.0;
                let d = Mat3::from_axis_angle(axis, t) * da;
                to_screen(origin + d * r)
            })
            .collect();
        let mid = Mat3::from_axis_angle(axis, total / 2.0) * da;
        let label = to_screen(origin + mid * (r + 0.45));
        let _ = pts;
        Some((arc, label))
    }

    fn lone_pair_lobes(&self, rot: Mat3, scale: f32, rect: (Vec2, Vec2)) -> Vec<(Vec<Vec2>, Vec2, Vec2)> {
        if !self.show_lone_pairs || self.fill {
            return Vec::new();
        }
        let Some((c, _)) = self.mol.lone_pairs else { return Vec::new() };
        let center = rect.0 + rect.1 / 2.0;
        let origin = self.mol.atoms[c].pos;
        lone_pair_dirs(&self.mol)
            .into_iter()
            .map(|d| {
                let q = rot * d;
                let dir2 = Vec2::new(q.x, -q.y);
                let len = 0.75 * scale * dir2.length().max(0.25);
                let dir2 = dir2.normalize_or(Vec2::Y);
                let o = rot * origin;
                let base = center + Vec2::new(o.x, -o.y) * scale;
                let c2 = base + dir2 * (self.radius_of(c) * scale * 0.6 + len * 0.55);
                let w = 0.32 * scale;
                let a = dir2.y.atan2(dir2.x);
                let lobe = ellipse(c2, len * 0.6, w, a, 32);
                let side = dir2.perp() * w * 0.35;
                (lobe, c2 + side, c2 - side)
            })
            .collect()
    }

    // --- Ink -------------------------------------------------------------

    pub fn stamp_lines(&self, screen_size: Vec2, ink: [f32; 4], now: f64) -> Vec<InkLine> {
        let size = Vec2::new(VIEW_W, VIEW_H) * 1.1;
        let top_left = ((screen_size - Vec2::new(0.0, 130.0) - size) / 2.0).max(Vec2::splat(10.0));
        let rect = (top_left, size);
        let (pts, scale, rot) = self.project(rect, now);
        let mut lines = Vec::new();
        let mut add = |points: Vec<Vec2>, width: f32| {
            if points.len() >= 2 {
                lines.push(InkLine { points: densify(&points, 3.0), color: ink, width });
            }
        };
        for item in self.items(&pts) {
            match item {
                Item::Bond { b, .. } => {
                    let (i, j, order) = self.mol.bonds[b];
                    let (pa, pb) = (pts[i].0, pts[j].0);
                    let dir = (pb - pa).normalize_or_zero();
                    let (ra, rb) = (self.radius_of(i) * scale, self.radius_of(j) * scale);
                    if pa.distance(pb) <= ra + rb {
                        continue;
                    }
                    let (a, bb) = (pa + dir * ra, pb - dir * rb);
                    for off in bond_offsets(order) {
                        let o = dir.perp() * off;
                        add(vec![a + o, bb + o], if order == 0 { 1.2 } else { 2.2 });
                    }
                }
                Item::Atom { i, .. } => {
                    let r = self.radius_of(i) * scale;
                    add(circle_points(pts[i].0, r, 40), 2.2);
                    let sym = self.mol.atoms[i].symbol;
                    let h = (r * 0.8).clamp(10.0, 34.0);
                    let w = stroke_text_width(sym, h);
                    for g in stroke_text(sym, pts[i].0 - Vec2::new(w / 2.0, h / 2.0), h) {
                        add(g, 1.8);
                    }
                }
            }
        }
        for (lobe, d1, d2) in self.lone_pair_lobes(rot, scale, rect) {
            add(lobe, 1.6);
            for d in [d1, d2] {
                add(circle_points(d, 2.0, 8), 3.0);
            }
        }
        if let Some((arc, label)) = self.angle_arc(&pts, scale, rot, rect) {
            add(arc, 2.0);
            let t = MOLECULES[self.index].angle.replace('≈', "").replace(" ve ", " ");
            let h = 20.0;
            let w = stroke_text_width(&t, h);
            for g in stroke_text(&t, label - Vec2::new(w / 2.0, h / 2.0), h) {
                add(g, 1.8);
            }
        }
        // Formula under the figure, digits as subscripts.
        let formula = MOLECULES[self.index].ink;
        let h = 34.0;
        let width: f32 = formula.chars().map(|c| if c.is_ascii_digit() { 0.45 } else { 0.8 } * h).sum();
        let mut x = top_left.x + (size.x - width) / 2.0;
        let y = top_left.y + size.y + 4.0;
        for c in formula.chars() {
            let s = c.to_string();
            let (gh, gy) = if c.is_ascii_digit() { (h * 0.6, y + h * 0.55) } else { (h, y) };
            for g in stroke_text(&s, Vec2::new(x, gy), gh) {
                add(g, 2.2);
            }
            x += if c.is_ascii_digit() { 0.45 } else { stroke_text_width(&s, h) / h + 0.15 } * h;
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
        font_atlas.push_text("Molekül", header_pos + Vec2::new(PADDING, 10.0), 16.0, COLOR_TEXT, glyph_vertices, glyph_indices);
        let grip = header_pos + header_size / 2.0;
        for i in -1..=1 {
            push_circle(Vec2::new(grip.x + i as f32 * 10.0, grip.y), 2.0, COLOR_TEXT, 8, out_vertices, out_indices);
        }
        let (close_pos, close_size) = self.close_rect();
        let c = close_pos + close_size / 2.0;
        let r = close_size.x * 0.2;
        push_line(c - Vec2::splat(r), c + Vec2::splat(r), 2.5, COLOR_TEXT, out_vertices, out_indices);
        push_line(c + Vec2::new(-r, r), c + Vec2::new(r, -r), 2.5, COLOR_TEXT, out_vertices, out_indices);

        let rect = self.view_rect();
        push_rect(rect.0, rect.1, COLOR_VIEW_BG, out_vertices, out_indices);
        let (pts, scale, rot) = self.project(rect, now);
        let lobes = self.lone_pair_lobes(rot, scale, rect);
        // Lobes behind everything: they're clouds, not solids.
        for (lobe, d1, d2) in &lobes {
            fill_polygon(lobe, COLOR_LONE_PAIR, out_vertices, out_indices);
            for d in [d1, d2] {
                push_circle(*d, 3.0, [0.55, 0.80, 1.0, 1.0], 8, out_vertices, out_indices);
            }
        }
        for item in self.items(&pts) {
            match item {
                Item::Bond { b, .. } => {
                    let (i, j, order) = self.mol.bonds[b];
                    let (pa, pb) = (pts[i].0, pts[j].0);
                    let dir = (pb - pa).normalize_or_zero();
                    let (width, color) = if order == 0 { (1.5, [0.75, 0.77, 0.82, 0.5]) } else { ((0.12 * scale).clamp(3.0, 9.0), COLOR_BOND) };
                    for off in bond_offsets(order) {
                        let o = dir.perp() * off * (scale / 60.0).clamp(0.8, 1.6);
                        push_line(pa + o, pb + o, width, color, out_vertices, out_indices);
                    }
                }
                Item::Atom { i, z } => {
                    let a = &self.mol.atoms[i];
                    let (rgb, _, _) = element_style(a.symbol);
                    let r = self.radius_of(i) * scale;
                    // Nearer atoms a touch brighter, for depth.
                    let light = (0.82 + z * 0.06).clamp(0.6, 1.0);
                    let base = [rgb[0] * light, rgb[1] * light, rgb[2] * light, 1.0];
                    push_circle(pts[i].0, r, base, 32, out_vertices, out_indices);
                    let hl = [(rgb[0] * 0.5 + 0.5).min(1.0), (rgb[1] * 0.5 + 0.5).min(1.0), (rgb[2] * 0.5 + 0.5).min(1.0), 0.55];
                    push_circle(pts[i].0 + Vec2::new(-r * 0.3, -r * 0.3), r * 0.38, hl, 20, out_vertices, out_indices);
                    if r >= 12.0 {
                        let h = (r * 0.7).clamp(11.0, 22.0);
                        let w = font_atlas.measure(a.symbol, h);
                        let dark = rgb.iter().sum::<f32>() > 2.0;
                        let tc = if dark { [0.1, 0.1, 0.12, 1.0] } else { COLOR_TEXT };
                        font_atlas.push_text(a.symbol, pts[i].0 - Vec2::new(w / 2.0, h * 0.62), h, tc, glyph_vertices, glyph_indices);
                    }
                }
            }
        }
        if let Some((arc, label)) = self.angle_arc(&pts, scale, rot, rect) {
            for w in arc.windows(2) {
                push_line(w[0], w[1], 2.5, COLOR_ANGLE, out_vertices, out_indices);
            }
            let t = MOLECULES[self.index].angle;
            let w = font_atlas.measure(t, 18.0);
            push_rect(label - Vec2::new(w / 2.0 + 5.0, 13.0), Vec2::new(w + 10.0, 26.0), [0.06, 0.07, 0.09, 0.75], out_vertices, out_indices);
            font_atlas.push_text(t, label - Vec2::new(w / 2.0, 11.0), 18.0, COLOR_ANGLE, glyph_vertices, glyph_indices);
        }
        font_atlas.push_text("Döndürmek için sürükleyin", rect.0 + Vec2::new(10.0, rect.1.y - 24.0), 13.0, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);

        self.render_side(font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);

        for (i, info) in MOLECULES.iter().enumerate() {
            let color = if i == self.index { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE };
            button(self.mol_rect(i), info.formula, color, 17.0, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
        }
    }

    fn render_side(
        &self,
        font_atlas: &FontAtlas,
        out_vertices: &mut Vec<Vertex>,
        out_indices: &mut Vec<u32>,
        glyph_vertices: &mut Vec<GlyphVertex>,
        glyph_indices: &mut Vec<u32>,
    ) {
        let info = &MOLECULES[self.index];
        let x = self.side_x();
        let mut y = self.view_rect().0.y;
        font_atlas.push_text(info.formula, Vec2::new(x, y), 36.0, COLOR_TEXT, glyph_vertices, glyph_indices);
        y += 46.0;
        font_atlas.push_text(info.name, Vec2::new(x, y), 17.0, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);
        y += 32.0;
        let h = 15.0;
        for (label, value) in [
            ("Geometri", info.geometry),
            ("VSEPR", info.vsepr),
            ("Bağ açısı", info.angle),
            ("Hibritleşme", info.hybrid),
            ("Polarlık", info.polarity),
        ] {
            font_atlas.push_text(label, Vec2::new(x, y), h, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);
            let v = crate::font_atlas::fit_text(value, SIDE_W - 110.0, false, |t| font_atlas.measure(t, h));
            font_atlas.push_text(&v, Vec2::new(x + 110.0, y), h, COLOR_TEXT, glyph_vertices, glyph_indices);
            y += 24.0;
        }
        for (i, &(t, label)) in TOGGLES.iter().enumerate() {
            let available = match t {
                Toggle::LonePairs => self.mol.lone_pairs.is_some() && !self.fill,
                Toggle::Angle => info.angle != "—",
                _ => true,
            };
            let color = if !available {
                [0.18, 0.19, 0.23, 1.0]
            } else if self.toggle_on(t) {
                COLOR_KEY_ACTIVE
            } else {
                COLOR_KEY_IDLE
            };
            button(self.toggle_rect(i), label, color, 15.0, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
        }
        button(self.stamp_rect(), "Tahtaya bas", COLOR_KEY_STAMP, 15.0, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
    }
}

/// Sideways offsets (px) of the sticks of a bond of `order`.
fn bond_offsets(order: u8) -> Vec<f32> {
    match order {
        2 => vec![-4.0, 4.0],
        3 => vec![-6.5, 0.0, 6.5],
        _ => vec![0.0],
    }
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

/// Fan-fills a convex polygon.
fn fill_polygon(points: &[Vec2], color: [f32; 4], out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) {
    if points.len() < 3 {
        return;
    }
    let c = points.iter().copied().sum::<Vec2>() / points.len() as f32;
    for w in points.windows(2) {
        crate::stroke::push_triangle(c, w[0], w[1], color, out_vertices, out_indices);
    }
}

#[allow(clippy::too_many_arguments)]
fn button(
    (pos, size): (Vec2, Vec2),
    label: &str,
    color: [f32; 4],
    h: f32,
    font_atlas: &FontAtlas,
    out_vertices: &mut Vec<Vertex>,
    out_indices: &mut Vec<u32>,
    glyph_vertices: &mut Vec<GlyphVertex>,
    glyph_indices: &mut Vec<u32>,
) {
    push_rect(pos, size, color, out_vertices, out_indices);
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

    fn angle_deg(mol: &Mol, (v, a, b): (usize, usize, usize)) -> f32 {
        let o = mol.atoms[v].pos;
        (mol.atoms[a].pos - o).angle_between(mol.atoms[b].pos - o).to_degrees()
    }

    #[test]
    fn geometry_matches_the_stated_angles() {
        let expect = [(2, 104.5), (3, 107.0), (4, 109.47), (5, 180.0), (6, 180.0), (7, 120.0), (8, 90.0), (9, 90.0), (11, 180.0), (12, 120.0), (13, 90.0)];
        for (i, deg) in expect {
            let mol = build(i);
            let got = angle_deg(&mol, mol.angle.unwrap());
            assert!((got - deg).abs() < 0.3, "{}: {got}", MOLECULES[i].formula);
        }
        // Every bonded pair of CH₄'s hydrogens is tetrahedral too.
        let ch4 = build(4);
        for a in 1..5 {
            for b in a + 1..5 {
                assert!((angle_deg(&ch4, (0, a, b)) - 109.47).abs() < 0.3);
            }
        }
        // PCl₅ equatorial chlorines are 120° apart.
        let pcl5 = build(8);
        assert!((angle_deg(&pcl5, (0, 3, 4)) - 120.0).abs() < 0.3);
    }

    #[test]
    fn lone_pairs_point_away_from_the_bonds() {
        let water = build(2);
        let dirs = lone_pair_dirs(&water);
        assert_eq!(dirs.len(), 2);
        for d in &dirs {
            assert!(d.y > 0.0, "above O, away from the hydrogens: {d:?}");
        }
        assert!((dirs[0].angle_between(dirs[1]).to_degrees() - 109.5).abs() < 1.0);
        assert_eq!(lone_pair_dirs(&build(3)).len(), 1);
        assert!(lone_pair_dirs(&build(4)).is_empty());
        // The crystal: each ion has its 6 neighbours.
        let nacl = build(13);
        let center = 13;
        assert_eq!(nacl.bonds.iter().filter(|b| b.0 == center || b.1 == center).count(), 6);
    }

    #[test]
    fn panel_buttons_and_drag() {
        let mut m = Molecule::new(SCREEN);
        assert!(m.position.y + m.panel_height() <= SCREEN.y - 75.0);
        let c = |r: (Vec2, Vec2)| r.0 + r.1 / 2.0;
        m.press_at(c(m.mol_rect(9)), 0.0);
        assert_eq!(MOLECULES[m.index].formula, "SF₆");
        m.press_at(c(m.toggle_rect(3)), 1.0);
        assert!(m.is_animating());
        assert_eq!(m.press_at(c(m.view_rect()), 3.0), MoleculePress::View);
        assert!(!m.is_animating(), "grabbing stops the spin");
        assert!((m.yaw - (0.5 + 2.0 * SPIN_SPEED)).abs() < 1e-4, "and keeps the turned angle");
        m.press_at(c(m.toggle_rect(0)), 3.0);
        assert!(m.fill);
        assert_eq!(m.press_at(c(m.stamp_rect()), 3.0), MoleculePress::Stamp);
    }

    #[test]
    fn every_molecule_stamps_on_screen() {
        let mut m = Molecule::new(SCREEN);
        for i in 0..MOLECULES.len() {
            m.index = i;
            m.mol = build(i);
            for c in MOLECULES[i].ink.chars() {
                assert!(!crate::ink::stroke_glyph(c).is_empty(), "{c}");
            }
            let lines = m.stamp_lines(SCREEN, [1.0; 4], 0.0);
            assert!(lines.len() >= 3);
            for l in &lines {
                for p in &l.points {
                    assert!(p.x >= 0.0 && p.x <= SCREEN.x && p.y >= 0.0 && p.y <= SCREEN.y - 70.0, "{}: {p:?}", MOLECULES[i].formula);
                }
            }
        }
    }
}
