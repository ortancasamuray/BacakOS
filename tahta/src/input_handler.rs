//! Central input state machine: routes every mouse/touch event to the
//! right place — drawing, panning, erasing, the toolbar, the radial menu,
//! or the palm-rejection heuristic — and owns the strokes those events
//! produce. `app.rs` is just winit glue; this is where "what does a touch
//! actually do" is decided.

use std::collections::HashMap;
use std::sync::Arc;

use glam::Vec2;
use winit::event::KeyEvent;
use winit::keyboard::{Key, NamedKey};

use crate::board::{self, Page};
use crate::brush::{next_palette_color, BrushType, PenSettings};
use crate::palm;
use crate::prediction::{TouchPredictor, TouchSample};
use crate::stroke::{push_circle, push_line, push_rect, Stroke, Vertex};
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

/// Two Pen taps landing within this many seconds of each other, and this
/// close together, are treated as a deliberate double-tap that opens the
/// radial menu — NOT a single-finger long-press. The compositor has its
/// own single-finger long-press → text-selection gesture
/// (`selection_recognizer`, armed whenever exactly one finger is down,
/// fired in parallel with whatever the client does with that same touch)
/// and a hold-triggered menu here would always collide with it. A quick
/// tap-release-tap never arms that recognizer (it requires a *hold*), so
/// double-tap is the safe equivalent — previously this was a two-finger
/// tap for the same reason, before the radial menu's trigger moved to
/// something reachable one-handed with the pen.
const DOUBLE_TAP_WINDOW_SECS: f64 = 0.35;
const DOUBLE_TAP_DISTANCE: f32 = 60.0;
/// How far a tap's points may wander from its start and still count as a
/// "tap" rather than a real stroke, for double-tap purposes.
const DOUBLE_TAP_MAX_MOVEMENT: f32 = 16.0;

/// Sentinel pointer id for the mouse, kept out of the touch id space.
const MOUSE_POINTER_ID: u64 = u64::MAX;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Left,
    Right,
}


enum PointerRole {
    /// Actively extending a stroke. `stroke_index` indexes `strokes`.
    /// `edge_snap`, when set (touch started near a drafting tool's edge),
    /// is the `(a, b)` edge line every point gets projected onto before
    /// being pushed — see `ruler::Ruler::nearest_edge` /
    /// `setsquare::SetSquare::nearest_edge`.
    Drawing { stroke_index: usize, predictor: TouchPredictor, edge_snap: Option<(Vec2, Vec2)> },
    /// Hand tool: drags `view_offset`.
    Panning { last_pos: Vec2 },
    /// Eraser tool: tombstones (clears) any stroke it passes near.
    Erasing,
    /// Compass tool: drag radius previewed live in `compass_preview`,
    /// committed as a circle stroke on release.
    CompassDrag { center: Vec2 },
    /// Dragging the ruler's rotate handle.
    RulerRotate,
    /// Dragging the ruler's body to reposition it.
    RulerMove { grab_offset: Vec2 },
    /// Dragging the set-square's rotate handle.
    SetSquareRotate,
    /// Dragging the set-square's body to reposition it.
    SetSquareMove { grab_offset: Vec2 },
    /// Dragging the protractor's rotate handle.
    ProtractorRotate,
    /// Dragging the protractor's body to reposition it.
    ProtractorMove { grab_offset: Vec2 },
    /// Dragging the calculator panel by its header.
    CalculatorMove { grab_offset: Vec2 },
    /// Touch landed on a calculator key; already applied on touch-down,
    /// nothing more happens until lift (no drag-to-repeat).
    CalculatorPress,
    /// Dragging the stopwatch panel by its header.
    StopwatchMove { grab_offset: Vec2 },
    /// Touch landed on a stopwatch button; already applied on touch-down.
    StopwatchPress,
    /// Dragging the dice panel by its header.
    DiceMove { grab_offset: Vec2 },
    /// Touch landed on a die or the roll button; already applied on
    /// touch-down.
    DicePress,
    /// Dragging the spotlight's rim handle to resize it.
    SpotlightResize,
    /// Dragging the spotlight's bright window to reposition it.
    SpotlightMove { grab_offset: Vec2 },
    /// Dragging the magnifier's rim handle to resize it.
    MagnifierResize,
    /// Dragging the magnifier lens to reposition it.
    MagnifierMove { grab_offset: Vec2 },
    /// Dragging the text box panel by its header.
    TextBoxMove { grab_offset: Vec2 },
    /// Touch landed on a text box key; already applied on touch-down.
    TextBoxPress,
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
    /// Live (center, radius) while a Compass drag is in progress.
    compass_preview: Option<(Vec2, f32)>,
    ruler: Option<crate::ruler::Ruler>,
    setsquare: Option<crate::setsquare::SetSquare>,
    protractor: Option<crate::protractor::Protractor>,
    calculator: Option<crate::calculator::Calculator>,
    stopwatch: Option<crate::stopwatch::Stopwatch>,
    dice: Option<crate::dice::Dice>,
    spotlight: Option<crate::spotlight::Spotlight>,
    magnifier: Option<crate::magnifier::Magnifier>,
    textbox: Option<crate::textbox::TextBox>,
    /// Whether the embedded browser panel should be shown. The real
    /// `webengine::WebPanel` lives in `app.rs` (it needs to hand its
    /// frames to the wgpu renderer directly) — this is just the on/off
    /// state and fixed bounds for it.
    pub browser_visible: bool,

