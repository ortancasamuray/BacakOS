//! Draggable "Grafik" panel — plots up to three `y = f(x)` functions live
//! as they're typed on its own math keypad (`expr.rs` parses them), with
//! sliders for the parameters `a`, `b`, `c` (`y = a·sin(x) + b` and drag
//! `a`). Same touch-only UI-panel pattern as `calculator.rs`/`textbox.rs`:
//! it swallows every touch that lands on it.
//!
//! "Tahtaya bas" stamps the current plot onto the page as ordinary ink
//! strokes (axes with arrows, tick marks, numbers in a tiny single-line
//! stroke font, the curves) — so undo, autosave, the eraser and PDF
//! export all just work, with no new page object type. The panel keeps
//! its state while hidden so reopening it continues where it left off.

use glam::Vec2;

use crate::expr::{self, Expr};
use crate::font_atlas::{fit_text, FontAtlas, GlyphVertex};
use crate::stroke::{push_circle, push_line, push_rect, Vertex};

const BTN: f32 = 52.0;
const KEY_H: f32 = 46.0;
const GAP: f32 = 6.0;
const COLS: usize = 10;
const HEADER_H: f32 = 36.0;
const PADDING: f32 = 12.0;
const ROW_H: f32 = 44.0;
const PLOT_H: f32 = 300.0;
const SLIDER_H: f32 = 40.0;
const MAX_CHARS: usize = 40;

/// Slider range for `a`, `b`, `c`, snapped to 0.1.
const PARAM_MIN: f64 = -5.0;
const PARAM_MAX: f64 = 5.0;
const DEFAULT_PX_PER_UNIT: f64 = 30.0;
/// Stamped graphs are drawn this much larger than the panel's preview —
/// big enough to read from the back of a classroom.
const STAMP_SCALE: f32 = 1.5;

const COLOR_PANEL_BG: [f32; 4] = [0.12, 0.13, 0.17, 0.95];
const COLOR_HEADER: [f32; 4] = [0.18, 0.19, 0.24, 1.0];
const COLOR_ROW_BG: [f32; 4] = [0.05, 0.06, 0.08, 1.0];
const COLOR_ROW_ACTIVE: [f32; 4] = [0.23, 0.51, 0.96, 1.0];
const COLOR_TEXT: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_TEXT_DIM: [f32; 4] = [0.95, 0.95, 0.97, 0.45];
const COLOR_TEXT_ERROR: [f32; 4] = [0.98, 0.55, 0.40, 1.0];
const COLOR_PLOT_BG: [f32; 4] = [0.06, 0.07, 0.09, 1.0];
const COLOR_GRID: [f32; 4] = [1.0, 1.0, 1.0, 0.08];
const COLOR_AXIS: [f32; 4] = [1.0, 1.0, 1.0, 0.55];
const COLOR_TICK_LABEL: [f32; 4] = [1.0, 1.0, 1.0, 0.55];
const COLOR_KEY_IDLE: [f32; 4] = [0.22, 0.24, 0.30, 1.0];
const COLOR_KEY_FUNC: [f32; 4] = [0.28, 0.30, 0.40, 1.0];
const COLOR_KEY_SPECIAL: [f32; 4] = [0.23, 0.51, 0.96, 1.0];
const COLOR_KEY_CLEAR: [f32; 4] = [0.55, 0.30, 0.30, 1.0];
const COLOR_KEY_STAMP: [f32; 4] = [0.98, 0.55, 0.15, 1.0];

/// Curve colors — red, blue, green from the pen palette (`brush::PALETTE`),
/// so a stamped graph matches ink the teacher draws next to it.
const CURVE_COLORS: [[f32; 4]; 3] = [crate::brush::PALETTE[1], crate::brush::PALETTE[2], crate::brush::PALETTE[3]];
const PARAM_NAMES: [&str; 3] = ["a", "b", "c"];
const SUBSCRIPTS: [&str; 3] = ["₁", "₂", "₃"];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Key {
    /// Inserts `text`; the key shows `label`.
    Ins { text: &'static str, label: &'static str },
    Backspace,
    Clear,
    ZoomOut,
    ZoomIn,
    Reset,
    Stamp,
}

const fn ins(text: &'static str, label: &'static str) -> Key {
    Key::Ins { text, label }
}

type Row = &'static [(Key, f32)];

const ROW_0: Row = &[
    (ins("7", "7"), 1.0), (ins("8", "8"), 1.0), (ins("9", "9"), 1.0), (ins("÷", "÷"), 1.0), (ins("(", "("), 1.0),
    (ins(")", ")"), 1.0), (ins("x", "x"), 1.0), (ins("a", "a"), 1.0), (ins("b", "b"), 1.0), (ins("c", "c"), 1.0),
];
const ROW_1: Row = &[
    (ins("4", "4"), 1.0), (ins("5", "5"), 1.0), (ins("6", "6"), 1.0), (ins("×", "×"), 1.0), (ins("sin(", "sin"), 1.0),
    (ins("cos(", "cos"), 1.0), (ins("tan(", "tan"), 1.0), (ins("√(", "√"), 1.0), (ins("^", "xⁿ"), 1.0), (Key::Backspace, 1.0),
];
const ROW_2: Row = &[
    (ins("1", "1"), 1.0), (ins("2", "2"), 1.0), (ins("3", "3"), 1.0), (ins("−", "−"), 1.0), (ins("²", "x²"), 1.0),
    (ins("π", "π"), 1.0), (ins("log(", "log"), 1.0), (ins("ln(", "ln"), 1.0), (ins("abs(", "|x|"), 1.0), (Key::Clear, 1.0),
];
const ROW_3: Row = &[
    (ins("0", "0"), 1.0), (ins(",", ","), 1.0), (ins("+", "+"), 1.0), (Key::ZoomOut, 1.0), (Key::ZoomIn, 1.0),
    (Key::Reset, 1.0), (Key::Stamp, 4.0),
];
const ROWS: &[Row] = &[ROW_0, ROW_1, ROW_2, ROW_3];

