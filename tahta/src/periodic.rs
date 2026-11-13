//! Draggable "Periyodik tablo" panel for chemistry lessons:
//!
//! - all 118 elements, colored by class (alkali metal … noble gas), by
//!   the MEB four-way split (metal / ametal / yarı metal / soy gaz) or by
//!   s/p/d/f block; tapping a legend chip highlights that group;
//! - the tapped element shown large in the table's empty top middle
//!   (name, class, short configuration, shells) and in a card beside the
//!   table: atomic number and mass, group (also as 1A…8B), period, block,
//!   state at room temperature, electronegativity, the full electron
//!   configuration and a Bohr (shell) model of the atom;
//! - "Tahtaya bas" stamps the element as ink: nuclide notation (mass
//!   number, atomic number, symbol), the Bohr model, the configuration
//!   and the shell distribution, like `solid.rs`/`graph.rs`.
//!
//! Data and chemistry live in `elements.rs`.

use glam::Vec2;

use crate::elements::{self as el, Class, Kind};
use crate::font_atlas::{FontAtlas, GlyphVertex};
use crate::ink::{densify, stroke_text, stroke_text_width, InkLine};
use crate::stroke::{push_circle, push_line, push_rect, Vertex};

const HEADER_H: f32 = 36.0;
const PADDING: f32 = 12.0;
const CELL: f32 = 50.0;
const PITCH: f32 = 52.0;
/// Gap between the main table and the lanthanide/actinide rows.
const F_GAP: f32 = 14.0;
const TABLE_W: f32 = 18.0 * PITCH - (PITCH - CELL);
const TABLE_H: f32 = 9.0 * PITCH - (PITCH - CELL) + F_GAP;
const CARD_W: f32 = 400.0;
const BTN_H: f32 = 44.0;
const CHIP_H: f32 = 40.0;
const GAP: f32 = 6.0;
const MODE_W: f32 = 170.0;

const COLOR_PANEL_BG: [f32; 4] = [0.12, 0.13, 0.17, 0.96];
const COLOR_HEADER: [f32; 4] = [0.18, 0.19, 0.24, 1.0];
const COLOR_CARD_BG: [f32; 4] = [0.06, 0.07, 0.09, 1.0];
const COLOR_TEXT: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_TEXT_DIM: [f32; 4] = [0.95, 0.95, 0.97, 0.6];
const COLOR_KEY_IDLE: [f32; 4] = [0.22, 0.24, 0.30, 1.0];
const COLOR_KEY_ACTIVE: [f32; 4] = [0.23, 0.51, 0.96, 1.0];
const COLOR_KEY_STAMP: [f32; 4] = [0.98, 0.55, 0.15, 1.0];
const COLOR_SHELL: [f32; 4] = [0.95, 0.95, 0.97, 0.55];
const COLOR_ELECTRON: [f32; 4] = [0.45, 0.75, 1.0, 1.0];
const COLOR_NUCLEUS: [f32; 4] = [0.85, 0.35, 0.30, 1.0];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Classes,
    Kinds,
    Blocks,
}

const MODES: [(Mode, &str); 3] = [(Mode::Classes, "Sınıflar"), (Mode::Kinds, "Metal / ametal"), (Mode::Blocks, "Bloklar")];

const BLOCKS: [(char, &str); 4] = [('s', "s bloku"), ('p', "p bloku"), ('d', "d bloku"), ('f', "f bloku")];

fn class_color(c: Class) -> [f32; 3] {
    match c {
        Class::Alkali => [0.78, 0.30, 0.30],
        Class::AlkalineEarth => [0.82, 0.50, 0.22],
        Class::Transition => [0.66, 0.56, 0.20],
        Class::PostTransition => [0.40, 0.58, 0.36],
        Class::Metalloid => [0.22, 0.58, 0.55],
        Class::Nonmetal => [0.24, 0.46, 0.78],
        Class::Halogen => [0.40, 0.38, 0.82],
        Class::NobleGas => [0.60, 0.36, 0.78],
        Class::Lanthanide => [0.72, 0.38, 0.58],
        Class::Actinide => [0.56, 0.33, 0.43],
    }
}

