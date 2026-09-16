//! Selection plugin — the text-selection + IME overlays and their input:
//! the floating Copy/Paste menu (Tier B), the native cosmic-text panel
//! (Tier A), the AT-SPI selection over foreign apps (Tier C), and the IME
//! candidate popups. Dispatches to `render_floating_menu` / `render_text_panel`
//! / `render_ime_popups` / `render_atspi_selection` and the `atspi_*` /
//! `selection_menu_*` / `panel_input_*` state.
//!
//! Renders **above** the overview; takes a press right after the keyboard
//! (Tier C handle-grab must beat the menu's off-tap dismiss).
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::backend::renderer::gles::GlesRenderer;

use super::{Plugin, PluginCtx};
use crate::render::BacakElements;
use crate::selection::NativeText;
use crate::state::BacakState;
use crate::wm::{OutputId, Rect};

// --- plugin-owned data types (Phase-2). Field instances + the FloatingMenu /
// TextPanel / AtspiSelection `impl`s stay on/near `BacakState`; the definitions
// live here. ---

/// Android-style floating action menu shown after a long-press: a horizontal
/// strip of touch-friendly buttons (Copy | Paste | Select all | Search) near
/// the touch point. Each button drives the *focused client* by synthesising the
/// keystroke the toolkit binds — so it works over Firefox/terminals/editors with
/// no per-app integration. Geometry is precomputed at open time (WM-global px).
#[derive(Debug, Clone)]
pub struct FloatingMenu {
    pub output: OutputId,
    /// The whole rounded background plaque.
    pub rect: Rect,
    pub items: Vec<FloatingMenuItem>,
    /// Per-item tap targets, parallel to `items`, laid out left→right.
    pub buttons: Vec<Rect>,
}

#[derive(Debug, Clone)]
pub struct FloatingMenuItem {
    pub label: String,
    pub action: SelectionAction,
    /// Freedesktop icon name drawn above the label (e.g. `edit-copy`).
    pub icon: &'static str,
}

/// What a [`FloatingMenu`] button does. Each maps to a synthesised chord sent
/// to the focused client (see `BacakState::run_selection_action`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionAction {
    /// Ctrl+C.
    Copy,
    /// Ctrl+V.
    Paste,
    /// Ctrl+A.
    SelectAll,
    /// Copy, then hand the clipboard text to a web search (later slice); for
    /// now behaves as Copy so the button is never a dead end.
    Search,
}

/// Who owns the compositor-set selection, so `SelectionHandler::send_selection`
/// knows how to serve a read request: the XWayland bridge, or the compositor's
/// own copied text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionOrigin {
    /// Mirrored from an X11 client — served via the `X11SelectionSink`.
    X11,
    /// Text copied out of a compositor-native surface (the text panel) —
    /// served from `BacakState::clipboard_text`.
    NativeText,
}

/// A compositor-native **text panel** — the Tier A demonstrator. It draws its
/// own text (via [`NativeText`]/cosmic-text), so it has the glyph geometry for
/// real Android-style selection: tap-to-caret, double/triple-tap word/line,
/// drag-select, draggable handles, accurate highlight — copying out of its own
/// buffer (no key synthesis).
pub struct TextPanel {
    pub output: OutputId,
    /// Background plaque (WM-global logical px).
    pub rect: Rect,
    /// Top-left of the text content (WM-global) = `rect` + padding.
    pub text_origin: (f32, f32),
    /// Laid-out text size `(w, h)` (text-local px).
    pub text_size: (f32, f32),
    /// The selectable text + live selection.
    pub native: NativeText,
    /// Cached glyph bitmap as a GPU-uploadable buffer, built once per layout;
    /// `(buffer, w, h)` keeps the pixel size for placement.
    pub bitmap: Option<(MemoryRenderBuffer, usize, usize)>,
    /// A drag-select or handle-drag is in progress on this slot.
    pub dragging: bool,
}

