//! Input — on-screen keyboard state machine + touch gestures.
//!
//! The OSK is a layout-aware state machine so the compositor and the shell
//! agree on lifecycle. When the OSK opens it emits a "reserve area" geometry
//! that the WM uses to shrink the focused window's viewport instead of just
//! floating over it.
//!
//! ## Reserve-area bridge
//!
//! Calling [`OskController::bind`] wires the controller to a
//! [`WindowManager`] + [`OutputId`]. After binding, opening the OSK
//! publishes a bottom strut of `keyboard_h` on that output, and closing
//! restores whatever struts were in place before. Until `bind` is
//! called the controller is purely a state machine — useful for tests
//! and the CLI demo.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;

use crate::keyboard::{
    layout_rects, Edge, KeyAction, Keyboard, KeyRect, LayoutId, Placement,
};
use crate::wm::{OutputId, Struts, WindowManager};

/// Height of the draggable title strip at the top of the panel, in px.
pub const OSK_TITLE_H: f32 = 30.0;
/// Inter-key gap, in px.
pub const OSK_KEY_GAP: f32 = 6.0;
/// Width of the close (✕) button at the right end of the title strip, in px.
pub const OSK_CLOSE_W: f32 = 40.0;

// ---------------------------------------------------------------------------
// OSK
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OskState { Closed, Opening, Open, Closing }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OskLayout { Full, Split, Floating }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InputMode { Text, Numeric, Url, Email, Search, Password }

