//! Draggable floating text box panel ("Metin Kutusu") — an on-screen
//! keyboard plus a live text display, same UI-panel pattern as
//! `calculator::Calculator` (swallows touches completely). Interactive
//! flat panels have no physical keyboard, so unlike a desktop app this
//! can't rely on the OS to supply one: the keyboard is drawn and hit-
//! tested by tahta itself, uppercase-only (`font5x7` only defines capitals
//! — halves the glyph table without losing legibility on a big panel) and
//! including the six Turkish letters outside plain ASCII.

use glam::Vec2;

use crate::font5x7::push_char;
use crate::stroke::{push_circle, push_line, push_rect, Vertex};

const BTN: f32 = 52.0;
const GAP: f32 = 6.0;
const HEADER_H: f32 = 36.0;
const DISPLAY_H: f32 = 56.0;
const PADDING: f32 = 12.0;
const MAX_CHARS: usize = 28;

const COLOR_PANEL_BG: [f32; 4] = [0.12, 0.13, 0.17, 0.95];
const COLOR_HEADER: [f32; 4] = [0.18, 0.19, 0.24, 1.0];
const COLOR_DISPLAY_BG: [f32; 4] = [0.05, 0.06, 0.08, 1.0];
const COLOR_DISPLAY_TEXT: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_KEY_IDLE: [f32; 4] = [0.22, 0.24, 0.30, 1.0];
const COLOR_KEY_SPECIAL: [f32; 4] = [0.23, 0.51, 0.96, 1.0];
const COLOR_KEY_CLEAR: [f32; 4] = [0.55, 0.30, 0.30, 1.0];
const COLOR_GLYPH: [f32; 4] = [0.95, 0.95, 0.97, 1.0];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Key {
    Char(char),
    Space,
    Backspace,
    Clear,
}

/// One keyboard row as `(key, width_in_units)` pairs — a unit is one
/// normal letter key's width, so the wide keys in the bottom row line up
/// with the 10-unit-wide rows above them.
type Row = &'static [(Key, f32)];

const ROW_DIGITS: Row = &[
    (Key::Char('1'), 1.0), (Key::Char('2'), 1.0), (Key::Char('3'), 1.0), (Key::Char('4'), 1.0), (Key::Char('5'), 1.0),
    (Key::Char('6'), 1.0), (Key::Char('7'), 1.0), (Key::Char('8'), 1.0), (Key::Char('9'), 1.0), (Key::Char('0'), 1.0),
];
const ROW_Q: Row = &[
    (Key::Char('Q'), 1.0), (Key::Char('W'), 1.0), (Key::Char('E'), 1.0), (Key::Char('R'), 1.0), (Key::Char('T'), 1.0),
    (Key::Char('Y'), 1.0), (Key::Char('U'), 1.0), (Key::Char('I'), 1.0), (Key::Char('O'), 1.0), (Key::Char('P'), 1.0),
];
const ROW_A: Row = &[
    (Key::Char('A'), 1.0), (Key::Char('S'), 1.0), (Key::Char('D'), 1.0), (Key::Char('F'), 1.0), (Key::Char('G'), 1.0),
    (Key::Char('H'), 1.0), (Key::Char('J'), 1.0), (Key::Char('K'), 1.0), (Key::Char('L'), 1.0),
];
const ROW_Z: Row = &[
    (Key::Char('Z'), 1.0), (Key::Char('X'), 1.0), (Key::Char('C'), 1.0), (Key::Char('V'), 1.0), (Key::Char('B'), 1.0),
    (Key::Char('N'), 1.0), (Key::Char('M'), 1.0),
];
const ROW_TR: Row = &[
    (Key::Char('Ç'), 1.0), (Key::Char('Ğ'), 1.0), (Key::Char('İ'), 1.0), (Key::Char('Ö'), 1.0), (Key::Char('Ş'), 1.0), (Key::Char('Ü'), 1.0),
];
const ROW_ACTIONS: Row = &[(Key::Space, 4.0), (Key::Backspace, 3.0), (Key::Clear, 3.0)];