/// Inserted multi-character tokens that ⌫ removes in one go.
const TOKENS: &[&str] = &["sin(", "cos(", "tan(", "log(", "ln(", "abs(", "√("];

/// What a touch on the panel turned into — `input_handler` picks the
/// pointer session (and for `Stamp`/`Close`, acts) from this.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum GraphPress {
    /// Header grabbed: drag the panel.
    Move,
    /// Header's ✕.
    Close,
    /// Plot area grabbed: drag pans the view.
    Pan,
    /// Slider for parameter `0..3` grabbed (value already set).
    Slider(usize),
    /// "Tahtaya bas".
    Stamp,
    /// Anything else (a key, a formula row, padding) — already applied.
    Other,
}

/// One polyline to stamp onto the page, in screen space.
pub struct InkLine {
    pub points: Vec<Vec2>,
    pub color: [f32; 4],
    pub width: f32,
}

pub struct Graph {
    pub position: Vec2,
    formulas: [String; 3],
    parsed: [Option<Expr>; 3],
    active: usize,
    params: [f64; 3],
    /// World coordinates at the plot's center.
    center: (f64, f64),
    px_per_unit: f64,
}

impl Graph {
    pub fn new(screen_size: Vec2) -> Self {
        let mut g = Self {
            position: Vec2::ZERO,
            formulas: [String::new(), String::new(), String::new()],
            parsed: [None, None, None],
            active: 0,
            params: [1.0, 1.0, 1.0],
            center: (0.0, 0.0),
            px_per_unit: DEFAULT_PX_PER_UNIT,
        };
        let size = Vec2::new(g.panel_width(), g.panel_height());
        // Centered in the space above the bottom toolbar (~140px band).
        g.position = Vec2::new((screen_size.x - size.x) / 2.0, ((screen_size.y - 140.0 - size.y) / 2.0).max(16.0));
        g
    }

    fn panel_width(&self) -> f32 {
        PADDING * 2.0 + COLS as f32 * BTN + (COLS as f32 - 1.0) * GAP
    }

    fn inner_width(&self) -> f32 {
        self.panel_width() - PADDING * 2.0
    }

    /// Which of `a`, `b`, `c` the formulas use — unused sliders are drawn
    /// dimmed (but stay, so the keypad never shifts under a finger).
    fn used_params(&self) -> [bool; 3] {
        let mut used = [false; 3];
        for e in self.parsed.iter().flatten() {
            e.params_used(&mut used);
        }
        used
    }

    fn panel_height(&self) -> f32 {
        self.keys_top() - self.position.y + ROWS.len() as f32 * KEY_H + (ROWS.len() as f32 - 1.0) * GAP + PADDING
    }

    fn header_rect(&self) -> (Vec2, Vec2) {
        (self.position, Vec2::new(self.panel_width(), HEADER_H))
    }

    fn close_rect(&self) -> (Vec2, Vec2) {
        let s = HEADER_H;
        (self.position + Vec2::new(self.panel_width() - s, 0.0), Vec2::splat(s))
    }

    fn formula_rect(&self, i: usize) -> (Vec2, Vec2) {
        let y = self.position.y + HEADER_H + PADDING + i as f32 * (ROW_H + GAP);
        (Vec2::new(self.position.x + PADDING, y), Vec2::new(self.inner_width(), ROW_H))
    }

    fn plot_rect(&self) -> (Vec2, Vec2) {
        let y = self.position.y + HEADER_H + PADDING + 3.0 * (ROW_H + GAP);
        (Vec2::new(self.position.x + PADDING, y), Vec2::new(self.inner_width(), PLOT_H))
    }

    fn slider_rect(&self, param: usize) -> (Vec2, Vec2) {
        let (plot_pos, _) = self.plot_rect();
        let y = plot_pos.y + PLOT_H + GAP + param as f32 * (SLIDER_H + GAP);
        (Vec2::new(self.position.x + PADDING, y), Vec2::new(self.inner_width(), SLIDER_H))
    }

    /// Track span `(x_start, x_end)` inside a slider row.
    fn slider_track(&self, param: usize) -> (f32, f32, f32) {
        let (pos, size) = self.slider_rect(param);
        (pos.x + 70.0, pos.x + size.x - 90.0, pos.y + size.y / 2.0)
    }

    fn keys_top(&self) -> f32 {
        let (plot_pos, _) = self.plot_rect();
        plot_pos.y + PLOT_H + GAP + 3.0 * (SLIDER_H + GAP)
    }

    fn key_rect(&self, row_index: usize, key_index: usize) -> (Vec2, Vec2) {
        let row = ROWS[row_index];
        let span = |units: f32| units * BTN + (units - 1.0).max(0.0) * GAP;
        let mut x = self.position.x + PADDING;
        for &(_, units) in &row[..key_index] {
            x += span(units) + GAP;
        }
        let y = self.keys_top() + row_index as f32 * (KEY_H + GAP);
        (Vec2::new(x, y), Vec2::new(span(row[key_index].1), KEY_H))
    }

    pub fn contains(&self, p: Vec2) -> bool {
        rect_contains((self.position, Vec2::new(self.panel_width(), self.panel_height())), p)
    }

    pub fn drag_to(&mut self, p: Vec2, grab_offset: Vec2) {
        self.position = p - grab_offset;
    }

    pub fn press_at(&mut self, p: Vec2) -> GraphPress {
        if rect_contains(self.close_rect(), p) {
            return GraphPress::Close;
        }
        if rect_contains(self.header_rect(), p) {
            return GraphPress::Move;
        }
        for i in 0..3 {
            if rect_contains(self.formula_rect(i), p) {
                self.active = i;
                return GraphPress::Other;
            }
        }
        if rect_contains(self.plot_rect(), p) {
            return GraphPress::Pan;
        }
        for param in 0..3 {
            if rect_contains(self.slider_rect(param), p) {
                self.set_slider(param, p.x);
                return GraphPress::Slider(param);
            }
        }
        for (row_index, row) in ROWS.iter().enumerate() {
            for key_index in 0..row.len() {
                if rect_contains(self.key_rect(row_index, key_index), p) {
                    return self.press_key(row[key_index].0);
                }
            }
        }
        GraphPress::Other
    }

