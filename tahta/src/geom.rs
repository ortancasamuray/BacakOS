//! Small 2D geometry helpers shared by the draggable drafting-tool
//! overlays (`ruler`, `setsquare`, ...).

use glam::Vec2;

pub fn distance_to_segment(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    p.distance(project_onto_segment(p, a, b))
}

/// Perpendicular projection of `p` onto segment `a..b`, clamped to the
/// segment's ends.
pub fn project_onto_segment(p: Vec2, a: Vec2, b: Vec2) -> Vec2 {
    let ab = b - a;
    let len_sq = ab.length_squared().max(1e-6);
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    a + ab * t
}

/// True if `p` lies inside (or on) triangle `a, b, c`, any winding order.
pub fn point_in_triangle(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> bool {
    let d1 = sign(p, a, b);
    let d2 = sign(p, b, c);
    let d3 = sign(p, c, a);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

fn sign(p1: Vec2, p2: Vec2, p3: Vec2) -> f32 {
    (p1.x - p3.x) * (p2.y - p3.y) - (p2.x - p3.x) * (p1.y - p3.y)
}
