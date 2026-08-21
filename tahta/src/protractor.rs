//! Draggable, rotatable half-disc protractor overlay ("İletki") — same
//! drag-to-move / handle-to-rotate / edge-to-snap interaction as `ruler`
//! and `setsquare`, but shaped as a semicircle so a Pen stroke can snap
//! along its flat baseline (the two straight-edge tools already cover
//! ruled lines; this one exists for the arc/angle-marking use case).

use glam::Vec2;

use crate::digits::push_number;
use crate::geom::distance_to_segment;
use crate::stroke::{push_circle, push_line, push_triangle, Vertex};

const RADIUS: f32 = 260.0;
const HANDLE_RADIUS: f32 = 18.0;
pub const EDGE_SNAP_DISTANCE: f32 = 20.0;
const ARC_SEGMENTS: usize = 36; // one every 5°

const COLOR_BODY: [f32; 4] = [0.85, 0.87, 0.92, 0.22];
const COLOR_EDGE: [f32; 4] = [0.95, 0.95, 0.98, 0.85];
const COLOR_TICK: [f32; 4] = [0.15, 0.15, 0.18, 0.55];
const COLOR_HANDLE: [f32; 4] = [0.35, 0.85, 0.45, 0.95]; // green — distinct from ruler's blue and set-square's orange

/// `angle` is the rotation of the baseline's "0°" end (local +X, at
/// `local_point(0.0)`) around `center`; the dome always bulges toward the
/// local -Y side of that baseline (same rotation convention as `SetSquare`).
pub struct Protractor {
    pub center: Vec2,
    pub angle: f32,
}

impl Protractor {
    pub fn new(center: Vec2) -> Self {
        Self { center, angle: 0.0 }
    }

    fn rotate_local(&self, local: Vec2) -> Vec2 {
        let (s, c) = self.angle.sin_cos();
        Vec2::new(local.x * c - local.y * s, local.x * s + local.y * c)
    }

    /// A point on the unrotated semicircle at protractor-angle `deg`
    /// (0° = right end of baseline, 90° = top of dome, 180° = left end).
    fn local_point(&self, deg: f32) -> Vec2 {
        let theta = deg.to_radians();
        Vec2::new(theta.cos(), -theta.sin()) * RADIUS
    }

    fn world_point(&self, deg: f32) -> Vec2 {
        self.center + self.rotate_local(self.local_point(deg))
    }

    /// Right end of the baseline (0°) — also where the rotate handle lives.
    pub fn end_right(&self) -> Vec2 {
        self.world_point(0.0)
    }

    /// Left end of the baseline (180°).
    pub fn end_left(&self) -> Vec2 {
        self.world_point(180.0)
    }

    pub fn handle_pos(&self) -> Vec2 {
        let dir = (self.end_right() - self.center).normalize_or_zero();
        self.end_right() + dir * (HANDLE_RADIUS + 8.0)
    }

    pub fn is_on_handle(&self, p: Vec2) -> bool {
        p.distance(self.handle_pos()) <= HANDLE_RADIUS + 10.0
    }

    /// True if `p` lands inside the half-disc (for "grab to move" — checked
    /// only after `is_on_handle` and edge-snap fail).
    pub fn is_on_body(&self, p: Vec2) -> bool {
        let local = self.rotate_local_inverse(p - self.center);
        local.y <= 0.0 && local.length() <= RADIUS
    }

    fn rotate_local_inverse(&self, world: Vec2) -> Vec2 {
        let (s, c) = (-self.angle).sin_cos();
        Vec2::new(world.x * c - world.y * s, world.x * s + world.y * c)
    }

    /// The only snap-able edge is the flat baseline — the curved edge isn't
    /// a useful straightedge, so it's excluded from `find_edge_snap`.
    pub fn nearest_edge(&self, _p: Vec2) -> (Vec2, Vec2) {
        (self.end_left(), self.end_right())
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
        // Filled half-disc as a triangle fan from the center.
        for i in 0..ARC_SEGMENTS {
            let d0 = i as f32 / ARC_SEGMENTS as f32 * 180.0;
            let d1 = (i + 1) as f32 / ARC_SEGMENTS as f32 * 180.0;
            push_triangle(self.center, self.world_point(d0), self.world_point(d1), COLOR_BODY, out_vertices, out_indices);
        }

        // Arc outline + baseline edge.
        for i in 0..ARC_SEGMENTS {
            let d0 = i as f32 / ARC_SEGMENTS as f32 * 180.0;
            let d1 = (i + 1) as f32 / ARC_SEGMENTS as f32 * 180.0;
            push_line(self.world_point(d0), self.world_point(d1), 2.0, COLOR_EDGE, out_vertices, out_indices);
        }
        push_line(self.end_left(), self.end_right(), 2.0, COLOR_EDGE, out_vertices, out_indices);

        // Degree ticks every 10°, longer every 30°.
        for i in 0..=18 {
            let deg = i as f32 * 10.0;
            let outer = self.world_point(deg);
            let inner_radius = if i % 3 == 0 { RADIUS * 0.88 } else { RADIUS * 0.94 };
            let inner = self.center + self.rotate_local(self.local_point(deg).normalize_or_zero() * inner_radius);
            push_line(inner, outer, 1.5, COLOR_TICK, out_vertices, out_indices);
        }

        push_circle(self.handle_pos(), HANDLE_RADIUS, COLOR_HANDLE, 20, out_vertices, out_indices);

        // Fixed reference labels — a real reading would need to track the
        // pen's own angle relative to the baseline, which no caller wires
        // up yet, so these mark the scale itself (like the ruler's tick
        // labels), not a live measurement.
        let digit_size = Vec2::new(8.0, 13.0);
        push_number("0", self.end_right() + Vec2::new(4.0, 8.0), digit_size, 2.0, COLOR_EDGE, out_vertices, out_indices);
        push_number("90", self.world_point(90.0) + self.rotate_local(Vec2::new(-10.0, -22.0)), digit_size, 2.0, COLOR_EDGE, out_vertices, out_indices);
        push_number("180", self.end_left() + Vec2::new(-24.0, 8.0), digit_size, 2.0, COLOR_EDGE, out_vertices, out_indices);
    }
}
