//! Floating tool/brush/action bar — the *only* control surface this app
//! needs, by design: interactive flat panels have no keyboard, so every
//! mode change (tool, brush, color, undo, clear, dual-zone) must be
//! reachable by tapping a button here. Buttons are sized well above the
//! 48-64px floor for finger/chalk-stylus hit targets.

use glam::Vec2;

use crate::board::{BoardBackground, GridPattern};
use crate::brush::BrushType;
use crate::stroke::{push_circle, push_line, push_rect, Vertex};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Pen,
    Hand,
    Eraser,
    /// Drag from center outward; releases a perfect circle stroke sized to
    /// the drag radius — see `input_handler`'s `compass_preview`.
    Compass,
}

/// Which erase behavior the Eraser tool currently uses — toggled by
/// tapping the Eraser toolbar button again while it's already selected
/// (the only discoverable way to reach it without a keyboard).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EraserMode {
    /// Rubs out only the ink within the eraser's radius (default) —
    /// splits a stroke rather than deleting it whole.
    Area,
    /// Deletes an entire stroke/object if any part of it is touched.
    Object,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ToolbarAction {
    SelectTool(Tool),
    /// Ballpoint → Calligraphy → Highlighter → LaserPointer → ...
    CycleBrush,
    /// Steps through the fixed 5-color palette.
    CycleColor,
    Undo,
    Clear,
    ToggleZone,
    /// Shows/hides the draggable, rotatable straightedge overlay.
    ToggleRuler,
    /// Shows/hides the draggable, rotatable set-square overlay.
    ToggleSetSquare,
    /// Shows/hides the draggable, rotatable protractor overlay.
    ToggleProtractor,
    /// Shows/hides the draggable calculator panel.
    ToggleCalculator,
    /// Shows/hides the draggable stopwatch panel.
    ToggleStopwatch,
    /// Shows/hides the draggable dice panel.
    ToggleDice,
    /// Steps through the curated background+grid presets.
    CycleBackground,
    PrevPage,
    NextPage,
}

/// What the toolbar needs to know to draw each button's current-state
/// glyph (which tool/brush/color/page is active, whether dual-zone is on).
pub struct ToolbarState {
    pub active_tool: Tool,
    pub brush_type: BrushType,
    pub color: [f32; 4],
    pub zone_enabled: bool,
    pub eraser_mode: EraserMode,
    pub background: BoardBackground,
    pub grid: GridPattern,
    pub page_index: usize,
    pub page_count: usize,
    pub ruler_visible: bool,
    pub setsquare_visible: bool,
    pub protractor_visible: bool,
    pub calculator_visible: bool,
    pub stopwatch_visible: bool,
    pub dice_visible: bool,
}

// Scaled down toward the dock's ~36px icon / ~10px gap proportions
// (`bacak-compositor/src/state.rs::dock_tiles_for`, dock_height=56 ->
// tile≈35.84px, gap≈10.08px) while staying at the 48-64px floor a touch/
// chalk-stylus target needs — going all the way to the dock's mouse-first
// 36px would be too small to reliably tap on a large panel.
const BUTTON_SIZE: f32 = 56.0;
const BUTTON_GAP: f32 = 10.0;
const BAR_MARGIN: f32 = 10.0;
const BAR_BOTTOM_MARGIN: f32 = 24.0;

const COLOR_BAR_BG: [f32; 4] = [0.12, 0.13, 0.17, 0.92];
const COLOR_BUTTON_IDLE: [f32; 4] = [0.22, 0.24, 0.30, 1.0];
const COLOR_BUTTON_ACTIVE: [f32; 4] = [0.23, 0.51, 0.96, 1.0]; // accent blue
const COLOR_GLYPH: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_SEPARATOR: [f32; 4] = [1.0, 1.0, 1.0, 0.10];

