//! Draggable, resizable magnifier lens ("Büyüteç") — shows a zoomed-in
//! view of whatever ink sits right under it.
//!
//! This is a *vector* zoom, not an optical/pixel one: tahta has no
//! render-to-texture pass over its own canvas (the only texture pipeline
//! that exists is for static PDF page images), so instead of sampling
//! pixels this walks the current page's actual `Stroke` points, keeps the
//! ones that fall inside the lens's source circle, scales them up around
//! the lens center, and re-tessellates them with the same
//! `Stroke::tessellate` used for normal ink — reusing its Bézier
//! smoothing, per-brush width/color logic, everything. A real magnifying
//! glass over paper would show pixels; this shows the same vectors, just
//! bigger, which is the more honest result for a whiteboard anyway.
//!
//! Like `spotlight::Spotlight`, touches outside the lens body/handle pass
//! straight through to the canvas — it doesn't swallow input.

use glam::Vec2;

use crate::board::Page;
use crate::stroke::{push_circle, push_line, Vertex};

const ZOOM: f32 = 2.2;
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

    pub fn render(&self, page: &Page, view_offset: Vec2, now: f64, out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) {
        // Opaque backdrop so the zoomed strokes read cleanly against the
        // board's own background instead of whatever unzoomed ink already
        // sits under the lens.
        push_circle(self.center, self.radius, page.background.color(), 48, out_vertices, out_indices);

        let source_radius = self.radius / ZOOM;
        for stroke in &page.strokes {
            self.render_zoomed_stroke(stroke, view_offset, source_radius, now, out_vertices, out_indices);
        }

        for i in 0..48 {
            let t0 = i as f32 / 48.0 * std::f32::consts::TAU;
            let t1 = (i + 1) as f32 / 48.0 * std::f32::consts::TAU;
            let p0 = self.center + Vec2::new(t0.cos(), t0.sin()) * self.radius;
            let p1 = self.center + Vec2::new(t1.cos(), t1.sin()) * self.radius;
            push_line(p0, p1, 2.5, COLOR_RIM, out_vertices, out_indices);
        }

        push_circle(self.handle_pos(), HANDLE_RADIUS, COLOR_HANDLE, 20, out_vertices, out_indices);
    }

    /// Keeps only the points of `stroke` whose on-screen position falls
    /// within the lens's source circle, splits on any gap that leaves
    /// (so a stroke that dips in and out of the lens doesn't draw a
    /// straight line across the gap), scales each surviving run up around
    /// `self.center`, and tessellates it with the stroke's own brush.
    fn render_zoomed_stroke(
        &self,
        stroke: &crate::stroke::Stroke,
        view_offset: Vec2,
        source_radius: f32,
        now: f64,
        out_vertices: &mut Vec<Vertex>,
        out_indices: &mut Vec<u32>,
    ) {
        let mut run_points: Vec<Vec2> = Vec::new();
        let mut run_times: Vec<f64> = Vec::new();

        let flush = |run_points: &mut Vec<Vec2>, run_times: &mut Vec<f64>, out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>| {
            if run_points.len() >= 2 {
                let zoomed = crate::stroke::Stroke {
                    points: std::mem::take(run_points),
                    timestamps: std::mem::take(run_times),
                    color: stroke.color,
                    width: stroke.width * ZOOM,
                    brush_type: stroke.brush_type,
                };
                zoomed.tessellate(&[], Vec2::ZERO, now, out_vertices, out_indices);
            } else {
                run_points.clear();
                run_times.clear();
            }
        };

        for (i, &p) in stroke.points.iter().enumerate() {
            let screen_p = p + view_offset;
            if screen_p.distance(self.center) <= source_radius {
                let local = (screen_p - self.center) * ZOOM + self.center;
                run_points.push(local);
                run_times.push(stroke.timestamps[i]);
            } else {
                flush(&mut run_points, &mut run_times, out_vertices, out_indices);
            }
        }
        flush(&mut run_points, &mut run_times, out_vertices, out_indices);
    }
}