fn kind_color(k: Kind) -> [f32; 3] {
    match k {
        Kind::Metal => [0.66, 0.50, 0.24],
        Kind::Nonmetal => [0.24, 0.46, 0.78],
        Kind::Metalloid => [0.22, 0.58, 0.55],
        Kind::NobleGas => [0.60, 0.36, 0.78],
    }
}

fn block_color(b: char) -> [f32; 3] {
    match b {
        's' => [0.78, 0.32, 0.32],
        'p' => [0.74, 0.60, 0.22],
        'd' => [0.28, 0.52, 0.80],
        _ => [0.36, 0.62, 0.38],
    }
}

/// What a touch on the panel turned into.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PeriodicPress {
    Move,
    Close,
    Stamp,
    Other,
}

pub struct Periodic {
    pub position: Vec2,
    selected: u32,
    mode: Mode,
    /// Legend chip (index into the current mode's legend) whose group is
    /// highlighted, the rest dimmed.
    highlight: Option<usize>,
}

impl Periodic {
    pub fn new(screen_size: Vec2) -> Self {
        let mut p = Self { position: Vec2::ZERO, selected: 8, mode: Mode::Classes, highlight: None };
        let size = Vec2::new(p.panel_width(), p.panel_height());
        p.position = Vec2::new((screen_size.x - size.x) / 2.0, ((screen_size.y - 120.0 - size.y) / 2.0).max(12.0));
        p
    }

    fn panel_width(&self) -> f32 {
        PADDING * 2.0 + TABLE_W + PADDING + CARD_W
    }

    fn panel_height(&self) -> f32 {
        HEADER_H + PADDING + TABLE_H + 10.0 + BTN_H + GAP + CHIP_H + PADDING
    }

    fn table_origin(&self) -> Vec2 {
        self.position + Vec2::new(PADDING, HEADER_H + PADDING)
    }

    fn header_rect(&self) -> (Vec2, Vec2) {
        (self.position, Vec2::new(self.panel_width(), HEADER_H))
    }

    fn close_rect(&self) -> (Vec2, Vec2) {
        (self.position + Vec2::new(self.panel_width() - HEADER_H, 0.0), Vec2::splat(HEADER_H))
    }

    fn card_rect(&self) -> (Vec2, Vec2) {
        (self.table_origin() + Vec2::new(TABLE_W + PADDING, 0.0), Vec2::new(CARD_W, TABLE_H))
    }

    fn cell_rect_at(&self, row: usize, col: usize) -> (Vec2, Vec2) {
        let y = row as f32 * PITCH + if row >= 7 { F_GAP } else { 0.0 };
        (self.table_origin() + Vec2::new(col as f32 * PITCH, y), Vec2::splat(CELL))
    }

    fn cell_rect(&self, z: u32) -> (Vec2, Vec2) {
        let (row, col) = el::position(z);
        self.cell_rect_at(row, col)
    }

    /// The empty top middle of the table (groups 3-12, periods 1-3).
    fn spotlight_rect(&self) -> (Vec2, Vec2) {
        let (tl, _) = self.cell_rect_at(0, 2);
        let (br, size) = self.cell_rect_at(2, 11);
        (tl + Vec2::new(4.0, 0.0), br + size - tl - Vec2::new(8.0, 0.0))
    }

    fn modes_top(&self) -> f32 {
        self.table_origin().y + TABLE_H + 10.0
    }

    fn mode_rect(&self, i: usize) -> (Vec2, Vec2) {
        (Vec2::new(self.position.x + PADDING + i as f32 * (MODE_W + GAP), self.modes_top()), Vec2::new(MODE_W, BTN_H))
    }

    fn stamp_rect(&self) -> (Vec2, Vec2) {
        let (card, size) = self.card_rect();
        (Vec2::new(card.x, self.modes_top()), Vec2::new(size.x, BTN_H))
    }

