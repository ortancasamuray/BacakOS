//! Central input state machine: routes every mouse/touch event to the
//! right place — drawing, panning, erasing, the toolbar, the radial menu,
//! or the palm-rejection heuristic — and owns the strokes those events
//! produce. `app.rs` is just winit glue; this is where "what does a touch
//! actually do" is decided.

use std::collections::HashMap;

use glam::Vec2;
use winit::event::KeyEvent;
use winit::keyboard::{Key, NamedKey};

use crate::board::{self, Page};
use crate::brush::{next_palette_color, BrushType, PenSettings};
use crate::palm;
use crate::prediction::{TouchPredictor, TouchSample};
use crate::stroke::{push_circle, push_rect, Stroke, Vertex};
use crate::toolbar::{EraserMode, Tool, Toolbar, ToolbarAction, ToolbarState};
use crate::ui::{RadialAction, RadialMenu};

/// How far ahead (ms) we extrapolate pointer motion to hide touch-to-photon
/// latency. 10-25ms covers one to a few frames at 60-120Hz.
const LOOKAHEAD_MS: f32 = 16.0;

const DEFAULT_ERASER_RADIUS: f32 = 24.0;
/// Best-effort "palm eraser": since this input stack can't measure contact
/// width (see `palm` module docs), a detected palm cluster erases a
/// generous radius around itself rather than just suppressing ink — an
/// approximation of the spec's contact-width-triggered area eraser.
const PALM_ERASE_RADIUS: f32 = 70.0;
const PALM_CLUSTER_DEMOTE_RADIUS: f32 = 90.0;

const DIVIDER_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 0.12];
const DIVIDER_WIDTH: f32 = 2.0;

/// Two Pen touches landing within this many seconds of each other, and
/// this close together, are treated as a deliberate two-finger tap that
/// opens the radial menu — NOT a single-finger long-press. The compositor
/// has its own single-finger long-press → text-selection gesture
/// (`selection_recognizer`, armed whenever exactly one finger is down) and
/// fires it in parallel with whatever the client does with that same
/// touch, so a long-press-triggered menu here would always collide with
/// it. Two fingers never arm the compositor's single-touch recognizer.
const TWO_FINGER_TAP_WINDOW_SECS: f64 = 0.25;
const TWO_FINGER_TAP_DISTANCE: f32 = 220.0;

/// Sentinel pointer id for the mouse, kept out of the touch id space.
const MOUSE_POINTER_ID: u64 = u64::MAX;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Left,
    Right,
}


enum PointerRole {
    /// Actively extending a stroke. `stroke_index` indexes `strokes`.
    Drawing { stroke_index: usize, predictor: TouchPredictor },
    /// Hand tool: drags `view_offset`.
    Panning { last_pos: Vec2 },
    /// Eraser tool: tombstones (clears) any stroke it passes near.
    Erasing,
    /// Touch landed on the toolbar; consumed, never reaches the canvas.
    Toolbar,
    /// Flagged by the palm-rejection heuristic, or consumed by a
    /// two-finger-tap gesture / the open radial menu; ignored until lift.
    Palm,
}

struct PointerSession {
    role: PointerRole,
    start_pos: Vec2,
    start_time: f64,
}

pub struct InputHandler {
    pages: Vec<Page>,
    current_page: usize,
    sessions: HashMap<u64, PointerSession>,
    last_mouse_pos: Vec2,

    active_tool: Tool,
    active_pen: PenSettings,
    eraser_mode: EraserMode,
    eraser_radius: f32,
    view_offset: Vec2,
    eraser_cursor: Option<Vec2>,

    zone_enabled: bool,
    zone_pens: [PenSettings; 2],

    radial_menu: Option<RadialMenu>,

    toolbar: Toolbar,
    screen_size: Vec2,

    last_input_at: Option<f64>,
}

impl InputHandler {
    pub fn new(screen_size: Vec2) -> Self {
        Self {
            pages: vec![Page::new()],
            current_page: 0,
            sessions: HashMap::new(),
            last_mouse_pos: Vec2::ZERO,
            active_tool: Tool::Pen,
            eraser_mode: EraserMode::Area,
            eraser_radius: DEFAULT_ERASER_RADIUS,
            active_pen: PenSettings::ballpoint([0.92, 0.92, 0.95, 1.0]),
            view_offset: Vec2::ZERO,
            eraser_cursor: None,
            zone_enabled: false,
            zone_pens: [
                PenSettings::ballpoint([0.92, 0.92, 0.95, 1.0]),
                PenSettings::ballpoint([0.25, 0.55, 0.95, 1.0]),
            ],
            radial_menu: None,
            toolbar: Toolbar::layout(screen_size),
            screen_size,
            last_input_at: None,
        }
    }

