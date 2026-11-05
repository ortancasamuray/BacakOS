//! Draggable "Cisim" panel — 3D geometric solids with plane sections, the
//! classroom version of a dynamic-geometry tool:
//!
//! - nine solids (cube, cuboid, triangular/hexagonal prism, square
//!   pyramid, regular tetrahedron, cylinder, cone, sphere), turned by
//!   dragging the view;
//! - drawn like a textbook figure: visible edges solid, hidden edges
//!   dashed, vertex letters (ABCD bottom, EFGH top…);
//! - a section plane through three points tapped onto edges (snapped to
//!   quarters — midpoints and vertices included), or a preset
//!   horizontal/vertical one, shifted along its normal with a slider;
//! - the section's true shape beside the solid, with its angles and its
//!   name ("Düzgün altıgen", "Eşkenar üçgen", and for the cone the conic
//!   sections: "Elips", "Parabol", "Hiperbol").
//!
//! No 3D pipeline is needed: every solid is convex, so a face is visible
//! exactly when its normal points at the viewer, an edge is visible when
//! either of its faces is, and a curved surface's outline is the set of
//! edges between a front and a back facet. Everything is drawn with the
//! same 2D vertex pipeline as the other panels.
//!
//! "Tahtaya bas" stamps the figure, the section outline and the true
//! shape (with angle labels) onto the page as ordinary ink, like
//! `graph.rs`.

use std::collections::BTreeMap;

use glam::{Mat3, Vec2, Vec3};

use crate::font_atlas::{FontAtlas, GlyphVertex};
use crate::ink::{dash_polyline, densify, stroke_text, stroke_text_width, InkLine};
use crate::stroke::{push_circle, push_line, push_rect, push_triangle, Vertex};

const HEADER_H: f32 = 36.0;
const PADDING: f32 = 12.0;
const GAP: f32 = 6.0;
const VIEW_W: f32 = 500.0;
const VIEW_H: f32 = 440.0;
const SHAPE_W: f32 = 260.0;
const BTN_H: f32 = 44.0;
const SLIDER_H: f32 = 40.0;
const TOOL_W: f32 = 130.0;

/// A finger that moves less than this before lifting is a tap (place a
/// section point), not a rotation.
pub const TAP_SLOP: f32 = 10.0;
/// How close (px) a tap must land to an edge to put a point on it.
const PICK_RADIUS: f32 = 24.0;
/// Stamped figures are drawn this much larger than the panel's view.
const STAMP_SCALE: f32 = 1.25;

const COLOR_PANEL_BG: [f32; 4] = [0.12, 0.13, 0.17, 0.95];
const COLOR_HEADER: [f32; 4] = [0.18, 0.19, 0.24, 1.0];
const COLOR_VIEW_BG: [f32; 4] = [0.06, 0.07, 0.09, 1.0];
const COLOR_TEXT: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_TEXT_DIM: [f32; 4] = [0.95, 0.95, 0.97, 0.45];
const COLOR_WARN: [f32; 4] = [0.98, 0.55, 0.40, 1.0];
const COLOR_EDGE: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_EDGE_HIDDEN: [f32; 4] = [0.95, 0.95, 0.97, 0.45];
const COLOR_FACE: [f32; 3] = [0.45, 0.62, 0.95];
const COLOR_SECTION_FILL: [f32; 4] = [0.30, 0.60, 1.0, 0.45];
const COLOR_SECTION_EDGE: [f32; 4] = [0.50, 0.78, 1.0, 1.0];
const COLOR_POINT: [f32; 4] = [0.98, 0.55, 0.15, 1.0];
const COLOR_KEY_IDLE: [f32; 4] = [0.22, 0.24, 0.30, 1.0];
const COLOR_KEY_ACTIVE: [f32; 4] = [0.23, 0.51, 0.96, 1.0];
const COLOR_KEY_CLEAR: [f32; 4] = [0.55, 0.30, 0.30, 1.0];
const COLOR_KEY_STAMP: [f32; 4] = [0.98, 0.55, 0.15, 1.0];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Cube,
    Cuboid,
    TriPrism,
    HexPrism,
    SquarePyramid,
    Tetrahedron,
    Cylinder,
    Cone,
    Sphere,
}

const KINDS: [(Kind, &str); 9] = [
    (Kind::Cube, "Küp"),
    (Kind::Cuboid, "Dikdörtgenler pr."),
    (Kind::TriPrism, "Üçgen prizma"),
    (Kind::HexPrism, "Altıgen prizma"),
    (Kind::SquarePyramid, "Kare piramit"),
    (Kind::Tetrahedron, "Dörtyüzlü"),
    (Kind::Cylinder, "Silindir"),
    (Kind::Cone, "Koni"),
    (Kind::Sphere, "Küre"),
];
const KINDS_PER_ROW: usize = 5;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tool {
    Horizontal,
    Vertical,
    ClearSection,
    Labels,
    Stamp,
}
const TOOLS: [(Tool, &str); 5] = [
    (Tool::Horizontal, "Yatay kesit"),
    (Tool::Vertical, "Dikey kesit"),
    (Tool::ClearSection, "Kesiti kaldır"),
    (Tool::Labels, "Harfler"),
    (Tool::Stamp, "Tahtaya bas"),
];

struct Face {
    idx: Vec<usize>,
    /// Outward unit normal.
    normal: Vec3,
    /// Plane offset: `normal · p = d` for points on the face.
    d: f32,
    /// 0 = flat face; faces sharing a non-zero group are facets of one
    /// curved surface, so the edges between them aren't drawn (only the
    /// outline where the surface turns away).
    group: u32,
}

struct Edge {
    a: usize,
    b: usize,
    faces: [usize; 2],
    smooth: bool,
}

struct Mesh {
    verts: Vec<Vec3>,
    faces: Vec<Face>,
    edges: Vec<Edge>,
    /// Per-vertex letter, or empty for curved solids.
    labels: Vec<String>,
    /// Bounding-sphere radius around the origin.
    radius: f32,
}

