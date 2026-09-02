//! Dock plugin — the compositor taskbar/launcher dock + its always-visible
//! floating button. Dispatches to `render_dock` / `render_dock_floating_button`
//! and the `dock_*` state. Renders *below* the keyboard so the OSK can overlap
//! it cleanly at the bottom edge.
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::backend::renderer::gles::GlesRenderer;

use super::{Plugin, PluginCtx};
use crate::render::BacakElements;
use crate::state::BacakState;
use crate::wm::{OutputId, Rect, WinState, WindowId};

// --- plugin-owned data types (Phase-2). The field instances + methods stay on
// `BacakState` (smithay's handler-data type); these definitions live here. ---

/// The dock's single hover-tooltip, GPU-cached. Only one tile is
/// hovered at a time, so a one-slot cache (vs. the per-window
/// [`LabelCacheEntry`](crate::state::LabelCacheEntry) map) is enough; `text` is
/// the string the buffer was rasterised for, so a different hovered tile
/// rebuilds it. `w`/`h` are the rasterised pixel size — kept so the renderer
/// can size the tooltip background and centre the glyphs on a cache hit.
pub struct DockTooltip {
    pub text: String,
    pub buffer: MemoryRenderBuffer,
    pub w: usize,
    pub h: usize,
}

/// Right-click context menu attached to a dock tile. Items come from
/// [`BacakState::dock_right_press`]; the menu's `rect` is its plaque in
/// WM-global px, computed once at open time so the click hit-test and the
/// renderer agree byte-for-byte.
#[derive(Debug, Clone)]
pub struct DockMenu {
    pub output: OutputId,
    pub rect: Rect,
    pub items: Vec<DockMenuItem>,
}

#[derive(Debug, Clone)]
pub struct DockMenuItem {
    pub label: String,
    pub action: DockMenuAction,
}

/// What a context-menu item does. Variants carry the data they need so the
/// action can fire without re-resolving the tile (whose order may have shifted
/// between open and click).
#[derive(Debug, Clone)]
pub enum DockMenuAction {
    /// Focus / un-minimise the window (never re-minimises, unlike a click).
    Activate(WindowId),
    /// Ask the client to close the window via xdg-toplevel.close.
    CloseWindow(WindowId),
    /// Spawn this app via its `.desktop` Exec (debounced).
    Launch(String),
    /// Add this app to `config.dock_pinned` (no-op if already pinned).
    Pin(String),
    /// Remove this app from `config.dock_pinned` (no-op if not pinned).
    Unpin(String),
}

/// A laid-out dock tile (icon slot) — its hit `rect`, the `app` id used for
/// both the icon and the `.desktop` `Exec=` launch, the `window` it represents
/// (if any), and whether it's `pinned`.
#[derive(Debug, Clone)]
pub struct DockEntry {
    pub rect: Rect,
    pub app: String,
    pub window: Option<WindowId>,
    pub pinned: bool,
}

/// Press-and-drag state for the pinned-tile reorder gesture. The `output` with
/// `from_idx` identifies the pinned slot — its index in `config.dock_pinned`
/// (pins come first in [`dock_tiles_for`](BacakState::dock_tiles_for)).
/// `started` flips true once the pointer passes `DOCK_DRAG_THRESHOLD`.
#[derive(Debug, Clone)]
pub struct DockDrag {
    pub output: OutputId,
    pub from_idx: usize,
    pub start_x: f32,
    pub start_y: f32,
    pub current_x: f32,
    pub current_y: f32,
    pub started: bool,
    /// True when the drag started on a pinned slot — lets the release path pick
    /// reorder/unpin vs pin/no-op without re-querying the (possibly moved) tile.
    pub source_was_pinned: bool,
    /// App id at the press site — pinned (drop onto bar) or unpinned (drop off).
    pub source_app: String,
}

/// True when some window on `output` is currently fullscreen — used to hide
/// the dock while e.g. tahta covers the whole panel, rather than drawing it
/// on top of (or under, per `udev_runtime.rs`'s forced full-composite path)
/// a fullscreen client. Reappears the moment that window closes/unfullscreens.
fn output_has_fullscreen(state: &BacakState, output: OutputId) -> bool {
    state.wm.workspaces_for(output).iter().any(|ws| {
        state
            .wm
            .windows_on_workspace(ws.id)
            .iter()
            .any(|w| matches!(w.state, WinState::Fullscreen))
    })
}

pub struct DockPlugin;

impl Plugin for DockPlugin {
    fn id(&self) -> &'static str {
        "dock"
    }

    fn z(&self) -> i32 {
        30
    }

    fn enabled(&self, state: &BacakState) -> bool {
        !state.is_greeter
            && state.config.dock
            && !state
                .wm
                .outputs()
                .iter()
                .any(|o| output_has_fullscreen(state, o.id))
    }

    fn on_pointer_press(&self, ctx: &mut PluginCtx, gx: f64, gy: f64) -> bool {
        let (px, py) = (gx as f32, gy as f32);
        let st = ctx.state();
        st.dock_menu_left_press(px, py)
            || st.dock_floating_button_press(px, py)
            || st.dock_press(px, py)
    }

    fn on_touch_press(&self, ctx: &mut PluginCtx, tx: f32, ty: f32, slot: i32) -> bool {
        let st = ctx.state();
        if st.dock_menu_left_press(tx, ty) || st.dock_floating_button_press(tx, ty) {
            return true;
        }
        // A press on a dock tile claims this slot — its motion/up route to the
        // dock (pinned-tile drag) instead of the gesture aggregator.
        if st.dock_drag.is_none() && st.dock_press(tx, ty) {
            st.dock_touch_slot = Some(slot);
            return true;
        }
        false
    }

    fn on_pointer_release(&self, ctx: &mut PluginCtx, gx: f64, gy: f64) -> bool {
        ctx.state().dock_release(gx as f32, gy as f32)
    }

    fn on_touch_motion(&self, ctx: &mut PluginCtx, tx: f32, ty: f32, slot: i32) -> bool {
        let st = ctx.state();
        if st.dock_touch_slot == Some(slot) {
            st.dock_pointer_motion(tx, ty);
            true
        } else {
            false
        }
    }

    fn on_touch_up(&self, ctx: &mut PluginCtx, slot: i32) -> bool {
        let st = ctx.state();
        if st.dock_touch_slot == Some(slot) {
            let (px, py) = st
                .dock_drag
                .as_ref()
                .map(|d| (d.current_x, d.current_y))
                .unwrap_or((0.0, 0.0));
            st.dock_release(px, py);
            st.dock_touch_slot = None;
            true
        } else {
            false
        }
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
        crate::render::render_dock(state, renderer, output, scale, off_x, off_y, out);
        crate::render::render_dock_floating_button(
            state, renderer, output, scale, off_x, off_y, out,
        );
    }
}