    fn press_key(&mut self, key: Key) -> GraphPress {
        let f = &mut self.formulas[self.active];
        match key {
            Key::Ins { text, .. } => {
                if f.chars().count() + text.chars().count() <= MAX_CHARS {
                    f.push_str(text);
                }
            }
            Key::Backspace => match TOKENS.iter().find(|t| f.ends_with(*t)) {
                Some(t) => f.truncate(f.len() - t.len()),
                None => {
                    f.pop();
                }
            },
            Key::Clear => f.clear(),
            Key::ZoomIn => self.px_per_unit = (self.px_per_unit * 1.5).min(2000.0),
            Key::ZoomOut => self.px_per_unit = (self.px_per_unit / 1.5).max(2.0),
            Key::Reset => {
                self.center = (0.0, 0.0);
                self.px_per_unit = DEFAULT_PX_PER_UNIT;
            }
            Key::Stamp => return GraphPress::Stamp,
        }
        self.parsed[self.active] = expr::parse(&self.formulas[self.active]).ok();
        GraphPress::Other
    }

    /// Slider drag: `param` follows the finger's x along its track.
    pub fn set_slider(&mut self, param: usize, x: f32) {
        let (x0, x1, _) = self.slider_track(param);
        let t = ((x - x0) / (x1 - x0)).clamp(0.0, 1.0) as f64;
        let v = PARAM_MIN + t * (PARAM_MAX - PARAM_MIN);
        self.params[param] = (v * 10.0).round() / 10.0;
    }

    /// Plot drag: moves the view by a screen-space delta.
    pub fn pan_by(&mut self, delta: Vec2) {
        self.center.0 -= delta.x as f64 / self.px_per_unit;
        self.center.1 += delta.y as f64 / self.px_per_unit;
    }

    pub fn has_curves(&self) -> bool {
        self.parsed.iter().any(|e| e.is_some())
    }

    /// The plot as ink lines for "Tahtaya bas", in screen space: the
    /// preview's world window drawn `STAMP_SCALE`× larger around the same
    /// center, kept on screen (`screen_size`, minus a band for the
    /// toolbar). `ink` is the axis/number color for the page background.
    pub fn stamp_lines(&self, screen_size: Vec2, ink: [f32; 4]) -> Vec<InkLine> {
        let (plot_pos, plot_size) = self.plot_rect();
        let size = plot_size * STAMP_SCALE;
        let center = plot_pos + plot_size / 2.0;
        let max = (screen_size - Vec2::new(0.0, 100.0) - size).max(Vec2::ZERO);
        let top_left = (center - size / 2.0).clamp(Vec2::ZERO, max);
        let view = View::new((top_left, size), self.center, self.px_per_unit * STAMP_SCALE as f64);

        let mut lines = Vec::new();
        let mut line = |points: Vec<Vec2>, color: [f32; 4], width: f32| {
            if points.len() >= 2 {
                lines.push(InkLine { points: densify(&points, 3.0), color, width });
            }
        };

        let step = nice_step(56.0 / view.ppu);
        let step_px = (step * view.ppu) as f32;
        let (x_axis_y, y_axis_x) = view.axis_positions();
        let (tl, br) = (view.rect.0, view.rect.0 + view.rect.1);
        const ARROW: f32 = 12.0;
        const TICK: f32 = 6.0;
        const DIGIT_H: f32 = 16.0;

        if let Some(y) = x_axis_y {
            line(vec![Vec2::new(tl.x, y), Vec2::new(br.x, y)], ink, 2.0);
            line(vec![Vec2::new(br.x - ARROW, y - ARROW * 0.6), Vec2::new(br.x, y), Vec2::new(br.x - ARROW, y + ARROW * 0.6)], ink, 2.0);
            for glyph in stroke_text("x", Vec2::new(br.x - DIGIT_H * 0.7, y + 10.0), DIGIT_H) {
                line(glyph, ink, 2.0);
            }
        }
        if let Some(x) = y_axis_x {
            line(vec![Vec2::new(x, br.y), Vec2::new(x, tl.y)], ink, 2.0);
            line(vec![Vec2::new(x - ARROW * 0.6, tl.y + ARROW), Vec2::new(x, tl.y), Vec2::new(x + ARROW * 0.6, tl.y + ARROW)], ink, 2.0);
            for glyph in stroke_text("y", Vec2::new(x + 10.0, tl.y), DIGIT_H) {
                line(glyph, ink, 2.0);
            }
        }

        // Ticks + numbers along each visible axis; the labels stay clear of
        // the arrowheads and the axis-name letters at the far ends.
        if let Some(y) = x_axis_y {
            for k in view.ticks_x(step) {
                let px = view.to_px(k as f64 * step, 0.0).x;
                if k == 0 || px < tl.x + 4.0 || px > br.x - ARROW * 2.5 {
                    continue;
                }
                line(vec![Vec2::new(px, y - TICK), Vec2::new(px, y + TICK)], ink, 2.0);
                if step_px >= 36.0 {
                    let label = format_tick(k as f64 * step, step);
                    let w = stroke_text_width(&label, DIGIT_H * 0.8);
                    for glyph in stroke_text(&label, Vec2::new(px - w / 2.0, y + TICK + 6.0), DIGIT_H * 0.8) {
                        line(glyph, ink, 1.6);
                    }
                }
            }
        }
        if let Some(x) = y_axis_x {
            for k in view.ticks_y(step) {
                let py = view.to_px(0.0, k as f64 * step).y;
                if k == 0 || py < tl.y + ARROW * 2.5 || py > br.y - 4.0 {
                    continue;
                }
                line(vec![Vec2::new(x - TICK, py), Vec2::new(x + TICK, py)], ink, 2.0);
                if step_px >= 24.0 {
                    let label = format_tick(k as f64 * step, step);
                    let w = stroke_text_width(&label, DIGIT_H * 0.8);
                    for glyph in stroke_text(&label, Vec2::new(x - TICK - 6.0 - w, py - DIGIT_H * 0.4), DIGIT_H * 0.8) {
                        line(glyph, ink, 1.6);
                    }
                }
            }
        }
        if let (Some(y), Some(x)) = (x_axis_y, y_axis_x) {
            for glyph in stroke_text("0", Vec2::new(x - 6.0 - DIGIT_H * 0.5, y + 6.0), DIGIT_H * 0.8) {
                line(glyph, ink, 1.6);
            }
        }

        for (i, e) in self.parsed.iter().enumerate() {
            let Some(e) = e else { continue };
            for poly in trace(e, &self.params, &view, view.rect.1.x as usize / 2) {
                line(poly, CURVE_COLORS[i], 3.5);
            }
        }
        lines
    }