    zone_enabled: bool,
    zone_pens: [PenSettings; 2],

    radial_menu: Option<RadialMenu>,
    /// The last Pen touch that looked like a tap (not a real stroke):
    /// `(position, end_time, stroke_index)`. Checked on the next press to
    /// recognize a double-tap — see `DOUBLE_TAP_WINDOW_SECS` docs.
    /// `stroke_index` is the tiny dot it left, removed if promoted to a
    /// double-tap so opening the menu doesn't also leave a stray mark.
    last_tap: Option<(Vec2, f64, Option<usize>)>,
    /// Whether the toolbar draws/accepts input at all — the radial menu
    /// covers color/width/eraser, so a teacher can hide the toolbar
    /// entirely for more canvas; toggled from a wedge in that same menu
    /// (`RadialAction::ToggleToolbar`) so it's still reachable while hidden.
    pub toolbar_visible: bool,

    toolbar: Toolbar,
    screen_size: Vec2,

    last_input_at: Option<f64>,
    /// (message, expires-at) for the brief on-screen confirmation after a
    /// one-shot action like PDF export — no toast/notification system
    /// exists yet, so this is the whole of it.
    toast: Option<(String, f64)>,
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
            compass_preview: None,
            ruler: None,
            setsquare: None,
            protractor: None,
            calculator: None,
            stopwatch: None,
            dice: None,
            spotlight: None,
            magnifier: None,
            textbox: None,
            browser_visible: false,
            zone_enabled: false,
            zone_pens: [
                PenSettings::ballpoint([0.92, 0.92, 0.95, 1.0]),
                PenSettings::ballpoint([0.25, 0.55, 0.95, 1.0]),
            ],
            radial_menu: None,
            last_tap: None,
            toolbar_visible: true,
            toolbar: Toolbar::layout(screen_size),
            screen_size,
            last_input_at: None,
            toast: None,
        }
    }

    pub fn resize(&mut self, screen_size: Vec2) {
        self.screen_size = screen_size;
        self.toolbar = Toolbar::layout(screen_size);
    }

    /// Fixed (not yet draggable — matches this app's other panels'
    /// starting scope) on-screen rect for the embedded browser panel:
    /// centered, leaving room above the toolbar.
    pub fn browser_bounds(&self) -> (Vec2, Vec2) {
        let margin_top = 40.0;
        let margin_bottom = 210.0; // clears the two-row toolbar + margin
        let margin_side = 60.0;
        let top_left = Vec2::new(margin_side, margin_top);
        let size = Vec2::new(
            (self.screen_size.x - margin_side * 2.0).max(200.0),
            (self.screen_size.y - margin_top - margin_bottom).max(200.0),
        );
        (top_left, size)
    }

    /// Replaces the whole board with one page per page of the PDF at
    /// `path` — matches how `bacak-belge` opens a fresh document rather
    /// than merging into whatever was already on the board.
    pub fn load_pdf(&mut self, path: &str) -> anyhow::Result<()> {
        let images = crate::pdf::load_pdf_pages(path)?;
        self.pages = images.into_iter().map(Page::from_pdf_image).collect();
        if self.pages.is_empty() {
            self.pages.push(Page::new());
        }
        self.current_page = 0;
        self.view_offset = Vec2::ZERO;
        Ok(())
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

    /// The nearest drafting-tool edge within snapping range of `position`,
    /// if any — checks the ruler, the set-square and the protractor's
    /// baseline, and picks whichever edge is actually closer when more
    /// than one is visible and in range.
    fn find_edge_snap(&self, position: Vec2) -> Option<(Vec2, Vec2)> {
        let ruler_edge = self
            .ruler
            .as_ref()
            .filter(|r| r.distance_to_edge(position) <= crate::ruler::EDGE_SNAP_DISTANCE)
            .map(|r| (r.nearest_edge(position), r.distance_to_edge(position)));
        let setsquare_edge = self
            .setsquare
            .as_ref()
            .filter(|s| s.distance_to_edge(position) <= crate::setsquare::EDGE_SNAP_DISTANCE)
            .map(|s| (s.nearest_edge(position), s.distance_to_edge(position)));
        let protractor_edge = self
            .protractor
            .as_ref()
            .filter(|p| p.distance_to_edge(position) <= crate::protractor::EDGE_SNAP_DISTANCE)
            .map(|p| (p.nearest_edge(position), p.distance_to_edge(position)));

        [ruler_edge, setsquare_edge, protractor_edge]
            .into_iter()
            .flatten()
            .min_by(|(_, d1), (_, d2)| d1.partial_cmp(d2).unwrap())
            .map(|(edge, _)| edge)
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
            ToolbarAction::ToggleRuler => {
                self.ruler = match self.ruler {
                    Some(_) => None,
                    None => Some(crate::ruler::Ruler::new(self.screen_size / 2.0)),
                };
            }
            ToolbarAction::ToggleSetSquare => {
                self.setsquare = match self.setsquare {
                    Some(_) => None,
                    None => Some(crate::setsquare::SetSquare::new(self.screen_size / 2.0)),
                };
            }
            ToolbarAction::ToggleProtractor => {
                self.protractor = match self.protractor {
                    Some(_) => None,
                    None => Some(crate::protractor::Protractor::new(self.screen_size / 2.0)),
                };
            }
            ToolbarAction::ToggleCalculator => {
                self.calculator = match self.calculator {
                    Some(_) => None,
                    None => Some(crate::calculator::Calculator::new(self.screen_size / 2.0 - Vec2::new(140.0, 200.0))),
                };
            }
            ToolbarAction::ToggleStopwatch => {
                self.stopwatch = match self.stopwatch {
                    Some(_) => None,
                    None => Some(crate::stopwatch::Stopwatch::new(self.screen_size / 2.0 - Vec2::new(280.0, 100.0))),
                };
            }
            ToolbarAction::ToggleDice => {
                self.dice = match self.dice {
                    Some(_) => None,
                    None => {
                        let seed = self.last_input_at.unwrap_or(1.0).to_bits() ^ self.screen_size.x.to_bits() as u64;
                        Some(crate::dice::Dice::new(self.screen_size / 2.0 + Vec2::new(60.0, -100.0), seed))
                    }
                };
            }
            ToolbarAction::ToggleSpotlight => {
                self.spotlight = match self.spotlight {
                    Some(_) => None,
                    None => Some(crate::spotlight::Spotlight::new(self.screen_size / 2.0)),
                };
            }
            ToolbarAction::ToggleMagnifier => {
                self.magnifier = match self.magnifier {
                    Some(_) => None,
                    None => Some(crate::magnifier::Magnifier::new(self.screen_size / 2.0)),
                };
            }
            ToolbarAction::ToggleTextBox => {
                self.textbox = match self.textbox {
                    Some(_) => None,
                    None => Some(crate::textbox::TextBox::new(self.screen_size / 2.0 - Vec2::new(310.0, 240.0))),
                };
            }
            ToolbarAction::ToggleBrowser => {
                self.browser_visible = !self.browser_visible;
            }
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
            ToolbarAction::ExportPdf => {
                let now = self.last_input_at.unwrap_or(0.0);
                let message = match crate::pdf_export::export_to_pdf(&self.pages, self.screen_size) {
                    Ok(path) => {
                        log::info!("PDF dışa aktarıldı: {}", path.display());
                        "PDF KAYDEDILDI".to_string()
                    }
                    Err(e) => {
                        log::error!("PDF dışa aktarma hatası: {e:#}");
                        "PDF HATASI".to_string()
                    }
                };
                self.toast = Some((message, now + 2.5));
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
            RadialAction::ToggleToolbar => {
                self.toolbar_visible = !self.toolbar_visible;
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

        if self.toolbar_visible && self.toolbar.contains(position) {
            if let Some(action) = self.toolbar.hit_test(position) {
                self.apply_toolbar_action(action, self.zone_of(position));
            }
            self.sessions.insert(id, PointerSession { role: PointerRole::Toolbar, start_pos: position, start_time: now });
            return;
        }

        // The calculator panel is UI chrome like the toolbar: its header
        // drags the whole thing, any other point on it is a key-press,
        // and either way the touch is fully consumed here — never reaches
        // the canvas or a drafting tool's edge-snap zone underneath.
        if let Some(calculator) = &mut self.calculator {
            if calculator.is_on_header(position) {
                let grab_offset = position - calculator.position;
                self.sessions.insert(id, PointerSession { role: PointerRole::CalculatorMove { grab_offset }, start_pos: position, start_time: now });
                return;
            }
            if calculator.contains(position) {
                calculator.press_at(position);
                self.sessions.insert(id, PointerSession { role: PointerRole::CalculatorPress, start_pos: position, start_time: now });
                return;
            }
        }

        // Same UI-panel priority for the stopwatch.
        if let Some(stopwatch) = &mut self.stopwatch {
            if stopwatch.is_on_header(position) {
                let grab_offset = position - stopwatch.position;
                self.sessions.insert(id, PointerSession { role: PointerRole::StopwatchMove { grab_offset }, start_pos: position, start_time: now });
                return;
            }
            if stopwatch.contains(position) {
                stopwatch.press_at(position, now);
                self.sessions.insert(id, PointerSession { role: PointerRole::StopwatchPress, start_pos: position, start_time: now });
                return;
            }
        }

        // Same UI-panel priority for the dice.
        if let Some(dice) = &mut self.dice {
            if dice.is_on_header(position) {
                let grab_offset = position - dice.position;
                self.sessions.insert(id, PointerSession { role: PointerRole::DiceMove { grab_offset }, start_pos: position, start_time: now });
                return;
            }
            if dice.contains(position) {
                dice.press_at(position, now);
                self.sessions.insert(id, PointerSession { role: PointerRole::DicePress, start_pos: position, start_time: now });
                return;
            }
        }

        // Same UI-panel priority for the text box.
        if let Some(textbox) = &mut self.textbox {
            if textbox.is_on_header(position) {
                let grab_offset = position - textbox.position;
                self.sessions.insert(id, PointerSession { role: PointerRole::TextBoxMove { grab_offset }, start_pos: position, start_time: now });
                return;
            }
            if textbox.contains(position) {
                textbox.press_at(position);
                self.sessions.insert(id, PointerSession { role: PointerRole::TextBoxPress, start_pos: position, start_time: now });
                return;
            }
        }

        // Grabbing the ruler's handle (rotate) or body (move) takes
        // priority over drawing — checked before edge-snap below, since
        // the handle/body zones and the snap zone don't overlap in
        // practice but handle/body is the more deliberate gesture.
        if let Some(ruler) = &self.ruler {
            if ruler.is_on_handle(position) {
                self.sessions.insert(id, PointerSession { role: PointerRole::RulerRotate, start_pos: position, start_time: now });
                return;
            }
            if ruler.is_on_body(position) && ruler.distance_to_edge(position) > crate::ruler::EDGE_SNAP_DISTANCE {
                let grab_offset = position - ruler.center;
                self.sessions.insert(id, PointerSession { role: PointerRole::RulerMove { grab_offset }, start_pos: position, start_time: now });
                return;
            }
        }

        // Same handle/body-grab priority for the set-square.
        if let Some(setsquare) = &self.setsquare {
            if setsquare.is_on_handle(position) {
                self.sessions.insert(id, PointerSession { role: PointerRole::SetSquareRotate, start_pos: position, start_time: now });
                return;
            }
            if setsquare.is_on_body(position) && setsquare.distance_to_edge(position) > crate::setsquare::EDGE_SNAP_DISTANCE {
                let grab_offset = position - setsquare.right_angle;
                self.sessions.insert(id, PointerSession { role: PointerRole::SetSquareMove { grab_offset }, start_pos: position, start_time: now });
                return;
            }
        }

        // Same handle/body-grab priority for the protractor.
        if let Some(protractor) = &self.protractor {
            if protractor.is_on_handle(position) {
                self.sessions.insert(id, PointerSession { role: PointerRole::ProtractorRotate, start_pos: position, start_time: now });
                return;
            }
            if protractor.is_on_body(position) && protractor.distance_to_edge(position) > crate::protractor::EDGE_SNAP_DISTANCE {
                let grab_offset = position - protractor.center;
                self.sessions.insert(id, PointerSession { role: PointerRole::ProtractorMove { grab_offset }, start_pos: position, start_time: now });
                return;
            }
        }

        // Same handle/body-grab priority for the spotlight — like the
        // drafting overlays above (and unlike the calculator/stopwatch/
        // dice panels), everything outside its handle/bright window still
        // falls through to drawing/erasing below.
        if let Some(spotlight) = &self.spotlight {
            if spotlight.is_on_handle(position) {
                self.sessions.insert(id, PointerSession { role: PointerRole::SpotlightResize, start_pos: position, start_time: now });
                return;
            }
            if spotlight.is_on_body(position) {
                let grab_offset = position - spotlight.center;
                self.sessions.insert(id, PointerSession { role: PointerRole::SpotlightMove { grab_offset }, start_pos: position, start_time: now });
                return;
            }
        }

        // Same for the magnifier.
        if let Some(magnifier) = &self.magnifier {
            if magnifier.is_on_handle(position) {
                self.sessions.insert(id, PointerSession { role: PointerRole::MagnifierResize, start_pos: position, start_time: now });
                return;
            }
            if magnifier.is_on_body(position) {
                let grab_offset = position - magnifier.center;
                self.sessions.insert(id, PointerSession { role: PointerRole::MagnifierMove { grab_offset }, start_pos: position, start_time: now });
                return;
            }
        }

        let zone = self.zone_of(position);

        // A quick double-tap with the Pen opens the radial menu right at
        // the tap point — see `DOUBLE_TAP_WINDOW_SECS` docs for why this,
        // not a long-press, is the trigger.
        if self.active_tool == Tool::Pen {
            if let Some((tap_pos, tap_time, stroke_idx)) = self.last_tap.take() {
                let close_in_time = (now - tap_time).abs() <= DOUBLE_TAP_WINDOW_SECS;
                let close_in_space = tap_pos.distance(position) <= DOUBLE_TAP_DISTANCE;
                if close_in_time && close_in_space {
                    // Remove the first tap's tiny dot so opening the menu
                    // doesn't also leave a stray mark on the page.
                    if let Some(idx) = stroke_idx {
                        let strokes = &mut self.pages[self.current_page].strokes;
                        if strokes.len() == idx + 1 {
                            strokes.pop();
                        }
                    }
                    self.open_radial_menu_at(position);
                    self.sessions.insert(id, PointerSession { role: PointerRole::Palm, start_pos: position, start_time: now });
                    return;
                }
            }
        }

        let role = match self.active_tool {
            Tool::Pen => {
                if palm::is_probable_palm(position, now, self.active_pen_touch_starts()) {
                    self.demote_cluster_to_palm(position);
                    self.erase_area(position, PALM_ERASE_RADIUS);
                    PointerRole::Palm
                } else {
                    let edge_snap = self.find_edge_snap(position);
                    let start = match edge_snap {
                        Some((a, b)) => crate::geom::project_onto_segment(position, a, b),
                        None => position,
                    };

                    let pen = self.pen_for(zone).clone();
                    let page = &mut self.pages[self.current_page];
                    let stroke_index = page.strokes.len();
                    let mut stroke = Stroke::new(pen.color, pen.base_width, pen.brush_type);
                    stroke.push_point(start, now);
                    page.strokes.push(stroke);

                    let mut predictor = TouchPredictor::new(LOOKAHEAD_MS);
                    predictor.push_sample(TouchSample { position: start, timestamp: now });

                    PointerRole::Drawing { stroke_index, predictor, edge_snap }
                }
            }
            Tool::Hand => PointerRole::Panning { last_pos: position },
            Tool::Eraser => {
                self.erase_at(position, self.eraser_radius);
                self.eraser_cursor = Some(position);
                PointerRole::Erasing
            }
            Tool::Compass => {
                self.compass_preview = Some((position, 0.0));
                PointerRole::CompassDrag { center: position }
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
            PointerRole::Drawing { stroke_index, predictor, edge_snap } => {
                let point = match edge_snap {
                    Some((a, b)) => crate::geom::project_onto_segment(position, *a, *b),
                    None => position,
                };
                let Some(stroke) = self.pages[self.current_page].strokes.get_mut(*stroke_index) else {
                    session.role = PointerRole::Palm; // stroke was undone from under us
                    return;
                };
                stroke.push_point(point, now);
                predictor.push_sample(TouchSample { position: point, timestamp: now });
            }
            PointerRole::Panning { last_pos } => {
                self.view_offset += position - *last_pos;
                *last_pos = position;
            }
            PointerRole::Erasing => {
                self.eraser_cursor = Some(position);
                do_erase = true;
            }
            PointerRole::CompassDrag { center } => {
                self.compass_preview = Some((*center, center.distance(position)));
            }
            PointerRole::RulerRotate => {
                if let Some(ruler) = &mut self.ruler {
                    ruler.rotate_toward(position);
                }
            }
            PointerRole::RulerMove { grab_offset } => {
                let grab_offset = *grab_offset;
                if let Some(ruler) = &mut self.ruler {
                    ruler.drag_to(position, grab_offset);
                }
            }
            PointerRole::SetSquareRotate => {
                if let Some(setsquare) = &mut self.setsquare {
                    setsquare.rotate_toward(position);
                }
            }
            PointerRole::SetSquareMove { grab_offset } => {
                let grab_offset = *grab_offset;
                if let Some(setsquare) = &mut self.setsquare {
                    setsquare.drag_to(position, grab_offset);
                }
            }
            PointerRole::ProtractorRotate => {
                if let Some(protractor) = &mut self.protractor {
                    protractor.rotate_toward(position);
                }
            }
            PointerRole::ProtractorMove { grab_offset } => {
                let grab_offset = *grab_offset;
                if let Some(protractor) = &mut self.protractor {
                    protractor.drag_to(position, grab_offset);
                }
            }
            PointerRole::CalculatorMove { grab_offset } => {
                let grab_offset = *grab_offset;
                if let Some(calculator) = &mut self.calculator {
                    calculator.drag_to(position, grab_offset);
                }
            }
            PointerRole::StopwatchMove { grab_offset } => {
                let grab_offset = *grab_offset;
                if let Some(stopwatch) = &mut self.stopwatch {
                    stopwatch.drag_to(position, grab_offset);
                }
            }
            PointerRole::DiceMove { grab_offset } => {
                let grab_offset = *grab_offset;
                if let Some(dice) = &mut self.dice {
                    dice.drag_to(position, grab_offset);
                }
            }
            PointerRole::SpotlightResize => {
                if let Some(spotlight) = &mut self.spotlight {
                    spotlight.resize_to(position);
                }
            }
            PointerRole::SpotlightMove { grab_offset } => {
                let grab_offset = *grab_offset;
                if let Some(spotlight) = &mut self.spotlight {
                    spotlight.drag_to(position, grab_offset);
                }
            }
            PointerRole::MagnifierResize => {
                if let Some(magnifier) = &mut self.magnifier {
                    magnifier.resize_to(position);
                }
            }
            PointerRole::MagnifierMove { grab_offset } => {
                let grab_offset = *grab_offset;
                if let Some(magnifier) = &mut self.magnifier {
                    magnifier.drag_to(position, grab_offset);
                }
            }
            PointerRole::TextBoxMove { grab_offset } => {
                let grab_offset = *grab_offset;
                if let Some(textbox) = &mut self.textbox {
                    textbox.drag_to(position, grab_offset);
                }
            }
            PointerRole::Toolbar
            | PointerRole::Palm
            | PointerRole::CalculatorPress
            | PointerRole::StopwatchPress
            | PointerRole::DicePress
            | PointerRole::TextBoxPress => {}
        }
        if do_erase {
            self.erase_at(position, self.eraser_radius);
        }
    }

    fn end_pointer(&mut self, id: u64, now: f64) {
        if let Some(session) = self.sessions.remove(&id) {
            match session.role {
                PointerRole::Erasing => self.eraser_cursor = None,
                PointerRole::CompassDrag { .. } => self.commit_compass_circle(),
                PointerRole::Drawing { stroke_index, .. } => {
                    // A stroke that barely moved is a tap — remember it so
                    // the *next* press can recognize a double-tap (see
                    // `DOUBLE_TAP_WINDOW_SECS` docs). A real stroke instead
                    // breaks any pending double-tap chain.
                    let is_tap = self.pages[self.current_page]
                        .strokes
                        .get(stroke_index)
                        .map(|s| {
                            let p0 = s.points.first().copied().unwrap_or(session.start_pos);
                            s.points.iter().map(|p| p.distance(p0)).fold(0.0f32, f32::max) <= DOUBLE_TAP_MAX_MOVEMENT
                        })
                        .unwrap_or(false);
                    self.last_tap = is_tap.then_some((session.start_pos, now, Some(stroke_index)));
                }
                _ => {}
            }
        }
    }

    /// Turns the live compass drag into a real circle stroke, sampled as a
    /// closed ring of points — reuses the ordinary `Stroke`/tessellation
    /// path, so a compass circle erases, exports, and re-colors exactly
    /// like freehand ink.
    fn commit_compass_circle(&mut self) {
        const MIN_RADIUS: f32 = 6.0;
        const SAMPLES: usize = 72;

        let Some((center, radius)) = self.compass_preview.take() else {
            return;
        };
        if radius < MIN_RADIUS {
            return;
        }

        let pen = self.active_pen.clone();
        let mut stroke = Stroke::new(pen.color, pen.base_width, pen.brush_type);
        let now = self.last_input_at.unwrap_or(0.0);
        for i in 0..=SAMPLES {
            let theta = i as f32 / SAMPLES as f32 * std::f32::consts::TAU;
            stroke.push_point(center + Vec2::new(theta.cos(), theta.sin()) * radius, now);
        }
        self.pages[self.current_page].strokes.push(stroke);
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

    pub fn mouse_released(&mut self, now: f64) {
        self.end_pointer(MOUSE_POINTER_ID, now);
    }

    // --- Touch -------------------------------------------------------------

    pub fn touch_started(&mut self, id: u64, pos: Vec2, now: f64) {
        self.begin_pointer(id, pos, now);
    }

    pub fn touch_moved(&mut self, id: u64, pos: Vec2, now: f64) {
        self.move_pointer(id, pos, now);
    }

    pub fn touch_ended(&mut self, id: u64, now: f64) {
        self.end_pointer(id, now);
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
    ) -> (Vec<Vertex>, Vec<u32>, Vec<Vertex>, Vec<u32>, Option<Arc<board::PdfImage>>) {
        let mut normal_v = Vec::new();
        let mut normal_i = Vec::new();
        let mut highlight_v = Vec::new();
        let mut highlight_i = Vec::new();

        let page = &self.pages[self.current_page];
        board::render_background(page, self.screen_size, self.view_offset, &mut normal_v, &mut normal_i);

        // Draw straightedge widgets before ink so strokes snapped to their
        // edges are never hidden underneath the widget body (see
        // project_tahta_whiteboard memory: visual-only overlap, not a bug).
        if let Some(ruler) = &self.ruler {
            ruler.render(&mut normal_v, &mut normal_i);
        }
        if let Some(setsquare) = &self.setsquare {
            setsquare.render(&mut normal_v, &mut normal_i);
        }
        if let Some(protractor) = &self.protractor {
            protractor.render(&mut normal_v, &mut normal_i);
        }

        for (index, stroke) in page.strokes.iter().enumerate() {
            if stroke.is_empty() {
                continue;
            }

            let predicted: Vec<Vec2> = self
                .sessions
                .values()
                .find_map(|s| match &s.role {
                    PointerRole::Drawing { stroke_index, predictor, .. } if *stroke_index == index => {
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

        if let Some((center, radius)) = self.compass_preview {
            const SEGMENTS: usize = 48;
            let color = self.active_pen.color;
            for i in 0..SEGMENTS {
                let a0 = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
                let a1 = (i + 1) as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
                let p0 = center + Vec2::new(a0.cos(), a0.sin()) * radius;
                let p1 = center + Vec2::new(a1.cos(), a1.sin()) * radius;
                push_line(p0, p1, self.active_pen.base_width, color, &mut normal_v, &mut normal_i);
            }
            push_circle(center, 3.0, color, 10, &mut normal_v, &mut normal_i);
        }

        if let Some(magnifier) = &self.magnifier {
            magnifier.render(page, self.view_offset, now, &mut normal_v, &mut normal_i);
        }
        if let Some(spotlight) = &self.spotlight {
            spotlight.render(self.screen_size, &mut normal_v, &mut normal_i);
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
            ruler_visible: self.ruler.is_some(),
            setsquare_visible: self.setsquare.is_some(),
            protractor_visible: self.protractor.is_some(),
            calculator_visible: self.calculator.is_some(),
            stopwatch_visible: self.stopwatch.is_some(),
            dice_visible: self.dice.is_some(),
            spotlight_visible: self.spotlight.is_some(),
            magnifier_visible: self.magnifier.is_some(),
            textbox_visible: self.textbox.is_some(),
            browser_visible: self.browser_visible,
        };
        if self.toolbar_visible {
            self.toolbar.render(&toolbar_state, &mut normal_v, &mut normal_i);
        }

        if let Some(calculator) = &self.calculator {
            calculator.render(&mut normal_v, &mut normal_i);
        }
        if let Some(stopwatch) = &self.stopwatch {
            stopwatch.render(now, &mut normal_v, &mut normal_i);
        }
        if let Some(dice) = &self.dice {
            dice.render(&mut normal_v, &mut normal_i);
        }
        if let Some(textbox) = &self.textbox {
            textbox.render(&mut normal_v, &mut normal_i);
        }

        if let Some(menu) = &self.radial_menu {
            // No live drag-hover any more (tap-to-select, see module docs).
            menu.render(None, &mut normal_v, &mut normal_i);
        }

        if let Some((message, expires_at)) = &self.toast {
            if now < *expires_at {
                let cell = Vec2::new(14.0, 22.0);
                let width = message.chars().count() as f32 * (cell.x + cell.x * 1.5);
                let pos = Vec2::new((self.screen_size.x - width) / 2.0, 32.0);
                push_rect(pos - Vec2::new(16.0, 12.0), Vec2::new(width + 32.0, cell.y + 24.0), [0.08, 0.09, 0.11, 0.88], &mut normal_v, &mut normal_i);
                crate::font5x7::push_text(message, pos, cell, [0.95, 0.95, 0.97, 1.0], &mut normal_v, &mut normal_i);
            }
        }

        (normal_v, normal_i, highlight_v, highlight_i, page.pdf_image.clone())
    }
}
