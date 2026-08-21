//! Draggable floating calculator panel ("Hesap Makinesi") — a basic
//! four-function calculator, touch-only like every other tool here: drag
//! its header to reposition, tap keys to enter digits/operators, tap `=`
//! to evaluate. The display reuses `digits.rs`'s 7-segment renderer, which
//! only draws `0-9`, `.` and `-` — there's no font for an "Error" message,
//! so a division by zero just resets to `0` instead of showing one.

use glam::Vec2;

use crate::digits::push_number;
use crate::stroke::{push_circle, push_line, push_rect, Vertex};

const BTN: f32 = 64.0;
const GAP: f32 = 8.0;
const COLS: usize = 4;
const ROWS: usize = 4;
const HEADER_H: f32 = 36.0;
const DISPLAY_H: f32 = 64.0;
const PADDING: f32 = 12.0;
const MAX_DIGITS: usize = 10;

const COLOR_PANEL_BG: [f32; 4] = [0.12, 0.13, 0.17, 0.95];
const COLOR_HEADER: [f32; 4] = [0.18, 0.19, 0.24, 1.0];
const COLOR_DISPLAY_BG: [f32; 4] = [0.05, 0.06, 0.08, 1.0];
const COLOR_DISPLAY_DIGIT: [f32; 4] = [0.55, 0.95, 0.65, 1.0];
const COLOR_KEY_IDLE: [f32; 4] = [0.22, 0.24, 0.30, 1.0];
const COLOR_KEY_OP: [f32; 4] = [0.23, 0.51, 0.96, 1.0];
const COLOR_KEY_EQUALS: [f32; 4] = [0.98, 0.55, 0.15, 1.0];
const COLOR_GLYPH: [f32; 4] = [0.95, 0.95, 0.97, 1.0];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum CalcKey {
    Digit(u8),
    Dot,
    Clear,
    Operator(Op),
}

const KEYS: [CalcKey; ROWS * COLS] = [
    CalcKey::Digit(7), CalcKey::Digit(8), CalcKey::Digit(9), CalcKey::Operator(Op::Div),
    CalcKey::Digit(4), CalcKey::Digit(5), CalcKey::Digit(6), CalcKey::Operator(Op::Mul),
    CalcKey::Digit(1), CalcKey::Digit(2), CalcKey::Digit(3), CalcKey::Operator(Op::Sub),
    CalcKey::Clear,    CalcKey::Digit(0), CalcKey::Dot,      CalcKey::Operator(Op::Add),
];

pub struct Calculator {
    pub position: Vec2, // top-left of the panel
    display: String,
    accumulator: Option<f64>,
    pending_op: Option<Op>,
    /// True right after an operator/equals/clear — the next digit starts a
    /// fresh entry instead of appending to the shown value.
    entry_fresh: bool,
}

impl Calculator {
    pub fn new(position: Vec2) -> Self {
        Self { position, display: "0".to_string(), accumulator: None, pending_op: None, entry_fresh: true }
    }

    fn panel_width(&self) -> f32 {
        PADDING * 2.0 + COLS as f32 * BTN + (COLS as f32 - 1.0) * GAP
    }

    fn panel_height(&self) -> f32 {
        HEADER_H + PADDING + DISPLAY_H + GAP + ROWS as f32 * BTN + (ROWS as f32 - 1.0) * GAP + GAP + BTN + PADDING
    }

    fn header_rect(&self) -> (Vec2, Vec2) {
        (self.position, Vec2::new(self.panel_width(), HEADER_H))
    }

    fn display_rect(&self) -> (Vec2, Vec2) {
        let top_left = self.position + Vec2::new(PADDING, HEADER_H + PADDING);
        (top_left, Vec2::new(self.panel_width() - PADDING * 2.0, DISPLAY_H))
    }

    fn grid_top(&self) -> f32 {
        self.position.y + HEADER_H + PADDING + DISPLAY_H + GAP
    }

    fn key_rect(&self, index: usize) -> (Vec2, Vec2) {
        let col = (index % COLS) as f32;
        let row = (index / COLS) as f32;
        let x = self.position.x + PADDING + col * (BTN + GAP);
        let y = self.grid_top() + row * (BTN + GAP);
        (Vec2::new(x, y), Vec2::splat(BTN))
    }

    fn equals_rect(&self) -> (Vec2, Vec2) {
        let y = self.grid_top() + ROWS as f32 * BTN + (ROWS as f32 - 1.0) * GAP + GAP;
        let x = self.position.x + PADDING;
        (Vec2::new(x, y), Vec2::new(self.panel_width() - PADDING * 2.0, BTN))
    }

