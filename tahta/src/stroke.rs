//! Stroke storage, Bézier smoothing, and triangle-mesh tessellation for the
//! whiteboard renderer.

use bytemuck::{Pod, Zeroable};
use glam::Vec2;

/// Points closer than this (in pixels) to the last recorded point are
/// dropped — cuts down on redundant geometry from noisy touch digitizers.
const MIN_POINT_DISTANCE: f32 = 2.0;

/// Centerline samples per smoothed segment (quadratic Bézier subdivision).
const SEGMENTS_PER_CURVE: usize = 8;

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 2],
    pub color: [f32; 4],
}

impl Vertex {
    pub const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4];

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

/// One freehand pen stroke: raw input points plus stable color/width.
pub struct Stroke {
    pub points: Vec<Vec2>,
    pub color: [f32; 4],
    pub width: f32,
}

impl Stroke {
    pub fn new(color: [f32; 4], width: f32) -> Self {
        Self {
            points: Vec::with_capacity(256),
            color,
            width,
        }
    }

    /// Append a real (non-predicted) point, filtering near-duplicates.
    pub fn push_point(&mut self, p: Vec2) {
        if let Some(&last) = self.points.last() {
            if last.distance(p) < MIN_POINT_DISTANCE {
                return;
            }
        }
        self.points.push(p);
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Smooth `points` (optionally followed by transient predicted points)
    /// via quadratic Bézier interpolation through consecutive segment
    /// midpoints, then tessellate into a quad-per-segment triangle mesh.
    ///
    /// `predicted` points are NEVER merged into `self.points` — they exist
    /// only for this call's output.
    pub fn tessellate(
        &self,
        predicted: &[Vec2],
        out_vertices: &mut Vec<Vertex>,
        out_indices: &mut Vec<u32>,
    ) {
        if self.points.is_empty() {
            return;
        }

        let mut combined: Vec<Vec2> = Vec::with_capacity(self.points.len() + predicted.len());
        combined.extend_from_slice(&self.points);
        for &p in predicted {
            if let Some(&last) = combined.last() {
                if last.distance(p) < MIN_POINT_DISTANCE {
                    continue;
                }
            }
            combined.push(p);
        }

        let centerline = smooth_polyline(&combined);
        build_quad_strip(&centerline, self.width, self.color, out_vertices, out_indices);
    }
}

/// Smooth a raw polyline using the classic "midpoint quadratic Bézier"
/// technique: for each interior point `p_i`, the curve runs from the
/// midpoint of `(p_{i-1}, p_i)` to the midpoint of `(p_i, p_{i+1})` with
/// `p_i` as the control point. This removes sharp corners from
/// finger/stylus jitter without any global curve fit.
fn smooth_polyline(points: &[Vec2]) -> Vec<Vec2> {
    match points.len() {
        0 => return Vec::new(),
        1 => return vec![points[0]],
        2 => return vec![points[0], points[1]],
        _ => {}
    }

    let mut out = Vec::with_capacity(points.len() * SEGMENTS_PER_CURVE);
    out.push(points[0]);

    for i in 1..points.len() - 1 {
        let p_prev = points[i - 1];
        let p_curr = points[i];
        let p_next = points[i + 1];

        let m0 = (p_prev + p_curr) * 0.5;
        let m1 = (p_curr + p_next) * 0.5;

        for step in 1..=SEGMENTS_PER_CURVE {
            let t = step as f32 / SEGMENTS_PER_CURVE as f32;
            out.push(quadratic_bezier(m0, p_curr, m1, t));
        }
    }

    out.push(*points.last().unwrap());
    out
}

fn quadratic_bezier(p0: Vec2, control: Vec2, p1: Vec2, t: f32) -> Vec2 {
    let one_minus_t = 1.0 - t;
    one_minus_t * one_minus_t * p0 + 2.0 * one_minus_t * t * control + t * t * p1
}

/// Builds a triangle-list "thick line" mesh (2 triangles per quad segment)
/// from a smoothed centerline, offsetting along the per-segment tangent
/// normal by `width / 2.0`. Consecutive quads are drawn independently
/// (small overlap at joints) rather than as a primitive-restart triangle
/// strip, since wgpu's default pipeline can't cheaply express strip resets
/// across multiple strokes.
fn build_quad_strip(
    centerline: &[Vec2],
    width: f32,
    color: [f32; 4],
    out_vertices: &mut Vec<Vertex>,
    out_indices: &mut Vec<u32>,
) {
    if centerline.len() < 2 {
        // Degenerate stroke (a tap): draw a small square dot.
        if let Some(&p) = centerline.first() {
            push_dot(p, width, color, out_vertices, out_indices);
        }
        return;
    }

    let half_width = width / 2.0;

    for pair in centerline.windows(2) {
        let (p0, p1) = (pair[0], pair[1]);
        let dir = (p1 - p0).normalize_or_zero();
        if dir == Vec2::ZERO {
            continue;
        }
        let normal = Vec2::new(-dir.y, dir.x) * half_width;

        let v0 = p0 + normal;
        let v1 = p0 - normal;
        let v2 = p1 + normal;
        let v3 = p1 - normal;

        let base = out_vertices.len() as u32;
        out_vertices.push(Vertex { position: v0.into(), color });
        out_vertices.push(Vertex { position: v1.into(), color });
        out_vertices.push(Vertex { position: v2.into(), color });
        out_vertices.push(Vertex { position: v3.into(), color });

        // Two triangles: (v0,v1,v2) and (v2,v1,v3)
        out_indices.extend_from_slice(&[
            base, base + 1, base + 2,
            base + 2, base + 1, base + 3,
        ]);
    }
}

fn push_dot(
    center: Vec2,
    width: f32,
    color: [f32; 4],
    out_vertices: &mut Vec<Vertex>,
    out_indices: &mut Vec<u32>,
) {
    let r = (width / 2.0).max(1.0);
    let base = out_vertices.len() as u32;
    out_vertices.push(Vertex { position: (center + Vec2::new(-r, -r)).into(), color });
    out_vertices.push(Vertex { position: (center + Vec2::new(-r, r)).into(), color });
    out_vertices.push(Vertex { position: (center + Vec2::new(r, -r)).into(), color });
    out_vertices.push(Vertex { position: (center + Vec2::new(r, r)).into(), color });
    out_indices.extend_from_slice(&[
        base, base + 1, base + 2,
        base + 2, base + 1, base + 3,
    ]);
}
