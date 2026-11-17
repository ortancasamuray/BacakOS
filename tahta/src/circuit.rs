//! Draggable "Elektrik devresi" panel (physics): build a DC circuit on a
//! square grid and see it solved.
//!
//! Elements sit on the grid's edges: tap an edge with a tool (wire,
//! resistor, bulb, cell, switch, ammeter, voltmeter, eraser) to place it,
//! or drag along the grid with the wire/eraser tool. Tapping an element of
//! the same kind selects it (its value then follows the slider); a second
//! tap flips a cell, any tap opens/closes a switch. The circuit is solved
//! by modified nodal analysis: every element's current, voltage and power,
//! the equivalent resistance, bulbs glowing by their power, meter
//! readings, a short-circuit warning, and dots that flow with the current.
//! "Tahtaya bas" stamps the diagram with standard symbols (see `marks.rs`).

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
const TOOL_H: f32 = 64.0;
const LABEL_W: f32 = 150.0;
/// Grid points across / down, and their spacing (px).
const COLS: i32 = 7;
const ROWS: i32 = 5;
const SP: f32 = 110.0;
/// Half the length of an element's symbol along its edge (px).
const SYM: f32 = 26.0;
/// A finger this near an edge picks it; this near a point starts a drag.
const EDGE_PICK: f32 = 30.0;
const POINT_PICK: f32 = 24.0;
/// Wires, ammeters and closed switches: almost no resistance (Ω).
const R_WIRE: f64 = 1e-5;
/// A cell giving more than this is shorted (A).
const SHORT_AMPS: f64 = 40.0;
/// A bulb at this power glows fully (W).
const FULL_GLOW: f32 = 14.4;
const STAMP_W: f32 = 900.0;
const STAMP_H: f32 = 560.0;

const COLOR_PANEL_BG: [f32; 4] = [0.12, 0.13, 0.17, 0.96];
const COLOR_HEADER: [f32; 4] = [0.18, 0.19, 0.24, 1.0];
const COLOR_FIELD: [f32; 4] = [0.06, 0.07, 0.09, 1.0];
const COLOR_TEXT: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_TEXT_DIM: [f32; 4] = [0.95, 0.95, 0.97, 0.55];
const COLOR_POINT: [f32; 4] = [1.0, 1.0, 1.0, 0.18];
const COLOR_LINE: [f32; 4] = [0.85, 0.87, 0.92, 1.0];
const COLOR_SELECTED: [f32; 4] = [0.35, 0.65, 1.0, 1.0];
const COLOR_FLOW: [f32; 4] = [1.0, 0.85, 0.30, 1.0];
const COLOR_GLOW: [f32; 4] = [1.0, 0.85, 0.30, 1.0];
const COLOR_METER: [f32; 4] = [0.45, 0.85, 0.55, 1.0];
const COLOR_WARN: [f32; 4] = [1.0, 0.45, 0.40, 1.0];
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
pub enum Kind {
    Wire,
    Resistor,
    Bulb,
    Cell,
    Switch,
    Ammeter,
    Voltmeter,
}

impl Kind {
    fn default_value(self) -> f32 {
        match self {
            Kind::Resistor | Kind::Bulb => 10.0,
            Kind::Cell => 12.0,
            _ => 0.0,
        }
    }

    /// Slider range (min, max, step), for the kinds that have a value.
    fn range(self) -> Option<(f32, f32, f32)> {
        match self {
            Kind::Resistor | Kind::Bulb => Some((1.0, 100.0, 1.0)),
            Kind::Cell => Some((1.5, 24.0, 0.5)),
            _ => None,
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Kind::Resistor => "R",
            Kind::Bulb => "L",
            Kind::Cell => "Pil ",
            Kind::Switch => "K",
            Kind::Ammeter => "A",
            Kind::Voltmeter => "V",
            Kind::Wire => "",
        }
    }
}

/// Tool row: a kind to place, or the eraser (`None`).
const TOOLS: [(Option<Kind>, &str); 8] = [
    (Some(Kind::Wire), "Kablo"),
    (Some(Kind::Resistor), "Direnç"),
    (Some(Kind::Bulb), "Ampul"),
    (Some(Kind::Cell), "Pil"),
    (Some(Kind::Switch), "Anahtar"),
    (Some(Kind::Ammeter), "Ampermetre"),
    (Some(Kind::Voltmeter), "Voltmetre"),
    (None, "Sil"),
];

const PRESETS: [&str; 4] = ["Seri", "Paralel", "Karışık", "Temizle"];

#[derive(Clone, Copy, Debug)]
pub struct Element {
    pub kind: Kind,
    /// Ω for resistors and bulbs, V for cells.
    pub value: f32,
    /// Cell: + at the edge's second end instead of the first.
    pub flipped: bool,
    /// Switch: closed.
    pub closed: bool,
    /// Placement order (names: R1, R2… by it).
    id: u32,
}

type Pt = (i32, i32);

const H_EDGES: usize = ((COLS - 1) * ROWS) as usize;
const EDGES: usize = H_EDGES + (COLS * (ROWS - 1)) as usize;

/// Edge `e`'s two grid points (left→right or top→bottom).
fn edge_points(e: usize) -> (Pt, Pt) {
    if e < H_EDGES {
        let (r, c) = ((e as i32) / (COLS - 1), (e as i32) % (COLS - 1));
        ((c, r), (c + 1, r))
    } else {
        let k = (e - H_EDGES) as i32;
        let (r, c) = (k / COLS, k % COLS);
        ((c, r), (c, r + 1))
    }
}

/// The edge between two neighboring points.
fn edge_between(a: Pt, b: Pt) -> Option<usize> {
    let (a, b) = if (a.1, a.0) <= (b.1, b.0) { (a, b) } else { (b, a) };
    let in_grid = |p: Pt| p.0 >= 0 && p.0 < COLS && p.1 >= 0 && p.1 < ROWS;
    if !in_grid(a) || !in_grid(b) {
        return None;
    }
    if a.1 == b.1 && b.0 == a.0 + 1 {
        Some((a.1 * (COLS - 1) + a.0) as usize)
    } else if a.0 == b.0 && b.1 == a.1 + 1 {
        Some(H_EDGES + (a.1 * COLS + a.0) as usize)
    } else {
        None
    }
}