    /// Full panel bounds — used to swallow touches so they never reach the
    /// canvas underneath, even between keys (mirrors `Toolbar::contains`).
    pub fn contains(&self, p: Vec2) -> bool {
        rect_contains((self.position, Vec2::new(self.panel_width(), self.panel_height())), p)
    }

    pub fn is_on_header(&self, p: Vec2) -> bool {
        rect_contains(self.header_rect(), p)
    }

    /// Applies whatever key/`=` is under `p`, if any — called once on
    /// touch-down, same as a toolbar button (no drag-to-repeat).
    pub fn press_at(&mut self, p: Vec2) {
        if rect_contains(self.equals_rect(), p) {
            self.press_equals();
            return;
        }
        for (i, key) in KEYS.into_iter().enumerate() {
            if rect_contains(self.key_rect(i), p) {
                self.press_key(key);
                return;
            }
        }
    }

    pub fn drag_to(&mut self, p: Vec2, grab_offset: Vec2) {
        self.position = p - grab_offset;
    }

    fn press_key(&mut self, key: CalcKey) {
        match key {
            CalcKey::Digit(d) => {
                if self.entry_fresh {
                    self.display = d.to_string();
                    self.entry_fresh = false;
                } else if self.display.len() < MAX_DIGITS {
                    if self.display == "0" {
                        self.display = d.to_string();
                    } else {
                        self.display.push_str(&d.to_string());
                    }
                }
            }
            CalcKey::Dot => {
                if self.entry_fresh {
                    self.display = "0.".to_string();
                    self.entry_fresh = false;
                } else if !self.display.contains('.') && self.display.len() < MAX_DIGITS {
                    self.display.push('.');
                }
            }
            CalcKey::Clear => {
                self.display = "0".to_string();
                self.accumulator = None;
                self.pending_op = None;
                self.entry_fresh = true;
            }
            CalcKey::Operator(op) => {
                let value = self.display.parse::<f64>().unwrap_or(0.0);
                self.accumulator = Some(match (self.accumulator, self.pending_op) {
                    (Some(acc), Some(pending)) => apply(pending, acc, value),
                    _ => value,
                });
                self.pending_op = Some(op);
                self.entry_fresh = true;
                self.display = format_number(self.accumulator.unwrap());
            }
        }
    }

    fn press_equals(&mut self) {
        let value = self.display.parse::<f64>().unwrap_or(0.0);
        if let (Some(acc), Some(op)) = (self.accumulator, self.pending_op) {
            self.display = format_number(apply(op, acc, value));
            self.accumulator = None;
            self.pending_op = None;
            self.entry_fresh = true;
        }
    }

    pub fn render(&self, out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) {
        push_rect(self.position, Vec2::new(self.panel_width(), self.panel_height()), COLOR_PANEL_BG, out_vertices, out_indices);
        let (header_pos, header_size) = self.header_rect();
        push_rect(header_pos, header_size, COLOR_HEADER, out_vertices, out_indices);
        // A small grip mark on the header, hinting it's draggable.
        let grip_center = header_pos + header_size / 2.0;
        for i in -1..=1 {
            let x = grip_center.x + i as f32 * 10.0;
            push_circle(Vec2::new(x, grip_center.y), 2.0, COLOR_GLYPH, 8, out_vertices, out_indices);
        }

        let (disp_pos, disp_size) = self.display_rect();
        push_rect(disp_pos, disp_size, COLOR_DISPLAY_BG, out_vertices, out_indices);
        let digit_size = Vec2::new(disp_size.y * 0.28, disp_size.y * 0.5);
        let text_width = self.display.chars().count() as f32 * digit_size.x * 1.35;
        let right_edge = disp_pos.x + disp_size.x - 10.0;
        let text_x = (right_edge - text_width).max(disp_pos.x + 8.0);
        let text_pos = Vec2::new(text_x, disp_pos.y + disp_size.y * 0.22);
        push_number(&self.display, text_pos, digit_size, digit_size.x * 0.22, COLOR_DISPLAY_DIGIT, out_vertices, out_indices);

        for (i, key) in KEYS.into_iter().enumerate() {
            let (pos, size) = self.key_rect(i);
            let color = match key {
                CalcKey::Operator(_) => COLOR_KEY_OP,
                _ => COLOR_KEY_IDLE,
            };
            push_rect(pos, size, color, out_vertices, out_indices);
            draw_key_glyph(key, pos, size, out_vertices, out_indices);
        }

        let (eq_pos, eq_size) = self.equals_rect();
        push_rect(eq_pos, eq_size, COLOR_KEY_EQUALS, out_vertices, out_indices);
        let center = eq_pos + eq_size / 2.0;
        let r = eq_size.y * 0.22;
        push_line(center + Vec2::new(-r, -r * 0.5), center + Vec2::new(r, -r * 0.5), 4.0, COLOR_GLYPH, out_vertices, out_indices);
        push_line(center + Vec2::new(-r, r * 0.5), center + Vec2::new(r, r * 0.5), 4.0, COLOR_GLYPH, out_vertices, out_indices);
    }
}