    fn legend(&self) -> Vec<(&'static str, [f32; 3])> {
        match self.mode {
            Mode::Classes => el::CLASSES.iter().map(|&(c, n)| (n, class_color(c))).collect(),
            Mode::Kinds => el::KINDS.iter().map(|&(k, n)| (n, kind_color(k))).collect(),
            Mode::Blocks => BLOCKS.iter().map(|&(b, n)| (n, block_color(b))).collect(),
        }
    }

    fn chip_rect(&self, i: usize, count: usize) -> (Vec2, Vec2) {
        let inner = self.panel_width() - PADDING * 2.0;
        let w = (inner - (count as f32 - 1.0) * GAP) / count as f32;
        (
            Vec2::new(self.position.x + PADDING + i as f32 * (w + GAP), self.modes_top() + BTN_H + GAP),
            Vec2::new(w, CHIP_H),
        )
    }

    /// Index of `z`'s group in the current mode's legend.
    fn legend_index(&self, z: u32) -> usize {
        match self.mode {
            Mode::Classes => el::CLASSES.iter().position(|(c, _)| *c == el::class(z)).unwrap(),
            Mode::Kinds => el::KINDS.iter().position(|(k, _)| *k == el::kind(z)).unwrap(),
            Mode::Blocks => BLOCKS.iter().position(|(b, _)| *b == el::block(z)).unwrap(),
        }
    }

    fn color_of(&self, z: u32) -> [f32; 3] {
        self.legend()[self.legend_index(z)].1
    }

    pub fn contains(&self, p: Vec2) -> bool {
        rect_contains((self.position, Vec2::new(self.panel_width(), self.panel_height())), p)
    }

    pub fn drag_to(&mut self, p: Vec2, grab_offset: Vec2) {
        self.position = p - grab_offset;
    }

    pub fn press_at(&mut self, p: Vec2) -> PeriodicPress {
        if rect_contains(self.close_rect(), p) {
            return PeriodicPress::Close;
        }
        if rect_contains(self.header_rect(), p) {
            return PeriodicPress::Move;
        }
        if rect_contains(self.stamp_rect(), p) {
            return PeriodicPress::Stamp;
        }
        if let Some(z) = (1..=118).find(|&z| rect_contains(self.cell_rect(z), p)) {
            self.selected = z;
            return PeriodicPress::Other;
        }
        for (i, &(mode, _)) in MODES.iter().enumerate() {
            if rect_contains(self.mode_rect(i), p) {
                if mode != self.mode {
                    self.mode = mode;
                    self.highlight = None;
                }
                return PeriodicPress::Other;
            }
        }
        let count = self.legend().len();
        if let Some(i) = (0..count).find(|&i| rect_contains(self.chip_rect(i, count), p)) {
            self.highlight = if self.highlight == Some(i) { None } else { Some(i) };
        }
        PeriodicPress::Other
    }

    pub fn selected(&self) -> u32 {
        self.selected
    }

    /// The card's property lines (label, value).
    fn properties(&self) -> Vec<(&'static str, String)> {
        let z = self.selected;
        let group = match el::group(z) {
            Some(g) => format!("{g} ({}) · {}. periyot", el::group_ab(g), el::period(z)),
            None => format!("{} · {}. periyot", if z < 89 { "Lantanitler" } else { "Aktinitler" }, el::period(z)),
        };
        vec![
            ("Atom numarası", z.to_string()),
            ("Atom kütlesi", el::mass_text(z)),
            ("Grup", group),
            ("Blok", el::block(z).to_string()),
            ("Hal (25 °C)", el::phase(z).unwrap_or("Bilinmiyor").to_string()),
            ("Elektronegatiflik", el::electronegativity_text(z).unwrap_or_else(|| "—".to_string())),
            ("Proton / nötron", format!("{} / {}", z, el::mass_number(z) - z)),
        ]
    }