    pub fn resize(&mut self, screen_size: Vec2) {
        self.screen_size = screen_size;
        self.toolbar = Toolbar::layout(screen_size);
    }

    fn zone_of(&self, pos: Vec2) -> Zone {
        if pos.x < self.screen_size.x / 2.0 {
            Zone::Left
        } else {
            Zone::Right
        }
    }

    fn pen_for(&self, zone: Zone) -> &PenSettings {
        if self.zone_enabled {
            &self.zone_pens[zone as usize]
        } else {
            &self.active_pen
        }
    }

    fn pen_for_mut(&mut self, zone: Zone) -> &mut PenSettings {
        if self.zone_enabled {
            &mut self.zone_pens[zone as usize]
        } else {
            &mut self.active_pen
        }
    }

    fn active_pen_touch_starts(&self) -> impl Iterator<Item = (Vec2, f64)> + '_ {
        self.sessions
            .values()
            .filter(|s| matches!(s.role, PointerRole::Drawing { .. }))
            .map(|s| (s.start_pos, s.start_time))
    }

    // --- Per-frame maintenance ---------------------------------------------

    /// Call once per frame before rendering: prunes expired LaserPointer
    /// points so the fade keeps animating even without new input.
    pub fn tick(&mut self, now: f64) {
        for stroke in &mut self.pages[self.current_page].strokes {
            if stroke.brush_type == BrushType::LaserPointer {
                stroke.prune_expired(now);
            }
        }
    }

    /// Looks for an already-active Pen/Eraser touch close enough in time and
    /// space to `position`/`now` to treat this new touch as the second half
    /// of a two-finger tap. Returns its session id.
    fn find_two_finger_tap_partner(&self, position: Vec2, now: f64) -> Option<u64> {
        self.sessions.iter().find_map(|(&id, s)| {
            let is_interactive = matches!(s.role, PointerRole::Drawing { .. } | PointerRole::Erasing);
            let close_in_time = (now - s.start_time).abs() <= TWO_FINGER_TAP_WINDOW_SECS;
            let close_in_space = s.start_pos.distance(position) <= TWO_FINGER_TAP_DISTANCE;
            (is_interactive && close_in_time && close_in_space).then_some(id)
        })
    }

    /// Neutralizes a touch that turned out to be half of a gesture rather
    /// than a real stroke/erase: tombstones any stroke it started, marks it
    /// inert until lift.
    fn cancel_touch_as_gesture(&mut self, id: u64) {
        if let Some(session) = self.sessions.get_mut(&id) {
            if let PointerRole::Drawing { stroke_index, .. } = session.role {
                if let Some(stroke) = self.pages[self.current_page].strokes.get_mut(stroke_index) {
                    stroke.clear();
                }
            }
            session.role = PointerRole::Palm;
        }
    }

    fn open_radial_menu_at(&mut self, at: Vec2) {
        self.radial_menu = Some(RadialMenu::open(at));
    }

    fn close_radial_menu(&mut self) {
        self.radial_menu = None;
    }

    fn apply_toolbar_action(&mut self, action: ToolbarAction, zone: Zone) {
        match action {
            ToolbarAction::SelectTool(tool) => {
                // Tapping Eraser again while it's already active cycles
                // Area/Object mode instead of a no-op reselect — the only
                // discoverable way to reach it without a keyboard.
                if tool == Tool::Eraser && self.active_tool == Tool::Eraser {
                    self.eraser_mode = match self.eraser_mode {
                        EraserMode::Area => EraserMode::Object,
                        EraserMode::Object => EraserMode::Area,
                    };
                } else {
                    self.active_tool = tool;
                }
            }
            ToolbarAction::CycleBrush => {
                let next_brush = self.pen_for(zone).brush_type.next();
                self.set_brush(next_brush);
            }
            ToolbarAction::CycleColor => {
                let next_color = next_palette_color(self.pen_for(zone).color);
                self.set_color(next_color);
            }
            ToolbarAction::Undo => {
                self.pages[self.current_page].strokes.pop();
            }
            ToolbarAction::Clear => self.pages[self.current_page].strokes.clear(),
            ToolbarAction::ToggleZone => self.zone_enabled = !self.zone_enabled,
            ToolbarAction::CycleBackground => {
                let page = &mut self.pages[self.current_page];
                let (bg, grid) = board::next_preset((page.background, page.grid));
                page.background = bg;
                page.grid = grid;
            }
            ToolbarAction::PrevPage => {
                self.current_page = self.current_page.saturating_sub(1);
            }
            ToolbarAction::NextPage => {
                self.current_page += 1;
                if self.current_page >= self.pages.len() {
                    self.pages.push(Page::new());
                }
            }
        }
    }

    fn apply_radial_action(&mut self, action: RadialAction, zone: Zone) {
        match action {
            RadialAction::Color(c) => {
                if self.active_tool == Tool::Pen {
                    let brush = self.pen_for(zone).brush_type;
                    *self.pen_for_mut(zone) = PenSettings::for_brush(brush, c);
                }
            }
            RadialAction::Width(w) => {
                // Reused for eraser sizing too — the same 3 preset widths
                // scaled up read naturally as small/medium/large eraser
                // radii, without needing a separate size picker.
                if self.active_tool == Tool::Eraser {
                    self.eraser_radius = w * 3.0;
                } else {
                    self.pen_for_mut(zone).base_width = w;
                }
            }
            RadialAction::Eraser => {
                self.active_tool = Tool::Eraser;
            }
        }
    }

    // --- Pointer routing -----------------------------------------------------

    fn begin_pointer(&mut self, id: u64, position: Vec2, now: f64) {
        self.last_input_at = Some(now);

        // The radial menu, once open, is modal: the next touch anywhere
        // either picks a wedge (landed inside the ring) or cancels it
        // (landed elsewhere) — either way it's consumed, never drawn.
        if self.radial_menu.is_some() {
            let zone = self.zone_of(position);
            if let Some(action) = self.radial_menu.as_ref().and_then(|m| m.action_at(position)) {
                self.apply_radial_action(action, zone);
            }
            self.close_radial_menu();
            self.sessions.insert(id, PointerSession { role: PointerRole::Palm, start_pos: position, start_time: now });
            return;
        }

        if self.toolbar.contains(position) {
            if let Some(action) = self.toolbar.hit_test(position) {
                self.apply_toolbar_action(action, self.zone_of(position));
            }
            self.sessions.insert(id, PointerSession { role: PointerRole::Toolbar, start_pos: position, start_time: now });
            return;
        }

        let zone = self.zone_of(position);

        // Two-finger tap (Pen or Eraser context) opens the radial menu —
        // checked before tool-specific routing since it applies to both.
        if matches!(self.active_tool, Tool::Pen | Tool::Eraser) {
            if let Some(partner_id) = self.find_two_finger_tap_partner(position, now) {
                let partner_pos = self.sessions.get(&partner_id).map(|s| s.start_pos).unwrap_or(position);
                self.cancel_touch_as_gesture(partner_id);
                self.open_radial_menu_at((position + partner_pos) / 2.0);
                self.sessions.insert(id, PointerSession { role: PointerRole::Palm, start_pos: position, start_time: now });
                return;
            }
        }

        let role = match self.active_tool {
            Tool::Pen => {
                if palm::is_probable_palm(position, now, self.active_pen_touch_starts()) {
                    self.demote_cluster_to_palm(position);
                    self.erase_area(position, PALM_ERASE_RADIUS);
                    PointerRole::Palm
                } else {
                    let pen = self.pen_for(zone).clone();
                    let page = &mut self.pages[self.current_page];
                    let stroke_index = page.strokes.len();
                    let mut stroke = Stroke::new(pen.color, pen.base_width, pen.brush_type);
                    stroke.push_point(position, now);
                    page.strokes.push(stroke);

                    let mut predictor = TouchPredictor::new(LOOKAHEAD_MS);
                    predictor.push_sample(TouchSample { position, timestamp: now });

                    PointerRole::Drawing { stroke_index, predictor }
                }
            }
            Tool::Hand => PointerRole::Panning { last_pos: position },
            Tool::Eraser => {
                self.erase_at(position, self.eraser_radius);
                self.eraser_cursor = Some(position);
                PointerRole::Erasing
            }
        };

        self.sessions.insert(id, PointerSession { role, start_pos: position, start_time: now });
    }

    /// We can't tell which of two clustered contacts is the real pen tip,
    /// so a detected palm also retroactively tombstones any other
    /// very-fresh nearby stroke — safer to under-draw than to keep a
    /// scribble from what was actually part of the same palm contact.
    fn demote_cluster_to_palm(&mut self, position: Vec2) {
        let strokes = &mut self.pages[self.current_page].strokes;
        for session in self.sessions.values_mut() {
            if let PointerRole::Drawing { stroke_index, .. } = session.role {
                if session.start_pos.distance(position) <= PALM_CLUSTER_DEMOTE_RADIUS
                    && strokes[stroke_index].points.len() <= 3
                {
                    strokes[stroke_index].clear();
                    session.role = PointerRole::Palm;
                }
            }
        }
    }

    fn move_pointer(&mut self, id: u64, position: Vec2, now: f64) {
        self.last_input_at = Some(now);

        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };

        let mut do_erase = false;
        match &mut session.role {
            PointerRole::Drawing { stroke_index, predictor } => {
                let Some(stroke) = self.pages[self.current_page].strokes.get_mut(*stroke_index) else {
                    session.role = PointerRole::Palm; // stroke was undone from under us
                    return;
                };
                stroke.push_point(position, now);
                predictor.push_sample(TouchSample { position, timestamp: now });
            }
            PointerRole::Panning { last_pos } => {
                self.view_offset += position - *last_pos;
                *last_pos = position;
            }
            PointerRole::Erasing => {
                self.eraser_cursor = Some(position);
                do_erase = true;
            }
            PointerRole::Toolbar | PointerRole::Palm => {}
        }
        if do_erase {
            self.erase_at(position, self.eraser_radius);
        }
    }

    fn end_pointer(&mut self, id: u64) {
        if let Some(session) = self.sessions.remove(&id) {
            if matches!(session.role, PointerRole::Erasing) {
                self.eraser_cursor = None;
            }
        }
    }

    /// Erases using whichever mode the Eraser tool is currently set to.
    fn erase_at(&mut self, position: Vec2, radius: f32) {
        match self.eraser_mode {
            EraserMode::Area => self.erase_area(position, radius),
            EraserMode::Object => self.erase_object(position, radius),
        }
    }

    /// Rubs out only the ink within `radius` — splits a stroke into two if
    /// the erased patch falls in its middle. Always used for the
    /// palm-rejection area-eraser regardless of the Eraser tool's current
    /// mode, since a resting palm shouldn't be able to wipe out an entire
    /// unrelated stroke it barely grazes.
    fn erase_area(&mut self, position: Vec2, radius: f32) {
        let view_offset = self.view_offset;
        let strokes = &mut self.pages[self.current_page].strokes;
        let mut new_strokes = Vec::new();
        for stroke in strokes.iter_mut() {
            if stroke.points.is_empty() {
                continue;
            }
            new_strokes.extend(stroke.erase_near(position, radius, view_offset));
        }
        strokes.extend(new_strokes);
    }

    /// Deletes an entire stroke if any part of it falls within `radius`.
    fn erase_object(&mut self, position: Vec2, radius: f32) {
        let view_offset = self.view_offset;
        for stroke in &mut self.pages[self.current_page].strokes {
            if !stroke.points.is_empty() && stroke.hit_test(position, radius, view_offset) {
                stroke.clear();
            }
        }
    }

    // --- Mouse -----------------------------------------------------------

    pub fn mouse_moved(&mut self, pos: Vec2, now: f64) {
        if self.sessions.contains_key(&MOUSE_POINTER_ID) {
            self.move_pointer(MOUSE_POINTER_ID, pos, now);
        }
        self.last_mouse_pos = pos;
    }

    pub fn mouse_pressed(&mut self, now: f64) {
        self.begin_pointer(MOUSE_POINTER_ID, self.last_mouse_pos, now);
    }

    pub fn mouse_released(&mut self) {
        self.end_pointer(MOUSE_POINTER_ID);
    }

    // --- Touch -------------------------------------------------------------

    pub fn touch_started(&mut self, id: u64, pos: Vec2, now: f64) {
        self.begin_pointer(id, pos, now);
    }

    pub fn touch_moved(&mut self, id: u64, pos: Vec2, now: f64) {
        self.move_pointer(id, pos, now);
    }

    pub fn touch_ended(&mut self, id: u64) {
        self.end_pointer(id);
    }

    // --- Keyboard ----------------------------------------------------------

    pub fn key_input(&mut self, event: &KeyEvent, now: f64) {
        if !event.state.is_pressed() {
            return;
        }
        match &event.logical_key {
            Key::Character(s) => match s.as_str() {
                "1" => self.set_color([0.92, 0.92, 0.95, 1.0]),
                "2" => self.set_color([0.95, 0.25, 0.25, 1.0]),
                "3" => self.set_color([0.25, 0.55, 0.95, 1.0]),
                "4" => self.set_color([0.30, 0.85, 0.35, 1.0]),
                "5" => self.set_color([0.98, 0.78, 0.15, 1.0]),
                "c" | "C" => self.pages[self.current_page].strokes.clear(),
                "p" | "P" => self.active_tool = Tool::Pen,
                "h" | "H" => self.active_tool = Tool::Hand,
                "e" | "E" => self.active_tool = Tool::Eraser,
                "b" | "B" => self.set_brush(BrushType::Ballpoint),
                "k" | "K" => self.set_brush(BrushType::Calligraphy),
                "f" | "F" => self.set_brush(BrushType::Highlighter),
                "r" | "R" => self.set_brush(BrushType::LaserPointer),
                "z" | "Z" => self.zone_enabled = !self.zone_enabled,
                "l" | "L" => {
                    if let Some(last) = self.last_input_at {
                        let ms = (now - last) * 1000.0;
                        log::info!("input->frame proxy latency: {ms:.1}ms (target <20-30ms)");
                    }
                }
                _ => {}
            },
            Key::Named(NamedKey::Backspace) => {
                self.pages[self.current_page].strokes.pop();
            }
            _ => {}
        }
    }

    fn set_color(&mut self, c: [f32; 4]) {
        if self.zone_enabled {
            // Keyboard shortcuts are shared/global; apply to both zones so
            // a color hotkey is still useful with the dual-zone canvas on.
            for pen in &mut self.zone_pens {
                *pen = PenSettings::for_brush(pen.brush_type, c);
            }
        } else {
            self.active_pen = PenSettings::for_brush(self.active_pen.brush_type, c);
        }
    }

    fn set_brush(&mut self, brush_type: BrushType) {
        if self.zone_enabled {
            for pen in &mut self.zone_pens {
                *pen = PenSettings::for_brush(brush_type, pen.color);
            }
        } else {
            self.active_pen = PenSettings::for_brush(brush_type, self.active_pen.color);
        }
    }

    // --- Geometry collection -------------------------------------------------

    /// Builds this frame's mesh, split into a normal batch (ink + UI,
    /// standard alpha blend) and a highlighter batch (Max blend so marker
    /// overlaps never darken — see `renderer`).
    pub fn collect_geometry(
        &self,
        now: f64,
    ) -> (Vec<Vertex>, Vec<u32>, Vec<Vertex>, Vec<u32>) {
        let mut normal_v = Vec::new();
        let mut normal_i = Vec::new();
        let mut highlight_v = Vec::new();
        let mut highlight_i = Vec::new();

        let page = &self.pages[self.current_page];
        board::render_background(page, self.screen_size, self.view_offset, &mut normal_v, &mut normal_i);

        for (index, stroke) in page.strokes.iter().enumerate() {
            if stroke.is_empty() {
                continue;
            }

            let predicted: Vec<Vec2> = self
                .sessions
                .values()
                .find_map(|s| match &s.role {
                    PointerRole::Drawing { stroke_index, predictor } if *stroke_index == index => {
                        Some(predictor.predict())
                    }
                    _ => None,
                })
                .unwrap_or_default();

            let (v, i) = if stroke.brush_type == BrushType::Highlighter {
                (&mut highlight_v, &mut highlight_i)
            } else {
                (&mut normal_v, &mut normal_i)
            };
            stroke.tessellate(&predicted, self.view_offset, now, v, i);
        }

        if self.zone_enabled {
            push_rect(
                Vec2::new(self.screen_size.x / 2.0 - DIVIDER_WIDTH / 2.0, 0.0),
                Vec2::new(DIVIDER_WIDTH, self.screen_size.y),
                DIVIDER_COLOR,
                &mut normal_v,
                &mut normal_i,
            );
        }

        if let Some(cursor) = self.eraser_cursor {
            push_circle(cursor, self.eraser_radius, [1.0, 1.0, 1.0, 0.18], 24, &mut normal_v, &mut normal_i);
        }

        let toolbar_state = ToolbarState {
            active_tool: self.active_tool,
            brush_type: self.active_pen.brush_type,
            color: self.active_pen.color,
            zone_enabled: self.zone_enabled,
            eraser_mode: self.eraser_mode,
            background: page.background,
            grid: page.grid,
            page_index: self.current_page,
            page_count: self.pages.len(),
        };
        self.toolbar.render(&toolbar_state, &mut normal_v, &mut normal_i);

        if let Some(menu) = &self.radial_menu {
            // No live drag-hover any more (tap-to-select, see module docs).
            menu.render(None, &mut normal_v, &mut normal_i);
        }

        (normal_v, normal_i, highlight_v, highlight_i)
    }
}
