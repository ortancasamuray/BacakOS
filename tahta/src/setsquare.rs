//! Draggable, rotatable 45°-45°-90° set-square overlay ("Gönye") — the
//! same drag-to-move / handle-to-rotate / edge-to-snap interaction as
//! `ruler::Ruler`, but with three straight edges instead of two, letting
//! a Pen stroke snap along any leg or the hypotenuse.

use glam::Vec2;

use crate::digits::push_number;
use crate::geom::{distance_to_segment, point_in_triangle};
use crate::stroke::{push_circle, push_line, push_triangle, Vertex};

const SIZE: f32 = 320.0; // leg length
const HANDLE_RADIUS: f32 = 18.0;
pub const EDGE_SNAP_DISTANCE: f32 = 20.0;

const COLOR_BODY: [f32; 4] = [0.85, 0.87, 0.92, 0.24];
const COLOR_EDGE: [f32; 4] = [0.95, 0.95, 0.98, 0.85];
const COLOR_HANDLE: [f32; 4] = [0.98, 0.55, 0.15, 0.95]; // orange — distinct from the ruler's blue handle

/// Anchored at the right-angle vertex; `angle` is the rotation of the
/// horizontal leg (vertex B) around it.
pub struct SetSquare {
    pub right_angle: Vec2,
    pub angle: f32,
}

impl SetSquare {
    pub fn new(right_angle: Vec2) -> Self {
        Self { right_angle, angle: 0.0 }
    }

    fn rotate_local(&self, local: Vec2) -> Vec2 {
        let (s, c) = self.angle.sin_cos();
        Vec2::new(local.x * c - local.y * s, local.x * s + local.y * c)
    }

    /// The right-angle vertex.
    pub fn vertex_a(&self) -> Vec2 {
        self.right_angle
    }

    /// End of the horizontal leg — also where the rotate handle lives.
    pub fn vertex_b(&self) -> Vec2 {
        self.right_angle + self.rotate_local(Vec2::new(SIZE, 0.0))
    }

    /// End of the vertical leg.
    pub fn vertex_c(&self) -> Vec2 {
        self.right_angle + self.rotate_local(Vec2::new(0.0, SIZE))
    }

    pub fn handle_pos(&self) -> Vec2 {
        let b = self.vertex_b();
        let dir = (b - self.right_angle).normalize_or_zero();
        b + dir * (HANDLE_RADIUS + 8.0)
    }

    pub fn is_on_handle(&self, p: Vec2) -> bool {
        p.distance(self.handle_pos()) <= HANDLE_RADIUS + 10.0
    }

    pub fn is_on_body(&self, p: Vec2) -> bool {
        point_in_triangle(p, self.vertex_a(), self.vertex_b(), self.vertex_c())
    }

    fn edges(&self) -> [(Vec2, Vec2); 3] {
        let (a, b, c) = (self.vertex_a(), self.vertex_b(), self.vertex_c());
        [(a, b), (a, c), (b, c)]
    }

    /// Whichever of the three edges (two legs + hypotenuse) `p` is
    /// closest to.
    pub fn nearest_edge(&self, p: Vec2) -> (Vec2, Vec2) {
        self.edges()
            .into_iter()
            .min_by(|e1, e2| {
                distance_to_segment(p, e1.0, e1.1)
                    .partial_cmp(&distance_to_segment(p, e2.0, e2.1))
                    .unwrap()
            })
            .unwrap()
    }

    pub fn distance_to_edge(&self, p: Vec2) -> f32 {
        let (a, b) = self.nearest_edge(p);
        distance_to_segment(p, a, b)
    }

    pub fn rotate_toward(&mut self, p: Vec2) {
        let rel = p - self.right_angle;
        if rel.length() > 1.0 {
            self.angle = rel.y.atan2(rel.x);
        }
    }

    pub fn drag_to(&mut self, p: Vec2, grab_offset: Vec2) {
        self.right_angle = p - grab_offset;
    }

    pub fn render(&self, out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) {
        let (a, b, c) = (self.vertex_a(), self.vertex_b(), self.vertex_c());

        push_triangle(a, b, c, COLOR_BODY, out_vertices, out_indices);
        push_line(a, b, 2.0, COLOR_EDGE, out_vertices, out_indices);
        push_line(a, c, 2.0, COLOR_EDGE, out_vertices, out_indices);
        push_line(b, c, 2.0, COLOR_EDGE, out_vertices, out_indices);

        push_circle(self.handle_pos(), HANDLE_RADIUS, COLOR_HANDLE, 20, out_vertices, out_indices);

        // Fixed angle labels at each vertex — a 45-45-90 set square's
        // angles never change, so these are just legible reference marks,
        // not a live measurement (unlike the ruler's length readout).
        let digit_size = Vec2::new(8.0, 13.0);
        push_number("90", a + (b - a).normalize_or_zero() * 14.0 + (c - a).normalize_or_zero() * 14.0, digit_size, 2.0, COLOR_EDGE, out_vertices, out_indices);
        push_number("45", b + (a - b).normalize_or_zero() * 22.0 + (c - b).normalize_or_zero() * 10.0, digit_size, 2.0, COLOR_EDGE, out_vertices, out_indices);
        push_number("45", c + (a - c).normalize_or_zero() * 22.0 + (b - c).normalize_or_zero() * 10.0, digit_size, 2.0, COLOR_EDGE, out_vertices, out_indices);
    }
}
