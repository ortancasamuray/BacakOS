//! Stroke storage, Bézier smoothing, and triangle-mesh tessellation for the
//! whiteboard renderer. Handles all four [`BrushType`]s: constant-width
//! ballpoint ink, speed-driven variable-width calligraphy, flat-alpha
//! highlighter (Max-blended by the renderer, see [`crate::renderer`]), and
//! age-fading laser pointer strokes.

use bytemuck::{Pod, Zeroable};
use glam::Vec2;

use crate::brush::{BrushType, LASER_LIFETIME_SECS};

/// Points closer than this (in pixels) to the last recorded point are
/// dropped — cuts down on redundant geometry from noisy touch digitizers.
const MIN_POINT_DISTANCE: f32 = 2.0;

/// Centerline samples per smoothed segment (quadratic Bézier subdivision).
const SEGMENTS_PER_CURVE: usize = 8;

/// Speed (px/s) at which a Calligraphy stroke reaches its thinnest width.
const CALLIGRAPHY_SPEED_NORM: f32 = 1400.0;
const CALLIGRAPHY_MIN_WIDTH_FRAC: f32 = 0.28;

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

/// One freehand pen stroke: raw input points (+ their arrival timestamps,
/// needed for Calligraphy's speed-to-width mapping and LaserPointer's
/// age-fade), plus stable base color/width/brush.
pub struct Stroke {
    pub points: Vec<Vec2>,
    pub timestamps: Vec<f64>,
    pub color: [f32; 4],
    pub width: f32,
    pub brush_type: BrushType,
}

impl Stroke {
    pub fn new(color: [f32; 4], width: f32, brush_type: BrushType) -> Self {
        Self {
            points: Vec::with_capacity(256),
            timestamps: Vec::with_capacity(256),
            color,
            width,
            brush_type,
        }
    }