    pub fn render(
        &self,
        font_atlas: &FontAtlas,
        out_vertices: &mut Vec<Vertex>,
        out_indices: &mut Vec<u32>,
        glyph_vertices: &mut Vec<GlyphVertex>,
        glyph_indices: &mut Vec<u32>,
    ) {
        push_rect(self.position, Vec2::new(self.panel_width(), self.panel_height()), COLOR_PANEL_BG, out_vertices, out_indices);
        let (header_pos, header_size) = self.header_rect();
        push_rect(header_pos, header_size, COLOR_HEADER, out_vertices, out_indices);
        font_atlas.push_text("Grafik", header_pos + Vec2::new(PADDING, 10.0), 16.0, COLOR_TEXT, glyph_vertices, glyph_indices);
        let grip_center = header_pos + header_size / 2.0;
        for i in -1..=1 {
            push_circle(Vec2::new(grip_center.x + i as f32 * 10.0, grip_center.y), 2.0, COLOR_TEXT, 8, out_vertices, out_indices);
        }
        let (close_pos, close_size) = self.close_rect();
        let c = close_pos + close_size / 2.0;
        let r = close_size.x * 0.2;
        push_line(c - Vec2::splat(r), c + Vec2::splat(r), 2.5, COLOR_TEXT, out_vertices, out_indices);
        push_line(c + Vec2::new(-r, r), c + Vec2::new(r, -r), 2.5, COLOR_TEXT, out_vertices, out_indices);

        for i in 0..3 {
            self.render_formula_row(i, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
        }
        self.render_plot(font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
        let used = self.used_params();
        for param in 0..3 {
            self.render_slider(param, used[param], font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
        }

        for (row_index, row) in ROWS.iter().enumerate() {
            for (key_index, &(key, _)) in row.iter().enumerate() {
                let (pos, size) = self.key_rect(row_index, key_index);
                let color = match key {
                    Key::Ins { text, .. } if text.ends_with('(') || text == "^" || text == "²" || text == "π" => COLOR_KEY_FUNC,
                    Key::Ins { .. } => COLOR_KEY_IDLE,
                    Key::Backspace | Key::ZoomIn | Key::ZoomOut | Key::Reset => COLOR_KEY_SPECIAL,
                    Key::Clear => COLOR_KEY_CLEAR,
                    Key::Stamp if self.has_curves() => COLOR_KEY_STAMP,
                    Key::Stamp => COLOR_KEY_IDLE,
                };
                push_rect(pos, size, color, out_vertices, out_indices);
                draw_key_glyph(font_atlas, key, pos, size, out_vertices, out_indices, glyph_vertices, glyph_indices);
            }
        }
    }

    fn render_formula_row(
        &self,
        i: usize,
        font_atlas: &FontAtlas,
        out_vertices: &mut Vec<Vertex>,
        out_indices: &mut Vec<u32>,
        glyph_vertices: &mut Vec<GlyphVertex>,
        glyph_indices: &mut Vec<u32>,
    ) {
        let (pos, size) = self.formula_rect(i);
        if i == self.active {
            push_rect(pos - Vec2::splat(2.0), size + Vec2::splat(4.0), COLOR_ROW_ACTIVE, out_vertices, out_indices);
        }
        push_rect(pos, size, COLOR_ROW_BG, out_vertices, out_indices);
        push_rect(pos + Vec2::new(8.0, 10.0), Vec2::new(8.0, size.y - 20.0), CURVE_COLORS[i], out_vertices, out_indices);

        let text_h = 20.0;
        let text_pos = pos + Vec2::new(26.0, (size.y - text_h) / 2.0);
        let prefix = format!("y{} = ", SUBSCRIPTS[i]);
        let prefix_w = font_atlas.push_text(&prefix, text_pos, text_h, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);
        let f = &self.formulas[i];
        let color = if f.is_empty() || self.parsed[i].is_some() { COLOR_TEXT } else { COLOR_TEXT_ERROR };
        let max_w = size.x - 26.0 - prefix_w - 16.0;
        let shown = fit_text(f, max_w, true, |s| font_atlas.measure(s, text_h));
        let w = font_atlas.push_text(&shown, text_pos + Vec2::new(prefix_w, 0.0), text_h, color, glyph_vertices, glyph_indices);
        if i == self.active {
            let x = text_pos.x + prefix_w + w + 2.0;
            push_line(Vec2::new(x, text_pos.y - 3.0), Vec2::new(x, text_pos.y + text_h + 3.0), 2.0, COLOR_TEXT, out_vertices, out_indices);
        }
    }

    fn render_plot(
        &self,
        font_atlas: &FontAtlas,
        out_vertices: &mut Vec<Vertex>,
        out_indices: &mut Vec<u32>,
        glyph_vertices: &mut Vec<GlyphVertex>,
        glyph_indices: &mut Vec<u32>,
    ) {
        let rect = self.plot_rect();
        push_rect(rect.0, rect.1, COLOR_PLOT_BG, out_vertices, out_indices);
        let view = View::new(rect, self.center, self.px_per_unit);
        let (tl, br) = (rect.0, rect.0 + rect.1);
        let step = nice_step(44.0 / view.ppu);

        for k in view.ticks_x(step) {
            let x = view.to_px(k as f64 * step, 0.0).x;
            push_line(Vec2::new(x, tl.y), Vec2::new(x, br.y), 1.0, COLOR_GRID, out_vertices, out_indices);
        }
        for k in view.ticks_y(step) {
            let y = view.to_px(0.0, k as f64 * step).y;
            push_line(Vec2::new(tl.x, y), Vec2::new(br.x, y), 1.0, COLOR_GRID, out_vertices, out_indices);
        }

        let (x_axis_y, y_axis_x) = view.axis_positions();
        if let Some(y) = x_axis_y {
            push_line(Vec2::new(tl.x, y), Vec2::new(br.x, y), 1.5, COLOR_AXIS, out_vertices, out_indices);
        }
        if let Some(x) = y_axis_x {
            push_line(Vec2::new(x, tl.y), Vec2::new(x, br.y), 1.5, COLOR_AXIS, out_vertices, out_indices);
        }

        // Tick numbers: along the axes when visible, else pinned to the
        // plot's bottom/left edge so the scale is always readable.
        let label_h = 12.0;
        let label_y = x_axis_y.unwrap_or(br.y - label_h - 8.0).min(br.y - label_h - 6.0) + 4.0;
        for k in view.ticks_x(step) {
            if k == 0 {
                continue;
            }
            let x = view.to_px(k as f64 * step, 0.0).x;
            let label = format_tick(k as f64 * step, step);
            let w = font_atlas.measure(&label, label_h);
            if x - w / 2.0 > tl.x + 2.0 && x + w / 2.0 < br.x - 2.0 {
                font_atlas.push_text(&label, Vec2::new(x - w / 2.0, label_y), label_h, COLOR_TICK_LABEL, glyph_vertices, glyph_indices);
            }
        }
        let label_x = y_axis_x.unwrap_or(tl.x + 40.0).max(tl.x + 40.0);
        for k in view.ticks_y(step) {
            if k == 0 {
                continue;
            }
            let y = view.to_px(0.0, k as f64 * step).y;
            let label = format_tick(k as f64 * step, step);
            let w = font_atlas.measure(&label, label_h);
            if y - label_h / 2.0 > tl.y + 2.0 && y + label_h / 2.0 < br.y - 2.0 {
                font_atlas.push_text(&label, Vec2::new(label_x - w - 5.0, y - label_h / 2.0), label_h, COLOR_TICK_LABEL, glyph_vertices, glyph_indices);
            }
        }

        for (i, e) in self.parsed.iter().enumerate() {
            let Some(e) = e else { continue };
            for poly in trace(e, &self.params, &view, rect.1.x as usize / 2) {
                for w in poly.windows(2) {
                    push_line(w[0], w[1], 3.0, CURVE_COLORS[i], out_vertices, out_indices);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_slider(
        &self,
        param: usize,
        used: bool,
        font_atlas: &FontAtlas,
        out_vertices: &mut Vec<Vertex>,
        out_indices: &mut Vec<u32>,
        glyph_vertices: &mut Vec<GlyphVertex>,
        glyph_indices: &mut Vec<u32>,
    ) {
        let (pos, size) = self.slider_rect(param);
        push_rect(pos, size, COLOR_ROW_BG, out_vertices, out_indices);
        let (text, fill) = if used { (COLOR_TEXT, COLOR_ROW_ACTIVE) } else { (COLOR_TEXT_DIM, COLOR_KEY_FUNC) };
        let text_h = 18.0;
        font_atlas.push_text(PARAM_NAMES[param], pos + Vec2::new(16.0, (size.y - text_h) / 2.0), text_h, text, glyph_vertices, glyph_indices);

        let (x0, x1, y) = self.slider_track(param);
        push_line(Vec2::new(x0, y), Vec2::new(x1, y), 4.0, COLOR_KEY_IDLE, out_vertices, out_indices);
        let zero_x = x0 + (x1 - x0) * ((0.0 - PARAM_MIN) / (PARAM_MAX - PARAM_MIN)) as f32;
        push_line(Vec2::new(zero_x, y - 7.0), Vec2::new(zero_x, y + 7.0), 2.0, COLOR_TEXT_DIM, out_vertices, out_indices);
        let t = ((self.params[param] - PARAM_MIN) / (PARAM_MAX - PARAM_MIN)) as f32;
        let knob_x = x0 + (x1 - x0) * t;
        push_line(Vec2::new(zero_x, y), Vec2::new(knob_x, y), 4.0, fill, out_vertices, out_indices);
        push_circle(Vec2::new(knob_x, y), 11.0, text, 20, out_vertices, out_indices);

        let value = format_decimal(self.params[param], 1);
        let w = font_atlas.measure(&value, text_h);
        font_atlas.push_text(&value, Vec2::new(pos.x + size.x - 20.0 - w, pos.y + (size.y - text_h) / 2.0), text_h, text, glyph_vertices, glyph_indices);
    }
}

/// A world-coordinate window mapped onto a screen rectangle (y up in the
/// world, y down on screen), with equal scale on both axes so a circle's
/// graph looks round.
struct View {
    rect: (Vec2, Vec2),
    ppu: f64,
    x_min: f64,
    x_max: f64,
    y_min: f64,
    y_max: f64,
}

impl View {
    fn new(rect: (Vec2, Vec2), center: (f64, f64), ppu: f64) -> Self {
        let half_w = rect.1.x as f64 / 2.0 / ppu;
        let half_h = rect.1.y as f64 / 2.0 / ppu;
        Self { rect, ppu, x_min: center.0 - half_w, x_max: center.0 + half_w, y_min: center.1 - half_h, y_max: center.1 + half_h }
    }

    fn to_px(&self, x: f64, y: f64) -> Vec2 {
        Vec2::new(
            self.rect.0.x + ((x - self.x_min) * self.ppu) as f32,
            self.rect.0.y + ((self.y_max - y) * self.ppu) as f32,
        )
    }

    fn ticks_x(&self, step: f64) -> std::ops::RangeInclusive<i64> {
        (self.x_min / step).ceil() as i64..=(self.x_max / step).floor() as i64
    }

    fn ticks_y(&self, step: f64) -> std::ops::RangeInclusive<i64> {
        (self.y_min / step).ceil() as i64..=(self.y_max / step).floor() as i64
    }

    /// Screen y of the x-axis and screen x of the y-axis, when on screen.
    fn axis_positions(&self) -> (Option<f32>, Option<f32>) {
        let x_axis = (self.y_min <= 0.0 && 0.0 <= self.y_max).then(|| self.to_px(0.0, 0.0).y);
        let y_axis = (self.x_min <= 0.0 && 0.0 <= self.x_max).then(|| self.to_px(0.0, 0.0).x);
        (x_axis, y_axis)
    }
}

/// Samples `e` across the view and returns screen-space polylines,
/// clipped to the view and split at gaps: undefined points (`√x` for
/// x < 0) and vertical asymptotes (`tan x`, `1/x` — a jump bigger than
/// the whole visible range whose midpoint doesn't lie between the ends).
fn trace(e: &Expr, params: &[f64; 3], view: &View, samples: usize) -> Vec<Vec<Vec2>> {
    let samples = samples.max(2);
    let span_y = view.y_max - view.y_min;
    // A little margin so curves leaving the top/bottom reach the edge.
    let (y_lo, y_hi) = (view.y_min - span_y * 0.01, view.y_max + span_y * 0.01);
    let mut out: Vec<Vec<Vec2>> = Vec::new();
    let mut current: Vec<Vec2> = Vec::new();
    let flush = |current: &mut Vec<Vec2>, out: &mut Vec<Vec<Vec2>>| {
        if current.len() >= 2 {
            out.push(std::mem::take(current));
        } else {
            current.clear();
        }
    };

    let dx = (view.x_max - view.x_min) / samples as f64;
    let mut prev: Option<(f64, f64)> = None;
    for i in 0..=samples {
        let x = view.x_min + i as f64 * dx;
        let y = e.eval(x, params);
        if !y.is_finite() {
            flush(&mut current, &mut out);
            prev = None;
            continue;
        }
        if let Some((x0, y0)) = prev {
            if (y - y0).abs() > span_y {
                let ym = e.eval((x0 + x) / 2.0, params);
                let (lo, hi) = (y0.min(y), y0.max(y));
                if !(ym.is_finite() && ym >= lo && ym <= hi) {
                    flush(&mut current, &mut out);
                    prev = Some((x, y));
                    continue;
                }
            }
            match clip_segment((x0, y0), (x, y), view.x_min, view.x_max, y_lo, y_hi) {
                Some((t0, t1)) => {
                    let lerp = |t: f64| view.to_px(x0 + (x - x0) * t, y0 + (y - y0) * t);
                    if t0 > 0.0 {
                        flush(&mut current, &mut out);
                    }
                    if current.is_empty() {
                        current.push(lerp(t0));
                    }
                    current.push(lerp(t1));
                    if t1 < 1.0 {
                        flush(&mut current, &mut out);
                    }
                }
                None => flush(&mut current, &mut out),
            }
        }
        prev = Some((x, y));
    }
    flush(&mut current, &mut out);
    out
}

/// Liang–Barsky: the `(t0, t1)` sub-range of segment `a→b` inside the box.
fn clip_segment(a: (f64, f64), b: (f64, f64), x_min: f64, x_max: f64, y_min: f64, y_max: f64) -> Option<(f64, f64)> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let mut t0 = 0.0f64;
    let mut t1 = 1.0f64;
    for (p, q) in [(-dx, a.0 - x_min), (dx, x_max - a.0), (-dy, a.1 - y_min), (dy, y_max - a.1)] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
        }
    }
    (t0 <= t1).then_some((t0, t1))
}

/// 1, 2 or 5 × 10ⁿ — the smallest "round" step at least `raw` long.
fn nice_step(raw: f64) -> f64 {
    let exp = 10f64.powf(raw.log10().floor());
    let m = raw / exp;
    let nice = if m <= 1.0 {
        1.0
    } else if m <= 2.0 {
        2.0
    } else if m <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * exp
}

/// Tick label for `v` on a grid of `step`: as many decimals as the step
/// needs, Turkish decimal comma, no `-0`.
fn format_tick(v: f64, step: f64) -> String {
    let decimals = if step >= 1.0 { 0 } else { (-step.log10()).ceil() as usize };
    format_decimal(v, decimals)
}

fn format_decimal(v: f64, decimals: usize) -> String {
    let s = format!("{:.*}", decimals, v).replace('.', ",");
    if s.trim_start_matches('-').chars().all(|c| c == '0' || c == ',') {
        s.trim_start_matches('-').to_string()
    } else {
        s
    }
}

/// Resamples a polyline so consecutive points are at most `spacing` apart
/// — `stroke.rs` smooths ink through segment midpoints, which would round
/// a sparse polyline's corners (an axis arrow, a digit's edges) away.
fn densify(points: &[Vec2], spacing: f32) -> Vec<Vec2> {
    let mut out = vec![points[0]];
    for w in points.windows(2) {
        let n = (w[0].distance(w[1]) / spacing).ceil().max(1.0) as usize;
        for k in 1..=n {
            out.push(w[0].lerp(w[1], k as f32 / n as f32));
        }
    }
    out
}

/// Single-line stroke glyphs for stamped numbers/axis names, in a box
/// 0.6 wide × 1 tall (y down). Each glyph is one or more polylines.
fn stroke_glyph(c: char) -> &'static [&'static [(f32, f32)]] {
    match c {
        '0' => &[&[(0.3, 0.0), (0.55, 0.12), (0.6, 0.5), (0.55, 0.88), (0.3, 1.0), (0.05, 0.88), (0.0, 0.5), (0.05, 0.12), (0.3, 0.0)]],
        '1' => &[&[(0.1, 0.2), (0.35, 0.0), (0.35, 1.0)]],
        '2' => &[&[(0.02, 0.18), (0.25, 0.0), (0.5, 0.05), (0.6, 0.25), (0.5, 0.48), (0.0, 1.0), (0.6, 1.0)]],
        '3' => &[&[(0.02, 0.1), (0.3, 0.0), (0.55, 0.12), (0.55, 0.38), (0.25, 0.48), (0.58, 0.6), (0.58, 0.88), (0.3, 1.0), (0.0, 0.9)]],
        '4' => &[&[(0.45, 1.0), (0.45, 0.0), (0.0, 0.68), (0.6, 0.68)]],
        '5' => &[&[(0.58, 0.0), (0.05, 0.0), (0.02, 0.45), (0.3, 0.4), (0.55, 0.52), (0.6, 0.78), (0.45, 0.97), (0.2, 1.0), (0.0, 0.9)]],
        '6' => &[&[(0.5, 0.02), (0.2, 0.12), (0.02, 0.5), (0.05, 0.88), (0.3, 1.0), (0.55, 0.88), (0.58, 0.62), (0.32, 0.5), (0.05, 0.6)]],
        '7' => &[&[(0.0, 0.0), (0.6, 0.0), (0.2, 1.0)]],
        '8' => &[&[(0.3, 0.48), (0.08, 0.36), (0.08, 0.1), (0.3, 0.0), (0.52, 0.1), (0.52, 0.36), (0.3, 0.48), (0.02, 0.62), (0.05, 0.9), (0.3, 1.0), (0.55, 0.9), (0.58, 0.62), (0.3, 0.48)]],
        '9' => &[&[(0.55, 0.4), (0.28, 0.5), (0.03, 0.38), (0.03, 0.12), (0.28, 0.0), (0.55, 0.12), (0.58, 0.5), (0.4, 0.88), (0.1, 0.98)]],
        '-' => &[&[(0.05, 0.55), (0.45, 0.55)]],
        ',' => &[&[(0.15, 0.85), (0.15, 1.0), (0.08, 1.12)]],
        'x' => &[&[(0.0, 0.35), (0.55, 1.0)], &[(0.55, 0.35), (0.0, 1.0)]],
        'y' => &[&[(0.0, 0.35), (0.3, 0.8)], &[(0.6, 0.35), (0.15, 1.25)]],
        _ => &[],
    }
}

/// Pen advance after a stroke glyph, in glyph heights — the minus sign
/// and comma are narrow, so a fixed advance would leave "- 8" gaps.
fn stroke_advance(c: char) -> f32 {
    match c {
        '-' => 0.55,
        ',' => 0.35,
        _ => 0.8,
    }
}

fn stroke_text_width(text: &str, height: f32) -> f32 {
    // The last glyph's ink ends at 0.6, not at its advance.
    let advance: f32 = text.chars().map(stroke_advance).sum();
    match text.chars().last() {
        Some(last) => (advance - stroke_advance(last) + 0.6) * height,
        None => 0.0,
    }
}

fn stroke_text(text: &str, top_left: Vec2, height: f32) -> Vec<Vec<Vec2>> {
    let mut out = Vec::new();
    let mut x = 0.0;
    for c in text.chars() {
        let origin = top_left + Vec2::new(x * height, 0.0);
        for poly in stroke_glyph(c) {
            out.push(poly.iter().map(|&(gx, gy)| origin + Vec2::new(gx, gy) * height).collect());
        }
        x += stroke_advance(c);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn draw_key_glyph(
    font_atlas: &FontAtlas,
    key: Key,
    top_left: Vec2,
    size: Vec2,
    out_vertices: &mut Vec<Vertex>,
    out_indices: &mut Vec<u32>,
    glyph_vertices: &mut Vec<GlyphVertex>,
    glyph_indices: &mut Vec<u32>,
) {
    let center = top_left + size / 2.0;
    let text = |label: &str, h: f32, glyph_vertices: &mut Vec<GlyphVertex>, glyph_indices: &mut Vec<u32>| {
        let w = font_atlas.measure(label, h);
        font_atlas.push_text(label, Vec2::new(center.x - w / 2.0, center.y - h / 2.0), h, COLOR_TEXT, glyph_vertices, glyph_indices);
    };
    match key {
        Key::Ins { label, .. } => {
            let h = if label.chars().count() > 2 { size.y * 0.36 } else { size.y * 0.46 };
            text(label, h, glyph_vertices, glyph_indices);
        }
        Key::Backspace => {
            let r = size.x * 0.16;
            let tip = center + Vec2::new(-size.x * 0.22, 0.0);
            push_line(tip, tip + Vec2::new(r, -r), 3.0, COLOR_TEXT, out_vertices, out_indices);
            push_line(tip, tip + Vec2::new(r, r), 3.0, COLOR_TEXT, out_vertices, out_indices);
            push_line(tip, tip + Vec2::new(size.x * 0.4, 0.0), 3.0, COLOR_TEXT, out_vertices, out_indices);
        }
        Key::Clear => text("Sil", size.y * 0.36, glyph_vertices, glyph_indices),
        Key::ZoomOut | Key::ZoomIn => {
            // Magnifier with − / +.
            let lens = center + Vec2::new(-3.0, -3.0);
            let r = size.y * 0.22;
            for i in 0..20 {
                let a0 = i as f32 / 20.0 * std::f32::consts::TAU;
                let a1 = (i + 1) as f32 / 20.0 * std::f32::consts::TAU;
                push_line(lens + Vec2::new(a0.cos(), a0.sin()) * r, lens + Vec2::new(a1.cos(), a1.sin()) * r, 2.0, COLOR_TEXT, out_vertices, out_indices);
            }
            let d = Vec2::new(1.0, 1.0).normalize();
            push_line(lens + d * r, lens + d * r * 1.8, 3.0, COLOR_TEXT, out_vertices, out_indices);
            let s = r * 0.55;
            push_line(lens - Vec2::new(s, 0.0), lens + Vec2::new(s, 0.0), 2.0, COLOR_TEXT, out_vertices, out_indices);
            if key == Key::ZoomIn {
                push_line(lens - Vec2::new(0.0, s), lens + Vec2::new(0.0, s), 2.0, COLOR_TEXT, out_vertices, out_indices);
            }
        }
        Key::Reset => {
            // Crosshair: back to the origin.
            let r = size.y * 0.24;
            push_line(center - Vec2::new(r, 0.0), center + Vec2::new(r, 0.0), 2.0, COLOR_TEXT, out_vertices, out_indices);
            push_line(center - Vec2::new(0.0, r), center + Vec2::new(0.0, r), 2.0, COLOR_TEXT, out_vertices, out_indices);
            push_circle(center, 3.5, COLOR_TEXT, 12, out_vertices, out_indices);
        }
        Key::Stamp => text("Tahtaya bas", size.y * 0.4, glyph_vertices, glyph_indices),
    }
}

fn rect_contains((top_left, size): (Vec2, Vec2), point: Vec2) -> bool {
    point.x >= top_left.x && point.x <= top_left.x + size.x && point.y >= top_left.y && point.y <= top_left.y + size.y
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> View {
        View::new((Vec2::ZERO, Vec2::new(600.0, 300.0)), (0.0, 0.0), 30.0)
    }

    #[test]
    fn tan_breaks_at_asymptotes_and_stays_inside() {
        let e = expr::parse("tan(x)").unwrap();
        let v = view();
        let polys = trace(&e, &[1.0; 3], &v, 300);
        // x ∈ [-10, 10] holds 6 asymptotes (±π/2, ±3π/2, ±5π/2) → 7 branches.
        assert_eq!(polys.len(), 7, "{}", polys.len());
        for p in polys.iter().flatten() {
            assert!(p.x >= -0.01 && p.x <= 600.01 && p.y >= -4.0 && p.y <= 304.0, "{p:?}");
        }
    }

    #[test]
    fn sqrt_only_right_half_and_parabola_is_one_piece() {
        let v = view();
        let polys = trace(&expr::parse("√x").unwrap(), &[1.0; 3], &v, 300);
        assert_eq!(polys.len(), 1);
        assert!(polys[0].iter().all(|p| p.x >= 299.0));
        let polys = trace(&expr::parse("x^2-3").unwrap(), &[1.0; 3], &v, 300);
        assert_eq!(polys.len(), 1);
    }

    #[test]
    fn ticks_and_labels() {
        assert_eq!(nice_step(1.4), 2.0);
        assert_eq!(nice_step(0.3), 0.5);
        assert_eq!(nice_step(7.0), 10.0);
        assert_eq!(format_tick(-2.0, 1.0), "-2");
        assert_eq!(format_tick(0.5, 0.5), "0,5");
        assert_eq!(format_tick(-0.0, 0.1), "0,0");
        assert_eq!(format_decimal(-0.04, 1), "0,0");
    }

    #[test]
    fn keypad_editing_and_sliders() {
        let mut g = Graph::new(Vec2::new(1920.0, 1080.0));
        for k in [ins("a", "a"), ins("sin(", "sin"), ins("x", "x"), ins(")", ")"), ins("+", "+"), ins("b", "b")] {
            g.press_key(k);
        }
        assert_eq!(g.formulas[0], "asin(x)+b");
        assert!(g.parsed[0].is_some());
        assert_eq!(g.used_params(), [true, true, false]);
        let keys_before = g.keys_top();
        g.formulas[0].clear();
        g.parsed[0] = None;
        assert_eq!(g.keys_top(), keys_before, "keypad must not move when sliders change");
        for k in [ins("a", "a"), ins("sin(", "sin"), ins("x", "x"), ins(")", ")"), ins("+", "+"), ins("b", "b")] {
            g.press_key(k);
        }
        g.press_key(Key::Backspace); // b
        g.press_key(Key::Backspace); // +
        g.press_key(Key::Backspace); // )
        g.press_key(Key::Backspace); // x
        g.press_key(Key::Backspace); // sin( as one token
        assert_eq!(g.formulas[0], "a");
        // Slider: far right end → max, snapped.
        g.press_key(ins("x", "x"));
        let (_, x1, _) = g.slider_track(0);
        g.set_slider(0, x1 + 50.0);
        assert_eq!(g.params[0], PARAM_MAX);
        assert_eq!(g.press_key(Key::Stamp), GraphPress::Stamp);
    }

    #[test]
    fn stamp_produces_axes_and_curve() {
        let mut g = Graph::new(Vec2::new(1920.0, 1080.0));
        g.formulas[0] = "x^2-3".into();
        g.parsed[0] = expr::parse("x^2-3").ok();
        let lines = g.stamp_lines(Vec2::new(1920.0, 1080.0), [1.0; 4]);
        assert!(lines.iter().any(|l| l.color == CURVE_COLORS[0]));
        assert!(lines.len() > 10);
        for l in &lines {
            for w in l.points.windows(2) {
                assert!(w[0].distance(w[1]) <= 3.01);
            }
        }
    }
}