fn node(p: Pt) -> usize {
    (p.1 * COLS + p.0) as usize
}

/// The solved circuit.
#[derive(Clone, Debug)]
pub struct Solution {
    pub volts: Vec<f64>,
    /// Current along each edge, + from its first point to its second.
    pub amps: Vec<f64>,
    pub short: bool,
}

impl Solution {
    pub fn current(&self, e: usize) -> f32 {
        self.amps[e] as f32
    }

    /// Voltage across edge `e` (first point minus second).
    pub fn voltage(&self, e: usize) -> f32 {
        let (a, b) = edge_points(e);
        (self.volts[node(a)] - self.volts[node(b)]) as f32
    }
}

/// Modified nodal analysis over the grid: node 0 is ground; every cell
/// adds its current as an unknown. A tiny leak from each node to ground
/// keeps unconnected parts solvable (at 0 V).
pub fn solve(elements: &[Option<Element>]) -> Solution {
    let n = (COLS * ROWS) as usize;
    let cells: Vec<usize> = (0..EDGES).filter(|&e| matches!(elements[e], Some(Element { kind: Kind::Cell, .. }))).collect();
    let size = n - 1 + cells.len();
    let mut a = vec![vec![0.0f64; size]; size];
    let mut z = vec![0.0f64; size];
    let conductance = |el: &Element| -> Option<f64> {
        match el.kind {
            Kind::Wire | Kind::Ammeter => Some(1.0 / R_WIRE),
            Kind::Switch => el.closed.then_some(1.0 / R_WIRE),
            Kind::Resistor | Kind::Bulb => Some(1.0 / el.value.max(1e-3) as f64),
            Kind::Cell | Kind::Voltmeter => None,
        }
    };
    for k in 1..n {
        a[k - 1][k - 1] += 1e-9;
    }
    for e in 0..EDGES {
        let Some(el) = &elements[e] else { continue };
        let Some(g) = conductance(el) else { continue };
        let (p, q) = edge_points(e);
        let (i, j) = (node(p), node(q));
        if i != 0 {
            a[i - 1][i - 1] += g;
        }
        if j != 0 {
            a[j - 1][j - 1] += g;
        }
        if i != 0 && j != 0 {
            a[i - 1][j - 1] -= g;
            a[j - 1][i - 1] -= g;
        }
    }
    for (k, &e) in cells.iter().enumerate() {
        let el = elements[e].unwrap();
        let (p, q) = edge_points(e);
        let (plus, minus) = if el.flipped { (node(q), node(p)) } else { (node(p), node(q)) };
        let r = n - 1 + k;
        if plus != 0 {
            a[r][plus - 1] += 1.0;
            a[plus - 1][r] += 1.0;
        }
        if minus != 0 {
            a[r][minus - 1] -= 1.0;
            a[minus - 1][r] -= 1.0;
        }
        z[r] = el.value as f64;
    }
    let x = gauss(a, z);
    let mut volts = vec![0.0; n];
    volts[1..n].copy_from_slice(&x[..(n - 1)]);
    let mut amps = vec![0.0; EDGES];
    let mut short = false;
    for e in 0..EDGES {
        let Some(el) = &elements[e] else { continue };
        let (p, q) = edge_points(e);
        if let Some(g) = conductance(el) {
            amps[e] = (volts[node(p)] - volts[node(q)]) * g;
        }
    }
    for (k, &e) in cells.iter().enumerate() {
        // x is the current through the source from + to − (inside), so
        // − to + inside is the current it drives round the circuit.
        let driven = -x[n - 1 + k];
        let el = elements[e].unwrap();
        // Inside the cell the current runs − → +, i.e. second → first
        // point unless flipped.
        amps[e] = if el.flipped { driven } else { -driven };
        short |= driven.abs() > SHORT_AMPS;
    }
    Solution { volts, amps, short }
}

/// Gaussian elimination with partial pivoting.
fn gauss(mut a: Vec<Vec<f64>>, mut z: Vec<f64>) -> Vec<f64> {
    let n = z.len();
    for col in 0..n {
        let pivot = (col..n).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs())).unwrap();
        if a[pivot][col].abs() < 1e-15 {
            continue;
        }
        a.swap(col, pivot);
        z.swap(col, pivot);
        for row in col + 1..n {
            let f = a[row][col] / a[col][col];
            if f == 0.0 {
                continue;
            }
            for k in col..n {
                a[row][k] -= f * a[col][k];
            }
            z[row] -= f * z[col];
        }
    }
    let mut x = vec![0.0; n];
    for row in (0..n).rev() {
        if a[row][row].abs() < 1e-15 {
            continue;
        }
        let s: f64 = (row + 1..n).map(|k| a[row][k] * x[k]).sum();
        x[row] = (z[row] - s) / a[row][row];
    }
    x
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CircuitPress {
    Move,
    Close,
    Slider,
    /// Finger down on a grid point with the wire/eraser tool: dragging
    /// lays (or erases) wire along the edges it passes (`paint_to`).
    Paint,
    Stamp,
    Other,
}

pub struct Circuit {
    pub position: Vec2,
    tool: usize,
    elements: Vec<Option<Element>>,
    selected: Option<usize>,
    next_id: u32,
    flow: bool,
    labels: bool,
    /// The grid point a wire drag last reached.
    paint_last: Option<Pt>,
}

impl Circuit {
    pub fn new(screen_size: Vec2) -> Self {
        let mut c = Self {
            position: Vec2::ZERO,
            tool: 0,
            elements: vec![None; EDGES],
            selected: None,
            next_id: 0,
            flow: true,
            labels: true,
            paint_last: None,
        };
        c.load_preset(0);
        let size = Vec2::new(c.panel_width(), c.panel_height());
        c.position = Vec2::new((screen_size.x - size.x) / 2.0, ((screen_size.y - 120.0 - size.y) / 2.0).max(12.0));
        c
    }

    fn put(&mut self, a: Pt, b: Pt, kind: Kind) {
        if let Some(e) = edge_between(a, b) {
            self.elements[e] = Some(Element { kind, value: kind.default_value(), flipped: false, closed: true, id: self.next_id });
            self.next_id += 1;
        }
    }