    // --- Ink for "Tahtaya bas" ----------------------------------------------

    /// The selected element as ink, centered on the screen above the
    /// toolbar: nuclide notation and the Bohr model side by side, the
    /// configuration and shell distribution under them.
    pub fn stamp_lines(&self, screen_size: Vec2, ink: [f32; 4]) -> Vec<InkLine> {
        let z = self.selected;
        let mut lines = Vec::new();
        let mut add = |points: Vec<Vec2>, width: f32| {
            if points.len() >= 2 {
                lines.push(InkLine { points: densify(&points, 3.0), color: ink, width });
            }
        };

        let sym = el::symbol(z);
        let (h_sym, h_small) = (96.0, 34.0);
        let mass = el::mass_number(z).to_string();
        let num = z.to_string();
        let prefix_w = stroke_text_width(&mass, h_small).max(stroke_text_width(&num, h_small)) + 12.0;
        let notation_w = prefix_w + stroke_text_width(sym, h_sym);
        let bohr_r = 170.0;
        let gap = 70.0;
        let width = notation_w + gap + 2.0 * bohr_r;
        let config = if z <= 36 { el::configuration(z) } else { el::short_configuration(z).map_or_else(|| el::configuration(z), |s| s.1) };
        let config_h = 30.0;
        let height = 2.0 * bohr_r + 30.0 + config_h * 3.2;
        let origin = ((screen_size - Vec2::new(0.0, 110.0) - Vec2::new(width, height)) / 2.0).max(Vec2::splat(20.0));

        // Nuclide notation: mass number top left, atomic number bottom
        // left, right-aligned against the symbol.
        let sym_top = origin + Vec2::new(prefix_w, bohr_r - h_sym / 2.0);
        let right = sym_top.x - 10.0;
        for g in stroke_text(&mass, Vec2::new(right - stroke_text_width(&mass, h_small), sym_top.y - h_small * 0.35), h_small) {
            add(g, 2.2);
        }
        for g in stroke_text(&num, Vec2::new(right - stroke_text_width(&num, h_small), sym_top.y + h_sym - h_small * 0.65), h_small) {
            add(g, 2.2);
        }
        for g in stroke_text(sym, sym_top, h_sym) {
            add(g, 3.5);
        }

        // Bohr model.
        let center = origin + Vec2::new(notation_w + gap + bohr_r, bohr_r);
        let shells = el::shells(z);
        let (nucleus_r, rings) = bohr_rings(bohr_r, shells.len());
        add(circle_points(center, nucleus_r, 40), 2.5);
        let (p_text, n_text) = (format!("{z}p"), format!("{}n", el::mass_number(z) - z));
        let th = (nucleus_r * 0.55).min(22.0);
        for (k, t) in [p_text, n_text].iter().enumerate() {
            let w = stroke_text_width(t, th);
            let y = center.y - th * 1.1 + k as f32 * th * 1.25;
            for g in stroke_text(t, Vec2::new(center.x - w / 2.0, y), th) {
                add(g, 1.8);
            }
        }
        for (i, &count) in shells.iter().enumerate() {
            let r = rings[i];
            add(circle_points(center, r, 72), 1.6);
            let dot = if count > 18 { 2.5 } else { 4.0 };
            for p in electron_positions(center, r, count, i) {
                add(circle_points(p, dot, 10), dot * 1.2);
            }
        }

        // Configuration (superscript counts raised and smaller) and the
        // shell distribution.
        let mut y = origin.y + 2.0 * bohr_r + 30.0;
        let mut x = origin.x;
        let max_x = origin.x + width.max(700.0);
        if let Some((core, _)) = el::short_configuration(z).filter(|_| z > 36) {
            let t = format!("[{}]", el::symbol(core));
            for g in stroke_text(&t, Vec2::new(x, y), config_h) {
                add(g, 2.2);
            }
            x += stroke_text_width(&t, config_h) + config_h * 0.5;
        }
        for (n, l, k) in config {
            let base = format!("{n}{}", el::subshell_letter(l));
            let sup = k.to_string();
            let w = stroke_text_width(&base, config_h) + 3.0 + stroke_text_width(&sup, config_h * 0.6);
            if x + w > max_x {
                x = origin.x;
                y += config_h * 1.6;
            }
            for g in stroke_text(&base, Vec2::new(x, y), config_h) {
                add(g, 2.2);
            }
            let sx = x + stroke_text_width(&base, config_h) + 3.0;
            for g in stroke_text(&sup, Vec2::new(sx, y - config_h * 0.3), config_h * 0.6) {
                add(g, 1.8);
            }
            x += w + config_h * 0.55;
        }
        y += config_h * 1.6;
        let shell_text = shells.iter().map(|c| format!("{c})")).collect::<Vec<_>>().join(" ");
        for g in stroke_text(&shell_text, Vec2::new(origin.x, y), config_h) {
            add(g, 2.2);
        }
        lines
    }