/// Tier C selection overlay: an accurate highlight + handles the compositor
/// draws *over a foreign client* (Firefox/LibreOffice/…), geometry pulled from
/// AT-SPI `GetRangeExtents`. All rects/points are WM-global screen px.
#[derive(Debug, Clone)]
pub struct AtspiSelection {
    /// The accessible being selected (re-queried during a drag).
    pub acc: crate::atspi::AccRef,
    /// Coordinate space the app speaks (SCREEN for X11, WINDOW for Wayland).
    pub coord: u32,
    /// Screen-px origin to add to WINDOW-coord extents (0,0 for SCREEN).
    pub origin: (f32, f32),
    /// Fixed end of the selection (offset), in characters.
    pub anchor: i32,
    /// Moving end (offset) — what a drag / grabbed handle changes.
    pub focus: i32,
    /// The selected text, read via AT-SPI `GetText` for Copy.
    pub text: String,
    /// Highlight rectangles (screen px) — one per visual line.
    pub highlight: Vec<Rect>,
    /// Start/end handle anchor points (screen px), at each line's baseline.
    pub handles: Option<((f32, f32), (f32, f32))>,
    /// A drag-select or handle-drag is in progress.
    pub dragging: bool,
    /// Latest un-applied drag point (screen px) — coalesced to avoid a D-Bus
    /// `SetSelection` per motion sample. Flushed on a debounce tick + release.
    pub pending: Option<(f32, f32)>,
    /// `start_time`-relative ms of the last drag D-Bus round-trip (debounce).
    pub last_query_ms: u64,
}

pub struct SelectionPlugin;

impl Plugin for SelectionPlugin {
    fn id(&self) -> &'static str {
        "selection"
    }

    fn z(&self) -> i32 {
        // Above the overview/menus: handles + the action menu sit on top.
        210
    }

    fn input_z(&self) -> i32 {
        // Just below the keyboard, above the other overlays.
        95
    }

    fn on_pointer_press(&self, ctx: &mut PluginCtx, gx: f64, gy: f64) -> bool {
        let (px, py) = (gx as f32, gy as f32);
        let st = ctx.state();
        // Tier C handle grab first (beats the menu's off-tap dismiss), then the
        // action menu, then the native text panel.
        st.atspi_handle_press(px, py) || st.selection_menu_press(px, py) || st.panel_input_down(px, py)
    }

    fn on_touch_press(&self, ctx: &mut PluginCtx, tx: f32, ty: f32, _slot: i32) -> bool {
        let st = ctx.state();
        if st.atspi_handle_press(tx, ty) {
            return true;
        }
        // An open floating menu swallows the touch (a button tap, or an off-menu
        // dismiss) regardless of whether a button was hit.
        if st.floating_menu.is_some() {
            st.selection_menu_press(tx, ty);
            return true;
        }
        st.panel_input_down(tx, ty)
    }

    fn on_pointer_motion(&self, ctx: &mut PluginCtx, gx: f64, gy: f64) -> bool {
        let st = ctx.state();
        st.panel_input_motion(gx as f32, gy as f32) || st.atspi_input_motion(gx as f32, gy as f32)
    }

    fn on_pointer_release(&self, ctx: &mut PluginCtx, _gx: f64, _gy: f64) -> bool {
        let st = ctx.state();
        st.panel_input_up() || st.atspi_input_up()
    }

    fn on_touch_motion(&self, ctx: &mut PluginCtx, tx: f32, ty: f32, _slot: i32) -> bool {
        // Panel / Tier-C drags track their own state (not slot-keyed).
        let st = ctx.state();
        st.panel_input_motion(tx, ty) || st.atspi_input_motion(tx, ty)
    }

    fn on_touch_up(&self, ctx: &mut PluginCtx, _slot: i32) -> bool {
        let st = ctx.state();
        st.panel_input_up() || st.atspi_input_up()
    }

    fn render(
        &self,
        state: &BacakState,
        renderer: &mut GlesRenderer,
        output: OutputId,
        scale: i32,
        off_x: i32,
        off_y: i32,
        out: &mut Vec<BacakElements>,
    ) {
        crate::render::render_floating_menu(state, renderer, output, scale, off_x, off_y, out);
        crate::render::render_text_panel(state, renderer, output, scale, off_x, off_y, out);
        // IME candidate + AT-SPI overlays are screen-space (no per-output gate).
        crate::render::render_ime_popups(state, renderer, scale, off_x, off_y, out);
        crate::render::render_atspi_selection(state, renderer, scale, off_x, off_y, out);
    }
}