/// Builds a closed convex mesh from face index lists, orienting every
/// face outward (by the vertex centroid, which is inside a convex solid)
/// and deriving the edge list.
fn build_mesh(verts: Vec<Vec3>, faces: Vec<(Vec<usize>, u32)>, labels: Vec<String>) -> Mesh {
    let centroid = verts.iter().copied().sum::<Vec3>() / verts.len() as f32;
    let faces: Vec<Face> = faces
        .into_iter()
        .map(|(mut idx, group)| {
            // Newell's method: robust for any planar polygon.
            let c = idx.iter().map(|&i| verts[i]).sum::<Vec3>() / idx.len() as f32;
            let mut n = Vec3::ZERO;
            for k in 0..idx.len() {
                let (p, q) = (verts[idx[k]] - c, verts[idx[(k + 1) % idx.len()]] - c);
                n += p.cross(q);
            }
            let mut n = n.normalize();
            if n.dot(c - centroid) < 0.0 {
                n = -n;
                idx.reverse();
            }
            Face { d: n.dot(verts[idx[0]]), idx, normal: n, group }
        })
        .collect();

    let mut map: BTreeMap<(usize, usize), Vec<usize>> = BTreeMap::new();
    for (fi, f) in faces.iter().enumerate() {
        for k in 0..f.idx.len() {
            let (a, b) = (f.idx[k], f.idx[(k + 1) % f.idx.len()]);
            map.entry((a.min(b), a.max(b))).or_default().push(fi);
        }
    }
    let edges = map
        .into_iter()
        .filter(|(_, fs)| fs.len() == 2)
        .map(|((a, b), fs)| {
            let (ga, gb) = (faces[fs[0]].group, faces[fs[1]].group);
            Edge { a, b, faces: [fs[0], fs[1]], smooth: ga != 0 && ga == gb }
        })
        .collect();
    let radius = verts.iter().map(|v| v.length()).fold(0.0, f32::max);
    Mesh { verts, faces, edges, labels, radius }
}

fn letters(n: usize) -> Vec<String> {
    const L: &str = "ABCDEFGHIJKL";
    if n > L.len() {
        return Vec::new();
    }
    L.chars().take(n).map(String::from).collect()
}

/// Regular `n`-gon of circumradius `r` in the xz-plane, starting at
/// angle `start` (radians, measured from +x toward +z).
fn ngon(n: usize, r: f32, start: f32) -> Vec<(f32, f32)> {
    (0..n)
        .map(|i| {
            let t = start + i as f32 * std::f32::consts::TAU / n as f32;
            (r * t.cos(), r * t.sin())
        })
        .collect()
}

/// Right prism over `base` (xz-plane), centered vertically.
fn prism(base: &[(f32, f32)], h: f32, curved: bool) -> Mesh {
    let n = base.len();
    let mut verts: Vec<Vec3> = base.iter().map(|&(x, z)| Vec3::new(x, -h / 2.0, z)).collect();
    verts.extend(base.iter().map(|&(x, z)| Vec3::new(x, h / 2.0, z)));
    let mut faces = vec![((0..n).collect(), 0), ((n..2 * n).collect(), 0)];
    for i in 0..n {
        let j = (i + 1) % n;
        faces.push((vec![i, j, n + j, n + i], if curved { 1 } else { 0 }));
    }
    let labels = if curved { Vec::new() } else { letters(2 * n) };
    build_mesh(verts, faces, labels)
}

/// Pyramid over `base` (at y = `y0`) with apex at (0, `y1`, 0).
fn pyramid(base: &[(f32, f32)], y0: f32, y1: f32, curved: bool, apex_label: &str) -> Mesh {
    let n = base.len();
    let mut verts: Vec<Vec3> = base.iter().map(|&(x, z)| Vec3::new(x, y0, z)).collect();
    verts.push(Vec3::new(0.0, y1, 0.0));
    let mut faces = vec![((0..n).collect(), 0)];
    for i in 0..n {
        faces.push((vec![i, (i + 1) % n, n], if curved { 1 } else { 0 }));
    }
    let labels = if curved {
        Vec::new()
    } else {
        let mut l = letters(n);
        l.push(apex_label.to_string());
        l
    };
    build_mesh(verts, faces, labels)
}

fn sphere(r: f32, slices: usize, stacks: usize) -> Mesh {
    let mut verts = vec![Vec3::new(0.0, r, 0.0)];
    for j in 1..stacks {
        let phi = j as f32 / stacks as f32 * std::f32::consts::PI;
        for i in 0..slices {
            let t = i as f32 / slices as f32 * std::f32::consts::TAU;
            verts.push(Vec3::new(r * phi.sin() * t.cos(), r * phi.cos(), r * phi.sin() * t.sin()));
        }
    }
    let bottom = verts.len();
    verts.push(Vec3::new(0.0, -r, 0.0));
    let ring = |j: usize, i: usize| 1 + (j - 1) * slices + i % slices;
    let mut faces = Vec::new();
    for i in 0..slices {
        faces.push((vec![0, ring(1, i), ring(1, i + 1)], 1));
        faces.push((vec![bottom, ring(stacks - 1, i + 1), ring(stacks - 1, i)], 1));
    }
    for j in 1..stacks - 1 {
        for i in 0..slices {
            faces.push((vec![ring(j, i), ring(j + 1, i), ring(j + 1, i + 1), ring(j, i + 1)], 1));
        }
    }
    build_mesh(verts, faces, Vec::new())
}

const CURVE_SEGMENTS: usize = 48;
const CONE_R: f32 = 1.15;
const CONE_H: f32 = 2.3;

fn mesh_for(kind: Kind) -> Mesh {
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};
    match kind {
        // ABCD: front-left, front-right, back-right, back-left (z toward
        // the viewer), EFGH above them — the textbook convention.
        Kind::Cube => prism(&[(-1.0, 1.0), (1.0, 1.0), (1.0, -1.0), (-1.0, -1.0)], 2.0, false),
        Kind::Cuboid => prism(&[(-1.4, 0.8), (1.4, 0.8), (1.4, -0.8), (-1.4, -0.8)], 1.8, false),
        // A front edge facing the viewer, the third vertex at the back.
        Kind::TriPrism => prism(&ngon(3, 1.25, FRAC_PI_2 + std::f32::consts::TAU / 3.0), 2.2, false),
        Kind::HexPrism => prism(&ngon(6, 1.15, 2.0 * FRAC_PI_2 / 3.0), 2.2, false),
        Kind::SquarePyramid => {
            let s = 1.15;
            pyramid(&[(-s, s), (s, s), (s, -s), (-s, -s)], -1.15, 1.15, false, "T")
        }
        Kind::Tetrahedron => {
            let a = 2.4f32;
            let h = a * (2.0f32 / 3.0).sqrt();
            pyramid(&ngon(3, a / 3f32.sqrt(), FRAC_PI_2 + std::f32::consts::TAU / 3.0), -h / 4.0, 3.0 * h / 4.0, false, "D")
        }
        Kind::Cylinder => prism(&ngon(CURVE_SEGMENTS, 1.0, FRAC_PI_4 / 6.0), 2.2, true),
        Kind::Cone => pyramid(&ngon(CURVE_SEGMENTS, CONE_R, FRAC_PI_4 / 6.0), -CONE_H / 2.0, CONE_H / 2.0, true, ""),
        Kind::Sphere => sphere(1.25, 36, 18),
    }
}