    /// Wire along a path of points.
    fn wire(&mut self, path: &[Pt]) {
        for w in path.windows(2) {
            self.put(w[0], w[1], Kind::Wire);
        }
    }

    fn load_preset(&mut self, k: usize) {
        self.elements = vec![None; EDGES];
        self.selected = None;
        self.next_id = 0;
        match k {
            // Seri: cell, switch, ammeter, two bulbs in one loop.
            0 => {
                self.wire(&[(1, 1), (2, 1)]);
                self.put((2, 1), (3, 1), Kind::Bulb);
                self.wire(&[(3, 1), (4, 1)]);
                self.put((4, 1), (5, 1), Kind::Bulb);
                self.wire(&[(5, 1), (5, 2), (5, 3), (4, 3)]);
                self.put((3, 3), (4, 3), Kind::Ammeter);
                self.put((2, 3), (3, 3), Kind::Cell);
                self.put((1, 3), (2, 3), Kind::Switch);
                self.wire(&[(1, 3), (1, 2), (1, 1)]);
            }
            // Paralel: two bulbs across the cell, ammeter in the main line.
            1 => {
                self.put((1, 1), (1, 2), Kind::Cell);
                self.put((1, 2), (1, 3), Kind::Ammeter);
                self.wire(&[(1, 1), (2, 1), (3, 1), (4, 1), (5, 1)]);
                self.wire(&[(1, 3), (2, 3), (3, 3), (4, 3), (5, 3)]);
                self.put((3, 1), (3, 2), Kind::Bulb);
                self.wire(&[(3, 2), (3, 3)]);
                self.put((5, 1), (5, 2), Kind::Bulb);
                self.wire(&[(5, 2), (5, 3)]);
            }
            // Karışık: R1 in series with two parallel bulbs, a voltmeter
            // across R1.
            2 => {
                self.put((1, 1), (1, 2), Kind::Cell);
                self.wire(&[(1, 2), (1, 3)]);
                self.wire(&[(1, 1), (2, 1)]);
                self.put((2, 1), (3, 1), Kind::Resistor);
                self.wire(&[(3, 1), (4, 1), (5, 1)]);
                self.wire(&[(2, 1), (2, 0)]);
                self.put((2, 0), (3, 0), Kind::Voltmeter);
                self.wire(&[(3, 0), (3, 1)]);
                self.put((4, 1), (4, 2), Kind::Bulb);
                self.wire(&[(4, 2), (4, 3)]);
                self.put((5, 1), (5, 2), Kind::Bulb);
                self.wire(&[(5, 2), (5, 3)]);
                self.wire(&[(1, 3), (2, 3), (3, 3), (4, 3), (5, 3)]);
            }
            _ => {}
        }
    }

    pub fn solution(&self) -> Solution {
        solve(&self.elements)
    }

    /// "R2", "L1", "Pil 1"… numbered by placement within each kind.
    fn name(&self, e: usize) -> String {
        let Some(el) = &self.elements[e] else { return String::new() };
        let rank = self.elements.iter().flatten().filter(|o| o.kind == el.kind && o.id <= el.id).count();
        let total = self.elements.iter().flatten().filter(|o| o.kind == el.kind).count();
        match el.kind {
            Kind::Cell | Kind::Switch | Kind::Ammeter | Kind::Voltmeter if total == 1 => el.kind.prefix().trim().to_string(),
            Kind::Cell => format!("Pil {rank}"),
            _ => format!("{}_{rank}", el.kind.prefix()),
        }
    }

    /// Edges in a stable order for the table: cells, resistors, bulbs, meters.
    fn listed(&self) -> Vec<usize> {
        let mut out = Vec::new();
        for kind in [Kind::Cell, Kind::Resistor, Kind::Bulb, Kind::Ammeter, Kind::Voltmeter] {
            let mut es: Vec<usize> = (0..EDGES).filter(|&e| self.elements[e].is_some_and(|el| el.kind == kind)).collect();
            es.sort_by_key(|&e| self.elements[e].unwrap().id);
            out.extend(es);
        }
        out
    }

    pub fn is_animating(&self, _now: f64) -> bool {
        self.flow && self.solution().amps.iter().any(|i| i.abs() > 1e-4)
    }

    // --- Layout ----------------------------------------------------------

    fn result_h() -> f32 {
        Self::view_h() - (2.0 * TOOL_H + GAP) - 4.0 * BTN_H - 5.0 * GAP
    }

    fn slot_heights() -> [f32; 6] {
        [2.0 * TOOL_H + GAP, BTN_H, BTN_H, Self::result_h(), BTN_H, BTN_H]
    }

