//! Draggable, rotatable straightedge overlay ("Cetvel"). Grab the body to
//! move it, the handle at one end to rotate it; a Pen stroke that starts
//! near its long edge snaps to run exactly along that edge for as long as
//! the touch stays close to it, giving a perfectly straight, precisely
//! angled line without steady hands.

use glam::Vec2;

use crate::digits::push_number;
use crate::stroke::{push_circle, push_line, Vertex};

const LENGTH: f32 = 640.0;
const THICKNESS: f32 = 64.0;
const HANDLE_RADIUS: f32 = 18.0;
/// How close a Pen touch must land to the drawing edge to snap to it.
pub const EDGE_SNAP_DISTANCE: f32 = 20.0;
/// Arbitrary px-per-unit tick spacing (~96dpi "cm") — this app has no way
/// to know the panel's physical size, so the printed number is a relative
/// reference scale, not a calibrated real-world measurement.
const TICK_SPACING: f32 = 37.8;

const COLOR_BODY: [f32; 4] = [0.85, 0.87, 0.92, 0.30];
const COLOR_EDGE: [f32; 4] = [0.95, 0.95, 0.98, 0.85];
const COLOR_TICK: [f32; 4] = [0.15, 0.15, 0.18, 0.55];
const COLOR_HANDLE: [f32; 4] = [0.23, 0.51, 0.96, 0.95];

pub struct Ruler {
    pub center: Vec2,
    /// Radians; 0 = horizontal, increasing clockwise (screen Y-down).
    pub angle: f32,
}

impl Ruler {
    pub fn new(center: Vec2) -> Self {
        Self { center, angle: 0.0 }
    }

    fn dir(&self) -> Vec2 {
        Vec2::new(self.angle.cos(), self.angle.sin())
    }

    fn normal(&self) -> Vec2 {
        Vec2::new(-self.angle.sin(), self.angle.cos())
    }

    fn end_a(&self) -> Vec2 {
        self.center - self.dir() * (LENGTH / 2.0)
    }

    fn end_b(&self) -> Vec2 {
        self.center + self.dir() * (LENGTH / 2.0)
    }

    pub fn handle_pos(&self) -> Vec2 {
        self.end_b() + self.dir() * (HANDLE_RADIUS + 8.0)
    }

    /// The two long edges ink can snap to — a real ruler draws along
    /// either side, not just one.
    fn top_edge(&self) -> (Vec2, Vec2) {
        let offset = self.normal() * (THICKNESS / 2.0);
        (self.end_a() + offset, self.end_b() + offset)
    }

    fn bottom_edge(&self) -> (Vec2, Vec2) {
        let offset = self.normal() * (THICKNESS / 2.0);
        (self.end_a() - offset, self.end_b() - offset)
    }

    /// Whichever of the two long edges `p` is closer to.
    pub fn nearest_edge(&self, p: Vec2) -> (Vec2, Vec2) {
        let top = self.top_edge();
        let bottom = self.bottom_edge();
        if distance_to_segment(p, top.0, top.1) <= distance_to_segment(p, bottom.0, bottom.1) {
            top
        } else {
            bottom
        }
    }

    pub fn is_on_handle(&self, p: Vec2) -> bool {
        p.distance(self.handle_pos()) <= HANDLE_RADIUS + 10.0
    }

    /// True if `p` is within the ruler's rectangular body (for "grab to
    /// move" — checked only after `is_on_handle` and edge-snap fail).
    pub fn is_on_body(&self, p: Vec2) -> bool {
        let rel = p - self.center;
        let along = rel.dot(self.dir());
        let across = rel.dot(self.normal());
        along.abs() <= LENGTH / 2.0 && across.abs() <= THICKNESS / 2.0
    }

    pub fn distance_to_edge(&self, p: Vec2) -> f32 {
        let (a, b) = self.nearest_edge(p);
        distance_to_segment(p, a, b)
    }

    pub fn rotate_toward(&mut self, p: Vec2) {
        let rel = p - self.center;
        if rel.length() > 1.0 {
            self.angle = rel.y.atan2(rel.x);
        }
    }

    pub fn drag_to(&mut self, p: Vec2, grab_offset: Vec2) {
        self.center = p - grab_offset;
    }

    pub fn render(&self, out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) {
        let (top_a, top_b) = self.top_edge();
        let (bot_a, bot_b) = self.bottom_edge();

        // Body as a thick line (works at any angle, unlike an axis-aligned
        // rect) plus brighter lines along both snap-able edges.
        push_line(self.end_a(), self.end_b(), THICKNESS, COLOR_BODY, out_vertices, out_indices);
        push_line(top_a, top_b, 2.0, COLOR_EDGE, out_vertices, out_indices);
        push_line(bot_a, bot_b, 2.0, COLOR_EDGE, out_vertices, out_indices);

        // Tick marks every TICK_SPACING px along the body, short lines
        // perpendicular to the ruler, growing every 5th tick.
        let ticks = (LENGTH / TICK_SPACING) as i32;
        let normal = self.normal();
        for i in 0..=ticks {
            let t = i as f32 * TICK_SPACING - LENGTH / 2.0;
            if t.abs() > LENGTH / 2.0 {
                continue;
            }
            let base = self.center + self.dir() * t + normal * (THICKNESS / 2.0);
            let tick_len = if i % 5 == 0 { THICKNESS * 0.35 } else { THICKNESS * 0.18 };
            push_line(base, base - normal * tick_len, 1.5, COLOR_TICK, out_vertices, out_indices);
        }

        // Rotate handle.
        push_circle(self.handle_pos(), HANDLE_RADIUS, COLOR_HANDLE, 20, out_vertices, out_indices);

        // Length readout near the center, upright regardless of rotation
        // (screen-space text, not rotated with the ruler — simplest to
        // read at a glance).
        let label = format!("{:.0}", LENGTH / TICK_SPACING);
        let label_pos = self.center + Vec2::new(-14.0, -THICKNESS / 2.0 - 26.0);
        push_number(&label, label_pos, Vec2::new(10.0, 16.0), 2.5, COLOR_EDGE, out_vertices, out_indices);
    }
}

fn distance_to_segment(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    p.distance(project_onto_segment(p, a, b))
}

/// Perpendicular projection of `p` onto segment `a..b`, clamped to the
/// segment's ends. Public so `input_handler` can re-snap a Pen stroke's
/// points to a ruler edge on every move, not just at touch-down.
pub fn project_onto_segment(p: Vec2, a: Vec2, b: Vec2) -> Vec2 {
    let ab = b - a;
    let len_sq = ab.length_squared().max(1e-6);
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    a + ab * t
}