/// Model → screen: orthographic, so parallel edges stay parallel like in
/// a textbook figure.
struct Proj {
    rot: Mat3,
    center: Vec2,
    ppu: f32,
}

impl Proj {
    fn pt(&self, p: Vec3) -> Vec2 {
        let q = self.rot * p;
        self.center + Vec2::new(q.x, -q.y) * self.ppu
    }

    fn facing(&self, n: Vec3) -> bool {
        (self.rot * n).z > 1e-4
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Look {
    Front,
    Hidden,
    /// A facet seam inside a curved surface — not drawn.
    Seam,
}

struct Plane {
    n: Vec3,
    /// Current offset (`n · p = d`), moved by the slider within `lo..=hi`.
    d: f32,
    /// Offset through the tapped points (preset: the solid's middle).
    d0: f32,
    lo: f32,
    hi: f32,
}

/// What a touch on the panel turned into — `input_handler` picks the
/// pointer session (and acts for `Close`/`Stamp`) from this.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SolidPress {
    Move,
    Close,
    /// The 3D view: a drag turns the solid, a tap places a point.
    View,
    /// The plane-offset slider (value already set).
    Slider,
    Stamp,
    Other,
}

pub struct Solid {
    pub position: Vec2,
    kind: Kind,
    mesh: Mesh,
    yaw: f32,
    pitch: f32,
    /// Section points as (edge index, parameter along the edge).
    points: Vec<(usize, f32)>,
    plane: Option<Plane>,
    labels: bool,
    message: Option<&'static str>,
}

/// The section of `mesh` by plane `n · p = d`: its corners in order, or
/// empty when the plane misses the solid or only touches it.
fn section_points(mesh: &Mesh, n: Vec3, d: f32) -> Vec<Vec3> {
    const EPS: f32 = 1e-4;
    let mut pts: Vec<Vec3> = Vec::new();
    let add = |p: Vec3, pts: &mut Vec<Vec3>| {
        if !pts.iter().any(|q| q.distance(p) < 1e-3) {
            pts.push(p);
        }
    };
    for e in &mesh.edges {
        let (va, vb) = (mesh.verts[e.a], mesh.verts[e.b]);
        let (sa, sb) = (n.dot(va) - d, n.dot(vb) - d);
        if sa.abs() < EPS {
            add(va, &mut pts);
        }
        if sb.abs() < EPS {
            add(vb, &mut pts);
        }
        if (sa > EPS && sb < -EPS) || (sa < -EPS && sb > EPS) {
            add(va.lerp(vb, sa / (sa - sb)), &mut pts);
        }
    }
    if pts.len() < 3 {
        return Vec::new();
    }
    let (u, v) = plane_basis(n);
    let c = pts.iter().copied().sum::<Vec3>() / pts.len() as f32;
    pts.sort_by(|a, b| {
        let ta = (*a - c).dot(v).atan2((*a - c).dot(u));
        let tb = (*b - c).dot(v).atan2((*b - c).dot(u));
        ta.partial_cmp(&tb).unwrap_or(std::cmp::Ordering::Equal)
    });
    // All points on one line (the plane only grazes an edge): no area.
    let area: f32 = (0..pts.len()).map(|i| (pts[i] - c).cross(pts[(i + 1) % pts.len()] - c).length()).sum();
    if area < 1e-4 {
        return Vec::new();
    }
    pts
}

fn plane_basis(n: Vec3) -> (Vec3, Vec3) {
    let helper = if n.y.abs() < 0.9 { Vec3::Y } else { Vec3::X };
    let u = n.cross(helper).normalize();
    (u, n.cross(u))
}

/// The section flattened onto its own plane (true shape), y up.
fn true_shape(pts: &[Vec3], n: Vec3) -> Vec<Vec2> {
    let (u, v) = plane_basis(n);
    pts.iter().map(|p| Vec2::new(p.dot(u), p.dot(v))).collect()
}

/// Interior angles in degrees (convex polygon).
fn angles(poly: &[Vec2]) -> Vec<f32> {
    let n = poly.len();
    (0..n)
        .map(|i| {
            let a = poly[(i + n - 1) % n] - poly[i];
            let b = poly[(i + 1) % n] - poly[i];
            (a.dot(b) / (a.length() * b.length())).clamp(-1.0, 1.0).acos().to_degrees()
        })
        .collect()
}

fn sides(poly: &[Vec2]) -> Vec<f32> {
    (0..poly.len()).map(|i| poly[i].distance(poly[(i + 1) % poly.len()])).collect()
}

/// Name of a straight-sided section, as a Turkish geometry textbook says it.
fn polygon_name(poly: &[Vec2]) -> String {
    let n = poly.len();
    let s = sides(poly);
    let a = angles(poly);
    let max = s.iter().copied().fold(0.0, f32::max);
    let eq = |x: f32, y: f32| (x - y).abs() <= max * 0.01;
    let all_sides_equal = s.iter().all(|&x| eq(x, s[0]));
    let all_angles_equal = a.iter().all(|&x| (x - a[0]).abs() < 0.5);
    let right = |x: f32| (x - 90.0).abs() < 0.5;
    let parallel = |i: usize, j: usize| {
        let d1 = (poly[(i + 1) % n] - poly[i]).normalize();
        let d2 = (poly[(j + 1) % n] - poly[j]).normalize();
        d1.perp_dot(d2).abs() < 0.01
    };
    match n {
        3 => {
            let has_right = a.iter().any(|&x| right(x));
            if all_sides_equal {
                "Eşkenar üçgen".into()
            } else if eq(s[0], s[1]) || eq(s[1], s[2]) || eq(s[0], s[2]) {
                if has_right { "İkizkenar dik üçgen".into() } else { "İkizkenar üçgen".into() }
            } else if has_right {
                "Dik üçgen".into()
            } else {
                "Çeşitkenar üçgen".into()
            }
        }
        4 => {
            let all_right = a.iter().all(|&x| right(x));
            if all_right && all_sides_equal {
                "Kare".into()
            } else if all_right {
                "Dikdörtgen".into()
            } else if all_sides_equal {
                "Eşkenar dörtgen".into()
            } else if parallel(0, 2) && parallel(1, 3) {
                "Paralelkenar".into()
            } else if parallel(0, 2) || parallel(1, 3) {
                "Yamuk".into()
            } else {
                "Dörtgen".into()
            }
        }
        _ => {
            let base = match n {
                5 => "beşgen",
                6 => "altıgen",
                7 => "yedigen",
                8 => "sekizgen",
                _ => return format!("{n}-gen"),
            };
            if all_sides_equal && all_angles_equal {
                format!("Düzgün {base}")
            } else {
                let mut c = base.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            }
        }
    }
}

impl Solid {
    pub fn new(screen_size: Vec2) -> Self {
        let mut s = Self {
            position: Vec2::ZERO,
            kind: Kind::Cube,
            mesh: mesh_for(Kind::Cube),
            yaw: -0.6,
            pitch: 0.38,
            points: Vec::new(),
            plane: None,
            labels: true,
            message: None,
        };
        let size = Vec2::new(s.panel_width(), s.panel_height());
        s.position = Vec2::new((screen_size.x - size.x) / 2.0, ((screen_size.y - 140.0 - size.y) / 2.0).max(16.0));
        s
    }