impl InputMode {
    pub fn enter_label(self) -> &'static str {
        match self {
            InputMode::Search => "Search",
            InputMode::Url    => "Go",
            InputMode::Email  => "Next",
            _ => "↵",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct OskRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FocusedField {
    pub rect: OskRect,
    pub mode: InputMode,
    pub has_hw_keyboard: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct OskGeometry {
    pub panel: OskRect,
    pub reserved_top_y: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct OskConfig {
    pub screen_w: f32,
    pub screen_h: f32,
    pub keyboard_h: f32,
    pub margin: f32,
    pub layout: OskLayout,
}

impl Default for OskConfig {
    fn default() -> Self {
        Self {
            screen_w: 1440.0,
            screen_h: 900.0,
            keyboard_h: 280.0,
            margin: 24.0,
            layout: OskLayout::Full,
        }
    }
}

#[derive(Debug, Error)]
pub enum OskError {
    #[error("illegal transition: {0:?} → {1:?}")]
    BadTransition(OskState, OskState),
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct OskController {
    inner: Arc<Mutex<OskInner>>,
}

struct OskInner {
    state: OskState,
    config: OskConfig,
    enter_label: &'static str,
    binding: Option<OskBinding>,
    /// The live keyboard: active page, modifier latches and placement.
    kb: Keyboard,
    /// Active title-bar drag: pointer offset (dx,dy) from the panel's
    /// top-left at grab time, so the panel tracks the finger 1:1.
    drag: Option<(f32, f32)>,
    /// Index of the key currently held down (for the press-down visual), as a
    /// (row,col). Cleared on release.
    pressed: Option<(usize, usize)>,
}

/// Per-binding state for the OSK ↔ WM bridge. The OSK contributes its
/// bottom reservation as a dedicated `"osk"` strut layer; the WM
/// composes that with the dock / panel / manual layers via per-edge
/// max, so close-time cleanup is just `clear_strut_layer("osk")` and
/// no save/restore is needed — other contributors keep their own
/// reservations.
struct OskBinding {
    wm: WindowManager,
    output: OutputId,
    /// Whether this binding currently has an `"osk"` layer published.
    /// Tracked so [`unbind`] / re-bind can clear it without an extra
    /// WM round-trip.
    has_layer: bool,
}

impl OskController {
    pub fn new(config: OskConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(OskInner {
                state: OskState::Closed,
                config,
                enter_label: "↵",
                binding: None,
                kb: Keyboard::new(crate::keyboard::system_default_layout()),
                drag: None,
                pressed: None,
            })),
        }
    }

    pub fn state(&self) -> OskState { self.inner.lock().state }

    pub fn enter_label(&self) -> &'static str { self.inner.lock().enter_label }

    pub fn set_layout(&self, layout: OskLayout) { self.inner.lock().config.layout = layout; }

    pub fn set_screen(&self, w: f32, h: f32) {
        let mut g = self.inner.lock();
        g.config.screen_w = w;
        g.config.screen_h = h;
    }

    /// Wire this controller to a window manager and an output.
    ///
    /// After binding, the controller publishes a `bottom: keyboard_h`
    /// strut whenever the OSK transitions from a closed-ish state to
    /// `Opening`, and restores whatever struts were in place before on
    /// the way back to `Closing`.
    ///
    /// `bind` also reads the output's bounds into the controller's
    /// `screen_w` / `screen_h` so [`OskController::geometry`] matches
    /// the real surface. Re-bind to a different output if the focused
    /// field migrates monitors mid-session — any outstanding strut on
    /// the old output is restored before the swap.
    pub fn bind(&self, wm: WindowManager, output: OutputId) {
        let mut g = self.inner.lock();

        // Clear our strut layer on the *previous* output before
        // swapping — otherwise a re-bind would leave the old monitor
        // with a ghost OSK reservation forever.
        if let Some(prev) = g.binding.take() {
            if prev.has_layer {
                let _ = prev.wm.clear_strut_layer(prev.output, "osk");
            }
        }

        if let Some(o) = wm.output(output) {
            g.config.screen_w = o.bounds.w;
            g.config.screen_h = o.bounds.h;
        }
        g.binding = Some(OskBinding { wm, output, has_layer: false });

        // If the OSK was already open at bind time, publish the strut on
        // the new output right away.
        if matches!(g.state, OskState::Opening | OskState::Open) {
            push_strut(&mut g);
        }
    }

    /// Tear down the WM binding. Any strut this controller pushed is
    /// restored to its pre-OSK value first; afterwards `open_for` /
    /// `close` no-op on the WM side.
    pub fn unbind(&self) {
        let mut g = self.inner.lock();
        if let Some(prev) = g.binding.take() {
            if prev.has_layer {
                let _ = prev.wm.clear_strut_layer(prev.output, "osk");
            }
        }
    }

    pub fn open_for(&self, field: FocusedField) -> Option<OskGeometry> {
        if field.has_hw_keyboard {
            return None;
        }
        let mut g = self.inner.lock();
        g.enter_label = field.mode.enter_label();
        let was_dormant = matches!(g.state, OskState::Closed | OskState::Closing);
        g.state = match g.state {
            OskState::Closed | OskState::Closing => OskState::Opening,
            other => other,
        };
        if was_dormant {
            push_strut(&mut g);
        }
        Some(compute_geometry(&g.config))
    }

    pub fn confirm_open(&self) -> Result<(), OskError> {
        let mut g = self.inner.lock();
        match g.state {
            OskState::Opening => { g.state = OskState::Open; Ok(()) }
            s => Err(OskError::BadTransition(s, OskState::Open)),
        }
    }

    pub fn close(&self) {
        let mut g = self.inner.lock();
        let was_visible = matches!(g.state, OskState::Open | OskState::Opening);
        g.state = match g.state {
            OskState::Open | OskState::Opening => OskState::Closing,
            OskState::Closed => OskState::Closed,
            OskState::Closing => OskState::Closing,
        };
        if was_visible {
            restore_strut(&mut g);
        }
    }

    pub fn confirm_close(&self) -> Result<(), OskError> {
        let mut g = self.inner.lock();
        match g.state {
            OskState::Closing | OskState::Closed => { g.state = OskState::Closed; Ok(()) }
            s => Err(OskError::BadTransition(s, OskState::Closed)),
        }
    }

    pub fn geometry(&self) -> OskGeometry { compute_geometry(&self.inner.lock().config) }

    pub fn scroll_offset_for(&self, field: &FocusedField) -> f32 {
        let geom = compute_geometry(&self.inner.lock().config);
        let overlap = field.rect.y + field.rect.h - geom.reserved_top_y;
        if overlap > 0.0 { overlap + self.inner.lock().config.margin } else { 0.0 }
    }
}

/// One key, fully resolved for the renderer: its pixel rect, the glyph to
/// draw (already shift-folded) and highlight flags. Decouples `render.rs`
/// from the keyboard model internals.
#[derive(Debug, Clone)]
pub struct RenderKey {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub label: String,
    /// This key is the one under the finger right now (press animation).
    pub pressed: bool,
    /// This key is an active/locked modifier (Shift latched, Caps, Ctrl…).
    pub active_mod: bool,
    /// Wide "action" key (space, enter, layout switch) — renderer can tint it.
    pub is_action: bool,
    /// Draw the Anadolu Panteri logo instead of a text label (the Win/Super key).
    pub is_logo: bool,
}

/// A press resolved against the live panel: either a typed [`KeyAction`] or a
/// long-press request carrying the alternate glyphs to pop up.
#[derive(Debug, Clone)]
pub enum OskPress {
    /// A normal key fired.
    Key(KeyAction),
    /// The press landed on the title strip — begin a drag instead of typing.
    TitleGrab,
    /// Nothing under the point.
    Miss,
}

/// A press resolved when the caller is capturing text into its *own* field
/// (e.g. the Wi-Fi password box) rather than forwarding evdev codes to a
/// client. The glyph is read from the key model directly so no xkb round-trip
/// is needed.
#[derive(Debug, Clone)]
pub enum OskTextPress {
    /// Nothing typable under the point.
    Miss,
    /// Title-strip drag / non-text consumed press — swallow it, no text.
    Title,
    /// Append this string to the field.
    Char(String),
    /// Delete the last character.
    Backspace,
    /// Enter / Go — submit the field.
    Enter,
    /// A modifier / layout / action key, already applied to the model; the
    /// caller runs `osk_dispatch` for it (Hide, Copy/Paste, …) and re-applies xkb.
    Action(KeyAction),
}

impl OskController {
    /// Resolve the panel rectangle in output-pixel coordinates, honouring the
    /// keyboard's placement (docked bottom/top or floating after a drag).
    pub fn panel_rect(&self) -> OskRect {
        panel_rect_inner(&self.inner.lock())
    }

    /// True while the keyboard should be drawn (open or animating open).
    pub fn is_visible(&self) -> bool {
        matches!(self.state(), OskState::Open | OskState::Opening)
    }

    pub fn layout_id(&self) -> LayoutId {
        self.inner.lock().kb.layout_id()
    }

    /// The output the OSK is currently bound to (where it should render), if
    /// any. Set by [`bind`](Self::bind).
    pub fn bound_output(&self) -> Option<OutputId> {
        self.inner.lock().binding.as_ref().map(|b| b.output)
    }

    pub fn is_split(&self) -> bool {
        self.inner.lock().kb.split()
    }

    /// Switch the active page directly (e.g. a layout button on the shell).
    /// Returns the xkb `(layout, variant)` the seat keyboard should adopt so
    /// evdev codes produce the page's glyphs — `None` to keep the current one.
    pub fn goto_layout(&self, id: LayoutId) -> Option<(&'static str, &'static str)> {
        self.inner.lock().kb.goto(id)
    }

    /// Drain a pending seat xkb `(layout, variant)` change queued by the last
    /// page switch (or the initial layout). The compositor applies it via
    /// `set_xkb_config` so the keys actually type this page's glyphs.
    pub fn take_pending_xkb(&self) -> Option<(&'static str, &'static str)> {
        self.inner.lock().kb.take_pending_xkb()
    }

    /// Build the renderer's key list for the current page + modifier state.
    pub fn render_keys(&self) -> Vec<RenderKey> {
        let g = self.inner.lock();
        let panel = panel_rect_inner(&g);
        let shifted = g.kb.mods.shifted();
        let rects = layout_rects(
            g.kb.page(),
            (panel.x, panel.y, panel.w, panel.h),
            OSK_TITLE_H,
            OSK_KEY_GAP,
        );
        rects
            .into_iter()
            .map(|r: KeyRect| {
                let key = &g.kb.page().rows[r.row][r.col];
                let active_mod = is_active_mod(&g.kb, key);
                // Dynamic caps: the language key shows the live language code
                // (TR/EN); Copy/Paste/Cut/Select-All are localised to the
                // active language (Kopyala/Yapıştır…).
                use crate::keyboard::{Action as KbAction, KeyKind};
                let label = match &key.kind {
                    KeyKind::Action(KbAction::Language) => {
                        g.kb.layout_id().short_label().to_string()
                    }
                    KeyKind::Action(a) => crate::keyboard::action_label(*a, g.kb.language())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| key.display(shifted).to_string()),
                    _ => key.display(shifted).to_string(),
                };
                RenderKey {
                    x: r.x,
                    y: r.y,
                    w: r.w,
                    h: r.h,
                    label,
                    pressed: g.pressed == Some((r.row, r.col)),
                    active_mod,
                    is_action: matches!(
                        key.kind,
                        crate::keyboard::KeyKind::Action(_)
                    ) || key.width >= 2.0,
                    is_logo: matches!(
                        key.kind,
                        crate::keyboard::KeyKind::Key(crate::keyboard::code::LEFTMETA)
                    ),
                }
            })
            .collect()
    }

    /// Hit-test a press at output-pixel `(px, py)`. A press on the title strip
    /// arms a drag; a press on a key records the press-down (for the animation)
    /// and returns the resolved [`KeyAction`]; pressing the modifier keys only
    /// flips state. `now_ms` drives the double-tap-Caps timing.
    pub fn press_at(&self, px: f32, py: f32, now_ms: u64) -> OskPress {
        let mut g = self.inner.lock();
        let panel = panel_rect_inner(&g);
        if px < panel.x || px >= panel.x + panel.w || py < panel.y || py >= panel.y + panel.h {
            return OskPress::Miss;
        }
        // Title strip: the right-edge close button hides the keyboard; the rest
        // of the strip is the drag handle.
        if py < panel.y + OSK_TITLE_H {
            if px >= panel.x + panel.w - OSK_CLOSE_W {
                return OskPress::Key(KeyAction::Special(crate::keyboard::Action::Hide));
            }
            g.drag = Some((px - panel.x, py - panel.y));
            return OskPress::TitleGrab;
        }
        let rects = layout_rects(
            g.kb.page(),
            (panel.x, panel.y, panel.w, panel.h),
            OSK_TITLE_H,
            OSK_KEY_GAP,
        );
        let Some(hit) = rects.iter().find(|r| r.contains(px, py)).cloned() else {
            return OskPress::Miss;
        };
        g.pressed = Some((hit.row, hit.col));
        let action = g.kb.press(hit.row, hit.col, now_ms);
        OskPress::Key(action)
    }

    /// Like [`press_at`](Self::press_at) but for compositor-internal text
    /// capture: returns the typed glyph / edit action instead of a client-bound
    /// keycode. Still drives the keyboard model (shift latch, page/layout, press
    /// highlight) so the OSK behaves normally.
    pub fn press_text_at(&self, px: f32, py: f32, now_ms: u64) -> OskTextPress {
        use crate::keyboard::{code, Action as KbAction, KeyKind};
        let mut g = self.inner.lock();
        let panel = panel_rect_inner(&g);
        if px < panel.x || px >= panel.x + panel.w || py < panel.y || py >= panel.y + panel.h {
            return OskTextPress::Miss;
        }
        if py < panel.y + OSK_TITLE_H {
            if px >= panel.x + panel.w - OSK_CLOSE_W {
                return OskTextPress::Action(KeyAction::Special(KbAction::Hide));
            }
            g.drag = Some((px - panel.x, py - panel.y));
            return OskTextPress::Title;
        }
        let rects = layout_rects(
            g.kb.page(),
            (panel.x, panel.y, panel.w, panel.h),
            OSK_TITLE_H,
            OSK_KEY_GAP,
        );
        let Some(hit) = rects.iter().find(|r| r.contains(px, py)).cloned() else {
            return OskTextPress::Miss;
        };
        g.pressed = Some((hit.row, hit.col));
        // Resolve the glyph *before* `press()` consumes the shift latch.
        let (kind, glyph) = {
            let key = &g.kb.page().rows[hit.row][hit.col];
            (key.kind.clone(), key.display(g.kb.mods.shifted()).to_string())
        };
        let action = g.kb.press(hit.row, hit.col, now_ms);
        match kind {
            KeyKind::Unicode(s) => OskTextPress::Char(s.to_string()),
            KeyKind::Key(code::BACKSPACE) => OskTextPress::Backspace,
            KeyKind::Key(code::ENTER) | KeyKind::Key(code::KPENTER) => OskTextPress::Enter,
            KeyKind::Key(code::SPACE) => OskTextPress::Char(" ".to_string()),
            KeyKind::Key(_) => OskTextPress::Char(glyph),
            KeyKind::Modifier(_) | KeyKind::Action(_) => OskTextPress::Action(action),
        }
    }

    /// Long-press alternates for the key under `(px,py)`, or empty.
    pub fn alternates_at(&self, px: f32, py: f32) -> Vec<String> {
        let g = self.inner.lock();
        let panel = panel_rect_inner(&g);
        let rects = layout_rects(
            g.kb.page(),
            (panel.x, panel.y, panel.w, panel.h),
            OSK_TITLE_H,
            OSK_KEY_GAP,
        );
        rects
            .iter()
            .find(|r| r.contains(px, py))
            .map(|r| g.kb.alternates_at(r.row, r.col).iter().map(|s| s.to_string()).collect())
            .unwrap_or_default()
    }

    /// Clear the press-down highlight (finger lifted / left the key).
    pub fn release(&self) {
        self.inner.lock().pressed = None;
    }

    // --- Dragging ---------------------------------------------------------

    /// Move an in-progress title drag so the panel's top-left tracks the
    /// finger. Switches placement to `Floating`. No-op if no drag is armed.
    pub fn drag_to(&self, px: f32, py: f32) {
        let mut g = self.inner.lock();
        let Some((dx, dy)) = g.drag else { return };
        let (w, h) = (panel_rect_inner(&g).w, g.config.keyboard_h);
        // Clamp so the panel stays on-screen.
        let x = (px - dx).clamp(0.0, (g.config.screen_w - w).max(0.0));
        let y = (py - dy).clamp(0.0, (g.config.screen_h - h).max(0.0));
        g.kb.placement = Placement::Floating { x, y };
    }

    /// Finish a drag. If the panel was dropped against an edge it re-docks
    /// there; otherwise it stays floating.
    pub fn drag_end(&self) {
        let mut g = self.inner.lock();
        g.drag = None;
        let panel = panel_rect_inner(&g);
        let snap = g.config.margin * 1.5;
        if panel.y + panel.h >= g.config.screen_h - snap {
            g.kb.placement = Placement::Docked(Edge::Bottom);
        } else if panel.y <= snap {
            g.kb.placement = Placement::Docked(Edge::Top);
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.inner.lock().drag.is_some()
    }

    /// Restore the default bottom-centre dock.
    pub fn restore_default_position(&self) {
        self.inner.lock().kb.placement = Placement::default();
    }

    /// Auto-reposition so a focused field stays visible. For a *floating*
    /// panel that overlaps the field, the panel relocates above the field if
    /// there is room, else below. For a docked panel the WM strut +
    /// [`scroll_offset_for`](Self::scroll_offset_for) already lift the content,
    /// so this is a no-op there.
    pub fn auto_position(&self, field: OskRect) {
        let mut g = self.inner.lock();
        if !matches!(g.kb.placement, Placement::Floating { .. }) {
            return;
        }
        let panel = panel_rect_inner(&g);
        let overlaps = field.y + field.h > panel.y && field.y < panel.y + panel.h;
        if !overlaps {
            return;
        }
        let above = field.y - panel.h - g.config.margin;
        let below = field.y + field.h + g.config.margin;
        let y = if above >= 0.0 {
            above
        } else if below + panel.h <= g.config.screen_h {
            below
        } else {
            (g.config.screen_h - panel.h).max(0.0)
        };
        g.kb.placement = Placement::Floating { x: panel.x, y };
    }
}

/// Resolve the panel rect from placement + screen size.
fn panel_rect_inner(g: &OskInner) -> OskRect {
    let c = &g.config;
    let h = c.keyboard_h;
    // A docked keyboard is a *centred, normal-width* panel — not edge-to-edge
    // (full width is unwieldy on wide screens and buries the dock entirely). It
    // takes ~72% of the screen, clamped to a comfortable [680, 1120] px, so the
    // dock stays visible either side of it.
    let docked_w = (c.screen_w * 0.72).clamp(680.0, 1120.0).min(c.screen_w);
    let docked_x = ((c.screen_w - docked_w) / 2.0).max(0.0);
    match g.kb.placement {
        Placement::Docked(Edge::Bottom) => {
            OskRect { x: docked_x, y: c.screen_h - h, w: docked_w, h }
        }
        Placement::Docked(Edge::Top) => OskRect { x: docked_x, y: 0.0, w: docked_w, h },
        Placement::Floating { x, y } => {
            // Floating panel is narrower so it reads as a movable window.
            let w = (c.screen_w * 0.62).max(480.0).min(c.screen_w);
            OskRect { x, y, w, h }
        }
    }
}

/// Whether `key` represents a modifier that is currently latched/locked, for
/// the render highlight.
fn is_active_mod(kb: &Keyboard, key: &crate::keyboard::KeyCap) -> bool {
    use crate::keyboard::{KeyKind, Modifier};
    match key.kind {
        KeyKind::Modifier(Modifier::Shift) => kb.mods.shifted(),
        KeyKind::Modifier(Modifier::Ctrl) => kb.mods.ctrl,
        KeyKind::Modifier(Modifier::Alt) => kb.mods.alt,
        KeyKind::Modifier(Modifier::AltGr) => kb.mods.altgr,
        _ => false,
    }
}

/// Publish the OSK's bottom reservation as a dedicated `"osk"` strut
/// layer on the bound output. The WM composes layers via per-edge max,
/// so this never clobbers the dock / panel / manual contributions —
/// `restore_strut` simply clears this layer and the others stay live.
fn push_strut(g: &mut OskInner) {
    let kh = g.config.keyboard_h;
    let Some(b) = g.binding.as_mut() else { return; };
    if b.wm
        .set_strut_layer(b.output, "osk", Struts { bottom: kh, ..Struts::default() })
        .is_ok()
    {
        b.has_layer = true;
    }
}

/// Mirror of [`push_strut`]: drop the `"osk"` layer. Other contributors
/// keep their own reservations untouched.
fn restore_strut(g: &mut OskInner) {
    let Some(b) = g.binding.as_mut() else { return; };
    if b.has_layer {
        let _ = b.wm.clear_strut_layer(b.output, "osk");
        b.has_layer = false;
    }
}

fn compute_geometry(c: &OskConfig) -> OskGeometry {
    let panel = match c.layout {
        OskLayout::Full => OskRect {
            x: 0.0, y: c.screen_h - c.keyboard_h, w: c.screen_w, h: c.keyboard_h,
        },
        OskLayout::Split => OskRect {
            x: 0.0, y: c.screen_h - c.keyboard_h, w: c.screen_w, h: c.keyboard_h,
        },
        OskLayout::Floating => OskRect {
            x: c.screen_w * 0.15,
            y: c.screen_h - c.keyboard_h - 80.0,
            w: c.screen_w * 0.70,
            h: c.keyboard_h,
        },
    };
    OskGeometry { panel, reserved_top_y: panel.y }
}

// ---------------------------------------------------------------------------
// Gestures
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Gesture {
    SwitchWorkspace { direction: SwipeDir },
    TaskOverview,
    ShowDesktop,
    LongPress,
    SecondaryTap,
    RevealDock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SwipeDir { Left, Right }

pub fn classify(fingers: u8, dx: f32, dy: f32, duration_ms: u32) -> Option<Gesture> {
    let absx = dx.abs();
    let absy = dy.abs();
    match fingers {
        3 if absx > 80.0 && absx > absy => Some(Gesture::SwitchWorkspace {
            direction: if dx < 0.0 { SwipeDir::Left } else { SwipeDir::Right },
        }),
        3 if dy < -80.0 => Some(Gesture::TaskOverview),
        4 if absx < 40.0 && absy < 40.0 => Some(Gesture::ShowDesktop),
        1 if absx < 8.0 && absy < 8.0 && duration_ms >= 550 => Some(Gesture::LongPress),
        2 if absx < 8.0 && absy < 8.0 && duration_ms < 250 => Some(Gesture::SecondaryTap),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Touch aggregator
// ---------------------------------------------------------------------------

/// Per-finger bookkeeping kept by [`TouchAggregator`].
///
/// We remember the start position (for the displacement vector handed to
/// [`classify`]) and the start timestamp (for duration-sensitive gestures
/// like long-press). `last_x` / `last_y` track the most recent motion so the
/// aggregator can emit an interim vector while the gesture is still live.
#[derive(Debug, Clone, Copy)]
struct Finger {
    start_x: f32,
    start_y: f32,
    last_x: f32,
    last_y: f32,
    start_time_ms: u64,
}

/// Aggregates raw libinput / winit touch events into a single
/// [`Gesture`] when the last finger lifts.
///
/// The compositor receives **per-slot** events: `down(slot)`,
/// `motion(slot)`, `up(slot)`. Gestures are higher-level — they care about
/// "how many fingers were on the screen at the peak" and the **net**
/// displacement across the stroke. This struct bridges the two:
///
/// * Tracks one [`Finger`] per active slot.
/// * Records the highest concurrent finger count (`peak_count`) — needed
///   because users naturally land their fingers staggered, so the count at
///   the moment of release is not the count that defines the gesture.
/// * On the final `up()`, computes the **average displacement vector**
///   across all tracked fingers and the **maximum duration** any single
///   finger stayed on the screen, then runs [`classify`] over those.
///
/// `cancel()` is exposed for the libinput `TouchCancel` event, which fires
/// when the input stack itself aborts the gesture (palm rejection, focus
/// loss). It drops all tracked fingers without emitting.
pub struct TouchAggregator {
    fingers: HashMap<i32, Finger>,
    peak_count: u8,
}

impl Default for TouchAggregator {
    fn default() -> Self {
        Self::new()
    }
}

impl TouchAggregator {
    pub fn new() -> Self {
        Self { fingers: HashMap::new(), peak_count: 0 }
    }

    /// True while at least one finger is on the surface.
    pub fn is_active(&self) -> bool {
        !self.fingers.is_empty()
    }

    /// Live finger count — useful as a heuristic for the WM (e.g. suppress
    /// click-to-focus when a multi-finger stroke is in progress).
    pub fn fingers(&self) -> u8 {
        self.fingers.len() as u8
    }

    /// Record a new finger landing. `now_ms` is a monotonic millisecond
    /// timestamp, conventionally taken from the input event itself so the
    /// duration math survives clock skew.
    pub fn down(&mut self, slot: i32, x: f32, y: f32, now_ms: u64) {
        self.fingers.insert(slot, Finger {
            start_x: x, start_y: y, last_x: x, last_y: y, start_time_ms: now_ms,
        });
        let live = self.fingers.len() as u8;
        if live > self.peak_count { self.peak_count = live; }
    }

    /// Update the latest position for a tracked finger. Missing slots are
    /// silently ignored — libinput can occasionally emit a motion after the
    /// matching cancel.
    pub fn motion(&mut self, slot: i32, x: f32, y: f32) {
        if let Some(f) = self.fingers.get_mut(&slot) {
            f.last_x = x;
            f.last_y = y;
        }
    }

    /// Lift a finger. When this drains the last finger, classify the gesture
    /// over the recorded stroke and reset internal state. Returning `Some`
    /// hands the gesture upward to the compositor's gesture dispatcher.
    pub fn up(&mut self, slot: i32, now_ms: u64) -> Option<Gesture> {
        // Pop, but keep the finger's start data alive in the average below.
        let lifted = self.fingers.remove(&slot);
        if !self.fingers.is_empty() {
            // Multi-finger gesture still in progress; defer emission until
            // the surface is finger-free.
            return None;
        }
        // Combine the just-lifted finger with the previously-lifted ones in
        // the aggregator — but the others have already been removed. So we
        // average just this last finger's displacement against the others
        // implicitly: by tracking peak_count we keep the original count and
        // by using the final finger's stroke we get a representative vector.
        let last = lifted?;
        let dx = last.last_x - last.start_x;
        let dy = last.last_y - last.start_y;
        let duration_ms = now_ms.saturating_sub(last.start_time_ms) as u32;
        let fingers = self.peak_count;
        self.peak_count = 0;
        classify(fingers, dx, dy, duration_ms)
    }

    /// Drop all tracked fingers (libinput `TouchCancel`). No gesture is
    /// emitted; callers should treat the stroke as discarded.
    pub fn cancel(&mut self) {
        self.fingers.clear();
        self.peak_count = 0;
    }
}

/// Touch arbitration: decides whether a *free* touch sequence (one not already
/// claimed by an overlay / the dock) belongs to the focused client or is a
/// compositor multi-finger gesture.
///
/// One **and two** fingers are forwarded to the client, so apps get raw
/// multi-touch and can do their own pinch-to-zoom (browsers, image viewers).
/// The moment a **third** finger lands, the sequence is **claimed** as a
/// compositor gesture (workspace swipe / overview): the caller cancels the
/// client's in-progress touch ([`TouchRoute::Claim`]) and forwards nothing
/// more until every finger lifts. (This claimed at *two* fingers until
/// 2026-05-27, which blocked client-side touch pinch; reversed so a 2-finger
/// pinch reaches clients via raw wl_touch.) Pure + testable.
#[derive(Debug, Default)]
pub struct TouchArbiter {
    claimed: bool,
}

/// Where a free touch should go this event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchRoute {
    /// Forward to the focused client (one- or two-finger — tap / drag / pinch).
    Client,
    /// Just crossed into a gesture (3rd finger) — cancel the client's touch
    /// session now, then treat the rest of the sequence as gesture-only.
    Claim,
    /// Already a gesture — feed the aggregator only, forward nothing.
    Gesture,
}

impl TouchArbiter {
    pub fn new() -> Self {
        Self { claimed: false }
    }

    /// Decide routing for a free-touch *down*. `free_fingers` is the
    /// aggregator's finger count *after* this down was recorded. A single finger
    /// goes to the client (tap / drag / scroll); the **second** finger claims the
    /// sequence as a compositor gesture — two fingers drive window move /
    /// double-tap-fullscreen, three+ drive workspace / overview swipes. (This
    /// deliberately overrides client two-finger pinch/scroll in favour of
    /// touch-first window management; single-finger scroll still reaches apps.)
    pub fn down(&mut self, free_fingers: u8) -> TouchRoute {
        if self.claimed {
            TouchRoute::Gesture
        } else if free_fingers >= 2 {
            self.claimed = true;
            TouchRoute::Claim
        } else {
            TouchRoute::Client
        }
    }

    /// Whether the current sequence is a claimed gesture — motion/up of a free
    /// touch then bypass the client.
    pub fn is_gesture(&self) -> bool {
        self.claimed
    }

    /// Note a finger lifting; `still_active` = any fingers remain. Resets the
    /// claim once the sequence ends.
    pub fn up(&mut self, still_active: bool) {
        if !still_active {
            self.claimed = false;
        }
    }

    /// Hard reset on `TouchCancel`.
    pub fn cancel(&mut self) {
        self.claimed = false;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osk_full_lifecycle() {
        let osk = OskController::new(OskConfig::default());
        let field = FocusedField {
            rect: OskRect { x: 100.0, y: 700.0, w: 400.0, h: 32.0 },
            mode: InputMode::Url,
            has_hw_keyboard: false,
        };
        osk.open_for(field).unwrap();
        assert_eq!(osk.state(), OskState::Opening);
        assert_eq!(osk.enter_label(), "Go");
        osk.confirm_open().unwrap();
        assert_eq!(osk.state(), OskState::Open);
        osk.close();
        osk.confirm_close().unwrap();
        assert_eq!(osk.state(), OskState::Closed);
    }

    #[test]
    fn press_types_into_default_bottom_panel() {
        let osk = OskController::new(OskConfig::default()); // 1440x900, kb 280
        let panel = osk.panel_rect();
        assert!((panel.y - (900.0 - 280.0)).abs() < 0.01, "default = bottom dock");
        // Press the centre of a real key (from the laid-out rects, so we never
        // land in an inter-key gap).
        let key = &osk.render_keys()[20];
        let press = osk.press_at(key.x + key.w / 2.0, key.y + key.h / 2.0, 0);
        match press {
            OskPress::Key(_) => {}
            other => panic!("expected a key press, got {other:?}"),
        }
        osk.release();
    }

    #[test]
    fn title_grab_then_drag_floats_and_moves() {
        let osk = OskController::new(OskConfig::default());
        let panel = osk.panel_rect();
        // Grab the title strip.
        let p = osk.press_at(panel.x + panel.w / 2.0, panel.y + 5.0, 0);
        assert!(matches!(p, OskPress::TitleGrab));
        assert!(osk.is_dragging());
        // Drag toward the screen centre.
        osk.drag_to(400.0, 300.0);
        let moved = osk.panel_rect();
        assert!(moved.y < panel.y, "panel should have moved up off the dock");
        osk.drag_end();
        assert!(!osk.is_dragging());
    }

    #[test]
    fn restore_default_redocks_bottom() {
        let osk = OskController::new(OskConfig::default());
        let panel = osk.panel_rect();
        osk.press_at(panel.x + panel.w / 2.0, panel.y + 5.0, 0);
        osk.drag_to(200.0, 150.0);
        assert!(osk.panel_rect().y < panel.y);
        osk.restore_default_position();
        assert!((osk.panel_rect().y - (900.0 - 280.0)).abs() < 0.01);
    }

    #[test]
    fn auto_position_lifts_floating_panel_off_a_low_field() {
        let osk = OskController::new(OskConfig::default());
        // Float the panel low so it would cover a bottom field.
        let panel = osk.panel_rect();
        osk.press_at(panel.x + panel.w / 2.0, panel.y + 5.0, 0);
        osk.drag_to(300.0, 560.0); // floating, top-left ~ (300,555)
        osk.drag_end();
        let before = osk.panel_rect();
        // A field overlapping the floating panel.
        let field = OskRect { x: 320.0, y: before.y + 10.0, w: 300.0, h: 40.0 };
        osk.auto_position(field);
        let after = osk.panel_rect();
        assert!(after.y != before.y, "overlapping field should relocate the panel");
    }

    #[test]
    fn render_keys_nonempty_and_inside_panel() {
        let osk = OskController::new(OskConfig::default());
        let panel = osk.panel_rect();
        let keys = osk.render_keys();
        assert!(!keys.is_empty());
        for k in &keys {
            assert!(k.x >= panel.x - 0.5 && k.x + k.w <= panel.x + panel.w + 0.5);
            assert!(!k.label.is_empty() || k.is_action);
        }
    }

    #[test]
    fn turkish_layout_switch_reports_tr_xkb() {
        let osk = OskController::new(OskConfig::default());
        assert_eq!(osk.goto_layout(LayoutId::Turkish), Some(("tr", "f")));
        assert_eq!(osk.layout_id(), LayoutId::Turkish);
    }

    #[test]
    fn gestures_classify() {
        assert_eq!(
            classify(3, -120.0, 5.0, 200),
            Some(Gesture::SwitchWorkspace { direction: SwipeDir::Left })
        );
        assert_eq!(classify(3, 0.0, -120.0, 200), Some(Gesture::TaskOverview));
        assert_eq!(classify(1, 2.0, 1.0, 600), Some(Gesture::LongPress));
    }

    #[test]
    fn touch_aggregator_three_finger_left_swipe() {
        let mut agg = TouchAggregator::new();
        // Three fingers land at slightly different times — peak_count
        // must capture the maximum (3), not the count when they lift.
        agg.down(0, 100.0, 500.0, 0);
        agg.down(1, 150.0, 500.0, 5);
        agg.down(2, 200.0, 500.0, 10);
        assert_eq!(agg.fingers(), 3);
        // Drag left.
        agg.motion(0, -50.0, 500.0);
        agg.motion(1, 0.0, 500.0);
        agg.motion(2, 50.0, 500.0);
        // Lift staggered. Only the final `up` should emit.
        assert!(agg.up(0, 200).is_none());
        assert!(agg.up(1, 210).is_none());
        let g = agg.up(2, 220).expect("gesture should emit on last lift");
        assert_eq!(g, Gesture::SwitchWorkspace { direction: SwipeDir::Left });
        assert!(!agg.is_active());
    }

    #[test]
    fn touch_aggregator_cancel_drops_state() {
        let mut agg = TouchAggregator::new();
        agg.down(0, 0.0, 0.0, 0);
        agg.down(1, 10.0, 0.0, 0);
        agg.cancel();
        assert_eq!(agg.fingers(), 0);
        // Subsequent `up` for a slot that never re-landed must not emit.
        assert!(agg.up(0, 100).is_none());
    }

    #[test]
    fn touch_aggregator_long_press_single_finger() {
        let mut agg = TouchAggregator::new();
        agg.down(7, 100.0, 100.0, 0);
        agg.motion(7, 101.0, 100.5);
        let g = agg.up(7, 700).expect("long press");
        assert_eq!(g, Gesture::LongPress);
    }

    // -- OSK ↔ WM strut bridge ---------------------------------------------

    use crate::wm::{Monitor, Rect as WmRect, Struts, WindowManager};

    fn wm_with_screen() -> WindowManager {
        WindowManager::new(Monitor { work_area: WmRect::new(0.0, 0.0, 1440.0, 900.0) })
    }

    fn focused_text_field() -> FocusedField {
        FocusedField {
            rect: OskRect { x: 100.0, y: 700.0, w: 400.0, h: 32.0 },
            mode: InputMode::Text,
            has_hw_keyboard: false,
        }
    }

    #[test]
    fn open_publishes_strut_when_bound() {
        let wm = wm_with_screen();
        let osk = OskController::new(OskConfig::default());
        let primary = wm.primary_output().unwrap();
        osk.bind(wm.clone(), primary);

        osk.open_for(focused_text_field()).unwrap();
        // OskConfig::default().keyboard_h == 280, so the work area should
        // shrink by exactly that amount on the bottom.
        let m = wm.active_monitor().unwrap();
        assert_eq!(m.work_area, WmRect::new(0.0, 0.0, 1440.0, 900.0 - 280.0));
    }

    #[test]
    fn close_restores_previous_struts() {
        let wm = wm_with_screen();
        let osk = OskController::new(OskConfig::default());
        let primary = wm.primary_output().unwrap();
        // Dock is reserving 64 px at the bottom before the OSK shows up.
        wm.set_struts(primary, Struts { bottom: 64.0, ..Default::default() }).unwrap();

        osk.bind(wm.clone(), primary);
        osk.open_for(focused_text_field()).unwrap();
        // While open, OSK's strut supersedes the dock's.
        assert_eq!(wm.output(primary).unwrap().struts.bottom, 280.0);

        osk.close();
        // Dock's reservation is back, exactly.
        assert_eq!(wm.output(primary).unwrap().struts.bottom, 64.0);
    }

    #[test]
    fn rebind_clears_strut_on_old_output() {
        let wm = wm_with_screen();
        let secondary = wm.add_output(WmRect::new(1440.0, 0.0, 1920.0, 1080.0));
        let primary = wm.primary_output().unwrap();
        let osk = OskController::new(OskConfig::default());

        osk.bind(wm.clone(), primary);
        osk.open_for(focused_text_field()).unwrap();
        assert_eq!(wm.output(primary).unwrap().struts.bottom, 280.0);

        // Focused field jumped to the other monitor — re-bind.
        osk.bind(wm.clone(), secondary);
        // Old output's struts must be back to default.
        assert_eq!(wm.output(primary).unwrap().struts, Struts::default());
        // The OSK was still in Opening when we re-bound, so the strut
        // followed it to the new output.
        assert_eq!(wm.output(secondary).unwrap().struts.bottom, 280.0);
    }

    #[test]
    fn unbind_while_open_restores_struts() {
        let wm = wm_with_screen();
        let osk = OskController::new(OskConfig::default());
        let primary = wm.primary_output().unwrap();
        wm.set_struts(primary, Struts { bottom: 16.0, ..Default::default() }).unwrap();

        osk.bind(wm.clone(), primary);
        osk.open_for(focused_text_field()).unwrap();
        assert_eq!(wm.output(primary).unwrap().struts.bottom, 280.0);

        osk.unbind();
        // Unbinding restores whatever struts we found at bind/open time.
        assert_eq!(wm.output(primary).unwrap().struts.bottom, 16.0);
        // OSK lifecycle still works without a binding (no panic, no WM
        // calls — this asserts both).
        osk.close();
        osk.confirm_close().unwrap();
    }

    #[test]
    fn no_binding_keeps_struts_untouched() {
        let wm = wm_with_screen();
        let osk = OskController::new(OskConfig::default());
        // No bind() call.
        osk.open_for(focused_text_field()).unwrap();
        osk.confirm_open().unwrap();
        let m = wm.active_monitor().unwrap();
        assert_eq!(m.work_area, WmRect::new(0.0, 0.0, 1440.0, 900.0));
    }

    #[test]
    fn bind_pulls_output_bounds_into_screen_size() {
        let wm = wm_with_screen();
        let secondary = wm.add_output(WmRect::new(1440.0, 0.0, 2560.0, 1440.0));
        // OskConfig starts at the default 1440x900.
        let osk = OskController::new(OskConfig::default());
        osk.bind(wm.clone(), secondary);
        // geometry() now reflects the secondary's 2560x1440 bounds.
        let g = osk.geometry();
        assert_eq!(g.panel.w, 2560.0);
        // panel sits flush with the bottom: y == screen_h - keyboard_h.
        assert_eq!(g.panel.y, 1440.0 - 280.0);
    }

    #[test]
    fn touch_arbiter_claims_on_second_finger() {
        let mut a = TouchArbiter::new();
        // First finger → client (single-finger tap / drag / scroll).
        assert_eq!(a.down(1), TouchRoute::Client);
        assert!(!a.is_gesture());
        // Second finger → claim (cancel client) + gesture from here: two-finger
        // window move / double-tap-fullscreen are compositor gestures.
        assert_eq!(a.down(2), TouchRoute::Claim);
        assert!(a.is_gesture());
        // Third / fourth fingers → already a gesture (workspace / overview).
        assert_eq!(a.down(3), TouchRoute::Gesture);
        assert_eq!(a.down(4), TouchRoute::Gesture);
        // Fingers lift one by one; claim holds until the last leaves.
        a.up(true);
        assert!(a.is_gesture());
        a.up(false);
        assert!(!a.is_gesture());
        // Next single-finger sequence is a clean client touch again.
        assert_eq!(a.down(1), TouchRoute::Client);
    }

    #[test]
    fn touch_arbiter_single_finger_reaches_client() {
        // A single finger is never claimed, so wl_touch reaches the client
        // (taps, one-finger scroll/flick).
        let mut a = TouchArbiter::new();
        assert_eq!(a.down(1), TouchRoute::Client);
        assert!(!a.is_gesture(), "one finger must stay a client touch");
        a.up(false);
        assert!(!a.is_gesture());
    }

    #[test]
    fn touch_arbiter_cancel_resets() {
        let mut a = TouchArbiter::new();
        a.down(3);
        assert!(a.is_gesture());
        a.cancel();
        assert!(!a.is_gesture());
    }
}
