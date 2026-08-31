//! Draggable floating stopwatch panel ("Kronometre") — start/pause/reset,
//! same UI-panel pattern as `calculator::Calculator` (swallows touches
//! completely, no drawing interaction). The display is `MM:SS.T`
//! (`digits.rs` has a `:` glyph now).

use glam::Vec2;

use crate::digits::push_number;
use crate::stroke::{push_circle, push_line, push_rect, push_triangle, Vertex};

const BTN: f32 = 64.0;
const GAP: f32 = 8.0;
const HEADER_H: f32 = 36.0;
const DISPLAY_H: f32 = 64.0;
const PADDING: f32 = 12.0;
const MAX_ELAPSED: f64 = 5999.9;

const COLOR_PANEL_BG: [f32; 4] = [0.12, 0.13, 0.17, 0.95];
const COLOR_HEADER: [f32; 4] = [0.18, 0.19, 0.24, 1.0];
const COLOR_DISPLAY_BG: [f32; 4] = [0.05, 0.06, 0.08, 1.0];
const COLOR_DISPLAY_DIGIT: [f32; 4] = [0.95, 0.75, 0.35, 1.0]; // amber — distinct from the calculator's green
const COLOR_BTN_START: [f32; 4] = [0.30, 0.78, 0.40, 1.0];
const COLOR_BTN_PAUSE: [f32; 4] = [0.95, 0.65, 0.20, 1.0];
const COLOR_BTN_RESET: [f32; 4] = [0.55, 0.30, 0.30, 1.0];
const COLOR_GLYPH: [f32; 4] = [0.95, 0.95, 0.97, 1.0];

pub struct Stopwatch {
    pub position: Vec2,
    base_elapsed: f64,
    running: bool,
    resumed_at: f64,
}

impl Stopwatch {
    pub fn new(position: Vec2) -> Self {
        Self { position, base_elapsed: 0.0, running: false, resumed_at: 0.0 }
    }

    fn panel_width(&self) -> f32 {
        PADDING * 2.0 + BTN * 2.0 + GAP
    }

    fn panel_height(&self) -> f32 {
        HEADER_H + PADDING + DISPLAY_H + GAP + BTN + PADDING
    }

    fn header_rect(&self) -> (Vec2, Vec2) {
        (self.position, Vec2::new(self.panel_width(), HEADER_H))
    }

    fn display_rect(&self) -> (Vec2, Vec2) {
        let top_left = self.position + Vec2::new(PADDING, HEADER_H + PADDING);
        (top_left, Vec2::new(self.panel_width() - PADDING * 2.0, DISPLAY_H))
    }

    fn toggle_rect(&self) -> (Vec2, Vec2) {
        let y = self.position.y + HEADER_H + PADDING + DISPLAY_H + GAP;
        (Vec2::new(self.position.x + PADDING, y), Vec2::splat(BTN))
    }

    fn reset_rect(&self) -> (Vec2, Vec2) {
        let y = self.position.y + HEADER_H + PADDING + DISPLAY_H + GAP;
        (Vec2::new(self.position.x + PADDING + BTN + GAP, y), Vec2::splat(BTN))
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

    fn current_elapsed(&self, now: f64) -> f64 {
        if self.running {
            self.base_elapsed + (now - self.resumed_at)
        } else {
            self.base_elapsed
        }
    }

    pub fn press_at(&mut self, p: Vec2, now: f64) {
        if rect_contains(self.toggle_rect(), p) {
            if self.running {
                self.base_elapsed = self.current_elapsed(now);
                self.running = false;
            } else {
                self.resumed_at = now;
                self.running = true;
            }
        } else if rect_contains(self.reset_rect(), p) {
            self.base_elapsed = 0.0;
            self.running = false;
        }
    }

    pub fn render(&self, now: f64, out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) {
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
        let elapsed = self.current_elapsed(now).clamp(0.0, MAX_ELAPSED);
        let minutes = (elapsed / 60.0) as u32;
        let seconds = elapsed - minutes as f64 * 60.0;
        let text = format!("{minutes:02}:{seconds:04.1}");
        // Always 7 chars ("MM:SS.T") — size digits to fit the panel exactly
        // rather than a fixed size that would overflow it.
        let digit_h = disp_size.y * 0.5;
        let digit_w = ((disp_size.x - 16.0) / (text.chars().count() as f32 * 1.35)).min(disp_size.y * 0.28);
        let digit_size = Vec2::new(digit_w, digit_h);
        let text_width = text.chars().count() as f32 * digit_size.x * 1.35;
        let right_edge = disp_pos.x + disp_size.x - 10.0;
        let text_x = (right_edge - text_width).max(disp_pos.x + 8.0);
        let text_pos = Vec2::new(text_x, disp_pos.y + disp_size.y * 0.22);
        push_number(&text, text_pos, digit_size, digit_size.x * 0.22, COLOR_DISPLAY_DIGIT, out_vertices, out_indices);

        let (toggle_pos, toggle_size) = self.toggle_rect();
        let toggle_color = if self.running { COLOR_BTN_PAUSE } else { COLOR_BTN_START };
        push_rect(toggle_pos, toggle_size, toggle_color, out_vertices, out_indices);
        let center = toggle_pos + toggle_size / 2.0;
        let r = toggle_size.x * 0.22;
        if self.running {
            push_rect(center - Vec2::new(r * 0.7, r), Vec2::new(r * 0.5, r * 2.0), COLOR_GLYPH, out_vertices, out_indices);
            push_rect(center + Vec2::new(r * 0.2, -r), Vec2::new(r * 0.5, r * 2.0), COLOR_GLYPH, out_vertices, out_indices);
        } else {
            let a = center + Vec2::new(-r * 0.6, -r);
            let b = center + Vec2::new(-r * 0.6, r);
            let c = center + Vec2::new(r * 0.9, 0.0);
            push_triangle(a, b, c, COLOR_GLYPH, out_vertices, out_indices);
        }

        // "Reset" reuses the calculator's circle+diagonal-slash glyph — the
        // same visual vocabulary for "clear/restart" across this app's
        // widget panels.
        let (reset_pos, reset_size) = self.reset_rect();
        push_rect(reset_pos, reset_size, COLOR_BTN_RESET, out_vertices, out_indices);
        let rcenter = reset_pos + reset_size / 2.0;
        let rr = reset_size.x * 0.22;
        for i in 0..20 {
            if i % 2 == 0 {
                continue;
            }
            let theta = i as f32 / 20.0 * std::f32::consts::TAU;
            let p0 = rcenter + Vec2::new(theta.cos(), theta.sin()) * rr;
            let theta1 = (i + 1) as f32 / 20.0 * std::f32::consts::TAU;
            let p1 = rcenter + Vec2::new(theta1.cos(), theta1.sin()) * rr;
            push_line(p0, p1, 3.0, COLOR_GLYPH, out_vertices, out_indices);
        }
        push_line(rcenter + Vec2::new(-rr * 0.7, -rr * 0.7), rcenter + Vec2::new(rr * 0.7, rr * 0.7), 3.0, COLOR_GLYPH, out_vertices, out_indices);
    }
}

fn rect_contains((top_left, size): (Vec2, Vec2), point: Vec2) -> bool {
    point.x >= top_left.x && point.x <= top_left.x + size.x && point.y >= top_left.y && point.y <= top_left.y + size.y
}