    fn panel_width(&self) -> f32 {
        PADDING * 2.0 + VIEW_W + 8.0 + SHAPE_W
    }

    fn inner_width(&self) -> f32 {
        self.panel_width() - PADDING * 2.0
    }

    fn kinds_top(&self) -> f32 {
        self.position.y + HEADER_H + PADDING + VIEW_H + 8.0
    }

    fn slider_top(&self) -> f32 {
        self.kinds_top() + 2.0 * (BTN_H + GAP)
    }

    fn tools_top(&self) -> f32 {
        self.slider_top() + SLIDER_H + GAP
    }

    fn panel_height(&self) -> f32 {
        self.tools_top() + BTN_H + PADDING - self.position.y
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

    fn shape_rect(&self) -> (Vec2, Vec2) {
        (self.position + Vec2::new(PADDING + VIEW_W + 8.0, HEADER_H + PADDING), Vec2::new(SHAPE_W, VIEW_H))
    }

    fn kind_rect(&self, i: usize) -> (Vec2, Vec2) {
        let w = (self.inner_width() - (KINDS_PER_ROW as f32 - 1.0) * GAP) / KINDS_PER_ROW as f32;
        let (row, col) = (i / KINDS_PER_ROW, i % KINDS_PER_ROW);
        (
            Vec2::new(self.position.x + PADDING + col as f32 * (w + GAP), self.kinds_top() + row as f32 * (BTN_H + GAP)),
            Vec2::new(w, BTN_H),
        )
    }

    fn slider_rect(&self) -> (Vec2, Vec2) {
        (Vec2::new(self.position.x + PADDING, self.slider_top()), Vec2::new(self.inner_width(), SLIDER_H))
    }

    fn slider_track(&self) -> (f32, f32, f32) {
        let (pos, size) = self.slider_rect();
        (pos.x + 100.0, pos.x + size.x - 24.0, pos.y + size.y / 2.0)
    }

    fn tool_rect(&self, i: usize) -> (Vec2, Vec2) {
        let x = self.position.x + PADDING + i as f32 * (TOOL_W + GAP);
        let w = if i == TOOLS.len() - 1 { self.position.x + PADDING + self.inner_width() - x } else { TOOL_W };
        (Vec2::new(x, self.tools_top()), Vec2::new(w, BTN_H))
    }

    fn proj_in(&self, rect: (Vec2, Vec2), scale: f32) -> Proj {
        let rot = Mat3::from_rotation_x(self.pitch) * Mat3::from_rotation_y(self.yaw);
        let ppu = rect.1.x.min(rect.1.y) / (2.0 * self.mesh.radius * 1.12) * scale;
        Proj { rot, center: rect.0 + rect.1 / 2.0, ppu }
    }

    fn proj(&self) -> Proj {
        self.proj_in(self.view_rect(), 1.0)
    }

    fn look(&self, e: &Edge, proj: &Proj) -> Look {
        let fa = proj.facing(self.mesh.faces[e.faces[0]].normal);
        let fb = proj.facing(self.mesh.faces[e.faces[1]].normal);
        if e.smooth {
            if fa != fb { Look::Front } else { Look::Seam }
        } else if fa || fb {
            Look::Front
        } else {
            Look::Hidden
        }
    }

    fn point_pos(&self, &(edge, t): &(usize, f32)) -> Vec3 {
        let e = &self.mesh.edges[edge];
        self.mesh.verts[e.a].lerp(self.mesh.verts[e.b], t)
    }

    pub fn contains(&self, p: Vec2) -> bool {
        rect_contains((self.position, Vec2::new(self.panel_width(), self.panel_height())), p)
    }

    pub fn drag_to(&mut self, p: Vec2, grab_offset: Vec2) {
        self.position = p - grab_offset;
    }

    pub fn press_at(&mut self, p: Vec2) -> SolidPress {
        if rect_contains(self.close_rect(), p) {
            return SolidPress::Close;
        }
        if rect_contains(self.header_rect(), p) {
            return SolidPress::Move;
        }
        if rect_contains(self.view_rect(), p) {
            return SolidPress::View;
        }
        if rect_contains(self.slider_rect(), p) {
            if self.plane.is_some() {
                self.set_slider(p.x);
                return SolidPress::Slider;
            }
            return SolidPress::Other;
        }
        for (i, &(kind, _)) in KINDS.iter().enumerate() {
            if rect_contains(self.kind_rect(i), p) {
                self.set_kind(kind);
                return SolidPress::Other;
            }
        }
        for (i, &(tool, _)) in TOOLS.iter().enumerate() {
            if rect_contains(self.tool_rect(i), p) {
                self.message = None;
                match tool {
                    Tool::Horizontal => self.preset_plane(Vec3::Y),
                    Tool::Vertical => self.preset_plane(Vec3::X),
                    Tool::ClearSection => {
                        self.points.clear();
                        self.plane = None;
                    }
                    Tool::Labels => self.labels = !self.labels,
                    Tool::Stamp => return SolidPress::Stamp,
                }
                return SolidPress::Other;
            }
        }
        SolidPress::Other
    }

    fn set_kind(&mut self, kind: Kind) {
        if kind != self.kind {
            self.kind = kind;
            self.mesh = mesh_for(kind);
            self.points.clear();
            self.plane = None;
            self.message = None;
        }
    }

    fn plane_range(&self, n: Vec3) -> (f32, f32) {
        let ds = self.mesh.verts.iter().map(|v| n.dot(*v));
        let lo = ds.clone().fold(f32::INFINITY, f32::min);
        let hi = ds.fold(f32::NEG_INFINITY, f32::max);
        (lo, hi)
    }

    fn preset_plane(&mut self, n: Vec3) {
        self.points.clear();
        let (lo, hi) = self.plane_range(n);
        let d0 = (lo + hi) / 2.0;
        self.plane = Some(Plane { n, d: d0, d0, lo, hi });
    }

    /// View drag (after the tap slop): turns the solid.
    pub fn rotate_by(&mut self, delta: Vec2) {
        self.yaw += delta.x * 0.01;
        self.pitch = (self.pitch + delta.y * 0.01).clamp(-1.5, 1.5);
    }

    pub fn set_slider(&mut self, x: f32) {
        let (x0, x1, _) = self.slider_track();
        if let Some(plane) = &mut self.plane {
            let t = ((x - x0) / (x1 - x0)).clamp(0.0, 1.0);
            plane.d = plane.lo + t * (plane.hi - plane.lo);
            // Snap back onto the tapped points' plane when close to it.
            if (plane.d - plane.d0).abs() < (plane.hi - plane.lo) * 0.015 {
                plane.d = plane.d0;
            }
        }
    }

    /// A tap in the view: removes a point under the finger, or puts a new
    /// one on the nearest edge (snapped to quarters). The third point
    /// defines the section plane; a fourth starts a new set.
    pub fn tap(&mut self, p: Vec2) {
        if !rect_contains(self.view_rect(), p) {
            return;
        }
        self.message = None;
        let proj = self.proj();
        if let Some(i) = self.points.iter().position(|pt| proj.pt(self.point_pos(pt)).distance(p) < 18.0) {
            self.points.remove(i);
            self.plane = None;
            return;
        }

        let mut best: Option<(f32, usize, f32)> = None;
        for (ei, e) in self.mesh.edges.iter().enumerate() {
            let look = self.look(e, &proj);
            if e.smooth || look == Look::Seam {
                continue;
            }
            let (a, b) = (proj.pt(self.mesh.verts[e.a]), proj.pt(self.mesh.verts[e.b]));
            let ab = b - a;
            let t = if ab.length_squared() < 1e-6 { 0.0 } else { ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) };
            // Hidden edges are behind visible ones: only win when clearly closer.
            let dist = a.lerp(b, t).distance(p) + if look == Look::Hidden { 8.0 } else { 0.0 };
            if dist < PICK_RADIUS && best.map_or(true, |(bd, _, _)| dist < bd) {
                best = Some((dist, ei, t));
            }
        }
        let Some((_, edge, t)) = best else { return };
        let t = (t * 4.0).round() / 4.0;
        if self.points.len() >= 3 {
            self.points.clear();
        }
        self.plane = None;
        let new = (edge, t);
        let pos = self.point_pos(&new);
        if self.points.iter().any(|q| self.point_pos(q).distance(pos) < 1e-3) {
            return;
        }
        self.points.push(new);
        if self.points.len() == 3 {
            let [p1, p2, p3] = [0, 1, 2].map(|i| self.point_pos(&self.points[i]));
            let n = (p2 - p1).cross(p3 - p1);
            if n.length() < 1e-4 {
                self.points.pop();
                self.message = Some("Bu üç nokta bir doğru üzerinde, başka nokta seçin");
                return;
            }
            let n = n.normalize();
            let d0 = n.dot(p1);
            let (lo, hi) = self.plane_range(n);
            self.plane = Some(Plane { n, d: d0, d0, lo, hi });
        }
    }