    fn view_h() -> f32 {
        586.0
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

    fn tool_rect(&self, k: usize) -> (Vec2, Vec2) {
        let (pos, size) = self.slot_rect(0);
        let w = (size.x - 3.0 * GAP) / 4.0;
        (pos + Vec2::new((k % 4) as f32 * (w + GAP), (k / 4) as f32 * (TOOL_H + GAP)), Vec2::new(w, TOOL_H))
    }

    fn preset_rect(&self, k: usize) -> (Vec2, Vec2) {
        let (pos, size) = self.slot_rect(1);
        let w = (size.x - 3.0 * GAP) / 4.0;
        (pos + Vec2::new(k as f32 * (w + GAP), 0.0), Vec2::new(w, size.y))
    }

    fn slider_track(&self) -> (f32, f32, f32) {
        let (pos, size) = self.slot_rect(2);
        (pos.x + LABEL_W, pos.x + size.x - 20.0, pos.y + size.y / 2.0)
    }

    fn half(&self, slot: usize, j: usize) -> (Vec2, Vec2) {
        let (pos, size) = self.slot_rect(slot);
        let w = (size.x - GAP) / 2.0;
        (pos + Vec2::new(j as f32 * (w + GAP), 0.0), Vec2::new(w, size.y))
    }

    fn flow_rect(&self) -> (Vec2, Vec2) {
        self.half(4, 0)
    }

    fn labels_rect(&self) -> (Vec2, Vec2) {
        self.half(4, 1)
    }

    fn stamp_rect(&self) -> (Vec2, Vec2) {
        self.slot_rect(5)
    }

    /// Grid point (0, 0) on screen.
    fn grid_origin(&self) -> Vec2 {
        let (pos, size) = self.view_rect();
        pos + Vec2::new((size.x - (COLS - 1) as f32 * SP) / 2.0, (size.y - (ROWS - 1) as f32 * SP) / 2.0 + 14.0)
    }

    fn pt(origin: Vec2, p: Pt) -> Vec2 {
        origin + Vec2::new(p.0 as f32, p.1 as f32) * SP
    }

    fn nearest_point(&self, p: Vec2) -> (Pt, f32) {
        let o = self.grid_origin();
        let g = ((p - o) / SP).round();
        let q = ((g.x as i32).clamp(0, COLS - 1), (g.y as i32).clamp(0, ROWS - 1));
        (q, Self::pt(o, q).distance(p))
    }

    fn nearest_edge(&self, p: Vec2) -> Option<usize> {
        let o = self.grid_origin();
        (0..EDGES)
            .map(|e| {
                let (a, b) = edge_points(e);
                (e, seg_distance(p, Self::pt(o, a), Self::pt(o, b)))
            })
            .filter(|(_, d)| *d <= EDGE_PICK)
            .min_by(|x, y| x.1.total_cmp(&y.1))
            .map(|(e, _)| e)
    }

    // --- Input -----------------------------------------------------------

    pub fn contains(&self, p: Vec2) -> bool {
        rect_contains((self.position, Vec2::new(self.panel_width(), self.panel_height())), p)
    }

    pub fn drag_to(&mut self, p: Vec2, grab_offset: Vec2) {
        self.position = p - grab_offset;
    }

    fn tool_kind(&self) -> Option<Kind> {
        TOOLS[self.tool].0
    }

    pub fn press_at(&mut self, p: Vec2) -> CircuitPress {
        self.paint_last = None;
        if rect_contains(self.close_rect(), p) {
            return CircuitPress::Close;
        }
        if rect_contains(self.header_rect(), p) {
            return CircuitPress::Move;
        }
        if rect_contains(self.stamp_rect(), p) {
            return CircuitPress::Stamp;
        }
        if rect_contains(self.view_rect(), p) {
            let painter = matches!(self.tool_kind(), Some(Kind::Wire) | None);
            let (q, d) = self.nearest_point(p);
            if painter && d <= POINT_PICK {
                self.paint_last = Some(q);
                return CircuitPress::Paint;
            }
            if let Some(e) = self.nearest_edge(p) {
                self.tap_edge(e);
            } else {
                self.selected = None;
            }
            return CircuitPress::Other;
        }
        if let Some(k) = (0..TOOLS.len()).find(|&k| rect_contains(self.tool_rect(k), p)) {
            self.tool = k;
            return CircuitPress::Other;
        }
        if let Some(k) = (0..PRESETS.len()).find(|&k| rect_contains(self.preset_rect(k), p)) {
            self.load_preset(k);
            return CircuitPress::Other;
        }
        if rect_contains(self.slot_rect(2), p) && self.selected_range().is_some() {
            self.set_slider(p.x);
            return CircuitPress::Slider;
        }
        if rect_contains(self.flow_rect(), p) {
            self.flow = !self.flow;
        } else if rect_contains(self.labels_rect(), p) {
            self.labels = !self.labels;
        }
        CircuitPress::Other
    }

    /// A tap on edge `e` with the current tool.
    pub fn tap_edge(&mut self, e: usize) {
        let Some(kind) = self.tool_kind() else {
            self.elements[e] = None;
            if self.selected == Some(e) {
                self.selected = None;
            }
            return;
        };
        match &mut self.elements[e] {
            Some(el) if el.kind == kind => match kind {
                Kind::Switch => el.closed = !el.closed,
                Kind::Cell if self.selected == Some(e) => el.flipped = !el.flipped,
                _ => {}
            },
            _ => {
                self.elements[e] = Some(Element { kind, value: kind.default_value(), flipped: false, closed: true, id: self.next_id });
                self.next_id += 1;
            }
        }
        self.selected = (kind != Kind::Wire).then_some(e);
    }

    /// Finger moved during a wire/eraser drag.
    pub fn paint_to(&mut self, p: Vec2) {
        let Some(last) = self.paint_last else { return };
        let (q, d) = self.nearest_point(p);
        if d > SP * 0.35 || q == last {
            return;
        }
        if let Some(e) = edge_between(last, q) {
            match self.tool_kind() {
                Some(Kind::Wire) => {
                    if self.elements[e].is_none() {
                        self.elements[e] = Some(Element { kind: Kind::Wire, value: 0.0, flipped: false, closed: true, id: self.next_id });
                        self.next_id += 1;
                    }
                }
                None => self.elements[e] = None,
                _ => {}
            }
        }
        // Diagonal jumps (fast finger) just move on.
        self.paint_last = Some(q);
    }

    fn selected_range(&self) -> Option<(f32, f32, f32)> {
        self.selected.and_then(|e| self.elements[e]).and_then(|el| el.kind.range())
    }

    pub fn set_slider(&mut self, x: f32) {
        let Some((lo, hi, step)) = self.selected_range() else { return };
        let (x0, x1, _) = self.slider_track();
        let v = lo + ((hi - lo) * ((x - x0) / (x1 - x0)).clamp(0.0, 1.0) / step).round() * step;
        if let Some(el) = self.selected.and_then(|e| self.elements[e].as_mut()) {
            el.value = v;
        }
    }

    // --- Scene -----------------------------------------------------------

    fn label_text(&self, e: usize, s: &Solution) -> Option<String> {
        let el = self.elements[e]?;
        match el.kind {
            Kind::Wire => None,
            Kind::Resistor | Kind::Bulb if self.labels => Some(format!("{} = {} Ω", self.name(e), num(el.value, 0))),
            Kind::Cell if self.labels => Some(format!("{} V", num(el.value, 1).trim_end_matches(",0"))),
            Kind::Switch if self.labels => Some(self.name(e)),
            Kind::Ammeter => Some(format!("{} A", num(s.current(e).abs(), 2))),
            Kind::Voltmeter => Some(format!("{} V", num(s.voltage(e).abs(), 2))),
            _ => None,
        }
    }

    fn scene(&self, origin: Vec2, now: f64, ink: bool) -> Vec<Mark> {
        let s = self.solution();
        let mut m = Vec::new();
        if !ink {
            for r in 0..ROWS {
                for c in 0..COLS {
                    m.push(Mark::Dot { c: Self::pt(origin, (c, r)), r: 3.0, style: screen(COLOR_POINT), filled: true });
                }
            }
        }
        // Junctions: three or more conductors meet.
        let mut degree = vec![0; (COLS * ROWS) as usize];
        for e in 0..EDGES {
            if self.elements[e].is_some() {
                let (a, b) = edge_points(e);
                degree[node(a)] += 1;
                degree[node(b)] += 1;
            }
        }
        for e in 0..EDGES {
            let Some(el) = self.elements[e] else { continue };
            let (a, b) = edge_points(e);
            let (p, q) = (Self::pt(origin, a), Self::pt(origin, b));
            if !ink {
                if self.selected == Some(e) {
                    let d = (q - p).normalize();
                    let n = Vec2::new(d.y, -d.x) * 30.0;
                    let mid = (p + q) / 2.0;
                    let h = d * (SYM + 14.0);
                    m.push(Mark::dashed(vec![mid - h + n, mid + h + n, mid + h - n, mid - h - n, mid - h + n], 1.6, screen(COLOR_SELECTED)));
                }
                if el.kind == Kind::Bulb {
                    let glow = (el.value * s.current(e).powi(2) / FULL_GLOW).sqrt().min(1.0);
                    if glow > 0.02 {
                        let mid = (p + q) / 2.0;
                        m.push(Mark::Dot { c: mid, r: 24.0 + 14.0 * glow, style: screen([COLOR_GLOW[0], COLOR_GLOW[1], COLOR_GLOW[2], 0.15 + 0.35 * glow]), filled: true });
                        m.push(Mark::Dot { c: mid, r: 17.0, style: screen([COLOR_GLOW[0], COLOR_GLOW[1], COLOR_GLOW[2], 0.25 + 0.6 * glow]), filled: true });
                    }
                }
            }
            symbol(&mut m, el, p, q);
            if let Some(text) = self.label_text(e, &s) {
                let color = if matches!(el.kind, Kind::Ammeter | Kind::Voltmeter) { COLOR_METER } else { COLOR_LINE };
                let mid = (p + q) / 2.0;
                if a.1 == b.1 {
                    m.push(Mark::text(text, mid + Vec2::new(0.0, -SYM - 22.0), 15.0, inked(color), Anchor::Center));
                } else {
                    // Name over value, on whichever side has no neighbor
                    // element next to it.
                    let busy = |dc: i32| {
                        edge_between((a.0 + dc, a.1), (b.0 + dc, b.1)).and_then(|n| self.elements[n]).is_some_and(|n| n.kind != Kind::Wire)
                    };
                    let left = busy(1) && !busy(-1);
                    let (x, anchor) = if left { (-SYM - 6.0, Anchor::Right) } else { (SYM + 6.0, Anchor::Left) };
                    let lines: Vec<&str> = text.splitn(2, " = ").collect();
                    if lines.len() == 2 {
                        m.push(Mark::text(lines[0], mid + Vec2::new(x, -20.0), 15.0, inked(color), anchor));
                        m.push(Mark::text(lines[1], mid + Vec2::new(x, 2.0), 15.0, inked(color), anchor));
                    } else {
                        m.push(Mark::text(text, mid + Vec2::new(x, -8.0), 15.0, inked(color), anchor));
                    }
                }
            }
            // Current dots, conventional direction (+ → −).
            if !ink && self.flow && !s.short {
                let i = s.current(e);
                if i.abs() > 1e-4 {
                    let len = SP;
                    let spacing = 22.0;
                    let speed = (i.abs() * 60.0).clamp(10.0, 140.0);
                    let phase = ((now as f32 * speed) % spacing) * i.signum();
                    let d = (q - p) / len;
                    let mut t = phase.rem_euclid(spacing);
                    while t < len {
                        m.push(Mark::Dot { c: p + d * t, r: 3.2, style: screen(COLOR_FLOW), filled: true });
                        t += spacing;
                    }
                }
            }
        }
        for r in 0..ROWS {
            for c in 0..COLS {
                if degree[node((c, r))] >= 3 {
                    m.push(Mark::Dot { c: Self::pt(origin, (c, r)), r: if ink { 3.0 } else { 5.0 }, style: inked(COLOR_LINE), filled: !ink });
                }
            }
        }
        m
    }

    // --- Ink -------------------------------------------------------------

    pub fn is_empty(&self) -> bool {
        self.elements.iter().all(|e| e.is_none())
    }

    pub fn stamp_lines(&self, screen_size: Vec2, ink: [f32; 4], now: f64) -> Vec<InkLine> {
        let used: Vec<Pt> = (0..EDGES).filter(|&e| self.elements[e].is_some()).flat_map(|e| {
            let (a, b) = edge_points(e);
            [a, b]
        }).collect();
        if used.is_empty() {
            return Vec::new();
        }
        let lo = Vec2::new(used.iter().map(|p| p.0).min().unwrap() as f32, used.iter().map(|p| p.1).min().unwrap() as f32);
        let hi = Vec2::new(used.iter().map(|p| p.0).max().unwrap() as f32, used.iter().map(|p| p.1).max().unwrap() as f32);
        let size = Vec2::new(STAMP_W.min(screen_size.x - 160.0), STAMP_H.min(screen_size.y - 240.0));
        let top_left = Vec2::new((screen_size.x - size.x) / 2.0, ((screen_size.y - 130.0 - size.y) / 2.0).max(20.0));
        let center = top_left + size / 2.0;
        let origin = center - (lo + hi) / 2.0 * SP;
        marks::to_ink(&self.scene(origin, now, true), ink)
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
        font_atlas.push_text("Elektrik devresi", header_pos + Vec2::new(PADDING, 10.0), 16.0, COLOR_TEXT, gv, gi);
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
        let s = self.solution();
        marks::draw(&self.scene(self.grid_origin(), now, false), font_atlas, v, i, gv, gi);
        let (hint, color) = if s.short {
            ("Kısa devre! Pil uçları direnç olmadan birleşiyor.".to_string(), COLOR_WARN)
        } else {
            let hint = match self.tool_kind() {
                Some(Kind::Wire) => "Kenarlara dokunun ya da noktalardan sürükleyerek kablo çizin",
                None => "Silmek için elemana dokunun ya da üstünden sürükleyin",
                Some(Kind::Switch) => "Anahtara dokunarak açıp kapatın",
                Some(Kind::Cell) => "Seçili pile yeniden dokunursanız kutupları değişir",
                _ => "Kenara dokunun: yerleştirir; yeniden dokunun: seçer",
            };
            (hint.to_string(), COLOR_TEXT_DIM)
        };
        font_atlas.push_text(&hint, view.0 + Vec2::new(16.0, 14.0), 15.0, color, gv, gi);

        // Tools.
        for (k, (kind, name)) in TOOLS.iter().enumerate() {
            let (pos, size) = self.tool_rect(k);
            push_rect(pos, size, if k == self.tool { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE }, v, i);
            let cy = pos.y + 22.0;
            let (a, b) = (Vec2::new(pos.x + 12.0, cy), Vec2::new(pos.x + size.x - 12.0, cy));
            match kind {
                Some(kind) => {
                    let mut icon = Vec::new();
                    let el = Element { kind: *kind, value: 0.0, flipped: false, closed: false, id: 0 };
                    symbol_scaled(&mut icon, el, a, b, 0.55);
                    marks::draw(&icon, font_atlas, v, i, gv, gi);
                }
                None => {
                    let m = (a + b) / 2.0;
                    push_line(m - Vec2::splat(9.0), m + Vec2::splat(9.0), 2.5, COLOR_WARN, v, i);
                    push_line(m + Vec2::new(-9.0, 9.0), m + Vec2::new(9.0, -9.0), 2.5, COLOR_WARN, v, i);
                }
            }
            let h = 12.0;
            let w = font_atlas.measure(name, h);
            font_atlas.push_text(name, Vec2::new(pos.x + (size.x - w) / 2.0, pos.y + size.y - 19.0), h, COLOR_TEXT, gv, gi);
        }
        for (k, name) in PRESETS.iter().enumerate() {
            button(self.preset_rect(k), name, COLOR_KEY_IDLE, font_atlas, v, i, gv, gi);
        }

        // Selected element's value.
        let (spos, ssize) = self.slot_rect(2);
        push_rect(spos, ssize, COLOR_FIELD, v, i);
        match (self.selected, self.selected_range()) {
            (Some(e), Some((lo, hi, _))) => {
                let el = self.elements[e].unwrap();
                let label = match el.kind {
                    Kind::Cell => format!("{} = {} V", self.name(e), num(el.value, 1)),
                    _ => format!("{} = {} Ω", self.name(e), num(el.value, 0)),
                };
                push_rich(font_atlas, &label, spos + Vec2::new(12.0, 14.0), 16.0, COLOR_TEXT, Anchor::Left, gv, gi);
                let (x0, x1, y) = self.slider_track();
                let x = x0 + (x1 - x0) * (el.value - lo) / (hi - lo);
                push_line(Vec2::new(x0, y), Vec2::new(x1, y), 4.0, COLOR_KEY_IDLE, v, i);
                push_line(Vec2::new(x0, y), Vec2::new(x, y), 4.0, COLOR_SLIDER, v, i);
                push_circle(Vec2::new(x, y), 11.0, COLOR_TEXT, 20, v, i);
            }
            _ => {
                font_atlas.push_text("Değerini değiştirmek için bir eleman seçin", spos + Vec2::new(12.0, 15.0), 14.0, COLOR_TEXT_DIM, gv, gi);
            }
        }

        // Results table.
        let (rpos, rsize) = self.slot_rect(3);
        push_rect(rpos, rsize, COLOR_FIELD, v, i);
        let cols = [12.0, 76.0, 146.0, 216.0, 280.0];
        let h = 14.0;
        for (k, title) in ["", "Değer", "Akım", "Gerilim", "Güç"].iter().enumerate() {
            font_atlas.push_text(title, rpos + Vec2::new(cols[k], 10.0), h, COLOR_TEXT_DIM, gv, gi);
        }
        let rows = self.listed();
        let max_rows = ((rsize.y - 90.0) / 22.0) as usize;
        for (row, &e) in rows.iter().take(max_rows).enumerate() {
            let el = self.elements[e].unwrap();
            let y = rpos.y + 34.0 + row as f32 * 22.0;
            let color = if self.selected == Some(e) { COLOR_SELECTED } else { COLOR_TEXT };
            let (amps, volts) = (s.current(e).abs(), s.voltage(e).abs());
            let value = match el.kind {
                Kind::Resistor | Kind::Bulb => format!("{} Ω", num(el.value, 0)),
                Kind::Cell => format!("{} V", num(el.value, 1)),
                _ => String::new(),
            };
            let (amps_s, volts_s, power_s) = match el.kind {
                Kind::Voltmeter => (String::new(), format!("{} V", num(volts, 2)), String::new()),
                Kind::Ammeter => (format!("{} A", num(amps, 2)), String::new(), String::new()),
                Kind::Cell => (format!("{} A", num(amps, 2)), format!("{} V", num(el.value, 1)), format!("{} W", num(amps * el.value, 1))),
                _ => (format!("{} A", num(amps, 2)), format!("{} V", num(volts, 2)), format!("{} W", num(amps * volts, 1))),
            };
            // A shorted branch: the numbers mean nothing but "too much".
            let (amps_s, power_s) = if amps as f64 > SHORT_AMPS {
                ("∞".to_string(), if power_s.is_empty() { power_s } else { "∞".to_string() })
            } else {
                (amps_s, power_s)
            };
            let cells = [self.name(e), value, amps_s, volts_s, power_s];
            for (k, text) in cells.iter().enumerate() {
                push_rich(font_atlas, text, Vec2::new(rpos.x + cols[k], y), h, color, Anchor::Left, gv, gi);
            }
        }
        if rows.len() > max_rows {
            font_atlas.push_text("…", Vec2::new(rpos.x + 12.0, rpos.y + 34.0 + max_rows as f32 * 22.0), h, COLOR_TEXT_DIM, gv, gi);
        }
        let summary = self.summary(&s);
        for (k, line) in summary.iter().enumerate() {
            let y = rpos.y + rsize.y - 52.0 + k as f32 * 22.0;
            push_rich(font_atlas, line, Vec2::new(rpos.x + 12.0, y), 15.0, if s.short { COLOR_WARN } else { COLOR_TEXT }, Anchor::Left, gv, gi);
        }

        button(self.flow_rect(), "Akım akışı", if self.flow { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE }, font_atlas, v, i, gv, gi);
        button(self.labels_rect(), "Değerler", if self.labels { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE }, font_atlas, v, i, gv, gi);
        button(self.stamp_rect(), "Tahtaya bas", COLOR_KEY_STAMP, font_atlas, v, i, gv, gi);
    }

    /// Two lines under the table: main current and equivalent resistance.
    fn summary(&self, s: &Solution) -> Vec<String> {
        if s.short {
            return vec!["Kısa devre: akım çok büyük,".into(), "pil ve kablolar zarar görür.".into()];
        }
        let cells: Vec<usize> = (0..EDGES).filter(|&e| self.elements[e].is_some_and(|el| el.kind == Kind::Cell)).collect();
        if cells.is_empty() {
            return vec!["Devrede pil yok.".into()];
        }
        let main = s.current(cells[0]).abs();
        if main < 1e-4 {
            return vec!["Devre açık: akım geçmiyor.".into()];
        }
        let mut out = vec![format!("Ana kol akımı: {} A", num(main, 2))];
        if cells.len() == 1 {
            let emf = self.elements[cells[0]].unwrap().value;
            let req = num(emf / main, 2);
            let req = req.trim_end_matches('0').trim_end_matches(',');
            out.push(format!("Eşdeğer direnç: R_{{eş}} = {req} Ω"));
        }
        out
    }
}

/// Standard symbol of `el` on the edge p→q.
fn symbol(m: &mut Vec<Mark>, el: Element, p: Vec2, q: Vec2) {
    symbol_scaled(m, el, p, q, 1.0);
}

fn symbol_scaled(m: &mut Vec<Mark>, el: Element, p: Vec2, q: Vec2, s: f32) {
    let st = inked(COLOR_LINE);
    let w = 2.4 * s.max(0.7);
    let d = (q - p).normalize();
    let n = Vec2::new(d.y, -d.x);
    let mid = (p + q) / 2.0;
    let hl = SYM * s;
    let leads = |m: &mut Vec<Mark>, half: f32| {
        m.push(Mark::line(vec![p, mid - d * half], w, st));
        m.push(Mark::line(vec![mid + d * half, q], w, st));
    };
    match el.kind {
        Kind::Wire => m.push(Mark::line(vec![p, q], w, st)),
        Kind::Resistor => {
            leads(m, hl);
            let mut zig = vec![mid - d * hl];
            for k in 0..6 {
                let t = -hl + (k as f32 + 0.5) * (2.0 * hl / 6.0);
                let side = if k % 2 == 0 { 1.0 } else { -1.0 };
                zig.push(mid + d * t + n * side * 10.0 * s);
            }
            zig.push(mid + d * hl);
            m.push(Mark::line(zig, w, st));
        }
        Kind::Bulb => {
            let r = 17.0 * s;
            leads(m, r);
            m.push(Mark::line(crate::periodic::circle_points(mid, r, 24), w, st));
            let k = r * 0.7;
            m.push(Mark::line(vec![mid + (d + n) * k * 0.72, mid - (d + n) * k * 0.72], w, st));
            m.push(Mark::line(vec![mid + (d - n) * k * 0.72, mid - (d - n) * k * 0.72], w, st));
        }
        Kind::Cell => {
            let gap = 6.0 * s;
            leads(m, gap);
            let (plus, minus) = if el.flipped { (mid + d * gap, mid - d * gap) } else { (mid - d * gap, mid + d * gap) };
            m.push(Mark::line(vec![plus - n * 20.0 * s, plus + n * 20.0 * s], w, st));
            m.push(Mark::line(vec![minus - n * 10.0 * s, minus + n * 10.0 * s], w * 2.0, st));
            if s >= 1.0 {
                let sign_at = plus + (plus - minus).normalize() * 12.0 + n * 22.0;
                m.push(Mark::text("+", sign_at - Vec2::new(0.0, 8.0), 14.0, st, Anchor::Center));
            }
        }
        Kind::Switch => {
            leads(m, hl);
            let (a, b) = (mid - d * hl, mid + d * hl);
            m.push(Mark::Dot { c: a, r: 3.5 * s, style: st, filled: false });
            m.push(Mark::Dot { c: b, r: 3.5 * s, style: st, filled: false });
            let tip = if el.closed { b } else { a + (d * 0.87 + n * 0.5) * 2.0 * hl };
            m.push(Mark::line(vec![a, tip], w, st));
        }
        Kind::Ammeter | Kind::Voltmeter => {
            let r = 17.0 * s;
            leads(m, r);
            m.push(Mark::line(crate::periodic::circle_points(mid, r, 24), w, st));
            let letter = if el.kind == Kind::Ammeter { "A" } else { "V" };
            m.push(Mark::text(letter, mid - Vec2::new(0.0, 8.0 * s), 16.0 * s, st, Anchor::Center));
        }
    }
}

fn seg_distance(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
    p.distance(a + ab * t)
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
        (a - b).abs() < 2e-3
    }

