//! Overview ("recents") plugin — the Android-style Recent-Apps carousel: a
//! full-screen, dimmed overlay of live window cards above everything else.
//!
//! Phase-2 status: render goes through the registry here (highest overlay `z`,
//! so it covers the dock/keyboard — which auto-suppress via `osk_blocked`). Its
//! **input** (`overview_press`/`overview_pointer_motion`/`overview_release`/
//! `overview_middle_click`, with `overview_touch_slot`) and **animation** (the
//! scroll spring + card-dismiss, stepped in `tick_animations`) still live on
//! [`BacakState`]; relocating those bodies needs a `PluginCtx` that borrows the
//! non-overview parts of state (otherwise the transitive closure of private
//! fields/helpers has to be exposed). That's the remaining migration.
use std::time::Instant;

use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::backend::renderer::gles::GlesRenderer;

use super::{Plugin, PluginCtx};
use crate::animation::Spring;
use crate::carousel::DragAxis;
use crate::render::BacakElements;
use crate::state::BacakState;
use crate::wm::{OutputId, Rect, WindowId};

// --- plugin-owned data types (Phase-2). Field instances + methods stay on
// `BacakState`; the definitions live here. ---

/// One card in the horizontal Recent-Apps carousel. Its on-screen geometry
/// (centre / scale / opacity) is *not* stored — it's derived every frame from
/// the carousel scroll offset by [`crate::carousel::card_transform`], so the
/// only per-card state is identity + label.
pub struct OverviewCard {
    pub id: WindowId,
    pub app: String,
    /// Pre-rasterised window title shown under the card.
    pub label: Option<(MemoryRenderBuffer, usize, usize)>,
}

/// The Recent-Apps Overview: a horizontal carousel of [`OverviewCard`]s over a
/// dimmed/blurred backdrop. `scroll.pos` is the px offset of the centred card
/// from index 0 (see [`crate::carousel`]); cards render at a uniform fixed frame
/// size; the focused card is marked by the accent ring. Rebuilt on open and
/// after a card is dismissed so it always reflects the live window set.
pub struct Overview {
    pub output: OutputId,
    pub cards: Vec<OverviewCard>,
    /// 1-D scroll spring (px). Drag drives `pos`; release injects `vel` for
    /// inertia and `retarget`s to the snap index.
    pub scroll: Spring,
    /// "Close all" pill at the bottom-centre (Android-style).
    pub close_all: Rect,
    pub close_all_label: Option<(MemoryRenderBuffer, usize, usize)>,
    /// In-flight per-card dismiss animation (at most one at a time).
    pub dismiss: Option<DismissAnim>,
    /// `(card, when)` for a brief red "couldn't close" pulse.
    pub error: Option<(WindowId, Instant)>,
}

/// In-flight single-finger / mouse drag over the carousel. A horizontal drag
/// scrolls (kinetic, fling-to-snap); a vertical drag dismisses the pressed card.
/// `current_x/current_y` are read by the backends' touch-slot routing.
#[derive(Clone, Copy)]
pub struct OverviewDrag {
    /// Window under the initial press — the vertical-dismiss target.
    pub card: WindowId,
    pub start_x: f32,
    pub start_y: f32,
    pub current_x: f32,
    pub current_y: f32,
    /// `scroll.pos` captured at press, so motion is a pure delta.
    pub start_scroll: f64,
    pub last_x: f32,
    pub last_y: f32,
    pub last_t: Instant,
    pub velocity: f32,
    pub axis: DragAxis,
    /// Vertical-dismiss offset of the pressed card (px upward, ≥ 0).
    pub dismiss_dy: f32,
}

/// Phase of a card's dismiss animation — graceful-close aware: the card flies
/// off (`Closing`), close is requested, the card is held (`Pending`) until the
/// client goes; if it refuses past a timeout it springs back (`Refused`).
#[derive(Clone, Copy, PartialEq)]
pub enum DismissKind {
    /// Below-threshold cancel → spring back to 0, then drop (card kept).
    SnapBack,
    /// Exit fly-up; on reaching `off_at` → request close + go `Pending`.
    Closing,
    /// Flew off, close requested at this instant; awaiting destroy or timeout.
    Pending(Instant),
    /// Client refused → spring back down to 0, then drop + flag an error pulse.
    Refused,
}

/// A card's dismiss animation: a 1-D vertical [`Spring`] on the upward offset
/// (`dy.pos`) plus its [`DismissKind`].
#[derive(Clone)]
pub struct DismissAnim {
    pub id: WindowId,
    pub dy: Spring,
    pub kind: DismissKind,
    /// `dy` at which the card is fully off the top edge.
    pub off_at: f32,
}

pub struct OverviewPlugin;

impl Plugin for OverviewPlugin {
    fn id(&self) -> &'static str {
        "overview"
    }

    fn z(&self) -> i32 {
        // Top-most overlay: a full-screen mode that dims + covers the dock and
        // keyboard (both of which suppress themselves while it's open).
        200
    }

    fn input_z(&self) -> i32 {
        // Renders top-most, but takes a press *after* the keyboard.
        80
    }

    fn on_pointer_press(&self, ctx: &mut PluginCtx, gx: f64, gy: f64) -> bool {
        ctx.state().overview_press(gx as f32, gy as f32)
    }

    fn on_touch_press(&self, ctx: &mut PluginCtx, tx: f32, ty: f32, slot: i32) -> bool {
        let st = ctx.state();
        if st.overview.is_none() {
            return false;
        }
        // A card drag already owns the overview → swallow extra fingers.
        if st.overview_touch_slot.is_some() {
            return true;
        }
        let consumed = st.overview_press(tx, ty);
        if st.overview_drag.is_some() {
            st.overview_touch_slot = Some(slot);
        }
        consumed
    }

    fn on_pointer_release(&self, ctx: &mut PluginCtx, gx: f64, gy: f64) -> bool {
        ctx.state().overview_release(gx as f32, gy as f32)
    }

    fn on_touch_motion(&self, ctx: &mut PluginCtx, tx: f32, ty: f32, slot: i32) -> bool {
        let st = ctx.state();
        if st.overview_touch_slot == Some(slot) {
            st.overview_pointer_motion(tx, ty);
            true
        } else {
            false
        }
    }

    fn on_touch_up(&self, ctx: &mut PluginCtx, slot: i32) -> bool {
        let st = ctx.state();
        if st.overview_touch_slot == Some(slot) {
            let (px, py) = st
                .overview_drag
                .as_ref()
                .map(|d| (d.current_x, d.current_y))
                .unwrap_or((0.0, 0.0));
            st.overview_release(px, py);
            st.overview_touch_slot = None;
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
        crate::render::render_overview(state, renderer, output, scale, off_x, off_y, out);
    }
}