const ROWS: &[Row] = &[ROW_DIGITS, ROW_Q, ROW_A, ROW_Z, ROW_TR, ROW_ACTIONS];
const MAX_UNITS: f32 = 10.0; // widest row (digits/QWERTY), everything else lines up against it

pub struct TextBox {
    pub position: Vec2,
    text: String,
}

impl TextBox {
    pub fn new(position: Vec2) -> Self {
        Self { position, text: String::new() }
    }

    fn panel_width(&self) -> f32 {
        PADDING * 2.0 + MAX_UNITS * BTN + (MAX_UNITS - 1.0) * GAP
    }

    fn panel_height(&self) -> f32 {
        HEADER_H + PADDING + DISPLAY_H + GAP + ROWS.len() as f32 * BTN + (ROWS.len() as f32 - 1.0) * GAP + PADDING
    }

    fn header_rect(&self) -> (Vec2, Vec2) {
        (self.position, Vec2::new(self.panel_width(), HEADER_H))
    }

    fn display_rect(&self) -> (Vec2, Vec2) {
        let top_left = self.position + Vec2::new(PADDING, HEADER_H + PADDING);
        (top_left, Vec2::new(self.panel_width() - PADDING * 2.0, DISPLAY_H))
    }

    fn keys_top(&self) -> f32 {
        self.position.y + HEADER_H + PADDING + DISPLAY_H + GAP
    }

    /// A unit's pixel width — spanning `units` of them (with the internal
    /// gaps between) via the standard "N cells plus N-1 gaps" formula.
    fn unit_span(units: f32) -> f32 {
        units * BTN + (units - 1.0).max(0.0) * GAP
    }

    fn key_rect(&self, row_index: usize, key_index: usize) -> (Vec2, Vec2) {
        let row = ROWS[row_index];
        let mut x = self.position.x + PADDING;
        for &(_, units) in &row[..key_index] {
            x += Self::unit_span(units) + GAP;
        }
        let y = self.keys_top() + row_index as f32 * (BTN + GAP);
        (Vec2::new(x, y), Vec2::new(Self::unit_span(row[key_index].1), BTN))
    }

    pub fn contains(&self, p: Vec2) -> bool {
        rect_contains((self.position, Vec2::new(self.panel_width(), self.panel_height())), p)
    }

    pub fn is_on_header(&self, p: Vec2) -> bool {
        rect_contains(self.header_rect(), p)
    }

    pub fn drag_to(&mut self, p: Vec2, grab_offset: Vec2) {
        self.position = p - grab_offset;
    }

    pub fn press_at(&mut self, p: Vec2) {
        for (row_index, row) in ROWS.iter().enumerate() {
            for key_index in 0..row.len() {
                if rect_contains(self.key_rect(row_index, key_index), p) {
                    self.press_key(row[key_index].0);
                    return;
                }
            }
        }
    }

    fn press_key(&mut self, key: Key) {
        match key {
            Key::Char(c) => {
                if self.text.chars().count() < MAX_CHARS {
                    self.text.push(c);
                }
            }
            Key::Space => {
                if self.text.chars().count() < MAX_CHARS {
                    self.text.push(' ');
                }
            }
            Key::Backspace => {
                self.text.pop();
            }
            Key::Clear => self.text.clear(),
        }
    }