    fn edge_of(c: &Circuit, kind: Kind, nth: usize) -> usize {
        let mut es: Vec<usize> = (0..EDGES).filter(|&e| c.elements[e].is_some_and(|el| el.kind == kind)).collect();
        es.sort_by_key(|&e| c.elements[e].unwrap().id);
        es[nth]
    }

    #[test]
    fn edges_round_trip() {
        for e in 0..EDGES {
            let (a, b) = edge_points(e);
            assert_eq!(edge_between(a, b), Some(e));
            assert_eq!(edge_between(b, a), Some(e));
        }
        assert_eq!(edge_between((0, 0), (1, 1)), None);
        assert_eq!(edge_between((6, 0), (7, 0)), None);
    }

    #[test]
    fn series_parallel_and_mixed() {
        let mut c = Circuit::new(SCREEN);
        // Seri: 12 V over 10 + 10 Ω = 0,6 A, 6 V each.
        let s = c.solution();
        let l1 = edge_of(&c, Kind::Bulb, 0);
        let a = edge_of(&c, Kind::Ammeter, 0);
        assert!(close(s.current(a).abs(), 0.6) && close(s.voltage(l1).abs(), 6.0), "{} {}", s.current(a), s.voltage(l1));
        assert!(!s.short);
        assert_eq!(c.summary(&s)[1], "Eşdeğer direnç: R_{eş} = 20 Ω");
        // Opening the switch stops everything.
        c.tool = 4;
        c.tap_edge(edge_of(&c, Kind::Switch, 0));
        assert!(c.solution().amps.iter().all(|i| i.abs() < 1e-6));
        assert!(!c.is_animating(0.0));
        // Paralel: each bulb 1,2 A, main 2,4 A, Req 5 Ω.
        c.load_preset(1);
        let s = c.solution();
        assert!(close(s.current(edge_of(&c, Kind::Ammeter, 0)).abs(), 2.4));
        assert!(close(s.current(edge_of(&c, Kind::Bulb, 1)).abs(), 1.2));
        // Karışık: 10 + 10∥10 = 15 Ω → 0,8 A; voltmeter over R1 reads 8 V,
        // each bulb 0,4 A.
        c.load_preset(2);
        let s = c.solution();
        assert!(close(s.voltage(edge_of(&c, Kind::Voltmeter, 0)).abs(), 8.0), "{}", s.voltage(edge_of(&c, Kind::Voltmeter, 0)));
        assert!(close(s.current(edge_of(&c, Kind::Bulb, 0)).abs(), 0.4));
        assert!(close(s.current(edge_of(&c, Kind::Cell, 0)).abs(), 0.8));
    }