    // --- Drawing -------------------------------------------------------------

    pub fn render(
        &self,
        font_atlas: &FontAtlas,
        out_vertices: &mut Vec<Vertex>,
        out_indices: &mut Vec<u32>,
        glyph_vertices: &mut Vec<GlyphVertex>,
        glyph_indices: &mut Vec<u32>,
    ) {
        let size = Vec2::new(self.panel_width(), self.panel_height());
        push_rect(self.position, size, COLOR_PANEL_BG, out_vertices, out_indices);
        let (header_pos, header_size) = self.header_rect();
        push_rect(header_pos, header_size, COLOR_HEADER, out_vertices, out_indices);
        font_atlas.push_text("Periyodik tablo", header_pos + Vec2::new(PADDING, 10.0), 16.0, COLOR_TEXT, glyph_vertices, glyph_indices);
        let grip = header_pos + header_size / 2.0;
        for i in -1..=1 {
            push_circle(Vec2::new(grip.x + i as f32 * 10.0, grip.y), 2.0, COLOR_TEXT, 8, out_vertices, out_indices);
        }
        let (close_pos, close_size) = self.close_rect();
        let c = close_pos + close_size / 2.0;
        let r = close_size.x * 0.2;
        push_line(c - Vec2::splat(r), c + Vec2::splat(r), 2.5, COLOR_TEXT, out_vertices, out_indices);
        push_line(c + Vec2::new(-r, r), c + Vec2::new(r, -r), 2.5, COLOR_TEXT, out_vertices, out_indices);

        // Group numbers above the columns would crowd the 50 px cells;
        // the card names the group instead.
        for z in 1..=118 {
            let (pos, size) = self.cell_rect(z);
            let dim = self.highlight.is_some_and(|h| h != self.legend_index(z));
            let [r, g, b] = self.color_of(z);
            let alpha = if dim { 0.18 } else { 1.0 };
            push_rect(pos, size, [r, g, b, alpha], out_vertices, out_indices);
            let text = if dim { COLOR_TEXT_DIM } else { COLOR_TEXT };
            font_atlas.push_text(&z.to_string(), pos + Vec2::new(4.0, 3.0), 11.0, text, glyph_vertices, glyph_indices);
            let sym = el::symbol(z);
            let w = font_atlas.measure(sym, 20.0);
            font_atlas.push_text(sym, pos + Vec2::new((size.x - w) / 2.0, 20.0), 20.0, text, glyph_vertices, glyph_indices);
        }
        // Placeholders where the f rows belong.
        for (row, label, z) in [(5, "57–71", 57), (6, "89–103", 89)] {
            let (pos, size) = self.cell_rect_at(row, 2);
            let [r, g, b] = self.color_of(z);
            let dim = self.highlight.is_some_and(|h| h != self.legend_index(z));
            push_rect(pos, size, [r, g, b, if dim { 0.12 } else { 0.45 }], out_vertices, out_indices);
            let w = font_atlas.measure(label, 11.0);
            font_atlas.push_text(label, pos + Vec2::new((size.x - w) / 2.0, 19.0), 11.0, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);
        }
        // Selected cell frame.
        let (pos, size) = self.cell_rect(self.selected);
        for (a, b) in frame_segments(pos - Vec2::splat(2.0), size + Vec2::splat(4.0)) {
            push_line(a, b, 3.0, COLOR_TEXT, out_vertices, out_indices);
        }

        self.render_spotlight(font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
        self.render_card(font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);

        for (i, &(mode, label)) in MODES.iter().enumerate() {
            let color = if mode == self.mode { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE };
            button(self.mode_rect(i), label, color, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
        }
        button(self.stamp_rect(), "Tahtaya bas", COLOR_KEY_STAMP, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);

        let legend = self.legend();
        for (i, &(label, [r, g, b])) in legend.iter().enumerate() {
            let (pos, size) = self.chip_rect(i, legend.len());
            let on = self.highlight == Some(i);
            push_rect(pos, size, if on { [r, g, b, 1.0] } else { COLOR_KEY_IDLE }, out_vertices, out_indices);
            push_rect(pos + Vec2::new(8.0, 12.0), Vec2::splat(16.0), [r, g, b, 1.0], out_vertices, out_indices);
            let h = 14.0;
            let text = crate::font_atlas::fit_text(label, size.x - 36.0, false, |t| font_atlas.measure(t, h));
            font_atlas.push_text(&text, pos + Vec2::new(30.0, (size.y - h) / 2.0), h, COLOR_TEXT, glyph_vertices, glyph_indices);
        }
    }

    fn render_spotlight(
        &self,
        font_atlas: &FontAtlas,
        out_vertices: &mut Vec<Vertex>,
        out_indices: &mut Vec<u32>,
        glyph_vertices: &mut Vec<GlyphVertex>,
        glyph_indices: &mut Vec<u32>,
    ) {
        let z = self.selected;
        let (pos, size) = self.spotlight_rect();
        let tile = size.y;
        let [r, g, b] = self.color_of(z);
        push_rect(pos, Vec2::splat(tile), [r, g, b, 1.0], out_vertices, out_indices);
        font_atlas.push_text(&z.to_string(), pos + Vec2::new(10.0, 8.0), 22.0, COLOR_TEXT, glyph_vertices, glyph_indices);
        let sym = el::symbol(z);
        let w = font_atlas.measure(sym, 60.0);
        font_atlas.push_text(sym, pos + Vec2::new((tile - w) / 2.0, tile * 0.27), 60.0, COLOR_TEXT, glyph_vertices, glyph_indices);
        let mass = el::mass_text(z);
        let w = font_atlas.measure(&mass, 16.0);
        font_atlas.push_text(&mass, pos + Vec2::new((tile - w) / 2.0, tile - 26.0), 16.0, COLOR_TEXT, glyph_vertices, glyph_indices);

        let x = pos.x + tile + 16.0;
        let max_w = pos.x + size.x - x;
        let mut y = pos.y + 4.0;
        font_atlas.push_text(el::name(z), Vec2::new(x, y), 28.0, COLOR_TEXT, glyph_vertices, glyph_indices);
        y += 40.0;
        let class = el::class_name(el::class(z));
        let kind = el::KINDS.iter().find(|(k, _)| *k == el::kind(z)).unwrap().1;
        let label = if class == kind { class.to_string() } else { format!("{class} · {kind}") };
        let [cr, cg, cb] = class_color(el::class(z));
        push_rect(Vec2::new(x, y + 2.0), Vec2::new(12.0, 12.0), [cr, cg, cb, 1.0], out_vertices, out_indices);
        font_atlas.push_text(&label, Vec2::new(x + 20.0, y), 16.0, COLOR_TEXT, glyph_vertices, glyph_indices);
        y += 30.0;
        let short = crate::font_atlas::fit_text(&el::short_configuration_text(z), max_w, false, |t| font_atlas.measure(t, 18.0));
        font_atlas.push_text(&short, Vec2::new(x, y), 18.0, COLOR_TEXT, glyph_vertices, glyph_indices);
        y += 30.0;
        let shells = el::shells(z).iter().map(|c| format!("{c})")).collect::<Vec<_>>().join(" ");
        font_atlas.push_text(&format!("Katmanlar: {shells}"), Vec2::new(x, y), 16.0, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);
    }

    fn render_card(
        &self,
        font_atlas: &FontAtlas,
        out_vertices: &mut Vec<Vertex>,
        out_indices: &mut Vec<u32>,
        glyph_vertices: &mut Vec<GlyphVertex>,
        glyph_indices: &mut Vec<u32>,
    ) {
        let z = self.selected;
        let (pos, size) = self.card_rect();
        push_rect(pos, size, COLOR_CARD_BG, out_vertices, out_indices);
        let x = pos.x + 14.0;
        let mut y = pos.y + 12.0;
        let h = 15.0;
        for (label, value) in self.properties() {
            font_atlas.push_text(label, Vec2::new(x, y), h, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);
            font_atlas.push_text(&value, Vec2::new(x + 150.0, y), h, COLOR_TEXT, glyph_vertices, glyph_indices);
            y += 23.0;
        }
        y += 4.0;
        font_atlas.push_text("Elektron dizilimi", Vec2::new(x, y), h, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);
        y += 22.0;
        for line in wrap(&el::configuration_text(z), size.x - 28.0, |t| font_atlas.measure(t, h)) {
            font_atlas.push_text(&line, Vec2::new(x, y), h, COLOR_TEXT, glyph_vertices, glyph_indices);
            y += 21.0;
        }

        // Bohr model in what's left of the card.
        let top = y + 6.0;
        let bottom = pos.y + size.y - 8.0;
        let radius = ((bottom - top) / 2.0).min(size.x / 2.0 - 30.0);
        if radius < 40.0 {
            return;
        }
        let center = Vec2::new(pos.x + size.x / 2.0, top + (bottom - top) / 2.0);
        let shells = el::shells(z);
        let (nucleus_r, rings) = bohr_rings(radius, shells.len());
        push_circle(center, nucleus_r, COLOR_NUCLEUS, 28, out_vertices, out_indices);
        let th = (nucleus_r * 0.5).clamp(9.0, 14.0);
        for (k, t) in [format!("{z}p"), format!("{}n", el::mass_number(z) - z)].iter().enumerate() {
            let w = font_atlas.measure(t, th);
            let ty = center.y - th * 1.05 + k as f32 * th * 1.1;
            font_atlas.push_text(t, Vec2::new(center.x - w / 2.0, ty), th, COLOR_TEXT, glyph_vertices, glyph_indices);
        }
        for (i, &count) in shells.iter().enumerate() {
            let r = rings[i];
            let ring = circle_points(center, r, 64);
            for w in ring.windows(2) {
                push_line(w[0], w[1], 1.2, COLOR_SHELL, out_vertices, out_indices);
            }
            let dot = if count > 18 { 2.2 } else { 3.4 };
            for p in electron_positions(center, r, count, i) {
                push_circle(p, dot, COLOR_ELECTRON, 10, out_vertices, out_indices);
            }
            let label = el::SHELL_NAMES[i];
            font_atlas.push_text(label, center + Vec2::new(r * 0.71 + 3.0, -r * 0.71 - 14.0), 11.0, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);
        }
    }
}

/// Nucleus radius and the shell radii for a Bohr model of `count`
/// shells within `radius`.
pub(crate) fn bohr_rings(radius: f32, count: usize) -> (f32, Vec<f32>) {
    let nucleus = (radius * 0.2).clamp(14.0, 34.0);
    let first = nucleus + (radius - nucleus) / (count as f32 + 0.5);
    let step = if count > 1 { (radius - first) / (count as f32 - 1.0) } else { 0.0 };
    (nucleus, (0..count).map(|i| first + step * i as f32).collect())
}

/// Evenly spread electrons on a ring, each shell turned a little so the
/// dots don't line up radially.
pub(crate) fn electron_positions(center: Vec2, r: f32, count: u32, shell: usize) -> Vec<Vec2> {
    let start = -std::f32::consts::FRAC_PI_2 + shell as f32 * 0.35;
    (0..count)
        .map(|k| {
            let a = start + k as f32 / count as f32 * std::f32::consts::TAU;
            center + Vec2::new(a.cos(), a.sin()) * r
        })
        .collect()
}

pub(crate) fn circle_points(center: Vec2, r: f32, segments: usize) -> Vec<Vec2> {
    (0..=segments)
        .map(|i| {
            let a = i as f32 / segments as f32 * std::f32::consts::TAU;
            center + Vec2::new(a.cos(), a.sin()) * r
        })
        .collect()
}

fn frame_segments(pos: Vec2, size: Vec2) -> [(Vec2, Vec2); 4] {
    let (a, b, c, d) = (pos, pos + Vec2::new(size.x, 0.0), pos + size, pos + Vec2::new(0.0, size.y));
    [(a, b), (b, c), (c, d), (d, a)]
}

/// Word-wraps `text` at spaces to lines no wider than `max_w`.
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

    #[test]
    fn panel_fits_above_the_toolbar() {
        let p = Periodic::new(SCREEN);
        assert!(p.position.x >= 0.0 && p.position.x + p.panel_width() <= SCREEN.x);
        assert!(p.position.y >= 0.0 && p.position.y + p.panel_height() <= SCREEN.y - 75.0, "{}", p.position.y + p.panel_height());
    }

    #[test]
    fn tapping_picks_elements_modes_and_highlights() {
        let mut p = Periodic::new(SCREEN);
        assert_eq!(p.press_at(center(p.cell_rect(26))), PeriodicPress::Other);
        assert_eq!(p.selected(), 26);
        // A placeholder cell selects nothing.
        p.press_at(center(p.cell_rect_at(5, 2)));
        assert_eq!(p.selected(), 26);
        // Metal/ametal mode, highlight "Soy gaz", tap it again to clear.
        p.press_at(center(p.mode_rect(1)));
        assert_eq!(p.mode, Mode::Kinds);
        let soy = center(p.chip_rect(3, 4));
        p.press_at(soy);
        assert_eq!(p.highlight, Some(3));
        assert_eq!(p.legend_index(18), 3);
        p.press_at(soy);
        assert_eq!(p.highlight, None);
        assert_eq!(p.press_at(center(p.stamp_rect())), PeriodicPress::Stamp);
        assert_eq!(p.press_at(center(p.close_rect())), PeriodicPress::Close);
        assert_eq!(p.press_at(p.position + Vec2::new(30.0, 10.0)), PeriodicPress::Move);
    }

    #[test]
    fn stamp_draws_every_glyph_and_stays_on_screen() {
        let mut p = Periodic::new(SCREEN);
        for z in 1..=118 {
            for c in el::symbol(z).chars() {
                assert!(!crate::ink::stroke_glyph(c).is_empty(), "no stroke glyph for {c}");
            }
        }
        for z in [1, 8, 26, 79, 118] {
            p.selected = z;
            let lines = p.stamp_lines(SCREEN, [1.0; 4]);
            assert!(lines.len() > 10);
            for l in &lines {
                for q in &l.points {
                    assert!(q.x >= 0.0 && q.x <= SCREEN.x && q.y >= 0.0 && q.y <= SCREEN.y - 70.0, "{z}: {q:?}");
                }
            }
        }
    }

    #[test]
    fn wrap_breaks_at_spaces() {
        let w = |t: &str| t.chars().count() as f32;
        assert_eq!(wrap("1s² 2s² 2p⁶ 3s²", 8.0, w), vec!["1s² 2s²", "2p⁶ 3s²"]);
    }
}