    fn section(&self) -> Vec<Vec3> {
        match &self.plane {
            Some(pl) => section_points(&self.mesh, pl.n, pl.d),
            None => Vec::new(),
        }
    }

    /// Whether the section edge from `a` to `b` lies on a face turned
    /// toward the viewer.
    fn section_edge_visible(&self, a: Vec3, b: Vec3, proj: &Proj) -> bool {
        let m = (a + b) / 2.0;
        self.mesh.faces.iter().any(|f| (f.normal.dot(m) - f.d).abs() < 2e-3 && proj.facing(f.normal))
    }

    /// The section's name — conic sections for the cone, circles and
    /// ellipses for the other curved solids, polygon names otherwise.
    fn section_name(&self, pts: &[Vec3], shape: &[Vec2]) -> String {
        let Some(plane) = &self.plane else { return String::new() };
        let curved = matches!(self.kind, Kind::Cylinder | Kind::Cone | Kind::Sphere);
        if !curved || shape.len() <= 8 {
            return polygon_name(shape);
        }
        let horizontal = plane.n.y.abs() > 0.999;
        let cut_flat = |y: f32| pts.iter().any(|p| (p.y - y).abs() < 1e-3);
        match self.kind {
            Kind::Sphere => "Daire".into(),
            Kind::Cylinder => {
                if horizontal {
                    "Daire".into()
                } else if cut_flat(1.1) || cut_flat(-1.1) {
                    "Elips parçası".into()
                } else {
                    "Elips".into()
                }
            }
            _ => {
                if horizontal {
                    return "Daire".into();
                }
                // Angle between the plane and the axis vs. the generator's.
                let gamma = plane.n.y.abs().asin();
                let alpha = (CONE_R / CONE_H).atan();
                if (gamma - alpha).abs() < 1.5f32.to_radians() {
                    "Parabol".into()
                } else if gamma > alpha {
                    if cut_flat(-CONE_H / 2.0) { "Elips parçası".into() } else { "Elips".into() }
                } else {
                    "Hiperbol".into()
                }
            }
        }
    }

    /// Visible and hidden polylines of the solid's outline, chained so
    /// curved rims dash evenly.
    fn outline(&self, proj: &Proj) -> (Vec<Vec<Vec2>>, Vec<Vec<Vec2>>) {
        let mut front = Vec::new();
        let mut hidden = Vec::new();
        for e in &self.mesh.edges {
            match self.look(e, proj) {
                Look::Front => front.push((e.a, e.b)),
                Look::Hidden => hidden.push((e.a, e.b)),
                Look::Seam => {}
            }
        }
        let to_px = |chains: Vec<Vec<usize>>| chains.into_iter().map(|c| c.into_iter().map(|i| proj.pt(self.mesh.verts[i])).collect()).collect();
        (to_px(chain(&front)), to_px(chain(&hidden)))
    }

