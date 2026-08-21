//! Radial floating palette: opened by a two-finger tap so a teacher never
//! has to reach the fixed toolbar on a 65-86" panel. 8 wedges — 4 quick
//! colors, 3 width presets, 1 eraser shortcut — arranged in a full circle
//! around the tap point. Selection is tap-to-pick: once open, the very
//! next touch anywhere either lands on a wedge (picks it) or elsewhere
//! (cancels) — see `input_handler`'s module docs for why this isn't a
//! single-finger long-press.

use glam::Vec2;

use crate::stroke::{push_ring_wedge, Vertex};

const INNER_RADIUS: f32 = 42.0;
const OUTER_RADIUS: f32 = 132.0;
const WEDGE_SEGMENTS: usize = 10;

const COLOR_IDLE: [f32; 4] = [0.16, 0.17, 0.21, 0.95];
const COLOR_HOVER: [f32; 4] = [0.23, 0.51, 0.96, 0.95];

// Per spec: black/red/blue/green quick colors. Note: pure black is low
// contrast on this app's dark chalkboard background — kept as specified
// rather than silently substituted; swap `QUICK_COLORS[0]` if a lighter
// "black" (e.g. dark slate) is preferred for this canvas.
pub const QUICK_COLORS: [[f32; 4]; 4] = [
    [0.05, 0.05, 0.06, 1.0], // black
    [0.95, 0.25, 0.25, 1.0], // red
    [0.25, 0.55, 0.95, 1.0], // blue
    [0.30, 0.85, 0.35, 1.0], // green
];

pub const QUICK_WIDTHS: [f32; 3] = [3.0, 8.0, 16.0];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RadialAction {
    Color([f32; 4]),
    Width(f32),
    Eraser,
}

struct Item {
    action: RadialAction,
    swatch_color: [f32; 4],
}

pub struct RadialMenu {
    pub center: Vec2,
    items: Vec<Item>,
}

impl RadialMenu {
    pub fn open(center: Vec2) -> Self {
        let mut items = Vec::with_capacity(8);
        for &c in &QUICK_COLORS {
            items.push(Item { action: RadialAction::Color(c), swatch_color: c });
        }
        for &w in &QUICK_WIDTHS {
            items.push(Item { action: RadialAction::Width(w), swatch_color: [0.85, 0.85, 0.88, 1.0] });
        }
        items.push(Item { action: RadialAction::Eraser, swatch_color: [0.9, 0.9, 0.92, 1.0] });
        Self { center, items }
    }

    fn wedge_angle(&self) -> f32 {
        std::f32::consts::TAU / self.items.len() as f32
    }

    /// Which wedge (if any) `point` currently lands on — `None` inside the
    /// inner deadzone or outside the outer ring (both cancel on release).
    pub fn hit_index(&self, point: Vec2) -> Option<usize> {
        let d = point - self.center;
        let dist = d.length();
        if dist < INNER_RADIUS || dist > OUTER_RADIUS {
            return None;
        }
        let angle = d.y.atan2(d.x);
        let normalized = if angle < 0.0 { angle + std::f32::consts::TAU } else { angle };
        Some(((normalized / self.wedge_angle()) as usize).min(self.items.len() - 1))
    }

    pub fn action(&self, index: usize) -> Option<RadialAction> {
        self.items.get(index).map(|i| i.action)
    }

    /// Which action (if any) a tap at `point` picks — `None` if it lands in
    /// the inner deadzone or outside the ring (both cancel the menu).
    pub fn action_at(&self, point: Vec2) -> Option<RadialAction> {
        self.hit_index(point).and_then(|i| self.action(i))
    }

    pub fn render(&self, hover: Option<usize>, out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) {
        let step = self.wedge_angle();
        for (i, item) in self.items.iter().enumerate() {
            let start = i as f32 * step;
            let color = if hover == Some(i) { COLOR_HOVER } else { COLOR_IDLE };
            push_ring_wedge(
                self.center,
                INNER_RADIUS,
                OUTER_RADIUS,
                start,
                start + step,
                color,
                WEDGE_SEGMENTS,
                out_vertices,
                out_indices,
            );
            // Swatch dot mid-wedge showing what it picks.
            let mid_angle = start + step / 2.0;
            let mid_r = (INNER_RADIUS + OUTER_RADIUS) / 2.0;
            let dot_center = self.center + Vec2::new(mid_angle.cos(), mid_angle.sin()) * mid_r;
            crate::stroke::push_circle(dot_center, 14.0, item.swatch_color, 16, out_vertices, out_indices);
        }
    }
}
