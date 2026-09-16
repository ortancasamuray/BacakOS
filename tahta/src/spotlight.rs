//! Draggable, resizable spotlight overlay ("Spot Işığı") — darkens the
//! whole board except a circular window, to pull attention to one spot.
//! Unlike the calculator/stopwatch/dice panels, this does NOT swallow
//! touches outside its own body/handle: everything past the dark ring
//! still reaches the canvas underneath (Pen, Eraser, etc.), same as the
//! ruler/set-square/protractor overlays.

use glam::Vec2;

use crate::stroke::{push_circle, push_line, push_ring_wedge, Vertex};

const HANDLE_RADIUS: f32 = 18.0;
const MIN_RADIUS: f32 = 60.0;
const MAX_RADIUS: f32 = 520.0;
const DEFAULT_RADIUS: f32 = 220.0;

const COLOR_DARK: [f32; 4] = [0.0, 0.0, 0.0, 0.72];
const COLOR_RIM: [f32; 4] = [0.98, 0.85, 0.25, 0.9]; // warm yellow — reads as a light beam
const COLOR_HANDLE: [f32; 4] = [0.98, 0.85, 0.25, 0.95];

pub struct Spotlight {
    pub center: Vec2,
    pub radius: f32,
}

impl Spotlight {
    pub fn new(center: Vec2) -> Self {
        Self { center, radius: DEFAULT_RADIUS }
    }

    pub fn handle_pos(&self) -> Vec2 {
        self.center + Vec2::new(self.radius, 0.0)
    }

    pub fn is_on_handle(&self, p: Vec2) -> bool {
        p.distance(self.handle_pos()) <= HANDLE_RADIUS + 10.0
    }

    /// True inside the bright window — the "grab to move" zone, checked
    /// only after `is_on_handle` fails.
    pub fn is_on_body(&self, p: Vec2) -> bool {
        p.distance(self.center) <= self.radius
    }

    pub fn drag_to(&mut self, p: Vec2, grab_offset: Vec2) {
        self.center = p - grab_offset;
    }

    pub fn resize_to(&mut self, p: Vec2) {
        self.radius = self.center.distance(p).clamp(MIN_RADIUS, MAX_RADIUS);
    }

    pub fn render(&self, screen_size: Vec2, out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) {
        // The dark mask is an annulus between the spotlight radius and an
        // outer radius guaranteed to reach past every screen corner from
        // wherever the spotlight currently sits — so it always fully
        // covers the visible area no matter how far off-center it's
        // dragged, without needing a real stencil/clip.
        let corners = [Vec2::ZERO, Vec2::new(screen_size.x, 0.0), Vec2::new(0.0, screen_size.y), screen_size];
        let outer = corners.iter().map(|&c| self.center.distance(c)).fold(0.0_f32, f32::max) + 50.0;
        push_ring_wedge(self.center, self.radius, outer, 0.0, std::f32::consts::TAU, COLOR_DARK, 64, out_vertices, out_indices);

        for i in 0..64 {
            let t0 = i as f32 / 64.0 * std::f32::consts::TAU;
            let t1 = (i + 1) as f32 / 64.0 * std::f32::consts::TAU;
            let p0 = self.center + Vec2::new(t0.cos(), t0.sin()) * self.radius;
            let p1 = self.center + Vec2::new(t1.cos(), t1.sin()) * self.radius;
            push_line(p0, p1, 2.5, COLOR_RIM, out_vertices, out_indices);
        }

        push_circle(self.handle_pos(), HANDLE_RADIUS, COLOR_HANDLE, 20, out_vertices, out_indices);
    }
}