    /// The figure as ink for "Tahtaya bas", in screen space: the view and
    /// the true-shape box drawn `STAMP_SCALE`× larger around their common
    /// center, kept on screen above the toolbar band.
    pub fn stamp_lines(&self, screen_size: Vec2, ink: [f32; 4]) -> Vec<InkLine> {
        let (vpos, vsize) = self.view_rect();
        let (spos, ssize) = self.shape_rect();
        let content = (vpos, spos + ssize - vpos);
        let size = content.1 * STAMP_SCALE;
        let center = content.0 + content.1 / 2.0;
        let max = (screen_size - Vec2::new(0.0, 100.0) - size).max(Vec2::ZERO);
        let top_left = (center - size / 2.0).clamp(Vec2::ZERO, max);
        let view = (top_left, vsize * STAMP_SCALE);
        let shape_box = (top_left + (spos - vpos) * STAMP_SCALE, ssize * STAMP_SCALE);
        let proj = self.proj_in(view, STAMP_SCALE);

        let mut lines = Vec::new();
        let mut line = |points: Vec<Vec2>, color: [f32; 4], width: f32| {
            if points.len() >= 2 {
                lines.push(InkLine { points: densify(&points, 3.0), color, width });
            }
        };
        let section_color = crate::brush::PALETTE[2];

        let (front, hidden) = self.outline(&proj);
        for poly in hidden {
            for dash in dash_polyline(&poly, 10.0, 7.0) {
                line(dash, ink, 2.0);
            }
        }
        for poly in front {
            line(poly, ink, 2.5);
        }

        let pts = self.section();
        for i in 0..pts.len() {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            let seg = vec![proj.pt(a), proj.pt(b)];
            if self.section_edge_visible(a, b, &proj) {
                line(seg, section_color, 3.0);
            } else {
                for dash in dash_polyline(&seg, 10.0, 7.0) {
                    line(dash, section_color, 2.5);
                }
            }
        }

        if self.labels {
            let h = 18.0;
            for (i, label) in self.mesh.labels.iter().enumerate() {
                let at = label_pos(proj.pt(self.mesh.verts[i]), proj.center, 16.0);
                let w = stroke_text_width(label, h);
                for glyph in stroke_text(label, at - Vec2::new(w / 2.0, h / 2.0), h) {
                    line(glyph, ink, 2.0);
                }
            }
        }

        if !pts.is_empty() {
            let shape = fit_shape(&true_shape(&pts, self.plane.as_ref().map_or(Vec3::Y, |p| p.n)), shape_box, 40.0);
            let mut closed = shape.clone();
            closed.push(shape[0]);
            line(closed, section_color, 3.0);
            if shape.len() <= 8 {
                let center = shape.iter().copied().sum::<Vec2>() / shape.len() as f32;
                for (i, a) in angles(&shape).into_iter().enumerate() {
                    let label = format!("{}°", a.round() as i32);
                    let h = 15.0;
                    let at = shape[i] + (center - shape[i]).normalize_or_zero() * 30.0;
                    let w = stroke_text_width(&label, h);
                    for glyph in stroke_text(&label, at - Vec2::new(w / 2.0, h / 2.0), h) {
                        line(glyph, ink, 1.6);
                    }
                }
            }
        }
        lines
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
        font_atlas.push_text("Cisim ve kesit", header_pos + Vec2::new(PADDING, 10.0), 16.0, COLOR_TEXT, glyph_vertices, glyph_indices);
        let grip = header_pos + header_size / 2.0;
        for i in -1..=1 {
            push_circle(Vec2::new(grip.x + i as f32 * 10.0, grip.y), 2.0, COLOR_TEXT, 8, out_vertices, out_indices);
        }
        let (close_pos, close_size) = self.close_rect();
        let c = close_pos + close_size / 2.0;
        let r = close_size.x * 0.2;
        push_line(c - Vec2::splat(r), c + Vec2::splat(r), 2.5, COLOR_TEXT, out_vertices, out_indices);
        push_line(c + Vec2::new(-r, r), c + Vec2::new(r, -r), 2.5, COLOR_TEXT, out_vertices, out_indices);

        self.render_view(font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
        self.render_shape(font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);

        for (i, &(kind, label)) in KINDS.iter().enumerate() {
            let rect = self.kind_rect(i);
            let color = if kind == self.kind { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE };
            button(rect, label, color, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
        }

        let (spos, ssize) = self.slider_rect();
        push_rect(spos, ssize, COLOR_VIEW_BG, out_vertices, out_indices);
        let dim = self.plane.is_none();
        let text = if dim { COLOR_TEXT_DIM } else { COLOR_TEXT };
        font_atlas.push_text("Düzlem", spos + Vec2::new(14.0, 11.0), 17.0, text, glyph_vertices, glyph_indices);
        let (x0, x1, y) = self.slider_track();
        push_line(Vec2::new(x0, y), Vec2::new(x1, y), 4.0, COLOR_KEY_IDLE, out_vertices, out_indices);
        if let Some(pl) = &self.plane {
            let span = (pl.hi - pl.lo).max(1e-6);
            let x_of = |d: f32| x0 + (x1 - x0) * ((d - pl.lo) / span);
            push_line(Vec2::new(x_of(pl.d0), y - 8.0), Vec2::new(x_of(pl.d0), y + 8.0), 2.0, COLOR_POINT, out_vertices, out_indices);
            push_circle(Vec2::new(x_of(pl.d), y), 11.0, COLOR_TEXT, 20, out_vertices, out_indices);
        }

        for (i, &(tool, label)) in TOOLS.iter().enumerate() {
            let rect = self.tool_rect(i);
            let color = match tool {
                Tool::Labels if self.labels => COLOR_KEY_ACTIVE,
                Tool::ClearSection => COLOR_KEY_CLEAR,
                Tool::Stamp => COLOR_KEY_STAMP,
                _ => COLOR_KEY_IDLE,
            };
            button(rect, label, color, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
        }
    }

    fn render_view(
        &self,
        font_atlas: &FontAtlas,
        out_vertices: &mut Vec<Vertex>,
        out_indices: &mut Vec<u32>,
        glyph_vertices: &mut Vec<GlyphVertex>,
        glyph_indices: &mut Vec<u32>,
    ) {
        let rect = self.view_rect();
        push_rect(rect.0, rect.1, COLOR_VIEW_BG, out_vertices, out_indices);
        let proj = self.proj();
        let (front, hidden) = self.outline(&proj);

        for poly in &hidden {
            for dash in dash_polyline(poly, 8.0, 6.0) {
                push_polyline(&dash, 1.6, COLOR_EDGE_HIDDEN, out_vertices, out_indices);
            }
        }

        let pts = self.section();
        if pts.len() >= 3 {
            let screen: Vec<Vec2> = pts.iter().map(|&p| proj.pt(p)).collect();
            for i in 1..screen.len() - 1 {
                push_triangle(screen[0], screen[i], screen[i + 1], COLOR_SECTION_FILL, out_vertices, out_indices);
            }
        }

        // Front faces: a faint fill shaded by orientation, for depth.
        let light = Vec3::new(-0.4, 0.7, 0.6).normalize();
        for f in &self.mesh.faces {
            let rn = proj.rot * f.normal;
            if rn.z <= 1e-4 {
                continue;
            }
            let shade = 0.05 + 0.10 * rn.dot(light).max(0.0);
            let color = [COLOR_FACE[0], COLOR_FACE[1], COLOR_FACE[2], shade];
            let p0 = proj.pt(self.mesh.verts[f.idx[0]]);
            for k in 1..f.idx.len() - 1 {
                push_triangle(p0, proj.pt(self.mesh.verts[f.idx[k]]), proj.pt(self.mesh.verts[f.idx[k + 1]]), color, out_vertices, out_indices);
            }
        }

        for i in 0..pts.len() {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            let (pa, pb) = (proj.pt(a), proj.pt(b));
            if self.section_edge_visible(a, b, &proj) {
                push_line(pa, pb, 2.5, COLOR_SECTION_EDGE, out_vertices, out_indices);
            } else {
                for dash in dash_polyline(&[pa, pb], 8.0, 6.0) {
                    push_polyline(&dash, 1.8, COLOR_SECTION_EDGE, out_vertices, out_indices);
                }
            }
        }

        for poly in &front {
            push_polyline(poly, 2.5, COLOR_EDGE, out_vertices, out_indices);
        }

        if self.labels {
            let h = 16.0;
            for (i, label) in self.mesh.labels.iter().enumerate() {
                let at = label_pos(proj.pt(self.mesh.verts[i]), proj.center, 14.0);
                let w = font_atlas.measure(label, h);
                font_atlas.push_text(label, at - Vec2::new(w / 2.0, h / 2.0), h, COLOR_TEXT, glyph_vertices, glyph_indices);
            }
        }

        // Tapped points; faded once the slider has moved the plane off them.
        let off = self.plane.as_ref().is_some_and(|pl| (pl.d - pl.d0).abs() > 1e-4);
        let color = if off { [COLOR_POINT[0], COLOR_POINT[1], COLOR_POINT[2], 0.4] } else { COLOR_POINT };
        for pt in &self.points {
            push_circle(proj.pt(self.point_pos(pt)), 7.0, color, 16, out_vertices, out_indices);
        }

        let (hint, color) = match self.message {
            Some(m) => (m, COLOR_WARN),
            None => ("Sürükle: döndür · Kenara dokun: kesit noktası (3 nokta)", COLOR_TEXT_DIM),
        };
        font_atlas.push_text(hint, rect.0 + Vec2::new(10.0, rect.1.y - 24.0), 13.0, color, glyph_vertices, glyph_indices);
    }

    fn render_shape(
        &self,
        font_atlas: &FontAtlas,
        out_vertices: &mut Vec<Vertex>,
        out_indices: &mut Vec<u32>,
        glyph_vertices: &mut Vec<GlyphVertex>,
        glyph_indices: &mut Vec<u32>,
    ) {
        let rect = self.shape_rect();
        push_rect(rect.0, rect.1, COLOR_VIEW_BG, out_vertices, out_indices);
        let pts = self.section();
        let Some(plane) = self.plane.as_ref().filter(|_| !pts.is_empty()) else {
            let msg = "Kesit yok";
            let w = font_atlas.measure(msg, 17.0);
            font_atlas.push_text(msg, rect.0 + Vec2::new((rect.1.x - w) / 2.0, rect.1.y / 2.0 - 10.0), 17.0, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);
            return;
        };
        let flat = true_shape(&pts, plane.n);
        let name = self.section_name(&pts, &flat);
        font_atlas.push_text("Gerçek şekil", rect.0 + Vec2::new(12.0, 12.0), 13.0, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);
        font_atlas.push_text(&name, rect.0 + Vec2::new(12.0, 32.0), 18.0, COLOR_TEXT, glyph_vertices, glyph_indices);

        let shape_box = (rect.0 + Vec2::new(0.0, 60.0), rect.1 - Vec2::new(0.0, 60.0));
        let shape = fit_shape(&flat, shape_box, 36.0);
        for i in 1..shape.len() - 1 {
            push_triangle(shape[0], shape[i], shape[i + 1], COLOR_SECTION_FILL, out_vertices, out_indices);
        }
        for i in 0..shape.len() {
            push_line(shape[i], shape[(i + 1) % shape.len()], 2.5, COLOR_SECTION_EDGE, out_vertices, out_indices);
        }
        if shape.len() <= 8 {
            let center = shape.iter().copied().sum::<Vec2>() / shape.len() as f32;
            for (i, a) in angles(&shape).into_iter().enumerate() {
                let label = format!("{}°", a.round() as i32);
                let h = 13.0;
                let w = font_atlas.measure(&label, h);
                let at = shape[i] + (center - shape[i]).normalize_or_zero() * 24.0;
                font_atlas.push_text(&label, at - Vec2::new(w / 2.0, h / 2.0), h, COLOR_TEXT, glyph_vertices, glyph_indices);
            }
        }
    }
}

/// Fits a flat shape into `rect` (centered, uniform scale, `margin` px
/// clear on every side), flipping y for the screen.
fn fit_shape(shape: &[Vec2], rect: (Vec2, Vec2), margin: f32) -> Vec<Vec2> {
    let min = shape.iter().copied().fold(Vec2::splat(f32::INFINITY), Vec2::min);
    let max = shape.iter().copied().fold(Vec2::splat(f32::NEG_INFINITY), Vec2::max);
    let size = (max - min).max(Vec2::splat(1e-6));
    let avail = (rect.1 - Vec2::splat(2.0 * margin)).max(Vec2::ONE);
    let scale = (avail.x / size.x).min(avail.y / size.y);
    let mid = (min + max) / 2.0;
    let center = rect.0 + rect.1 / 2.0;
    shape.iter().map(|&p| center + Vec2::new(p.x - mid.x, mid.y - p.y) * scale).collect()
}

/// Where a vertex letter goes: just outside the figure, pushed away from
/// its projected center.
fn label_pos(vertex: Vec2, center: Vec2, dist: f32) -> Vec2 {
    let dir = (vertex - center).normalize_or_zero();
    vertex + if dir == Vec2::ZERO { Vec2::new(0.0, -dist) } else { dir * dist }
}

/// Joins edge segments sharing vertices into polylines.
fn chain(segs: &[(usize, usize)]) -> Vec<Vec<usize>> {
    let mut adj: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, &(a, b)) in segs.iter().enumerate() {
        adj.entry(a).or_default().push(i);
        adj.entry(b).or_default().push(i);
    }
    let mut used = vec![false; segs.len()];
    let mut out = Vec::new();
    // Start from chain ends (vertices of odd degree) first so open chains
    // come out whole; closed loops are picked up afterwards.
    let mut starts: Vec<usize> = adj.iter().filter(|(_, v)| v.len() % 2 == 1).map(|(&k, _)| k).collect();
    starts.extend(segs.iter().map(|s| s.0));
    for start in starts {
        loop {
            let Some(&first) = adj[&start].iter().find(|&&s| !used[s]) else { break };
            let mut poly = vec![start];
            let mut cur = start;
            let mut seg = first;
            loop {
                used[seg] = true;
                let (a, b) = segs[seg];
                cur = if a == cur { b } else { a };
                poly.push(cur);
                match adj[&cur].iter().find(|&&s| !used[s]) {
                    Some(&next) => seg = next,
                    None => break,
                }
            }
            out.push(poly);
        }
    }
    out
}

fn push_polyline(points: &[Vec2], width: f32, color: [f32; 4], out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) {
    for w in points.windows(2) {
        push_line(w[0], w[1], width, color, out_vertices, out_indices);
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

    fn name_of(kind: Kind, n: Vec3, d: f32) -> String {
        let mut s = Solid::new(Vec2::new(1920.0, 1080.0));
        s.set_kind(kind);
        let n = n.normalize();
        let (lo, hi) = s.plane_range(n);
        s.plane = Some(Plane { n, d, d0: d, lo, hi });
        let pts = s.section();
        assert!(!pts.is_empty(), "{kind:?} {n:?} {d}");
        let flat = true_shape(&pts, n);
        s.section_name(&pts, &flat)
    }

    #[test]
    fn meshes_are_closed_and_convex() {
        for (kind, _) in KINDS {
            let m = mesh_for(kind);
            // Euler: V - E + F = 2 for every closed solid here.
            assert_eq!(m.verts.len() as i64 - m.edges.len() as i64 + m.faces.len() as i64, 2, "{kind:?}");
            for f in &m.faces {
                for v in &m.verts {
                    assert!(f.normal.dot(*v) - f.d < 1e-4, "{kind:?}: vertex outside a face plane");
                }
            }
        }
        assert_eq!(mesh_for(Kind::Cube).labels.join(""), "ABCDEFGH");
        assert_eq!(mesh_for(Kind::SquarePyramid).labels.join(""), "ABCDT");
    }

    #[test]
    fn cube_sections_have_textbook_names() {
        // Through three vertices adjacent to one corner: equilateral triangle.
        assert_eq!(name_of(Kind::Cube, Vec3::new(1.0, 1.0, 1.0), 1.0), "Eşkenar üçgen");
        // Perpendicular to the space diagonal through the center: regular hexagon.
        assert_eq!(name_of(Kind::Cube, Vec3::new(1.0, 1.0, 1.0), 0.0), "Düzgün altıgen");
        assert_eq!(name_of(Kind::Cube, Vec3::Y, 0.3), "Kare");
        // Through two opposite edges: rectangle (√2 : 1).
        assert_eq!(name_of(Kind::Cube, Vec3::new(1.0, 0.0, 1.0), 0.0), "Dikdörtgen");
    }

    #[test]
    fn cone_gives_the_conic_sections() {
        assert_eq!(name_of(Kind::Cone, Vec3::Y, 0.0), "Daire");
        // Plane steeper than the generator: ellipse; parallel: parabola;
        // flatter: hyperbola.
        let alpha = (CONE_R / CONE_H).atan();
        let tilt = |gamma: f32| Vec3::new(gamma.cos(), gamma.sin(), 0.0);
        assert_eq!(name_of(Kind::Cone, tilt(alpha + 0.5), 0.15), "Elips");
        assert_eq!(name_of(Kind::Cone, tilt(alpha), 0.3), "Parabol");
        assert_eq!(name_of(Kind::Cone, Vec3::X, 0.3), "Hiperbol");
        assert_eq!(name_of(Kind::Sphere, Vec3::new(1.0, 2.0, 0.5), 0.4), "Daire");
    }

    #[test]
    fn hidden_edges_of_a_cube_seen_from_the_front_top() {
        let s = Solid::new(Vec2::new(1920.0, 1080.0));
        let proj = s.proj();
        let hidden = s.mesh.edges.iter().filter(|e| s.look(e, &proj) == Look::Hidden).count();
        // A general view of a cube hides the 3 edges at the back corner.
        assert_eq!(hidden, 3);
        let (front, back) = s.outline(&proj);
        assert_eq!(back.iter().map(|p| p.len() - 1).sum::<usize>(), 3);
        assert_eq!(front.iter().map(|p| p.len() - 1).sum::<usize>(), 9);
    }

    #[test]
    fn three_tapped_points_make_the_section_plane() {
        let mut s = Solid::new(Vec2::new(1920.0, 1080.0));
        let proj = s.proj();
        // Tap the midpoints of AB, BC and BF (the corner at B).
        let taps: Vec<Vec2> = [(0, 1), (1, 2), (1, 5)].iter().map(|&(a, b)| proj.pt((s.mesh.verts[a] + s.mesh.verts[b]) / 2.0)).collect();
        for p in taps {
            s.tap(p);
        }
        assert_eq!(s.points.len(), 3);
        let pts = s.section();
        let flat = true_shape(&pts, s.plane.as_ref().unwrap().n);
        assert_eq!(s.section_name(&pts, &flat), "Eşkenar üçgen");
        // Slide the plane to the center: hexagon.
        let pl = s.plane.as_mut().unwrap();
        pl.d = 0.0;
        let pts = s.section();
        let flat = true_shape(&pts, s.plane.as_ref().unwrap().n);
        assert_eq!(s.section_name(&pts, &flat), "Düzgün altıgen");
    }

    #[test]
    fn chain_joins_a_loop_and_a_path() {
        let chains = chain(&[(0, 1), (1, 2), (2, 0), (5, 6), (6, 7)]);
        let mut lens: Vec<usize> = chains.iter().map(|c| c.len()).collect();
        lens.sort();
        assert_eq!(lens, vec![3, 4]);
    }

    #[test]
    fn stamp_has_dashes_and_true_shape() {
        let mut s = Solid::new(Vec2::new(1920.0, 1080.0));
        s.preset_plane(Vec3::Y);
        let lines = s.stamp_lines(Vec2::new(1920.0, 1080.0), [1.0; 4]);
        assert!(lines.iter().filter(|l| l.color == crate::brush::PALETTE[2]).count() >= 2);
        assert!(lines.len() > 20);
    }
}
