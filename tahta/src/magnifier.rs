//! Draggable, resizable magnifier lens ("Büyüteç") — shows a real
//! optical/pixel zoom of whatever's under it (page background image,
//! grid, ink — everything), not just re-scaled ink.
//!
//! This widget only draws its own rim + drag handle as vector geometry;
//! the zoomed circle itself is a GPU texture sample (`renderer.rs`
//! renders the whole page into an offscreen "content" texture every
//! frame, then the fragment shader in `shader.wgsl::fs_magnifier` maps
//! each pixel inside the lens circle back to a source pixel in that
//! texture via `(frag_pos - center) / ZOOM + center`, discarding outside
//! the circle or outside the source texture — see `lens()` for the
//! uniform values the renderer needs). This replaced an earlier
//! vector-based approach (walk `Stroke` points, keep ones inside the
//! lens, re-tessellate scaled up) that could only show ink, never a PDF
//! page's background image.
//!
//! Like `spotlight::Spotlight`, touches outside the lens body/handle pass
//! straight through to the canvas — it doesn't swallow input.

use glam::Vec2;

use crate::stroke::{push_circle, push_line, Vertex};

pub const ZOOM: f32 = 2.2;
const HANDLE_RADIUS: f32 = 18.0;
const MIN_RADIUS: f32 = 80.0;
const MAX_RADIUS: f32 = 260.0;
const DEFAULT_RADIUS: f32 = 160.0;

const COLOR_RIM: [f32; 4] = [0.35, 0.85, 0.95, 0.95]; // cyan — distinct from every other overlay's handle color
const COLOR_HANDLE: [f32; 4] = [0.35, 0.85, 0.95, 0.95];

pub struct Magnifier {
    pub center: Vec2,
    pub radius: f32,
}

impl Magnifier {
    pub fn new(center: Vec2) -> Self {
        Self { center, radius: DEFAULT_RADIUS }
    }

    pub fn handle_pos(&self) -> Vec2 {
        self.center + Vec2::new(self.radius, 0.0)
    }

    pub fn is_on_handle(&self, p: Vec2) -> bool {
        p.distance(self.handle_pos()) <= HANDLE_RADIUS + 10.0
    }

    pub fn is_on_body(&self, p: Vec2) -> bool {
        p.distance(self.center) <= self.radius
    }

    pub fn drag_to(&mut self, p: Vec2, grab_offset: Vec2) {
        self.center = p - grab_offset;
    }

    pub fn resize_to(&mut self, p: Vec2) {
        self.radius = self.center.distance(p).clamp(MIN_RADIUS, MAX_RADIUS);
    }

    /// `(center, radius, zoom)` — what `renderer.rs` needs to draw the
    /// texture-sampled lens circle before this draws its rim/handle on
    /// top of it.
    pub fn lens(&self) -> (Vec2, f32, f32) {
        (self.center, self.radius, ZOOM)
    }

    /// Draws only the rim + drag handle — the zoomed circle itself is a
    /// texture sample the renderer draws separately (see `lens()`).
    pub fn render(&self, out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) {
        for i in 0..48 {
            let t0 = i as f32 / 48.0 * std::f32::consts::TAU;
            let t1 = (i + 1) as f32 / 48.0 * std::f32::consts::TAU;
            let p0 = self.center + Vec2::new(t0.cos(), t0.sin()) * self.radius;
            let p1 = self.center + Vec2::new(t1.cos(), t1.sin()) * self.radius;
            push_line(p0, p1, 2.5, COLOR_RIM, out_vertices, out_indices);
        }

        push_circle(self.handle_pos(), HANDLE_RADIUS, COLOR_HANDLE, 20, out_vertices, out_indices);
    }
}