    #[test]
    fn current_directions_and_shorts() {
        let mut c = Circuit::new(SCREEN);
        c.load_preset(3);
        // A lone loop: cell on the left side, + on top, resistor on top.
        c.put((1, 1), (1, 2), Kind::Cell);
        c.put((1, 1), (2, 1), Kind::Resistor);
        c.wire(&[(2, 1), (2, 2), (1, 2)]);
        let s = c.solution();
        let r = edge_between((1, 1), (2, 1)).unwrap();
        let cell = edge_between((1, 1), (1, 2)).unwrap();
        // Out of + (top-left) through R to the right; up through the cell.
        assert!(close(s.current(r), 1.2) && close(s.current(cell), -1.2), "{} {}", s.current(r), s.current(cell));
        // Flipped: everything reverses.
        c.elements[cell].as_mut().unwrap().flipped = true;
        assert!(close(c.solution().current(r), -1.2));
        // A wire across the resistor: short circuit, and a bulb shorted by
        // a wire in the series preset goes dark.
        c.put((1, 1), (1, 0), Kind::Wire);
        c.wire(&[(1, 0), (2, 0), (2, 1)]);
        assert!(c.solution().short);
        c.load_preset(0);
        c.wire(&[(2, 1), (2, 0), (3, 0), (3, 1)]);
        let s = c.solution();
        assert!(s.current(edge_of(&c, Kind::Bulb, 0)).abs() < 1e-2 && close(s.current(edge_of(&c, Kind::Bulb, 1)).abs(), 1.2));
    }

