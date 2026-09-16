//! Draggable floating dice panel ("Zar") — two six-sided dice, same
//! UI-panel pattern as `calculator::Calculator`/`stopwatch::Stopwatch`.
//! Tap either die to reroll just that one; tap the roll button to reroll
//! both. No `rand` dependency: a tiny xorshift64 PRNG reseeded with the
//! touch timestamp on every roll is more than enough entropy for a
//! classroom dice widget.

use glam::Vec2;

use crate::stroke::{push_circle, push_rect, Vertex};

const DIE: f32 = 72.0;
const GAP: f32 = 12.0;
const HEADER_H: f32 = 36.0;
const PADDING: f32 = 12.0;
const BTN_H: f32 = 56.0;
const DICE_COUNT: usize = 2;

const COLOR_PANEL_BG: [f32; 4] = [0.12, 0.13, 0.17, 0.95];
const COLOR_HEADER: [f32; 4] = [0.18, 0.19, 0.24, 1.0];
const COLOR_DIE: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_PIP: [f32; 4] = [0.12, 0.13, 0.17, 1.0];
const COLOR_ROLL_BTN: [f32; 4] = [0.23, 0.51, 0.96, 1.0];
const COLOR_GLYPH: [f32; 4] = [0.95, 0.95, 0.97, 1.0];

pub struct Dice {
    pub position: Vec2,
    faces: [u8; DICE_COUNT],
    rng_state: u64,
}

impl Dice {
    /// `seed` should be some bits that differ run to run (e.g. the touch
    /// timestamp that opened the panel) — it only seeds the very first
    /// faces shown; every later roll reseeds from the tap's own timestamp.
    pub fn new(position: Vec2, seed: u64) -> Self {
        let mut dice = Self { position, faces: [1; DICE_COUNT], rng_state: seed | 1 };
        for i in 0..DICE_COUNT {
            let v = dice.next_rand(i as f64);
            dice.faces[i] = 1 + (v % 6) as u8;
        }
        dice
    }

    fn panel_width(&self) -> f32 {
        PADDING * 2.0 + DICE_COUNT as f32 * DIE + (DICE_COUNT as f32 - 1.0) * GAP
    }

    fn panel_height(&self) -> f32 {
        HEADER_H + PADDING + DIE + GAP + BTN_H + PADDING
    }

    fn header_rect(&self) -> (Vec2, Vec2) {
        (self.position, Vec2::new(self.panel_width(), HEADER_H))
    }

    fn die_rect(&self, i: usize) -> (Vec2, Vec2) {
        let x = self.position.x + PADDING + i as f32 * (DIE + GAP);
        let y = self.position.y + HEADER_H + PADDING;
        (Vec2::new(x, y), Vec2::splat(DIE))
    }

    fn roll_rect(&self) -> (Vec2, Vec2) {
        let y = self.position.y + HEADER_H + PADDING + DIE + GAP;
        (Vec2::new(self.position.x + PADDING, y), Vec2::new(self.panel_width() - PADDING * 2.0, BTN_H))
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

    fn next_rand(&mut self, mix: f64) -> u64 {
        self.rng_state ^= mix.to_bits();
        let mut x = self.rng_state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng_state = x;
        x
    }

    fn roll_die(&mut self, i: usize, now: f64) {
        let v = self.next_rand(now);
        self.faces[i] = 1 + (v % 6) as u8;
    }

    pub fn press_at(&mut self, p: Vec2, now: f64) {
        if rect_contains(self.roll_rect(), p) {
            for i in 0..DICE_COUNT {
                self.roll_die(i, now + i as f64);
            }
            return;
        }
        for i in 0..DICE_COUNT {
            if rect_contains(self.die_rect(i), p) {
                self.roll_die(i, now);
                return;
            }
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

        for i in 0..DICE_COUNT {
            let (pos, size) = self.die_rect(i);
            push_rect(pos, size, COLOR_DIE, out_vertices, out_indices);
            draw_pips(self.faces[i], pos, size, out_vertices, out_indices);
        }

        let (roll_pos, roll_size) = self.roll_rect();
        push_rect(roll_pos, roll_size, COLOR_ROLL_BTN, out_vertices, out_indices);
        let center = roll_pos + roll_size / 2.0;
        let r = roll_size.y * 0.16;
        push_rect(center - Vec2::splat(r), Vec2::splat(r * 2.0), COLOR_GLYPH, out_vertices, out_indices);
        draw_pips(5, center - Vec2::splat(r), Vec2::splat(r * 2.0), out_vertices, out_indices);
    }
}

fn draw_pips(face: u8, top_left: Vec2, size: Vec2, out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) {
    let pip_r = size.x * 0.08;
    let positions: &[(f32, f32)] = match face {
        1 => &[(0.5, 0.5)],
        2 => &[(0.28, 0.28), (0.72, 0.72)],
        3 => &[(0.28, 0.28), (0.5, 0.5), (0.72, 0.72)],
        4 => &[(0.28, 0.28), (0.72, 0.28), (0.28, 0.72), (0.72, 0.72)],
        5 => &[(0.28, 0.28), (0.72, 0.28), (0.5, 0.5), (0.28, 0.72), (0.72, 0.72)],
        _ => &[(0.28, 0.22), (0.28, 0.5), (0.28, 0.78), (0.72, 0.22), (0.72, 0.5), (0.72, 0.78)],
    };
    for &(fx, fy) in positions {
        let p = top_left + Vec2::new(size.x * fx, size.y * fy);
        push_circle(p, pip_r, COLOR_PIP, 12, out_vertices, out_indices);
    }
}

fn rect_contains((top_left, size): (Vec2, Vec2), point: Vec2) -> bool {
    point.x >= top_left.x && point.x <= top_left.x + size.x && point.y >= top_left.y && point.y <= top_left.y + size.y
}
