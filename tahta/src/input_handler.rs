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

const MIN_VIEW_SCALE: f32 = 0.4;
const MAX_VIEW_SCALE: f32 = 3.0;

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

/// Two-finger pinch-to-zoom, engaged from `Tool::Hand` when a second
/// finger touches down while one is already panning (see `begin_pointer`).
/// Drives `view_scale`/`view_offset` directly every `move_pointer` call
/// for either id — the two touches don't get their own `PointerRole`
/// beyond the placeholder `Palm` (this state machine, not per-touch role
/// dispatch, owns the transform while it's active).
struct PinchState {
    id_a: u64,
    id_b: u64,
    pos_a: Vec2,
    pos_b: Vec2,
    /// Distance between the two fingers when the pinch began — the
    /// current/start ratio scales `start_scale`.
    start_distance: f32,
    start_scale: f32,
    /// Canvas-space point that was under both fingers' midpoint when the
    /// pinch began, kept fixed under the (possibly moving) midpoint for
    /// the whole gesture so zooming feels anchored, not like it recenters.
    canvas_anchor: Vec2,
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
    /// Dragging the graph panel by its header.
    GraphMove { grab_offset: Vec2 },
    /// Dragging inside the graph's plot: pans its view.
    GraphPan { last_pos: Vec2 },
    /// Dragging one of the graph's `a`/`b`/`c` sliders.
    GraphSlider { param: usize },
    /// Touch landed on a graph key/formula row; already applied.
    GraphPress,
    /// Dragging the solid panel by its header.
    SolidMove { grab_offset: Vec2 },
    /// Finger on the solid's 3D view: turns it once it has moved past
    /// the tap slop; lifted before that, it's a tap (section point).
    SolidView { last_pos: Vec2, moved: f32 },
    /// Dragging the solid's section-plane slider.
    SolidSlider,
    /// Touch landed on a solid button; already applied.
    SolidPress,
    /// Dragging the periodic table panel by its header.
    PeriodicMove { grab_offset: Vec2 },
    /// Touch landed on a periodic table cell/button; already applied.
    PeriodicPress,
    /// Framing a handwritten formula for the graph panel's "Tahtadan oku".
    FormulaPick,
    /// Dragging the "Sor" card by its header.
    AskMove { grab_offset: Vec2 },
    /// Finger on the card's answer: drag scrolls it.
    AskScroll { last_y: f32 },
    /// Touch landed on a card button; already applied.
    AskPress,
    /// Dragging the selected stamped figure. The undo checkpoint is taken
    /// on the first real movement, so a tap leaves no empty undo step.
    SelectionMove { last_pos: Vec2, started: bool },
    /// Dragging the selected picture; checkpoint on first real movement.
    PictureMove { last_pos: Vec2, started: bool },
    /// Dragging one of the selected picture's corners: `anchor` is the
    /// opposite corner (screen space), which stays put; aspect is kept.
    PictureResize { anchor: Vec2, dir: Vec2, started: bool },
    /// Touch landed on the toolbar; consumed, never reaches the canvas.
    Toolbar,
    /// Flagged by the palm-rejection heuristic, or consumed by a
    /// two-finger-tap gesture / the open radial menu / the open "Aç"
    /// panel; ignored until lift.
    Palm,
}

/// Selected picture: drawn radius of its corner handles, and how far
/// from a corner a finger still grabs it.
const PICTURE_CORNER_R: f32 = 12.0;
const PICTURE_CORNER_GRAB: f32 = 34.0;
/// Smallest a picture can be resized to (screen pixels, shorter side).
const PICTURE_MIN_SIZE: f32 = 40.0;

/// What a touch on/around the selected picture grabs.
#[derive(Debug, PartialEq)]
enum PictureGrab {
    Delete,
    /// Corner `k` (0 top-left, 1 top-right, 2 bottom-right, 3 bottom-left).
    Corner(usize),
    Body,
    Outside,
}

fn picture_corners((tl, size): (Vec2, Vec2)) -> [Vec2; 4] {
    [tl, tl + Vec2::new(size.x, 0.0), tl + size, tl + Vec2::new(0.0, size.y)]
}

/// Screen-space frame (top-left, bottom-right) of a picture rect, for the
/// delete button's placement (shared with stamped figures' move handle).
fn picture_frame((tl, size): (Vec2, Vec2)) -> (Vec2, Vec2) {
    (tl, tl + size)
}

fn picture_grab(rect: (Vec2, Vec2), position: Vec2, screen_size: Vec2) -> PictureGrab {
    if position.distance(selection_handle(picture_frame(rect), screen_size)) <= SELECTION_HANDLE_R + 8.0 {
        return PictureGrab::Delete;
    }
    let corners = picture_corners(rect);
    let nearest = (0..4).min_by(|&a, &b| corners[a].distance(position).total_cmp(&corners[b].distance(position))).unwrap();
    if corners[nearest].distance(position) <= PICTURE_CORNER_GRAB {
        return PictureGrab::Corner(nearest);
    }
    let (tl, br) = picture_frame(rect);
    if position.cmpge(tl).all() && position.cmple(br).all() {
        PictureGrab::Body
    } else {
        PictureGrab::Outside
    }
}

/// Where a picture fits on a fresh page: as large as it can be within
/// most of the screen above the toolbar, centered there, aspect kept.
/// The area is below the toast band (so the delete button above the
/// picture stays reachable) and above the toolbar and its page dots.
fn fit_picture(width: u32, height: u32, screen_size: Vec2) -> (Vec2, Vec2) {
    let (top, bottom) = (150.0, screen_size.y - 130.0);
    let area = Vec2::new(screen_size.x * 0.8, (bottom - top).max(100.0));
    let natural = Vec2::new(width as f32, height as f32);
    let scale = (area.x / natural.x).min(area.y / natural.y);
    let size = natural * scale;
    let center = Vec2::new(screen_size.x / 2.0, (top + bottom) / 2.0);
    (center - size / 2.0, size)
}

/// New screen rect for a corner drag: the opposite corner `anchor` stays,
/// the size follows the finger projected onto the diagonal in the
/// corner's direction `dir` (±1 per axis), keeping `aspect` (w / h).
fn resize_from_anchor(anchor: Vec2, dir: Vec2, finger: Vec2, aspect: f32) -> (Vec2, Vec2) {
    let diagonal = Vec2::new(aspect, 1.0);
    let t = ((finger - anchor) * dir).dot(diagonal) / diagonal.length_squared();
    let min_t = PICTURE_MIN_SIZE / aspect.min(1.0);
    let size = diagonal * t.max(min_t);
    let tl = Vec2::new(if dir.x > 0.0 { anchor.x } else { anchor.x - size.x }, if dir.y > 0.0 { anchor.y } else { anchor.y - size.y });
    (tl, size)
}

/// Radius of the selected figure's move handle.
const SELECTION_HANDLE_R: f32 = 20.0;

/// Where the selected figure's move handle sits: centered above its
/// frame, or below it when the frame reaches the top of the screen.
fn selection_handle((tl, br): (Vec2, Vec2), screen_size: Vec2) -> Vec2 {
    let x = ((tl.x + br.x) / 2.0).clamp(SELECTION_HANDLE_R, screen_size.x - SELECTION_HANDLE_R);
    let above = tl.y - SELECTION_HANDLE_R - 8.0;
    let y = if above >= SELECTION_HANDLE_R + 4.0 { above } else { br.y + SELECTION_HANDLE_R + 8.0 };
    Vec2::new(x, y)
}

/// One drag step on the selected picture (screen space).
enum PictureChange {
    Move(Vec2),
    Resize { anchor: Vec2, dir: Vec2, finger: Vec2 },
}

/// What `InputHandler::loading` will do with its page images.
#[derive(Clone, Copy)]
enum Loading {
    Pdf,
    Screen,
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
    /// Canvas zoom factor — `1.0` is unzoomed. See `to_canvas`/`PinchState`.
    view_scale: f32,
    /// Live two-finger pinch-zoom gesture, if one is in progress.
    pinch: Option<PinchState>,
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
    graph: Option<crate::graph::Graph>,
    /// The graph panel while hidden — reopening it brings back the same
    /// formulas, sliders and view.
    graph_stash: Option<crate::graph::Graph>,
    solid: Option<crate::solid::Solid>,
    /// Like `graph_stash`, for the solid panel.
    solid_stash: Option<crate::solid::Solid>,
    periodic: Option<crate::periodic::Periodic>,
    /// Like `graph_stash`: the table reopens on the same element and mode.
    periodic_stash: Option<crate::periodic::Periodic>,
    /// The stamped figure (`Stroke::group`) currently selected for moving:
    /// set right after "Tahtaya bas", or by tapping a figure with the Hand
    /// tool; a touch outside its frame lets go.
    selection: Option<u32>,
    /// "Tahtadan oku" is waiting for the teacher to frame a formula; the
    /// live frame (screen space) while they drag.
    formula_pick: bool,
    formula_rect: Option<(Vec2, Vec2)>,
    /// "Sor" armed: the next drag frames the question (shares
    /// `formula_rect` and the `FormulaPick` session with "Tahtadan oku").
    ask_pick: bool,
    ask: Option<crate::ask::AskCard>,
    /// "Alanı yeni sayfaya al" armed: the next drag frames part of the
    /// page image (shares `formula_rect` and the `FormulaPick` session).
    crop_pick: bool,
    /// Selected placed picture, as (page, index) — shows its frame,
    /// corner handles and delete button.
    picture_selection: Option<(usize, usize)>,
    /// The "Aç" panel, while open (modal, see `opener.rs`).
    opener: Option<crate::opener::Opener>,
    /// Page images being made on their own thread — a PDF picked in the
    /// "Aç" panel (a long document takes seconds) or the screen grab —
    /// picked up by `tick`.
    loading: Option<(Loading, std::sync::mpsc::Receiver<anyhow::Result<Vec<Arc<board::PdfImage>>>>)>,

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