    pub fn render(&self, out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) {
        push_rect(self.position, Vec2::new(self.panel_width(), self.panel_height()), COLOR_PANEL_BG, out_vertices, out_indices);
        let (header_pos, header_size) = self.header_rect();
        push_rect(header_pos, header_size, COLOR_HEADER, out_vertices, out_indices);
        let grip_center = header_pos + header_size / 2.0;
        for i in -1..=1 {
            let x = grip_center.x + i as f32 * 10.0;
            push_circle(Vec2::new(x, grip_center.y), 2.0, COLOR_GLYPH, 8, out_vertices, out_indices);
        }

        let (disp_pos, disp_size) = self.display_rect();
        push_rect(disp_pos, disp_size, COLOR_DISPLAY_BG, out_vertices, out_indices);
        let cell = Vec2::new(disp_size.y * 0.32, disp_size.y * 0.55);
        let text_pos = disp_pos + Vec2::new(10.0, disp_size.y * 0.22);
        let width = crate::font5x7::push_text(&self.text, text_pos, cell, COLOR_DISPLAY_TEXT, out_vertices, out_indices);
        // A static caret right after the last character — always-editable
        // hint; no blink state needed for a first pass.
        push_line(
            text_pos + Vec2::new(width + 2.0, -2.0),
            text_pos + Vec2::new(width + 2.0, cell.y + 2.0),
            2.0,
            COLOR_DISPLAY_TEXT,
            out_vertices,
            out_indices,
        );

        for (row_index, row) in ROWS.iter().enumerate() {
            for (key_index, &(key, _)) in row.iter().enumerate() {
                let (pos, size) = self.key_rect(row_index, key_index);
                let color = match key {
                    Key::Space | Key::Backspace => COLOR_KEY_SPECIAL,
                    Key::Clear => COLOR_KEY_CLEAR,
                    Key::Char(_) => COLOR_KEY_IDLE,
                };
                push_rect(pos, size, color, out_vertices, out_indices);
                draw_key_glyph(key, pos, size, out_vertices, out_indices);
            }
        }
    }
}

fn draw_key_glyph(key: Key, top_left: Vec2, size: Vec2, out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) {
    let center = top_left + size / 2.0;
    match key {
        Key::Char(c) => {
            let cell = Vec2::new(size.x * 0.36, size.y * 0.6);
            let pos = center - cell / 2.0;
            push_char(c, pos, cell, COLOR_GLYPH, out_vertices, out_indices);
        }
        Key::Space => {
            push_line(center + Vec2::new(-size.x * 0.28, 0.0), center + Vec2::new(size.x * 0.28, 0.0), 4.0, COLOR_GLYPH, out_vertices, out_indices);
        }
        Key::Backspace => {
            let r = size.x * 0.16;
            let tip = center + Vec2::new(-size.x * 0.22, 0.0);
            push_line(tip, tip + Vec2::new(r, -r), 3.0, COLOR_GLYPH, out_vertices, out_indices);
            push_line(tip, tip + Vec2::new(r, r), 3.0, COLOR_GLYPH, out_vertices, out_indices);
            push_line(tip, tip + Vec2::new(size.x * 0.4, 0.0), 3.0, COLOR_GLYPH, out_vertices, out_indices);
        }
        Key::Clear => {
            let r = size.x * 0.18;
            for i in 0..20 {
                if i % 2 == 0 {
                    continue;
                }
                let theta = i as f32 / 20.0 * std::f32::consts::TAU;
                let theta1 = (i + 1) as f32 / 20.0 * std::f32::consts::TAU;
                let p0 = center + Vec2::new(theta.cos(), theta.sin()) * r;
                let p1 = center + Vec2::new(theta1.cos(), theta1.sin()) * r;
                push_line(p0, p1, 2.5, COLOR_GLYPH, out_vertices, out_indices);
            }
            push_line(center + Vec2::new(-r * 0.7, -r * 0.7), center + Vec2::new(r * 0.7, r * 0.7), 2.5, COLOR_GLYPH, out_vertices, out_indices);
        }
    }
}

fn rect_contains((top_left, size): (Vec2, Vec2), point: Vec2) -> bool {
    point.x >= top_left.x && point.x <= top_left.x + size.x && point.y >= top_left.y && point.y <= top_left.y + size.y
}