const BUTTONS: [ToolbarAction; 18] = [
    ToolbarAction::SelectTool(Tool::Pen),
    ToolbarAction::SelectTool(Tool::Hand),
    ToolbarAction::SelectTool(Tool::Eraser),
    ToolbarAction::SelectTool(Tool::Compass),
    ToolbarAction::ToggleRuler,
    ToolbarAction::ToggleSetSquare,
    ToolbarAction::ToggleProtractor,
    ToolbarAction::ToggleCalculator,
    ToolbarAction::ToggleStopwatch,
    ToolbarAction::ToggleDice,
    ToolbarAction::CycleBrush,
    ToolbarAction::CycleColor,
    ToolbarAction::Undo,
    ToolbarAction::Clear,
    ToolbarAction::ToggleZone,
    ToolbarAction::CycleBackground,
    ToolbarAction::PrevPage,
    ToolbarAction::NextPage,
];

/// Index right before which a thin separator is drawn, to visually group
/// tool-select (0-3) / drafting+widget tools (4-9) / brush+color (10-11) /
/// actions (12-14).
const SEPARATOR_BEFORE: [usize; 4] = [4, 10, 12, 15];

/// Bottom-center floating toolbar. Screen-space, never affected by canvas
/// pan/zoom, recomputed each frame from the current window size.
pub struct Toolbar {
    bar_rect: (Vec2, Vec2), // (top_left, size)
    button_rects: [(Vec2, Vec2); BUTTONS.len()],
}

impl Toolbar {
    pub fn layout(screen_size: Vec2) -> Self {
        let content_w = BUTTON_SIZE * BUTTONS.len() as f32 + BUTTON_GAP * (BUTTONS.len() as f32 - 1.0);
        let bar_w = content_w + BAR_MARGIN * 2.0;
        let bar_h = BUTTON_SIZE + BAR_MARGIN * 2.0;
        let bar_top_left = Vec2::new(
            (screen_size.x - bar_w) / 2.0,
            screen_size.y - bar_h - BAR_BOTTOM_MARGIN,
        );

        let mut button_rects = [(Vec2::ZERO, Vec2::ZERO); BUTTONS.len()];
        for (i, rect) in button_rects.iter_mut().enumerate() {
            let x = bar_top_left.x + BAR_MARGIN + i as f32 * (BUTTON_SIZE + BUTTON_GAP);
            let y = bar_top_left.y + BAR_MARGIN;
            *rect = (Vec2::new(x, y), Vec2::splat(BUTTON_SIZE));
        }

        Self { bar_rect: (bar_top_left, Vec2::new(bar_w, bar_h)), button_rects }
    }

    /// True if `point` lands anywhere on the toolbar's background — used to
    /// swallow touches so they never start a stroke, even between buttons.
    pub fn contains(&self, point: Vec2) -> bool {
        rect_contains(self.bar_rect, point)
    }

    /// Which action (if any) `point` lands on.
    pub fn hit_test(&self, point: Vec2) -> Option<ToolbarAction> {
        self.button_rects
            .iter()
            .zip(BUTTONS)
            .find(|(rect, _)| rect_contains(**rect, point))
            .map(|(_, action)| action)
    }

    pub fn render(&self, state: &ToolbarState, out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) {
        push_rect(self.bar_rect.0, self.bar_rect.1, COLOR_BAR_BG, out_vertices, out_indices);

        for (i, (rect, action)) in self.button_rects.iter().zip(BUTTONS).enumerate() {
            if SEPARATOR_BEFORE.contains(&i) {
                let x = rect.0.x - BUTTON_GAP / 2.0;
                push_rect(
                    Vec2::new(x - 1.0, self.bar_rect.0.y + 10.0),
                    Vec2::new(2.0, self.bar_rect.1.y - 20.0),
                    COLOR_SEPARATOR,
                    out_vertices,
                    out_indices,
                );
            }

            let active = is_active(action, state);
            let color = if active { COLOR_BUTTON_ACTIVE } else { COLOR_BUTTON_IDLE };
            push_rect(rect.0, rect.1, color, out_vertices, out_indices);
            draw_glyph(action, *rect, state, out_vertices, out_indices);
        }

        self.render_page_dots(state, out_vertices, out_indices);
    }