fn apply(op: Op, a: f64, b: f64) -> f64 {
    match op {
        Op::Add => a + b,
        Op::Sub => a - b,
        Op::Mul => a * b,
        Op::Div => {
            if b == 0.0 {
                0.0
            } else {
                a / b
            }
        }
    }
}

/// Trims a computed value down to something that fits the display and the
/// digit renderer's limited character set (no exponent notation available).
fn format_number(v: f64) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    let mut s = format!("{v:.6}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s.len() > MAX_DIGITS {
        s.truncate(MAX_DIGITS);
    }
    if s.is_empty() || s == "-" {
        s = "0".to_string();
    }
    s
}

fn draw_key_glyph(key: CalcKey, top_left: Vec2, size: Vec2, out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) {
    let center = top_left + size / 2.0;
    let r = size.x * 0.22;
    match key {
        CalcKey::Digit(d) => {
            let digit_size = Vec2::new(size.x * 0.22, size.y * 0.4);
            let pos = center - Vec2::new(digit_size.x * 0.5, digit_size.y * 0.5);
            push_number(&d.to_string(), pos, digit_size, digit_size.x * 0.28, COLOR_GLYPH, out_vertices, out_indices);
        }
        CalcKey::Dot => {
            push_circle(center, 4.0, COLOR_GLYPH, 10, out_vertices, out_indices);
        }
        CalcKey::Clear => {
            // A "reset" glyph: circle with a diagonal slash through it —
            // avoids an X, which is reused below for multiply.
            for i in 0..20 {
                if i % 2 == 0 {
                    continue;
                }
                let theta = i as f32 / 20.0 * std::f32::consts::TAU;
                let p0 = center + Vec2::new(theta.cos(), theta.sin()) * r;
                let theta1 = (i + 1) as f32 / 20.0 * std::f32::consts::TAU;
                let p1 = center + Vec2::new(theta1.cos(), theta1.sin()) * r;
                push_line(p0, p1, 3.0, COLOR_GLYPH, out_vertices, out_indices);
            }
            push_line(center + Vec2::new(-r * 0.7, -r * 0.7), center + Vec2::new(r * 0.7, r * 0.7), 3.0, COLOR_GLYPH, out_vertices, out_indices);
        }
        CalcKey::Operator(Op::Add) => {
            push_line(center + Vec2::new(-r, 0.0), center + Vec2::new(r, 0.0), 4.0, COLOR_GLYPH, out_vertices, out_indices);
            push_line(center + Vec2::new(0.0, -r), center + Vec2::new(0.0, r), 4.0, COLOR_GLYPH, out_vertices, out_indices);
        }
        CalcKey::Operator(Op::Sub) => {
            push_line(center + Vec2::new(-r, 0.0), center + Vec2::new(r, 0.0), 4.0, COLOR_GLYPH, out_vertices, out_indices);
        }
        CalcKey::Operator(Op::Mul) => {
            push_line(center + Vec2::new(-r, -r), center + Vec2::new(r, r), 4.0, COLOR_GLYPH, out_vertices, out_indices);
            push_line(center + Vec2::new(-r, r), center + Vec2::new(r, -r), 4.0, COLOR_GLYPH, out_vertices, out_indices);
        }
        CalcKey::Operator(Op::Div) => {
            push_line(center + Vec2::new(-r, 0.0), center + Vec2::new(r, 0.0), 4.0, COLOR_GLYPH, out_vertices, out_indices);
            push_circle(center + Vec2::new(0.0, -r * 0.8), 3.0, COLOR_GLYPH, 8, out_vertices, out_indices);
            push_circle(center + Vec2::new(0.0, r * 0.8), 3.0, COLOR_GLYPH, 8, out_vertices, out_indices);
        }
    }
}

fn rect_contains((top_left, size): (Vec2, Vec2), point: Vec2) -> bool {
    point.x >= top_left.x && point.x <= top_left.x + size.x && point.y >= top_left.y && point.y <= top_left.y + size.y
}