    /// Real-font glyph metrics for `textbox.rs`'s display/keyboard — see
    /// `font_atlas.rs`. Built once in `main.rs`, shared with the renderer
    /// (which owns the matching GPU texture) via `Arc`.
    font_atlas: Arc<crate::font_atlas::FontAtlas>,

    /// Set by the toolbar's "return to desktop" button — `app.rs` polls
    /// this via `take_minimize_request` (only it owns the winit `Window`
    /// this needs) since this module stays toolkit-agnostic.
    minimize_requested: bool,

    /// The board changed since the last autosave; `last_change` is when
    /// (input clock), so `take_save_request` can wait for a quiet moment.
    dirty: bool,
    last_change: f64,
    /// Save right away instead of waiting for a quiet moment (minimize).
    save_now: bool,
    /// "New board" needs a second tap before this deadline (input clock).
    new_board_armed_until: Option<f64>,
    /// "Bu sayfayı sil" needs a second tap before this deadline.
    delete_page_armed_until: Option<f64>,
    /// The board "New board" replaced, for `app.rs` to keep as a backup.
    replaced_board: Option<crate::persist::BoardData>,
}

impl InputHandler {
    pub fn new(screen_size: Vec2, font_atlas: Arc<crate::font_atlas::FontAtlas>) -> Self {
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
            view_scale: 1.0,
            pinch: None,
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
            graph: None,
            graph_stash: None,
            solid: None,
            solid_stash: None,
            periodic: None,
            periodic_stash: None,
            selection: None,
            formula_pick: false,
            formula_rect: None,
            ask_pick: false,
            ask: None,
            crop_pick: false,
            picture_selection: None,
            opener: None,
            loading: None,
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
            font_atlas,
            minimize_requested: false,
            dirty: false,
            last_change: 0.0,
            save_now: false,
            new_board_armed_until: None,
            delete_page_armed_until: None,
            replaced_board: None,
        }
    }

    /// True (once) if the toolbar's "return to desktop" button was tapped
    /// since the last call — clears the flag so `app.rs` only acts on it
    /// once per tap.
    pub fn take_minimize_request(&mut self) -> bool {
        std::mem::take(&mut self.minimize_requested)
    }

    pub fn resize(&mut self, screen_size: Vec2) {
        self.screen_size = screen_size;
        self.toolbar = Toolbar::layout(screen_size);
    }

    /// Replaces the whole board with one page per page of the PDF at
    /// `path` — matches how `bacak-belge` opens a fresh document rather
    /// than merging into whatever was already on the board.
    pub fn load_pdf(&mut self, path: &str) -> anyhow::Result<()> {
        let images = crate::pdf::load_pdf_pages(path)?;
        self.set_pdf_pages(images);
        Ok(())
    }

    fn set_pdf_pages(&mut self, images: Vec<Arc<board::PdfImage>>) {
        self.release_drawing_pointers();
        self.pages = images.into_iter().map(Page::from_pdf_image).collect();
        if self.pages.is_empty() {
            self.pages.push(Page::new());
        }
        self.current_page = 0;
        self.mark_dirty();
        self.view_offset = Vec2::ZERO;
        self.view_scale = 1.0;
    }

    /// Screen-space `p` to canvas-space, inverting the current pan/zoom
    /// transform — `Stroke.points` are stored in canvas-space so old ink
    /// stays correctly placed under it regardless of when it was drawn
    /// relative to the current view (pan/zoom applied at render/hit-test
    /// time via the same transform, see `stroke.rs`/`board.rs`).
    fn to_canvas(&self, p: Vec2) -> Vec2 {
        (p - self.view_offset) / self.view_scale
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
            if stroke.brush_type == BrushType::LaserPointer && !stroke.points.is_empty() {
                Arc::make_mut(stroke).prune_expired(now);
            }
        }
        if self.new_board_armed_until.is_some_and(|t| now > t) {
            self.new_board_armed_until = None;
        }
        if self.delete_page_armed_until.is_some_and(|t| now > t) {
            self.delete_page_armed_until = None;
        }
        self.poll_loading(now);
        if let Some(card) = &mut self.ask {
            card.poll();
        }
    }

    // --- Opening a PDF / the backup ("Aç" panel) ----------------------------

    fn open_choice(&mut self, choice: crate::opener::Choice, now: f64) {
        match choice {
            crate::opener::Choice::Backup => {
                let message = match crate::persist::load(&crate::persist::backup_dir()) {
                    Ok(Some(data)) => {
                        // A swap: the board in use becomes the backup, so
                        // opening it was never a step that loses work.
                        self.release_drawing_pointers();
                        self.keep_as_backup();
                        self.restore_board(data);
                        self.view_offset = Vec2::ZERO;
                        self.view_scale = 1.0;
                        self.mark_dirty();
                        self.save_now = true;
                        "Önceki tahta açıldı"
                    }
                    Ok(None) => "Önceki tahta yok",
                    Err(e) => {
                        log::error!("önceki tahta açılamadı: {e:#}");
                        "Önceki tahta açılamadı"
                    }
                };
                self.toast = Some((message.to_string(), now + 2.5));
            }
            crate::opener::Choice::Pdf(path) => {
                self.start_loading(Loading::Pdf, move || crate::pdf::load_pdf_pages(&path.to_string_lossy()));
                self.toast = Some(("PDF açılıyor…".to_string(), now + 120.0));
            }
        }
    }

    fn start_loading(&mut self, kind: Loading, job: impl FnOnce() -> anyhow::Result<Vec<Arc<board::PdfImage>>> + Send + 'static) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(job());
        });
        self.loading = Some((kind, rx));
    }

    /// A decoded PDF replaces the board (the board it replaces is kept as
    /// the backup); a screen grab becomes a new page after the current one.
    fn poll_loading(&mut self, now: f64) {
        let Some((kind, rx)) = &self.loading else { return };
        let kind = *kind;
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(anyhow::anyhow!("yükleyici durdu")),
        };
        self.loading = None;
        let message = match (kind, result) {
            (Loading::Pdf, Ok(images)) => {
                self.keep_as_backup();
                self.set_pdf_pages(images);
                self.save_now = true;
                "PDF açıldı"
            }
            (Loading::Pdf, Err(e)) => {
                log::error!("PDF açılamadı: {e:#}");
                "PDF açılamadı"
            }
            (Loading::Screen, Ok(images)) => {
                self.release_drawing_pointers();
                for (i, image) in images.into_iter().enumerate() {
                    self.pages.insert(self.current_page + 1 + i, Page::from_pdf_image(image));
                }
                self.current_page = (self.current_page + 1).min(self.pages.len() - 1);
                self.view_offset = Vec2::ZERO;
                self.view_scale = 1.0;
                self.mark_dirty();
                "Ekran yeni sayfaya alındı"
            }
            (Loading::Screen, Err(e)) => {
                log::error!("ekran alınamadı: {e:#}");
                "Ekran alınamadı"
            }
        };
        self.toast = Some((message.to_string(), now + 2.5));
    }

    /// Hand the board about to be replaced to `app.rs` as the backup —
    /// unless it's blank, which would only push out a backup worth more.
    fn keep_as_backup(&mut self) {
        let blank = self.pages.len() == 1 && self.pages[0].pdf_image.is_none()
            && self.pages[0].strokes.is_empty()
            && self.pages[0].pictures.is_empty();
        if !blank {
            self.replaced_board = Some(self.board_data());
        }
    }

    // --- History & autosave --------------------------------------------------

    fn mark_dirty(&mut self) {
        self.dirty = true;
        self.last_change = self.last_input_at.unwrap_or(self.last_change);
    }

    /// Undo step for the current page, taken right before a change.
    fn checkpoint(&mut self) {
        self.pages[self.current_page].checkpoint();
        self.mark_dirty();
    }

    /// Undo/redo swap the stroke list out from under any finger still
    /// drawing; its `stroke_index` would then point at the wrong stroke.
    fn release_drawing_pointers(&mut self) {
        for session in self.sessions.values_mut() {
            if matches!(session.role, PointerRole::Drawing { .. }) {
                session.role = PointerRole::Palm;
            }
        }
    }

    fn undo(&mut self) {
        self.release_drawing_pointers();
        if self.pages[self.current_page].undo() {
            self.mark_dirty();
        }
    }

    fn redo(&mut self) {
        self.release_drawing_pointers();
        if self.pages[self.current_page].redo() {
            self.mark_dirty();
        }
    }

    fn clear_page(&mut self) {
        if !self.pages[self.current_page].strokes.is_empty() {
            self.checkpoint();
            self.pages[self.current_page].strokes.clear();
        }
    }

    /// Everything the board file stores, cheaply (strokes and page images
    /// are shared, not copied) — safe to hand to a writer thread.
    pub fn board_data(&self) -> crate::persist::BoardData {
        crate::persist::BoardData {
            current_page: self.current_page,
            pages: self
                .pages
                .iter()
                .map(|p| crate::persist::PageData {
                    background: p.background,
                    grid: p.grid,
                    pdf_image: p.pdf_image.clone(),
                    strokes: p.strokes.clone(),
                    pictures: p.pictures.clone(),
                })
                .collect(),
        }
    }

    /// Replace the board with a saved one (startup restore).
    pub fn restore_board(&mut self, data: crate::persist::BoardData) {
        let pages: Vec<Page> = data
            .pages
            .into_iter()
            .map(|p| {
                let mut page = Page::with(p.background, p.grid, p.pdf_image);
                page.strokes = p.strokes;
                page.pictures = p.pictures;
                page
            })
            .collect();
        if pages.is_empty() {
            return;
        }
        self.current_page = data.current_page.min(pages.len() - 1);
        self.pages = pages;
        self.picture_selection = None;
        self.dirty = false;
    }

    /// The board to autosave, once it changed and has been quiet for
    /// `quiet_secs` with no finger still drawing (a half-drawn stroke is
    /// saved on the next round instead). `None` when there's nothing new.
    pub fn take_save_request(&mut self, now: f64, quiet_secs: f64) -> Option<crate::persist::BoardData> {
        if !self.dirty {
            return None;
        }
        let drawing = self.sessions.values().any(|s| matches!(s.role, PointerRole::Drawing { .. } | PointerRole::Erasing));
        if !self.save_now && (drawing || now - self.last_change < quiet_secs) {
            return None;
        }
        self.dirty = false;
        self.save_now = false;
        Some(self.board_data())
    }

    /// Whether something on screen moves by itself right now, so frames must
    /// keep coming without input: a finger down (stroke prediction, eraser
    /// cursor, pinch), a fading laser mark, a toast, a running stopwatch.
    /// Otherwise the app sleeps until the next event — an idle board costs
    /// no CPU/GPU.
    pub fn needs_animation(&self, now: f64) -> bool {
        !self.sessions.is_empty()
            || self.toast.as_ref().is_some_and(|(_, until)| now < *until)
            || self.stopwatch.as_ref().is_some_and(|s| s.is_running())
            || self.loading.is_some()
            || self.ask.as_ref().is_some_and(|c| c.is_waiting())
            || self.pages[self.current_page]
                .strokes
                .iter()
                .any(|s| s.brush_type == BrushType::LaserPointer && !s.points.is_empty())
    }

    /// When the pending autosave becomes due (input clock), if one is.
    pub fn save_due_at(&self, quiet_secs: f64) -> Option<f64> {
        self.dirty.then(|| self.last_change + quiet_secs)
    }

    /// Whether the board has changes not yet handed to `take_save_request`.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn take_replaced_board(&mut self) -> Option<crate::persist::BoardData> {
        self.replaced_board.take()
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
            ToolbarAction::Undo => self.undo(),
            ToolbarAction::Redo => self.redo(),
            ToolbarAction::Clear => self.clear_page(),
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
            ToolbarAction::ToggleGraph => self.toggle_graph(),
            ToolbarAction::ToggleAsk => {
                let now = self.last_input_at.unwrap_or(0.0);
                if self.ask.take().is_none() {
                    self.ask_pick = !self.ask_pick;
                    self.formula_pick = false;
                    self.crop_pick = false;
                    self.toast = self.ask_pick.then(|| ("Sorunun çevresine bir dikdörtgen çizin".to_string(), now + 4.0));
                }
            }
            ToolbarAction::ToggleSolid => self.toggle_solid(),
            ToolbarAction::TogglePeriodic => self.toggle_periodic(),
            ToolbarAction::OpenBrowser => match crate::masaustu::open_browser(crate::masaustu::BROWSER_HOME) {
                // The fullscreen board would cover the browser window: step
                // aside like "return to desktop" (back via the dock).
                Ok(()) => {
                    self.minimize_requested = true;
                    self.save_now = true;
                }
                Err(e) => {
                    log::error!("tarayıcı açılamadı: {e:#}");
                    let now = self.last_input_at.unwrap_or(0.0);
                    self.toast = Some(("Tarayıcı açılamadı".to_string(), now + 2.5));
                }
            },
            ToolbarAction::CropToPage => {
                let now = self.last_input_at.unwrap_or(0.0);
                if self.crop_pick {
                    self.crop_pick = false;
                    self.toast = None;
                } else if self.pages[self.current_page].pdf_image.is_some() {
                    self.crop_pick = true;
                    self.ask_pick = false;
                    self.formula_pick = false;
                    self.toast = Some(("Almak istediğiniz alanın çevresine bir dikdörtgen çizin".to_string(), now + 4.0));
                } else {
                    self.toast = Some(("Bu sayfada PDF yok · önce Aç'tan bir PDF açın".to_string(), now + 3.0));
                }
            }
            ToolbarAction::CaptureScreen => {
                if self.loading.is_none() {
                    self.start_loading(Loading::Screen, crate::masaustu::capture_screen);
                }
            }
            ToolbarAction::CycleBackground => {
                self.checkpoint();
                let page = &mut self.pages[self.current_page];
                let (bg, grid) = board::next_preset((page.background, page.grid));
                page.background = bg;
                page.grid = grid;
            }
            ToolbarAction::PrevPage => {
                self.crop_pick = false;
                self.current_page = self.current_page.saturating_sub(1);
                self.mark_dirty();
            }
            ToolbarAction::NextPage => {
                self.crop_pick = false;
                self.current_page += 1;
                if self.current_page >= self.pages.len() {
                    self.pages.push(Page::new());
                }
                self.mark_dirty();
            }
            ToolbarAction::GoToPage(page) => {
                if page < self.pages.len() && page != self.current_page {
                    self.crop_pick = false;
                    self.release_drawing_pointers();
                    self.current_page = page;
                    self.mark_dirty();
                }
            }
            ToolbarAction::DeletePage => {
                let now = self.last_input_at.unwrap_or(0.0);
                if self.delete_page_armed_until.is_some_and(|t| now <= t) {
                    self.delete_page_armed_until = None;
                    self.toolbar.close_group();
                    self.release_drawing_pointers();
                    // The whole board before the deletion stays reachable
                    // from "Aç" → "Önceki tahta".
                    self.keep_as_backup();
                    if self.pages.len() > 1 {
                        self.pages.remove(self.current_page);
                        self.current_page = self.current_page.min(self.pages.len() - 1);
                    } else {
                        self.pages = vec![Page::new()];
                        self.current_page = 0;
                    }
                    self.selection = None;
                    self.picture_selection = None;
                    self.mark_dirty();
                    self.save_now = true;
                    self.toast = Some(("Sayfa silindi · geri almak için Aç → Önceki tahta".to_string(), now + 3.5));
                } else {
                    self.delete_page_armed_until = Some(now + 4.0);
                    self.new_board_armed_until = None;
                    self.toast = Some(("Bu sayfayı silmek için tekrar dokunun".to_string(), now + 4.0));
                }
            }
            ToolbarAction::NewBoard => {
                // Wipes every page, so it takes a second tap; the old board
                // is kept as a backup file (see `take_replaced_board`).
                let now = self.last_input_at.unwrap_or(0.0);
                if self.new_board_armed_until.is_some_and(|t| now <= t) {
                    self.new_board_armed_until = None;
                    self.toolbar.close_group();
                    self.release_drawing_pointers();
                    self.keep_as_backup();
                    self.pages = vec![Page::new()];
                    self.current_page = 0;
                    self.view_offset = Vec2::ZERO;
                    self.view_scale = 1.0;
                    self.mark_dirty();
                    self.save_now = true;
                    self.toast = Some(("Yeni tahta".to_string(), now + 2.0));
                } else {
                    self.new_board_armed_until = Some(now + 4.0);
                    self.toast = Some(("Yeni tahta için tekrar dokunun".to_string(), now + 4.0));
                }
            }
            ToolbarAction::OpenFile => {
                self.opener = match self.opener {
                    Some(_) => None,
                    None => Some(crate::opener::Opener::new()),
                };
            }
            ToolbarAction::ExportPdf => {
                let now = self.last_input_at.unwrap_or(0.0);
                let message = match crate::pdf_export::export_to_pdf(&self.pages, self.screen_size) {
                    Ok(path) => {
                        log::info!("PDF dışa aktarıldı: {}", path.display());
                        "PDF kaydedildi: Belgeler/tahta-pdf".to_string()
                    }
                    Err(e) => {
                        log::error!("PDF dışa aktarma hatası: {e:#}");
                        "PDF kaydedilemedi".to_string()
                    }
                };
                self.toast = Some((message, now + 2.5));
            }
            ToolbarAction::MinimizeToDesktop => {
                // Only `app.rs` owns the winit `Window`, so this just
                // raises a flag — see `take_minimize_request`.
                self.minimize_requested = true;
                self.save_now = true;
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

        // The "Aç" panel is modal the same way: a touch inside picks a row
        // or a button, anywhere else closes it.
        if let Some(opener) = &mut self.opener {
            let press = if opener.contains(self.screen_size, position) {
                opener.press_at(self.screen_size, position)
            } else {
                crate::opener::Press::Close
            };
            match press {
                crate::opener::Press::None => {}
                crate::opener::Press::Close => self.opener = None,
                crate::opener::Press::Choose(choice) => {
                    self.opener = None;
                    self.open_choice(choice, now);
                }
            }
            self.sessions.insert(id, PointerSession { role: PointerRole::Palm, start_pos: position, start_time: now });
            return;
        }

        if self.toolbar_visible {
            self.toolbar.set_page_count(self.pages.len());
            if self.toolbar.contains(position) {
                if let Some(action) = self.toolbar.hit_test(position) {
                    self.apply_toolbar_action(action, self.zone_of(position));
                }
                self.sessions.insert(id, PointerSession { role: PointerRole::Toolbar, start_pos: position, start_time: now });
                return;
            }
            // A group flyout is modal-ish but not fully: a touch that
            // lands elsewhere (the canvas, another panel) still does
            // whatever it would normally do — it just also dismisses the
            // now-stale flyout first, since it can never be reached from
            // there again this touch.
            if self.toolbar.has_open_group() {
                self.toolbar.close_group();
            }
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

        // The "Sor" card sits on top of the other panels.
        if let Some(card) = &mut self.ask {
            if card.contains(position) {
                let role = match card.press_at(position, now) {
                    crate::ask::AskPress::Move => PointerRole::AskMove { grab_offset: position - card.position },
                    crate::ask::AskPress::Scroll => PointerRole::AskScroll { last_y: position.y },
                    crate::ask::AskPress::Close => {
                        self.ask = None;
                        PointerRole::AskPress
                    }
                    crate::ask::AskPress::Other => PointerRole::AskPress,
                };
                self.sessions.insert(id, PointerSession { role, start_pos: position, start_time: now });
                return;
            }
        }

        // Same UI-panel priority for the graph.
        if let Some(graph) = &mut self.graph {
            if graph.contains(position) {
                let role = match graph.press_at(position) {
                    crate::graph::GraphPress::Move => PointerRole::GraphMove { grab_offset: position - graph.position },
                    crate::graph::GraphPress::Pan => PointerRole::GraphPan { last_pos: position },
                    crate::graph::GraphPress::Slider(param) => PointerRole::GraphSlider { param },
                    crate::graph::GraphPress::Close => {
                        self.toggle_graph();
                        PointerRole::GraphPress
                    }
                    crate::graph::GraphPress::Stamp => {
                        self.stamp_graph();
                        PointerRole::GraphPress
                    }
                    crate::graph::GraphPress::Read => {
                        self.toggle_graph();
                        self.formula_pick = true;
                        self.toast = Some(("Formülün çevresine bir dikdörtgen çizin".to_string(), now + 4.0));
                        PointerRole::GraphPress
                    }
                    crate::graph::GraphPress::Other => PointerRole::GraphPress,
                };
                self.sessions.insert(id, PointerSession { role, start_pos: position, start_time: now });
                return;
            }
        }

        // Same UI-panel priority for the solid.
        if let Some(solid) = &mut self.solid {
            if solid.contains(position) {
                let role = match solid.press_at(position) {
                    crate::solid::SolidPress::Move => PointerRole::SolidMove { grab_offset: position - solid.position },
                    crate::solid::SolidPress::View => PointerRole::SolidView { last_pos: position, moved: 0.0 },
                    crate::solid::SolidPress::Slider => PointerRole::SolidSlider,
                    crate::solid::SolidPress::Close => {
                        self.toggle_solid();
                        PointerRole::SolidPress
                    }
                    crate::solid::SolidPress::Stamp => {
                        self.stamp_solid();
                        PointerRole::SolidPress
                    }
                    crate::solid::SolidPress::Other => PointerRole::SolidPress,
                };
                self.sessions.insert(id, PointerSession { role, start_pos: position, start_time: now });
                return;
            }
        }

        // Same UI-panel priority for the periodic table.
        if let Some(table) = &mut self.periodic {
            if table.contains(position) {
                let role = match table.press_at(position) {
                    crate::periodic::PeriodicPress::Move => PointerRole::PeriodicMove { grab_offset: position - table.position },
                    crate::periodic::PeriodicPress::Close => {
                        self.toggle_periodic();
                        PointerRole::PeriodicPress
                    }
                    crate::periodic::PeriodicPress::Stamp => {
                        self.stamp_periodic();
                        PointerRole::PeriodicPress
                    }
                    crate::periodic::PeriodicPress::Other => PointerRole::PeriodicPress,
                };
                self.sessions.insert(id, PointerSession { role, start_pos: position, start_time: now });
                return;
            }
        }

        // "Tahtadan oku": this drag frames the handwritten formula.
        if self.formula_pick || self.ask_pick || self.crop_pick {
            self.formula_rect = Some((position, position));
            self.sessions.insert(id, PointerSession { role: PointerRole::FormulaPick, start_pos: position, start_time: now });
            return;
        }

        // A selected picture: corners resize, the body moves, the red
        // button deletes; any other touch only lets go of it.
        if let Some((index, rect)) = self.selected_picture_rect() {
            let role = match picture_grab(rect, position, self.screen_size) {
                PictureGrab::Delete => {
                    self.checkpoint();
                    self.pages[self.current_page].pictures.remove(index);
                    self.picture_selection = None;
                    PointerRole::Palm
                }
                PictureGrab::Corner(k) => {
                    let corners = picture_corners(rect);
                    let anchor = corners[(k + 2) % 4];
                    let dir = (corners[k] - anchor).signum();
                    PointerRole::PictureResize { anchor, dir, started: false }
                }
                PictureGrab::Body => PointerRole::PictureMove { last_pos: position, started: false },
                PictureGrab::Outside => {
                    self.picture_selection = None;
                    PointerRole::Palm
                }
            };
            self.sessions.insert(id, PointerSession { role, start_pos: position, start_time: now });
            return;
        }
        self.picture_selection = None;

        // A selected stamped figure: its frame or handle grabs it; any
        // other touch only lets go of it (consumed, so letting go never
        // leaves a stray dot).
        if let Some((_, bounds)) = self.selection_bounds() {
            let inside = position.cmpge(bounds.0).all() && position.cmple(bounds.1).all();
            if inside || position.distance(selection_handle(bounds, self.screen_size)) <= SELECTION_HANDLE_R + 8.0 {
                let role = PointerRole::SelectionMove { last_pos: position, started: false };
                self.sessions.insert(id, PointerSession { role, start_pos: position, start_time: now });
                return;
            }
            self.selection = None;
            self.sessions.insert(id, PointerSession { role: PointerRole::Palm, start_pos: position, start_time: now });
            return;
        }
        self.selection = None;

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

        // Two-finger pinch-to-zoom: only from the Hand tool, only when a
        // second finger touches down while exactly one other is already
        // panning — never interrupts Pen/Eraser drawing, and never
        // triggers from a touch that landed on a panel/toolbar (those all
        // returned above already). See `PinchState` docs.
        if self.active_tool == Tool::Hand {
            let existing_pan = self.sessions.iter().find_map(|(&sid, s)| match s.role {
                PointerRole::Panning { last_pos } => Some((sid, last_pos)),
                _ => None,
            });
            if let Some((other_id, other_pos)) = existing_pan {
                let start_midpoint = (other_pos + position) / 2.0;
                let canvas_anchor = (start_midpoint - self.view_offset) / self.view_scale;
                self.pinch = Some(PinchState {
                    id_a: other_id,
                    id_b: id,
                    pos_a: other_pos,
                    pos_b: position,
                    start_distance: other_pos.distance(position).max(1.0),
                    start_scale: self.view_scale,
                    canvas_anchor,
                });
                if let Some(session) = self.sessions.get_mut(&other_id) {
                    session.role = PointerRole::Palm;
                }
                self.sessions.insert(id, PointerSession { role: PointerRole::Palm, start_pos: position, start_time: now });
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
                        let page = &mut self.pages[self.current_page];
                        if page.strokes.len() == idx + 1 {
                            page.strokes.pop();
                            page.discard_checkpoint(idx);
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
                    // Undoable: a misread palm can rub out real ink.
                    self.checkpoint();
                    self.demote_cluster_to_palm(position);
                    self.erase_area(position, PALM_ERASE_RADIUS);
                    PointerRole::Palm
                } else {
                    let edge_snap = self.find_edge_snap(position);
                    let start_screen = match edge_snap {
                        Some((a, b)) => crate::geom::project_onto_segment(position, a, b),
                        None => position,
                    };
                    // `Stroke.points` are canvas-space (see `to_canvas`) so
                    // old ink stays correctly placed under pan/zoom no
                    // matter when it was drawn; `edge_snap` itself stays
                    // screen-space (the ruler/setsquare are screen-fixed).
                    let start = self.to_canvas(start_screen);

                    let pen = self.pen_for(zone).clone();
                    // Laser marks fade on their own — nothing to undo.
                    if pen.brush_type != BrushType::LaserPointer {
                        self.checkpoint();
                    }
                    let page = &mut self.pages[self.current_page];
                    let stroke_index = page.strokes.len();
                    let mut stroke = Stroke::new(pen.color, pen.base_width, pen.brush_type);
                    stroke.push_point(start, now);
                    page.strokes.push(Arc::new(stroke));

                    let mut predictor = TouchPredictor::new(LOOKAHEAD_MS);
                    predictor.push_sample(TouchSample { position: start, timestamp: now });

                    PointerRole::Drawing { stroke_index, predictor, edge_snap }
                }
            }
            Tool::Hand => PointerRole::Panning { last_pos: position },
            Tool::Eraser => {
                self.checkpoint();
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
                    Arc::make_mut(&mut strokes[stroke_index]).clear();
                    session.role = PointerRole::Palm;
                }
            }
        }
    }

    fn move_pointer(&mut self, id: u64, position: Vec2, now: f64) {
        self.last_input_at = Some(now);

        if let Some(pinch) = &mut self.pinch {
            if id == pinch.id_a || id == pinch.id_b {
                if id == pinch.id_a {
                    pinch.pos_a = position;
                } else {
                    pinch.pos_b = position;
                }
                let current_distance = pinch.pos_a.distance(pinch.pos_b).max(1.0);
                let new_scale = (pinch.start_scale * (current_distance / pinch.start_distance)).clamp(MIN_VIEW_SCALE, MAX_VIEW_SCALE);
                let midpoint = (pinch.pos_a + pinch.pos_b) / 2.0;
                self.view_scale = new_scale;
                self.view_offset = midpoint - pinch.canvas_anchor * new_scale;
                return;
            }
        }

        // Local copies — `to_canvas` needs `&self`, which would conflict
        // with the `&mut session.role` borrow held through this match.
        let view_offset = self.view_offset;
        let inv_scale = 1.0 / self.view_scale;

        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };

        let mut do_erase = false;
        let mut move_selection: Option<(Vec2, bool)> = None;
        // (new screen rect or move delta, first real movement)
        let mut picture_change: Option<(PictureChange, bool)> = None;
        match &mut session.role {
            PointerRole::Drawing { stroke_index, predictor, edge_snap } => {
                let point_screen = match edge_snap {
                    Some((a, b)) => crate::geom::project_onto_segment(position, *a, *b),
                    None => position,
                };
                let point = (point_screen - view_offset) * inv_scale;
                let Some(stroke) = self.pages[self.current_page].strokes.get_mut(*stroke_index).map(Arc::make_mut) else {
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
            PointerRole::GraphMove { grab_offset } => {
                let grab_offset = *grab_offset;
                if let Some(graph) = &mut self.graph {
                    graph.drag_to(position, grab_offset);
                }
            }
            PointerRole::GraphPan { last_pos } => {
                let delta = position - *last_pos;
                *last_pos = position;
                if let Some(graph) = &mut self.graph {
                    graph.pan_by(delta);
                }
            }
            PointerRole::GraphSlider { param } => {
                let param = *param;
                if let Some(graph) = &mut self.graph {
                    graph.set_slider(param, position.x);
                }
            }
            PointerRole::SolidMove { grab_offset } => {
                let grab_offset = *grab_offset;
                if let Some(solid) = &mut self.solid {
                    solid.drag_to(position, grab_offset);
                }
            }
            PointerRole::SolidView { last_pos, moved } => {
                let delta = position - *last_pos;
                *last_pos = position;
                *moved += delta.length();
                if *moved > crate::solid::TAP_SLOP {
                    if let Some(solid) = &mut self.solid {
                        solid.rotate_by(delta);
                    }
                }
            }
            PointerRole::SolidSlider => {
                if let Some(solid) = &mut self.solid {
                    solid.set_slider(position.x);
                }
            }
            PointerRole::PeriodicMove { grab_offset } => {
                let grab_offset = *grab_offset;
                if let Some(table) = &mut self.periodic {
                    table.drag_to(position, grab_offset);
                }
            }
            PointerRole::FormulaPick => {
                if let Some((_, end)) = &mut self.formula_rect {
                    *end = position;
                }
            }
            PointerRole::AskMove { grab_offset } => {
                let grab_offset = *grab_offset;
                if let Some(card) = &mut self.ask {
                    card.drag_to(position, grab_offset);
                }
            }
            PointerRole::AskScroll { last_y } => {
                let dy = position.y - *last_y;
                *last_y = position.y;
                if let Some(card) = &mut self.ask {
                    card.scroll_by(dy, &self.font_atlas);
                }
            }
            PointerRole::SelectionMove { last_pos, started } => {
                let first = !*started && position.distance(session.start_pos) > 3.0;
                if first {
                    *started = true;
                }
                if *started {
                    move_selection = Some((position - *last_pos, first));
                    *last_pos = position;
                }
            }
            PointerRole::PictureMove { last_pos, started } => {
                let first = !*started && position.distance(session.start_pos) > 3.0;
                if first {
                    *started = true;
                }
                if *started {
                    picture_change = Some((PictureChange::Move(position - *last_pos), first));
                    *last_pos = position;
                }
            }
            PointerRole::PictureResize { anchor, dir, started } => {
                let first = !*started && position.distance(session.start_pos) > 3.0;
                if first {
                    *started = true;
                }
                if *started {
                    picture_change = Some((PictureChange::Resize { anchor: *anchor, dir: *dir, finger: position }, first));
                }
            }
            PointerRole::Toolbar
            | PointerRole::Palm
            | PointerRole::CalculatorPress
            | PointerRole::StopwatchPress
            | PointerRole::DicePress
            | PointerRole::TextBoxPress
            | PointerRole::GraphPress
            | PointerRole::AskPress
            | PointerRole::PeriodicPress
            | PointerRole::SolidPress => {}
        }
        if do_erase {
            self.erase_at(position, self.eraser_radius);
        }
        if let Some((change, first)) = picture_change {
            self.change_picture(change, first);
        }
        if let (Some((delta, first)), Some(group)) = (move_selection, self.selection) {
            if first {
                self.checkpoint();
            }
            self.move_group(group, delta);
        }
    }

    fn end_pointer(&mut self, id: u64, now: f64) {
        if let Some(pinch) = &self.pinch {
            if id == pinch.id_a || id == pinch.id_b {
                // One finger lifted — hand the transform back to ordinary
                // single-finger panning for whichever finger is still down.
                let (remaining_id, remaining_pos) =
                    if id == pinch.id_a { (pinch.id_b, pinch.pos_b) } else { (pinch.id_a, pinch.pos_a) };
                self.pinch = None;
                self.sessions.remove(&id);
                if let Some(session) = self.sessions.get_mut(&remaining_id) {
                    session.role = PointerRole::Panning { last_pos: remaining_pos };
                }
                return;
            }
        }
        if let Some(session) = self.sessions.remove(&id) {
            match session.role {
                PointerRole::Erasing => {
                    self.eraser_cursor = None;
                    self.mark_dirty();
                }
                PointerRole::CompassDrag { .. } => self.commit_compass_circle(),
                PointerRole::SelectionMove { started: true, .. }
                | PointerRole::PictureMove { started: true, .. }
                | PointerRole::PictureResize { started: true, .. } => self.mark_dirty(),
                PointerRole::FormulaPick if self.crop_pick => self.finish_crop_pick(now),
                PointerRole::FormulaPick if self.ask_pick => self.finish_ask_pick(now),
                PointerRole::FormulaPick => self.finish_formula_pick(now),
                // A Hand-tool tap (no real pan) on a stamped figure selects it.
                PointerRole::Panning { last_pos } if last_pos.distance(session.start_pos) < 10.0 => {
                    let (offset, scale) = (self.view_offset, self.view_scale);
                    let page = &self.pages[self.current_page];
                    self.selection = page
                        .strokes
                        .iter()
                        .rev()
                        .find(|s| s.group != 0 && s.hit_test(session.start_pos, 14.0, offset, scale))
                        .map(|s| s.group);
                    // Otherwise a placed picture under the finger (topmost).
                    if self.selection.is_none() {
                        self.picture_selection = page
                            .pictures
                            .iter()
                            .rposition(|p| {
                                let (tl, br) = picture_frame(p.screen_rect(offset, scale));
                                session.start_pos.cmpge(tl).all() && session.start_pos.cmple(br).all()
                            })
                            .map(|i| (self.current_page, i));
                    }
                }
                PointerRole::SolidView { moved, .. } => {
                    if moved <= crate::solid::TAP_SLOP {
                        if let Some(solid) = &mut self.solid {
                            solid.tap(session.start_pos);
                        }
                    }
                }
                PointerRole::Drawing { stroke_index, .. } => {
                    self.mark_dirty();
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

        // Live preview (`center`/`radius`) is screen-space, matching the
        // raw drag; converted to canvas-space here, same as ordinary ink,
        // so the committed circle stays correctly placed under pan/zoom.
        let center = self.to_canvas(center);
        let radius = radius / self.view_scale;

        let pen = self.active_pen.clone();
        let mut stroke = Stroke::new(pen.color, pen.base_width, pen.brush_type);
        let now = self.last_input_at.unwrap_or(0.0);
        for i in 0..=SAMPLES {
            let theta = i as f32 / SAMPLES as f32 * std::f32::consts::TAU;
            stroke.push_point(center + Vec2::new(theta.cos(), theta.sin()) * radius, now);
        }
        self.checkpoint();
        self.pages[self.current_page].strokes.push(Arc::new(stroke));
    }

    fn toggle_graph(&mut self) {
        self.graph = match self.graph.take() {
            Some(graph) => {
                self.graph_stash = Some(graph);
                None
            }
            None => Some(self.graph_stash.take().unwrap_or_else(|| crate::graph::Graph::new(self.screen_size))),
        };
    }

    /// "Tahtaya bas": the graph panel's plot becomes ordinary ink on the
    /// current page (one undo step), and the panel steps aside so the
    /// result is visible.
    fn stamp_graph(&mut self) {
        let Some(graph) = &self.graph else { return };
        let now = self.last_input_at.unwrap_or(0.0);
        if !graph.has_curves() {
            self.toast = Some(("Önce bir fonksiyon yazın".to_string(), now + 2.5));
            return;
        }
        let lines = graph.stamp_lines(self.screen_size, self.stamp_ink());
        self.commit_ink(lines);
        self.toggle_graph();
        self.toast = Some(("Grafik tahtaya eklendi · sürükleyerek taşıyın".to_string(), now + 3.5));
    }

    fn toggle_solid(&mut self) {
        self.solid = match self.solid.take() {
            Some(solid) => {
                self.solid_stash = Some(solid);
                None
            }
            None => Some(self.solid_stash.take().unwrap_or_else(|| crate::solid::Solid::new(self.screen_size))),
        };
    }

    /// "Tahtaya bas" for the solid panel — same as `stamp_graph`.
    fn stamp_solid(&mut self) {
        let Some(solid) = &self.solid else { return };
        let lines = solid.stamp_lines(self.screen_size, self.stamp_ink());
        self.commit_ink(lines);
        self.toggle_solid();
        let now = self.last_input_at.unwrap_or(0.0);
        self.toast = Some(("Cisim tahtaya eklendi · sürükleyerek taşıyın".to_string(), now + 3.5));
    }

    /// Axis/edge/number color for stamped figures: dark on the white
    /// board, chalk on the dark ones.
    fn toggle_periodic(&mut self) {
        self.periodic = match self.periodic.take() {
            Some(table) => {
                self.periodic_stash = Some(table);
                None
            }
            None => Some(self.periodic_stash.take().unwrap_or_else(|| crate::periodic::Periodic::new(self.screen_size))),
        };
    }

    fn stamp_periodic(&mut self) {
        let Some(table) = &self.periodic else { return };
        let z = table.selected();
        let lines = table.stamp_lines(self.screen_size, self.stamp_ink());
        self.commit_ink(lines);
        self.toggle_periodic();
        let now = self.last_input_at.unwrap_or(0.0);
        self.toast = Some((format!("{} tahtaya eklendi · sürükleyerek taşıyın", crate::elements::name(z)), now + 3.5));
    }

    fn stamp_ink(&self) -> [f32; 4] {
        match self.pages[self.current_page].background {
            board::BoardBackground::White => [0.12, 0.12, 0.15, 1.0],
            _ => crate::brush::PALETTE[0],
        }
    }

    /// Screen-space ink lines → strokes on the current page, as one undo
    /// step and one movable figure (a fresh `Stroke::group`), selected so
    /// it can be dragged into place right away.
    fn commit_ink(&mut self, lines: Vec<crate::ink::InkLine>) {
        let now = self.last_input_at.unwrap_or(0.0);
        let group = self.pages.iter().flat_map(|p| p.strokes.iter().map(|s| s.group)).max().unwrap_or(0) + 1;
        self.checkpoint();
        for line in lines {
            let mut stroke = Stroke::new(line.color, line.width / self.view_scale, BrushType::Ballpoint);
            stroke.group = group;
            for p in line.points {
                stroke.push_point(self.to_canvas(p), now);
            }
            self.pages[self.current_page].strokes.push(Arc::new(stroke));
        }
        self.selection = Some(group);
    }

    /// The teacher lifted the finger after framing a formula: read the ink
    /// inside and hand the result to the graph panel (reopened either way;
    /// a mere tap just cancels).
    fn finish_formula_pick(&mut self, now: f64) {
        self.formula_pick = false;
        let Some((a, b)) = self.formula_rect.take() else { return };
        let (tl, br) = (a.min(b), a.max(b));
        if self.graph.is_none() {
            self.toggle_graph();
        }
        let message = if (br - tl).min_element() < 20.0 {
            None
        } else {
            let (offset, scale) = (self.view_offset, self.view_scale);
            let strokes: Vec<Vec<Vec2>> = self.pages[self.current_page]
                .strokes
                .iter()
                .filter(|s| s.brush_type != BrushType::LaserPointer && s.group == 0 && !s.points.is_empty())
                .filter(|s| {
                    let n = s.points.len() as f32;
                    let c = s.points.iter().copied().sum::<Vec2>() / n * scale + offset;
                    c.cmpge(tl).all() && c.cmple(br).all()
                })
                .map(|s| s.points.clone())
                .collect();
            Some(match crate::handwriting::recognize(&strokes) {
                Some(text) => {
                    let ok = self.graph.as_mut().is_some_and(|g| g.set_formula(&text));
                    if ok { format!("Okundu: y = {text}") } else { format!("Okundu: {text} · tuşlarla düzeltin") }
                }
                None => "Çerçevede yazı bulunamadı".to_string(),
            })
        };
        self.toast = message.map(|m| (m, now + 4.0));
    }

    /// The teacher framed a question for "Sor": draw the ink inside as an
    /// image and open the card, which sends it to YZsunucu.
    fn finish_ask_pick(&mut self, now: f64) {
        self.ask_pick = false;
        let Some((a, b)) = self.formula_rect.take() else { return };
        let (tl, br) = (a.min(b), a.max(b));
        if (br - tl).min_element() < 20.0 {
            self.toast = None;
            return;
        }
        let (offset, scale) = (self.view_offset, self.view_scale);
        let strokes: Vec<crate::ask::InkStroke> = self.pages[self.current_page]
            .strokes
            .iter()
            .filter(|s| s.brush_type != BrushType::LaserPointer && !s.points.is_empty())
            .map(|s| crate::ask::InkStroke {
                points: s.points.iter().map(|&p| p * scale + offset).collect(),
                width: s.width * scale,
                color: s.color,
            })
            .filter(|s| s.points.iter().any(|p| p.cmpge(tl).all() && p.cmple(br).all()))
            .collect();
        match crate::ask::render_png(&strokes, (tl, br)) {
            Some(png) => {
                self.ask = Some(crate::ask::AskCard::open(png, (tl, br), self.screen_size, now));
                self.toast = None;
            }
            None => self.toast = Some(("Çerçevede yazı bulunamadı".to_string(), now + 4.0)),
        }
    }

    /// The teacher framed part of the PDF page: cut it out and put it on a
    /// new page right after this one, large and selected so it can be
    /// moved and resized at once (a mere tap just cancels).
    fn finish_crop_pick(&mut self, now: f64) {
        self.crop_pick = false;
        let Some(frame) = self.formula_rect.take() else { return };
        if (frame.1 - frame.0).abs().min_element() < 20.0 {
            self.toast = None;
            return;
        }
        let Some(image) = self.pages[self.current_page].pdf_image.clone() else { return };
        let Some(bounds) = board::crop_bounds(&image, frame, self.screen_size) else {
            self.toast = Some(("Çerçeve PDF sayfasının dışında".to_string(), now + 3.0));
            return;
        };
        let [x0, y0, x1, y1] = bounds;
        let (pos, size) = fit_picture(x1 - x0, y1 - y0, self.screen_size);
        // Drawn again from the PDF at (a bit above) the size it's shown
        // at, so it stays sharp; the page image's own pixels otherwise
        // (screen grabs, a PDF that has since moved).
        let sharp = image.source.as_ref().and_then(|source| {
            let (w, h) = (image.width as f32, image.height as f32);
            let fractions = [x0 as f32 / w, y0 as f32 / h, x1 as f32 / w, y1 as f32 / h];
            let long_side = (size.max_element() * 1.5).clamp(64.0, 3200.0) as u32;
            crate::pdf::render_region(source, fractions, long_side)
                .map_err(|e| log::warn!("alan PDF'den yeniden çizilemedi: {e:#}"))
                .ok()
        });
        let Some(cut) = sharp.or_else(|| board::crop_image(&image, frame, self.screen_size)) else { return };
        self.release_drawing_pointers();
        let mut page = Page::new();
        page.pictures.push(board::Picture { image: Arc::new(cut), pos, size });
        self.current_page += 1;
        self.pages.insert(self.current_page, page);
        // `pos`/`size` are screen space for an unpanned, unzoomed view.
        self.view_offset = Vec2::ZERO;
        self.view_scale = 1.0;
        self.selection = None;
        self.picture_selection = Some((self.current_page, 0));
        self.mark_dirty();
        self.toast = Some(("Alan yeni sayfaya alındı · köşelerden boyutlandırın".to_string(), now + 4.0));
    }

    /// Index and screen rect (top-left, size) of the selected picture —
    /// `None` when none is selected or it's gone (undone, another page).
    fn selected_picture_rect(&self) -> Option<(usize, (Vec2, Vec2))> {
        let (page, index) = self.picture_selection?;
        if page != self.current_page {
            return None;
        }
        let picture = self.pages[page].pictures.get(index)?;
        Some((index, picture.screen_rect(self.view_offset, self.view_scale)))
    }

    fn change_picture(&mut self, change: PictureChange, first: bool) {
        let Some((index, _)) = self.selected_picture_rect() else { return };
        if first {
            self.checkpoint();
        }
        let (offset, scale) = (self.view_offset, self.view_scale);
        let picture = &mut self.pages[self.current_page].pictures[index];
        match change {
            PictureChange::Move(delta) => picture.pos += delta / scale,
            PictureChange::Resize { anchor, dir, finger } => {
                let aspect = picture.size.x / picture.size.y.max(1e-3);
                let (tl, size) = resize_from_anchor(anchor, dir, finger, aspect);
                picture.pos = (tl - offset) / scale;
                picture.size = size / scale;
            }
        }
    }

    /// Screen-space frame (top-left, bottom-right) around the selected
    /// figure on the current page — `None` when nothing is selected or the
    /// figure is gone (undone, cleared, erased, another page).
    fn selection_bounds(&self) -> Option<(u32, (Vec2, Vec2))> {
        let group = self.selection?;
        let mut min = Vec2::splat(f32::INFINITY);
        let mut max = Vec2::splat(f32::NEG_INFINITY);
        for s in self.pages[self.current_page].strokes.iter().filter(|s| s.group == group) {
            for &p in &s.points {
                min = min.min(p);
                max = max.max(p);
            }
        }
        if min.x > max.x {
            return None;
        }
        let pad = Vec2::splat(14.0);
        Some((group, (min * self.view_scale + self.view_offset - pad, max * self.view_scale + self.view_offset + pad)))
    }

    /// Moves every stroke of `group` by a screen-space delta.
    fn move_group(&mut self, group: u32, delta: Vec2) {
        let d = delta / self.view_scale;
        for s in self.pages[self.current_page].strokes.iter_mut().filter(|s| s.group == group) {
            for p in &mut Arc::make_mut(s).points {
                *p += d;
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
        let view_scale = self.view_scale;
        let strokes = &mut self.pages[self.current_page].strokes;
        let mut new_strokes = Vec::new();
        for stroke in strokes.iter_mut() {
            // Only touch (and so un-share from undo snapshots) strokes the
            // eraser actually reaches.
            if stroke.points.is_empty() || !stroke.hit_test(position, radius, view_offset, view_scale) {
                continue;
            }
            new_strokes.extend(Arc::make_mut(stroke).erase_near(position, radius, view_offset, view_scale));
        }
        strokes.extend(new_strokes.into_iter().map(Arc::new));
    }

    /// Deletes an entire stroke if any part of it falls within `radius`.
    fn erase_object(&mut self, position: Vec2, radius: f32) {
        let view_offset = self.view_offset;
        let view_scale = self.view_scale;
        for stroke in &mut self.pages[self.current_page].strokes {
            if !stroke.points.is_empty() && stroke.hit_test(position, radius, view_offset, view_scale) {
                Arc::make_mut(stroke).clear();
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
                "c" | "C" => self.clear_page(),
                "y" | "Y" => self.redo(),
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
            Key::Named(NamedKey::Backspace) => self.undo(),
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
    /// Returns `(normal_vertices, normal_indices, highlighter_vertices,
    /// highlighter_indices, page_image, (background_index_count,
    /// content_index_count), glyph_vertices, glyph_indices)` — the first
    /// `background_index_count` of `normal_indices` are the page
    /// background (drawn under the highlighter batch) and the first
    /// `content_index_count` are "page content" rather than floating UI
    /// chrome; see the comment at its call site below and
    /// `renderer.rs::render`'s `content_ranges` param, which uses it
    /// to render the magnifier lens's texture sample. `glyph_vertices`/
    /// `glyph_indices` are the text box's real-font text (see
    /// `font_atlas.rs`), drawn through a separate pipeline from the rest
    /// of this vector geometry.
    pub fn collect_geometry(
        &self,
        now: f64,
    ) -> (
        Vec<Vertex>,
        Vec<u32>,
        Vec<Vertex>,
        Vec<u32>,
        Option<Arc<board::PdfImage>>,
        Vec<crate::renderer::ScreenPicture>,
        (usize, usize),
        Vec<crate::font_atlas::GlyphVertex>,
        Vec<u32>,
    ) {
        let mut normal_v = Vec::new();
        let mut normal_i = Vec::new();
        let mut highlight_v = Vec::new();
        let mut highlight_i = Vec::new();
        let mut glyph_v = Vec::new();
        let mut glyph_i = Vec::new();

        let page = &self.pages[self.current_page];
        board::render_background(page, self.screen_size, self.view_offset, self.view_scale, &mut normal_v, &mut normal_i);
        // The (opaque) background is drawn before the highlighter batch,
        // everything else in the content range after it — see
        // `renderer.rs::render`'s `content_ranges` param.
        let background_index_count = normal_i.len();

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
            stroke.tessellate(&predicted, self.view_offset, self.view_scale, now, v, i);
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

        // Everything above this line is "page content" (background, page
        // image, ink, live tool previews) — the split point the magnifier
        // lens samples from (see `renderer.rs::render`'s `content_ranges`
        // param). Everything below is floating UI chrome (the magnifier's
        // own rim/handle included) that should never itself be magnified.
        let content_index_count = normal_i.len();

        if let Some(magnifier) = &self.magnifier {
            magnifier.render(&mut normal_v, &mut normal_i);
        }
        if let Some(spotlight) = &self.spotlight {
            spotlight.render(self.screen_size, &mut normal_v, &mut normal_i);
        }

        if let Some((a, b)) = self.formula_rect {
            let (tl, br) = (a.min(b), a.max(b));
            let frame = [tl, Vec2::new(br.x, tl.y), br, Vec2::new(tl.x, br.y), tl];
            for dash in crate::ink::dash_polyline(&frame, 10.0, 6.0) {
                push_line(dash[0], dash[dash.len() - 1], 2.5, [0.98, 0.55, 0.15, 1.0], &mut normal_v, &mut normal_i);
            }
        }
        if let Some((_, rect)) = self.selected_picture_rect() {
            let accent = [0.23, 0.51, 0.96, 1.0];
            let white = [0.97, 0.97, 0.99, 1.0];
            let (tl, br) = picture_frame(rect);
            let frame = [tl, Vec2::new(br.x, tl.y), br, Vec2::new(tl.x, br.y), tl];
            for dash in crate::ink::dash_polyline(&frame, 10.0, 6.0) {
                push_line(dash[0], dash[dash.len() - 1], 2.0, accent, &mut normal_v, &mut normal_i);
            }
            for c in picture_corners(rect) {
                push_circle(c, PICTURE_CORNER_R + 2.0, white, 20, &mut normal_v, &mut normal_i);
                push_circle(c, PICTURE_CORNER_R, accent, 20, &mut normal_v, &mut normal_i);
            }
            // Delete button: a red circle with a white ×.
            let c = selection_handle((tl, br), self.screen_size);
            push_circle(c, SELECTION_HANDLE_R, [0.86, 0.24, 0.22, 1.0], 24, &mut normal_v, &mut normal_i);
            let a = SELECTION_HANDLE_R * 0.42;
            push_line(c - Vec2::splat(a), c + Vec2::splat(a), 2.5, white, &mut normal_v, &mut normal_i);
            push_line(c + Vec2::new(a, -a), c + Vec2::new(-a, a), 2.5, white, &mut normal_v, &mut normal_i);
        }
        if let Some((_, (tl, br))) = self.selection_bounds() {
            let accent = [0.23, 0.51, 0.96, 1.0];
            let frame = [tl, Vec2::new(br.x, tl.y), br, Vec2::new(tl.x, br.y), tl];
            for dash in crate::ink::dash_polyline(&frame, 10.0, 6.0) {
                push_line(dash[0], dash[dash.len() - 1], 2.0, accent, &mut normal_v, &mut normal_i);
            }
            // Move handle: a filled circle with four arrows.
            let c = selection_handle((tl, br), self.screen_size);
            push_circle(c, SELECTION_HANDLE_R, accent, 24, &mut normal_v, &mut normal_i);
            let white = [0.97, 0.97, 0.99, 1.0];
            let (a, h) = (SELECTION_HANDLE_R * 0.62, SELECTION_HANDLE_R * 0.24);
            for dir in [Vec2::X, -Vec2::X, Vec2::Y, -Vec2::Y] {
                let tip = c + dir * a;
                let side = dir.perp();
                push_line(c, tip, 2.0, white, &mut normal_v, &mut normal_i);
                push_line(tip, tip - dir * h + side * h, 2.0, white, &mut normal_v, &mut normal_i);
                push_line(tip, tip - dir * h - side * h, 2.0, white, &mut normal_v, &mut normal_i);
            }
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
            graph_visible: self.graph.is_some(),
            solid_visible: self.solid.is_some(),
            periodic_visible: self.periodic.is_some(),
            ask_visible: self.ask.is_some() || self.ask_pick,
            crop_armed: self.crop_pick,
            crop_available: page.pdf_image.is_some(),
            can_undo: page.can_undo(),
            can_redo: page.can_redo(),
            new_board_armed: self.new_board_armed_until.is_some(),
            delete_page_armed: self.delete_page_armed_until.is_some(),
            opener_visible: self.opener.is_some(),
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
            textbox.render(&self.font_atlas, &mut normal_v, &mut normal_i, &mut glyph_v, &mut glyph_i);
        }
        if let Some(graph) = &self.graph {
            graph.render(&self.font_atlas, &mut normal_v, &mut normal_i, &mut glyph_v, &mut glyph_i);
        }
        if let Some(solid) = &self.solid {
            solid.render(&self.font_atlas, &mut normal_v, &mut normal_i, &mut glyph_v, &mut glyph_i);
        }
        if let Some(table) = &self.periodic {
            table.render(&self.font_atlas, &mut normal_v, &mut normal_i, &mut glyph_v, &mut glyph_i);
        }
        if let Some(card) = &self.ask {
            card.render(now, &self.font_atlas, &mut normal_v, &mut normal_i, &mut glyph_v, &mut glyph_i);
        }

        if let Some(opener) = &self.opener {
            opener.render(self.screen_size, &self.font_atlas, &mut normal_v, &mut normal_i, &mut glyph_v, &mut glyph_i);
        }

        if let Some(menu) = &self.radial_menu {
            // No live drag-hover any more (tap-to-select, see module docs).
            menu.render(None, &mut normal_v, &mut normal_i);
        }

        if let Some((message, expires_at)) = &self.toast {
            if now < *expires_at {
                let text_h = 24.0;
                let width = self.font_atlas.measure(message, text_h);
                let pos = Vec2::new((self.screen_size.x - width) / 2.0, 36.0);
                push_rect(pos - Vec2::new(20.0, 14.0), Vec2::new(width + 40.0, text_h * 1.3 + 28.0), [0.08, 0.09, 0.11, 0.88], &mut normal_v, &mut normal_i);
                self.font_atlas.push_text(message, pos, text_h, [0.95, 0.95, 0.97, 1.0], &mut glyph_v, &mut glyph_i);
            }
        }

        let pictures = page
            .pictures
            .iter()
            .map(|p| {
                let (tl, size) = p.screen_rect(self.view_offset, self.view_scale);
                (p.image.clone(), tl, size)
            })
            .collect();

        (normal_v, normal_i, highlight_v, highlight_i, page.pdf_image.clone(), pictures, (background_index_count, content_index_count), glyph_v, glyph_i)
    }

    /// `(center, radius, zoom)` for the renderer to draw the magnifier's
    /// texture-sampled lens, or `None` when the widget isn't open.
    pub fn magnifier_lens(&self) -> Option<(Vec2, f32, f32)> {
        self.magnifier.as_ref().map(|m| m.lens())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Vec2 = Vec2::new(1600.0, 1000.0);

    fn handler_with_pdf_page() -> InputHandler {
        let mut h = InputHandler::new(SCREEN, Arc::new(crate::font_atlas::FontAtlas::new()));
        // 160×100 page image: drawn letterboxed it fills the screen, 10
        // screen px per image pixel; red = column, green = row.
        let rgba: Vec<u8> = (0..160 * 100u32).flat_map(|i| [(i % 160) as u8, (i / 160) as u8, 0, 255]).collect();
        h.set_pdf_pages(vec![Arc::new(board::PdfImage::new(160, 100, rgba))]);
        h
    }

    fn drag(h: &mut InputHandler, from: Vec2, to: Vec2, t: &mut f64) {
        h.touch_started(1, from, *t);
        for k in 1..=4 {
            *t += 0.02;
            h.touch_moved(1, from.lerp(to, k as f32 / 4.0), *t);
        }
        *t += 0.02;
        h.touch_ended(1, *t);
    }

    #[test]
    fn resize_keeps_the_anchor_and_aspect() {
        let anchor = Vec2::new(100.0, 100.0);
        // Bottom-right corner pulled to (500, 300) on a 2:1 picture.
        let (tl, size) = resize_from_anchor(anchor, Vec2::ONE, Vec2::new(500.0, 300.0), 2.0);
        assert_eq!((tl, size), (anchor, Vec2::new(400.0, 200.0)));
        // Off the diagonal: projected onto it (reach 400×100 → 360×180).
        let (_, size) = resize_from_anchor(anchor, Vec2::ONE, Vec2::new(500.0, 200.0), 2.0);
        assert_eq!(size, Vec2::new(360.0, 180.0));
        // Top-left corner (dir -1,-1): grows up/left, anchor is bottom-right.
        let (tl, size) = resize_from_anchor(anchor, -Vec2::ONE, Vec2::new(-100.0, -100.0), 1.0);
        assert_eq!((tl + size, size), (anchor, Vec2::splat(200.0)));
        // Pushed past the anchor: never smaller than the minimum.
        let (_, size) = resize_from_anchor(anchor, Vec2::ONE, Vec2::ZERO, 1.0);
        assert_eq!(size, Vec2::splat(PICTURE_MIN_SIZE));
    }

    #[test]
    fn framed_pdf_area_goes_to_a_new_page_as_a_resizable_picture() {
        let mut h = handler_with_pdf_page();
        let mut t = 1.0;
        h.apply_toolbar_action(ToolbarAction::CropToPage, Zone::Left);
        assert!(h.crop_pick);
        // Image pixels x 20..60, y 10..30.
        drag(&mut h, Vec2::new(200.0, 100.0), Vec2::new(600.0, 300.0), &mut t);
        assert!(!h.crop_pick && h.formula_rect.is_none());
        assert_eq!((h.pages.len(), h.current_page), (2, 1), "a new page after the PDF page");
        assert!(h.pages[0].strokes.is_empty(), "the frame drew no ink");
        let pic = h.pages[1].pictures[0].clone();
        assert_eq!((pic.image.width, pic.image.height), (40, 20));
        assert_eq!(&pic.image.rgba[0..2], &[20, 10]);
        assert!((pic.size.x / pic.size.y - 2.0).abs() < 1e-3);
        assert!(pic.size.x > 1000.0, "shown large: {:?}", pic.size);
        assert_eq!(h.picture_selection, Some((1, 0)));

        // Bottom-right corner dragged up-left along the diagonal:
        // smaller, top-left stays.
        let br = pic.pos + pic.size;
        drag(&mut h, br, br - Vec2::new(400.0, 200.0), &mut t);
        let p = &h.pages[1].pictures[0];
        assert!(p.pos.distance(pic.pos) < 1e-2);
        assert!((p.size.x - (pic.size.x - 400.0)).abs() < 1e-2 && (p.size.x / p.size.y - 2.0).abs() < 1e-3);

        // The body moves it.
        let (pos, size) = (p.pos, p.size);
        drag(&mut h, pos + size / 2.0, pos + size / 2.0 + Vec2::new(50.0, 30.0), &mut t);
        assert_eq!(h.pages[1].pictures[0].pos, pos + Vec2::new(50.0, 30.0));

        // Undo walks back the move and the resize; a touch outside lets go.
        h.undo();
        assert_eq!(h.pages[1].pictures[0].pos, pos);
        h.undo();
        assert_eq!(h.pages[1].pictures[0].size, pic.size);
        drag(&mut h, Vec2::new(5.0, 5.0), Vec2::new(5.0, 5.0), &mut t);
        assert_eq!(h.picture_selection, None);
        assert!(h.pages[1].strokes.is_empty(), "letting go drew nothing");

        // Hand-tool tap selects it again; the red button deletes it.
        h.active_tool = Tool::Hand;
        let p = h.pages[1].pictures[0].clone();
        drag(&mut h, p.pos + p.size / 2.0, p.pos + p.size / 2.0, &mut t);
        assert_eq!(h.picture_selection, Some((1, 0)));
        let del = selection_handle((p.pos, p.pos + p.size), SCREEN);
        drag(&mut h, del, del, &mut t);
        assert!(h.pages[1].pictures.is_empty());
        h.undo();
        assert_eq!(h.pages[1].pictures.len(), 1);
    }

    #[test]
    fn crop_needs_a_pdf_page_and_a_real_frame() {
        let mut h = InputHandler::new(SCREEN, Arc::new(crate::font_atlas::FontAtlas::new()));
        h.apply_toolbar_action(ToolbarAction::CropToPage, Zone::Left);
        assert!(!h.crop_pick, "no PDF on a plain page");

        let mut h = handler_with_pdf_page();
        let mut t = 1.0;
        h.apply_toolbar_action(ToolbarAction::CropToPage, Zone::Left);
        drag(&mut h, Vec2::new(300.0, 300.0), Vec2::new(305.0, 302.0), &mut t);
        assert_eq!(h.pages.len(), 1, "a tap cancels");
        assert!(!h.crop_pick);
    }
}