    /// A small row of dots above the page-nav buttons — the only "which
    /// page am I on" indicator, since this app has no text renderer yet.
    fn render_page_dots(&self, state: &ToolbarState, out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) {
        if state.page_count <= 1 {
            return;
        }
        let dot_r = 4.0;
        let gap = 14.0;
        let count = state.page_count.min(20) as f32;
        let total_w = (count - 1.0) * gap;
        let start_x = self.bar_rect.0.x + self.bar_rect.1.x / 2.0 - total_w / 2.0;
        let y = self.bar_rect.0.y - 14.0;
        for i in 0..state.page_count.min(20) {
            let color = if i == state.page_index { COLOR_BUTTON_ACTIVE } else { COLOR_BUTTON_IDLE };
            push_circle(Vec2::new(start_x + i as f32 * gap, y), dot_r, color, 10, out_vertices, out_indices);
        }
    }
}

fn is_active(action: ToolbarAction, state: &ToolbarState) -> bool {
    match action {
        ToolbarAction::SelectTool(t) => t == state.active_tool,
        ToolbarAction::ToggleZone => state.zone_enabled,
        ToolbarAction::ToggleRuler => state.ruler_visible,
        ToolbarAction::ToggleSetSquare => state.setsquare_visible,
        ToolbarAction::ToggleProtractor => state.protractor_visible,
        ToolbarAction::ToggleCalculator => state.calculator_visible,
        ToolbarAction::ToggleStopwatch => state.stopwatch_visible,
        ToolbarAction::ToggleDice => state.dice_visible,
        _ => false,
    }
}

fn rect_contains((top_left, size): (Vec2, Vec2), point: Vec2) -> bool {
    point.x >= top_left.x
        && point.x <= top_left.x + size.x
        && point.y >= top_left.y
        && point.y <= top_left.y + size.y
}