    #[test]
    fn panel_tools_paint_and_stamp() {
        let mut c = Circuit::new(SCREEN);
        assert!(c.position.y + c.panel_height() <= SCREEN.y - 75.0);
        assert!(Circuit::result_h() > 180.0, "{}", Circuit::result_h());
        let ctr = |r: (Vec2, Vec2)| r.0 + r.1 / 2.0;
        c.press_at(ctr(c.preset_rect(3)));
        assert!(c.is_empty());
        // Drag a wire from (0,0) to (2,0) then down to (2,1).
        c.press_at(ctr(c.tool_rect(0)));
        let o = c.grid_origin();
        assert_eq!(c.press_at(Circuit::pt(o, (0, 0)) + Vec2::new(4.0, 3.0)), CircuitPress::Paint);
        for p in [(1, 0), (2, 0), (2, 1)] {
            c.paint_to(Circuit::pt(o, p) + Vec2::new(-5.0, 6.0));
        }
        assert_eq!(c.elements.iter().flatten().count(), 3);
        // Tap the middle of (2,1)-(3,1) with the bulb tool: placed and
        // selected, the slider sets its resistance.
        c.press_at(ctr(c.tool_rect(2)));
        c.press_at((Circuit::pt(o, (2, 1)) + Circuit::pt(o, (3, 1))) / 2.0 + Vec2::new(0.0, 8.0));
        let e = edge_between((2, 1), (3, 1)).unwrap();
        assert_eq!(c.elements[e].unwrap().kind, Kind::Bulb);
        assert_eq!(c.selected, Some(e));
        let (_, x1, y) = c.slider_track();
        assert_eq!(c.press_at(Vec2::new(x1 + 10.0, y)), CircuitPress::Slider);
        assert_eq!(c.elements[e].unwrap().value, 100.0);
        // Eraser drag removes the wire.
        c.press_at(ctr(c.tool_rect(7)));
        assert_eq!(c.press_at(Circuit::pt(o, (0, 0))), CircuitPress::Paint);
        c.paint_to(Circuit::pt(o, (1, 0)));
        assert!(c.elements[edge_between((0, 0), (1, 0)).unwrap()].is_none());
        // Stamps fit on screen and every label has ink glyphs.
        for k in 0..3 {
            c.load_preset(k);
            let lines = c.stamp_lines(SCREEN, [1.0; 4], 0.0);
            assert!(lines.len() > 20);
            for l in &lines {
                for p in &l.points {
                    assert!(p.x >= 0.0 && p.x <= SCREEN.x && p.y >= 0.0 && p.y <= SCREEN.y - 70.0, "{k}: {p:?}");
                }
            }
            assert_eq!(marks::missing_glyphs(&c.scene(Vec2::new(500.0, 300.0), 0.0, true)), vec![]);
        }
    }
}