    /// Append a real (non-predicted) point, filtering near-duplicates.
    pub fn push_point(&mut self, p: Vec2, time: f64) {
        if let Some(&last) = self.points.last() {
            if last.distance(p) < MIN_POINT_DISTANCE {
                return;
            }
        }
        self.points.push(p);
        self.timestamps.push(time);
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    pub fn clear(&mut self) {
        self.points.clear();
        self.timestamps.clear();
    }

    /// Drops points older than [`LASER_LIFETIME_SECS`]. No-op for brushes
    /// other than [`BrushType::LaserPointer`]. Call once per frame before
    /// tessellating so the fade-out is continuous even without new input.
    pub fn prune_expired(&mut self, now: f64) {
        if self.brush_type != BrushType::LaserPointer {
            return;
        }
        while let Some(&oldest) = self.timestamps.first() {
            if now - oldest > LASER_LIFETIME_SECS {
                self.points.remove(0);
                self.timestamps.remove(0);
            } else {
                break;
            }
        }
    }

    /// Smooth `points` (optionally followed by transient predicted points)
    /// via quadratic Bézier interpolation through consecutive segment
    /// midpoints, then tessellate into a per-point-width/color triangle
    /// mesh. `predicted` points are NEVER merged into `self.points` — they
    /// exist only for this call's output. `now` drives LaserPointer fade.
    pub fn tessellate(
        &self,
        predicted: &[Vec2],
        view_offset: Vec2,
        now: f64,
        out_vertices: &mut Vec<Vertex>,
        out_indices: &mut Vec<u32>,
    ) {
        if self.points.is_empty() {
            return;
        }

        let mut positions: Vec<Vec2> = Vec::with_capacity(self.points.len() + predicted.len());
        let mut times: Vec<f64> = Vec::with_capacity(self.points.len() + predicted.len());
        positions.extend_from_slice(&self.points);
        times.extend_from_slice(&self.timestamps);
        for &p in predicted {
            if let Some(&last) = positions.last() {
                if last.distance(p) < MIN_POINT_DISTANCE {
                    continue;
                }
            }
            positions.push(p);
            times.push(now);
        }

        let widths = self.per_point_widths(&positions, &times);
        let colors = self.per_point_colors(&times, now);

        let (centerline, c_widths, c_colors) = smooth_with_attrs(&positions, &widths, &colors);
        let centerline: Vec<Vec2> = if view_offset == Vec2::ZERO {
            centerline
        } else {
            centerline.into_iter().map(|p| p + view_offset).collect()
        };

        build_variable_quad_strip(&centerline, &c_widths, &c_colors, out_vertices, out_indices);
    }

    fn per_point_widths(&self, positions: &[Vec2], times: &[f64]) -> Vec<f32> {
        if self.brush_type != BrushType::Calligraphy {
            return vec![self.width; positions.len()];
        }
        let min_w = self.width * CALLIGRAPHY_MIN_WIDTH_FRAC;
        let mut widths = Vec::with_capacity(positions.len());
        for i in 0..positions.len() {
            let speed = if i == 0 {
                0.0
            } else {
                let dt = (times[i] - times[i - 1]).max(1.0 / 1000.0) as f32;
                positions[i].distance(positions[i - 1]) / dt
            };
            let speed_factor = (speed / CALLIGRAPHY_SPEED_NORM).clamp(0.0, 1.0);
            widths.push((self.width * (1.0 - speed_factor)).clamp(min_w, self.width));
        }
        widths
    }

    fn per_point_colors(&self, times: &[f64], now: f64) -> Vec<[f32; 4]> {
        if self.brush_type != BrushType::LaserPointer {
            return vec![self.color; times.len()];
        }
        times
            .iter()
            .map(|&t| {
                let age = (now - t).max(0.0);
                let fade = (1.0 - age / LASER_LIFETIME_SECS).clamp(0.0, 1.0) as f32;
                [self.color[0], self.color[1], self.color[2], self.color[3] * fade]
            })
            .collect()
    }

    /// Distance-to-segment hit test (not just to sample points — raw touch
    /// samples can be tens of pixels apart, so point-only testing misses
    /// touches that are visually right on the smoothed curve between two
    /// samples). `view_offset` must match whatever was passed to
    /// [`Self::tessellate`] so hit-testing lines up with what's on screen.
    pub fn hit_test(&self, point: Vec2, radius: f32, view_offset: Vec2) -> bool {
        let threshold = radius + self.width / 2.0;
        if self.points.len() < 2 {
            return self.points.iter().any(|&p| (p + view_offset).distance(point) <= threshold);
        }
        self.points
            .windows(2)
            .any(|w| distance_to_segment(point, w[0] + view_offset, w[1] + view_offset) <= threshold)
    }

    /// Area erase: removes only the points within `radius` of `point`,
    /// rather than the whole stroke. If that opens a gap in the middle,
    /// `self` keeps the first surviving run of points and any further
    /// runs are returned as new strokes for the caller to push — so
    /// tessellation never draws a line straight across the erased gap.
    /// Returns an empty `Vec` (and leaves `self` untouched) if nothing in
    /// this stroke was within range.
    pub fn erase_near(&mut self, point: Vec2, radius: f32, view_offset: Vec2) -> Vec<Stroke> {
        if self.points.is_empty() {
            return Vec::new();
        }
        let threshold = radius + self.width / 2.0;
        let keep: Vec<bool> =
            self.points.iter().map(|&p| (p + view_offset).distance(point) > threshold).collect();
        if keep.iter().all(|&k| k) {
            return Vec::new();
        }

        let mut runs: Vec<(Vec<Vec2>, Vec<f64>)> = Vec::new();
        let mut cur_pts = Vec::new();
        let mut cur_times = Vec::new();
        for (i, &k) in keep.iter().enumerate() {
            if k {
                cur_pts.push(self.points[i]);
                cur_times.push(self.timestamps[i]);
            } else if !cur_pts.is_empty() {
                runs.push((std::mem::take(&mut cur_pts), std::mem::take(&mut cur_times)));
            }
        }
        if !cur_pts.is_empty() {
            runs.push((cur_pts, cur_times));
        }

        let mut runs = runs.into_iter();
        match runs.next() {
            Some((pts, times)) => {
                self.points = pts;
                self.timestamps = times;
            }
            None => self.clear(),
        }

        runs.map(|(pts, times)| Stroke { points: pts, timestamps: times, color: self.color, width: self.width, brush_type: self.brush_type })
            .collect()
    }
}

fn distance_to_segment(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_squared();
    if len_sq < 1e-6 {
        return p.distance(a);
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

/// Smooth a raw polyline using the classic "midpoint quadratic Bézier"
/// technique (see module docs) while carrying per-source-point width/color
/// along for the ride, linearly interpolated at the same `t` used for
/// position — so a Calligraphy stroke's width and a LaserPointer stroke's
/// fade both vary smoothly along the curve instead of stepping at each raw
/// sample.
fn smooth_with_attrs(
    points: &[Vec2],
    widths: &[f32],
    colors: &[[f32; 4]],
) -> (Vec<Vec2>, Vec<f32>, Vec<[f32; 4]>) {
    match points.len() {
        0 => return (Vec::new(), Vec::new(), Vec::new()),
        1 => return (vec![points[0]], vec![widths[0]], vec![colors[0]]),
        2 => {
            return (
                vec![points[0], points[1]],
                vec![widths[0], widths[1]],
                vec![colors[0], colors[1]],
            )
        }
        _ => {}
    }

    let n = points.len();
    let mut out_pos = Vec::with_capacity(n * SEGMENTS_PER_CURVE);
    let mut out_w = Vec::with_capacity(n * SEGMENTS_PER_CURVE);
    let mut out_c = Vec::with_capacity(n * SEGMENTS_PER_CURVE);

    out_pos.push(points[0]);
    out_w.push(widths[0]);
    out_c.push(colors[0]);

    for i in 1..n - 1 {
        let p_prev = points[i - 1];
        let p_curr = points[i];
        let p_next = points[i + 1];

        let m0 = (p_prev + p_curr) * 0.5;
        let m1 = (p_curr + p_next) * 0.5;

        for step in 1..=SEGMENTS_PER_CURVE {
            let t = step as f32 / SEGMENTS_PER_CURVE as f32;
            out_pos.push(quadratic_bezier(m0, p_curr, m1, t));
            // Width/color travel with the same curve: lerp(i, i+1, t) is a
            // close enough approximation to a true per-subdivision blend
            // and keeps this a single pass.
            let next = (i + 1).min(widths.len() - 1);
            out_w.push(lerp(widths[i], widths[next], t));
            out_c.push(lerp_color(colors[i], colors[next], t));
        }
    }

    out_pos.push(*points.last().unwrap());
    out_w.push(*widths.last().unwrap());
    out_c.push(*colors.last().unwrap());

    (out_pos, out_w, out_c)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp_color(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [lerp(a[0], b[0], t), lerp(a[1], b[1], t), lerp(a[2], b[2], t), lerp(a[3], b[3], t)]
}

fn quadratic_bezier(p0: Vec2, control: Vec2, p1: Vec2, t: f32) -> Vec2 {
    let one_minus_t = 1.0 - t;
    one_minus_t * one_minus_t * p0 + 2.0 * one_minus_t * t * control + t * t * p1
}

/// Builds a triangle-list "thick line" mesh (2 triangles per quad segment)
/// from a smoothed centerline with a width and color carried per point,
/// offsetting along the per-segment tangent normal by `width / 2.0`.
fn build_variable_quad_strip(
    centerline: &[Vec2],
    widths: &[f32],
    colors: &[[f32; 4]],
    out_vertices: &mut Vec<Vertex>,
    out_indices: &mut Vec<u32>,
) {
    if centerline.len() < 2 {
        if let (Some(&p), Some(&w), Some(&c)) = (centerline.first(), widths.first(), colors.first()) {
            push_dot(p, w, c, out_vertices, out_indices);
        }
        return;
    }

    for i in 0..centerline.len() - 1 {
        let (p0, p1) = (centerline[i], centerline[i + 1]);
        let dir = (p1 - p0).normalize_or_zero();
        if dir == Vec2::ZERO {
            continue;
        }
        let n0 = Vec2::new(-dir.y, dir.x) * (widths[i] / 2.0);
        let n1 = Vec2::new(-dir.y, dir.x) * (widths[i + 1] / 2.0);

        let v0 = p0 + n0;
        let v1 = p0 - n0;
        let v2 = p1 + n1;
        let v3 = p1 - n1;

        let base = out_vertices.len() as u32;
        out_vertices.push(Vertex { position: v0.into(), color: colors[i] });
        out_vertices.push(Vertex { position: v1.into(), color: colors[i] });
        out_vertices.push(Vertex { position: v2.into(), color: colors[i + 1] });
        out_vertices.push(Vertex { position: v3.into(), color: colors[i + 1] });

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

/// A single filled triangle in screen-space, for drafting-tool overlays
/// like the set-square ("gönye").
pub fn push_triangle(
    a: Vec2,
    b: Vec2,
    c: Vec2,
    color: [f32; 4],
    out_vertices: &mut Vec<Vertex>,
    out_indices: &mut Vec<u32>,
) {
    let base = out_vertices.len() as u32;
    out_vertices.push(Vertex { position: a.into(), color });
    out_vertices.push(Vertex { position: b.into(), color });
    out_vertices.push(Vertex { position: c.into(), color });
    out_indices.extend_from_slice(&[base, base + 1, base + 2]);
}

/// Axis-aligned filled rectangle in screen-space pixels, for UI chrome
/// (toolbar buttons, dividers) that shares the same render pass as strokes.
pub fn push_rect(
    top_left: Vec2,
    size: Vec2,
    color: [f32; 4],
    out_vertices: &mut Vec<Vertex>,
    out_indices: &mut Vec<u32>,
) {
    let base = out_vertices.len() as u32;
    out_vertices.push(Vertex { position: top_left.into(), color });
    out_vertices.push(Vertex { position: (top_left + Vec2::new(0.0, size.y)).into(), color });
    out_vertices.push(Vertex { position: (top_left + Vec2::new(size.x, 0.0)).into(), color });
    out_vertices.push(Vertex { position: (top_left + size).into(), color });
    out_indices.extend_from_slice(&[
        base, base + 1, base + 2,
        base + 2, base + 1, base + 3,
    ]);
}

/// Filled circle approximated with a triangle fan, for round UI glyphs
/// (tool icons drawn without a font renderer) and the eraser cursor.
pub fn push_circle(
    center: Vec2,
    radius: f32,
    color: [f32; 4],
    segments: usize,
    out_vertices: &mut Vec<Vertex>,
    out_indices: &mut Vec<u32>,
) {
    let base = out_vertices.len() as u32;
    out_vertices.push(Vertex { position: center.into(), color });
    for i in 0..=segments {
        let theta = (i as f32 / segments as f32) * std::f32::consts::TAU;
        let p = center + Vec2::new(theta.cos(), theta.sin()) * radius;
        out_vertices.push(Vertex { position: p.into(), color });
    }
    for i in 1..=segments as u32 {
        out_indices.extend_from_slice(&[base, base + i, base + i + 1]);
    }
}

/// A filled ring wedge (annulus sector) between `inner_r` and `outer_r`,
/// spanning `[start_angle, end_angle)` radians — the building block for the
/// radial floating menu ([`crate::ui`]).
#[allow(clippy::too_many_arguments)]
pub fn push_ring_wedge(
    center: Vec2,
    inner_r: f32,
    outer_r: f32,
    start_angle: f32,
    end_angle: f32,
    color: [f32; 4],
    segments: usize,
    out_vertices: &mut Vec<Vertex>,
    out_indices: &mut Vec<u32>,
) {
    let base = out_vertices.len() as u32;
    for i in 0..=segments {
        let t = i as f32 / segments as f32;
        let theta = start_angle + (end_angle - start_angle) * t;
        let dir = Vec2::new(theta.cos(), theta.sin());
        out_vertices.push(Vertex { position: (center + dir * inner_r).into(), color });
        out_vertices.push(Vertex { position: (center + dir * outer_r).into(), color });
    }
    for i in 0..segments as u32 {
        let a = base + i * 2;
        out_indices.extend_from_slice(&[a, a + 1, a + 2, a + 2, a + 1, a + 3]);
    }
}

/// Rounded-rect-ish thick line segment in screen space, for UI glyphs (e.g.
/// a hand/pen icon stroke) drawn without a font renderer.
pub fn push_line(
    a: Vec2,
    b: Vec2,
    width: f32,
    color: [f32; 4],
    out_vertices: &mut Vec<Vertex>,
    out_indices: &mut Vec<u32>,
) {
    let dir = (b - a).normalize_or_zero();
    if dir == Vec2::ZERO {
        return;
    }
    let normal = Vec2::new(-dir.y, dir.x) * (width / 2.0);
    let base = out_vertices.len() as u32;
    out_vertices.push(Vertex { position: (a + normal).into(), color });
    out_vertices.push(Vertex { position: (a - normal).into(), color });
    out_vertices.push(Vertex { position: (b + normal).into(), color });
    out_vertices.push(Vertex { position: (b - normal).into(), color });
    out_indices.extend_from_slice(&[
        base, base + 1, base + 2,
        base + 2, base + 1, base + 3,
    ]);
}