fn draw_glyph(
    action: ToolbarAction,
    (top_left, size): (Vec2, Vec2),
    state: &ToolbarState,
    out_vertices: &mut Vec<Vertex>,
    out_indices: &mut Vec<u32>,
) {
    let center = top_left + size / 2.0;
    let r = size.x * 0.28;
    match action {
        ToolbarAction::SelectTool(Tool::Pen) => {
            let a = center + Vec2::new(-r, r);
            let b = center + Vec2::new(r, -r);
            push_line(a, b, 5.0, COLOR_GLYPH, out_vertices, out_indices);
            push_circle(b, 4.0, COLOR_GLYPH, 12, out_vertices, out_indices);
        }
        ToolbarAction::SelectTool(Tool::Hand) => {
            push_line(center + Vec2::new(-r, 0.0), center + Vec2::new(r, 0.0), 5.0, COLOR_GLYPH, out_vertices, out_indices);
            push_line(center + Vec2::new(0.0, -r), center + Vec2::new(0.0, r), 5.0, COLOR_GLYPH, out_vertices, out_indices);
            push_circle(center, 5.0, COLOR_GLYPH, 12, out_vertices, out_indices);
        }
        ToolbarAction::SelectTool(Tool::Eraser) => {
            push_rect(center - Vec2::splat(r * 0.55), Vec2::splat(r * 1.1), COLOR_GLYPH, out_vertices, out_indices);
            match state.eraser_mode {
                // Area mode: a dashed-looking ring (short arc segments)
                // around the eraser block, hinting "only this radius".
                EraserMode::Area => {
                    for i in 0..8 {
                        if i % 2 == 0 {
                            continue;
                        }
                        let theta = i as f32 / 8.0 * std::f32::consts::TAU;
                        let dir = Vec2::new(theta.cos(), theta.sin());
                        push_line(center + dir * (r * 0.85), center + dir * r, 2.5, COLOR_GLYPH, out_vertices, out_indices);
                    }
                }
                // Object mode: a solid outline box around the eraser
                // block, hinting "whole object".
                EraserMode::Object => {
                    let outline = [COLOR_GLYPH[0], COLOR_GLYPH[1], COLOR_GLYPH[2], 0.5];
                    let s = r * 1.7;
                    let t = 2.5;
                    push_rect(center + Vec2::new(-s / 2.0, -s / 2.0), Vec2::new(s, t), outline, out_vertices, out_indices);
                    push_rect(center + Vec2::new(-s / 2.0, s / 2.0 - t), Vec2::new(s, t), outline, out_vertices, out_indices);
                    push_rect(center + Vec2::new(-s / 2.0, -s / 2.0), Vec2::new(t, s), outline, out_vertices, out_indices);
                    push_rect(center + Vec2::new(s / 2.0 - t, -s / 2.0), Vec2::new(t, s), outline, out_vertices, out_indices);
                }
            }
        }
        ToolbarAction::SelectTool(Tool::Compass) => {
            // A drafting compass: two legs meeting at a pivot dot, one leg
            // tipped with a small point.
            let pivot = center + Vec2::new(0.0, -r * 0.8);
            let leg_a_end = center + Vec2::new(-r * 0.7, r * 0.8);
            let leg_b_end = center + Vec2::new(r * 0.7, r * 0.8);
            push_line(pivot, leg_a_end, 3.5, COLOR_GLYPH, out_vertices, out_indices);
            push_line(pivot, leg_b_end, 3.5, COLOR_GLYPH, out_vertices, out_indices);
            push_circle(pivot, 3.0, COLOR_GLYPH, 10, out_vertices, out_indices);
            push_circle(leg_a_end, 2.0, COLOR_GLYPH, 8, out_vertices, out_indices);
        }
        ToolbarAction::CycleBrush => draw_brush_glyph(state.brush_type, center, r, out_vertices, out_indices),
        ToolbarAction::CycleColor => {
            push_circle(center, r * 0.75, state.color, 20, out_vertices, out_indices);
        }
        ToolbarAction::Undo => {
            // Left-pointing chevron ("<") — simplest arrow without a font.
            push_line(center + Vec2::new(r * 0.5, -r), center + Vec2::new(-r * 0.5, 0.0), 5.0, COLOR_GLYPH, out_vertices, out_indices);
            push_line(center + Vec2::new(-r * 0.5, 0.0), center + Vec2::new(r * 0.5, r), 5.0, COLOR_GLYPH, out_vertices, out_indices);
        }
        ToolbarAction::Clear => {
            push_line(center + Vec2::new(-r, -r), center + Vec2::new(r, r), 5.0, COLOR_GLYPH, out_vertices, out_indices);
            push_line(center + Vec2::new(-r, r), center + Vec2::new(r, -r), 5.0, COLOR_GLYPH, out_vertices, out_indices);
        }
        ToolbarAction::ToggleZone => {
            let gap = 3.0;
            let half_w = r * 0.85;
            push_rect(center + Vec2::new(-half_w, -r), Vec2::new(half_w - gap, r * 2.0), COLOR_GLYPH, out_vertices, out_indices);
            let outline_color = [COLOR_GLYPH[0], COLOR_GLYPH[1], COLOR_GLYPH[2], 0.35];
            push_rect(center + Vec2::new(gap, -r), Vec2::new(half_w - gap, r * 2.0), outline_color, out_vertices, out_indices);
        }
        ToolbarAction::ToggleRuler => {
            // A tilted ruler: a rect with a few tick marks.
            let w = r * 1.7;
            let h = r * 0.6;
            push_rect(center - Vec2::new(w / 2.0, h / 2.0), Vec2::new(w, h), COLOR_GLYPH, out_vertices, out_indices);
            let tick_color = [0.15, 0.15, 0.18, 0.8];
            for i in 0..4 {
                let x = center.x - w / 2.0 + w * (i as f32 + 1.0) / 5.0;
                push_line(Vec2::new(x, center.y - h / 2.0), Vec2::new(x, center.y), 1.5, tick_color, out_vertices, out_indices);
            }
        }
        ToolbarAction::ToggleSetSquare => {
            // A little right triangle.
            let a = center + Vec2::new(-r, r);
            let b = center + Vec2::new(r, r);
            let c = center + Vec2::new(-r, -r);
            push_line(a, b, 3.0, COLOR_GLYPH, out_vertices, out_indices);
            push_line(a, c, 3.0, COLOR_GLYPH, out_vertices, out_indices);
            push_line(b, c, 3.0, COLOR_GLYPH, out_vertices, out_indices);
        }
        ToolbarAction::ToggleProtractor => {
            // A small half-circle (dome) over a flat baseline.
            const ARC_STEPS: usize = 8;
            for i in 0..ARC_STEPS {
                let t0 = i as f32 / ARC_STEPS as f32 * std::f32::consts::PI;
                let t1 = (i + 1) as f32 / ARC_STEPS as f32 * std::f32::consts::PI;
                let p0 = center + Vec2::new(t0.cos(), -t0.sin()) * r;
                let p1 = center + Vec2::new(t1.cos(), -t1.sin()) * r;
                push_line(p0, p1, 3.0, COLOR_GLYPH, out_vertices, out_indices);
            }
            push_line(center + Vec2::new(-r, 0.0), center + Vec2::new(r, 0.0), 3.0, COLOR_GLYPH, out_vertices, out_indices);
        }
        ToolbarAction::ToggleCalculator => {
            // A little calculator body: outline, a display line near the
            // top, and a 2x2 grid of key dots below.
            let w = r * 1.5;
            let h = r * 1.9;
            let top_left = center - Vec2::new(w / 2.0, h / 2.0);
            let t = 2.0;
            push_rect(top_left, Vec2::new(w, t), COLOR_GLYPH, out_vertices, out_indices);
            push_rect(top_left + Vec2::new(0.0, h - t), Vec2::new(w, t), COLOR_GLYPH, out_vertices, out_indices);
            push_rect(top_left, Vec2::new(t, h), COLOR_GLYPH, out_vertices, out_indices);
            push_rect(top_left + Vec2::new(w - t, 0.0), Vec2::new(t, h), COLOR_GLYPH, out_vertices, out_indices);
            push_rect(top_left + Vec2::new(t, t * 1.5), Vec2::new(w - t * 2.0, h * 0.22), [0.55, 0.95, 0.65, 0.9], out_vertices, out_indices);
            for row in 0..2 {
                for col in 0..2 {
                    let x = top_left.x + w * (0.28 + col as f32 * 0.44);
                    let y = top_left.y + h * (0.62 + row as f32 * 0.3);
                    push_circle(Vec2::new(x, y), 1.6, COLOR_GLYPH, 8, out_vertices, out_indices);
                }
            }
        }
        ToolbarAction::ToggleStopwatch => {
            // A stopwatch: circle body, a top knob, and a hand pointing to
            // ~2 o'clock (mid-count, not 12, so it doesn't read as a plain
            // clock).
            push_line(center + Vec2::new(-r * 0.35, -r * 1.15), center + Vec2::new(r * 0.35, -r * 1.15), 2.5, COLOR_GLYPH, out_vertices, out_indices);
            for i in 0..24 {
                let theta = i as f32 / 24.0 * std::f32::consts::TAU;
                let theta1 = (i + 1) as f32 / 24.0 * std::f32::consts::TAU;
                let p0 = center + Vec2::new(theta.cos(), theta.sin()) * r;
                let p1 = center + Vec2::new(theta1.cos(), theta1.sin()) * r;
                push_line(p0, p1, 2.0, COLOR_GLYPH, out_vertices, out_indices);
            }
            push_line(center, center + Vec2::new(r * 0.6, -r * 0.5), 2.0, COLOR_GLYPH, out_vertices, out_indices);
        }
        ToolbarAction::ToggleDice => {
            let s = r * 1.6;
            push_rect(center - Vec2::splat(s / 2.0), Vec2::splat(s), COLOR_GLYPH, out_vertices, out_indices);
            let pip_color = [0.15, 0.15, 0.18, 1.0];
            for &(fx, fy) in &[(0.25, 0.25), (0.75, 0.25), (0.5, 0.5), (0.25, 0.75), (0.75, 0.75)] {
                push_circle(center + Vec2::new((fx - 0.5) * s, (fy - 0.5) * s), 1.8, pip_color, 8, out_vertices, out_indices);
            }
        }
        ToolbarAction::CycleBackground => {
            push_rect(center - Vec2::splat(r), Vec2::splat(r * 2.0), state.background.color(), out_vertices, out_indices);
            let border = [COLOR_GLYPH[0], COLOR_GLYPH[1], COLOR_GLYPH[2], 0.4];
            let t = 2.0;
            push_rect(center + Vec2::new(-r, -r), Vec2::new(r * 2.0, t), border, out_vertices, out_indices);
            push_rect(center + Vec2::new(-r, r - t), Vec2::new(r * 2.0, t), border, out_vertices, out_indices);

            let grid_line = [border[0], border[1], border[2], 0.6];
            match state.grid {
                GridPattern::Plain => {}
                GridPattern::Lined => {
                    push_line(center + Vec2::new(-r * 0.7, 0.0), center + Vec2::new(r * 0.7, 0.0), 1.5, grid_line, out_vertices, out_indices);
                }
                GridPattern::Checkered => {
                    push_line(center + Vec2::new(-r * 0.7, 0.0), center + Vec2::new(r * 0.7, 0.0), 1.5, grid_line, out_vertices, out_indices);
                    push_line(center + Vec2::new(0.0, -r * 0.7), center + Vec2::new(0.0, r * 0.7), 1.5, grid_line, out_vertices, out_indices);
                }
            }
        }
        ToolbarAction::PrevPage => {
            push_line(center + Vec2::new(r * 0.4, -r), center + Vec2::new(-r * 0.4, 0.0), 5.0, COLOR_GLYPH, out_vertices, out_indices);
            push_line(center + Vec2::new(-r * 0.4, 0.0), center + Vec2::new(r * 0.4, r), 5.0, COLOR_GLYPH, out_vertices, out_indices);
            push_line(center + Vec2::new(r * 0.55, -r), center + Vec2::new(r * 0.55, r), 3.0, COLOR_GLYPH, out_vertices, out_indices);
        }
        ToolbarAction::NextPage => {
            push_line(center + Vec2::new(-r * 0.4, -r), center + Vec2::new(r * 0.4, 0.0), 5.0, COLOR_GLYPH, out_vertices, out_indices);
            push_line(center + Vec2::new(r * 0.4, 0.0), center + Vec2::new(-r * 0.4, r), 5.0, COLOR_GLYPH, out_vertices, out_indices);
            push_line(center + Vec2::new(-r * 0.55, -r), center + Vec2::new(-r * 0.55, r), 3.0, COLOR_GLYPH, out_vertices, out_indices);
        }
    }
}

fn draw_brush_glyph(brush: BrushType, center: Vec2, r: f32, out_vertices: &mut Vec<Vertex>, out_indices: &mut Vec<u32>) {
    match brush {
        BrushType::Ballpoint => {
            push_line(center + Vec2::new(-r, r), center + Vec2::new(r, -r), 3.0, COLOR_GLYPH, out_vertices, out_indices);
        }
        BrushType::Calligraphy => {
            // A wedge: thick at one end, thin at the other, drawn as two
            // stacked lines of different width along the same diagonal.
            push_line(center + Vec2::new(-r, r), center, 7.0, COLOR_GLYPH, out_vertices, out_indices);
            push_line(center, center + Vec2::new(r, -r), 2.5, COLOR_GLYPH, out_vertices, out_indices);
        }
        BrushType::Highlighter => {
            let translucent = [COLOR_GLYPH[0], COLOR_GLYPH[1], COLOR_GLYPH[2], 0.55];
            push_rect(center - Vec2::new(r, r * 0.4), Vec2::new(r * 2.0, r * 0.8), translucent, out_vertices, out_indices);
        }
        BrushType::LaserPointer => {
            push_circle(center, 4.0, COLOR_GLYPH, 12, out_vertices, out_indices);
            for i in 0..6 {
                let theta = i as f32 / 6.0 * std::f32::consts::TAU;
                let dir = Vec2::new(theta.cos(), theta.sin());
                push_line(center + dir * (r * 0.55), center + dir * r, 2.0, COLOR_GLYPH, out_vertices, out_indices);
            }
        }
    }
}
