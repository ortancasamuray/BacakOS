//! Screenshot plugin — the capture options dialog (scope + delay) and the
//! "saved" notification toast. Dispatches to `render_shot_dialog` +
//! `render_toast` and the `shot_*` state. Highest overlay layer (the dialog is
//! modal, the toast is a notification); the delayed-capture timer ticks here.
use std::time::Instant;

use smithay::backend::renderer::gles::GlesRenderer;

use super::{Plugin, PluginCtx};
use crate::render::BacakElements;
use crate::state::{BacakState, Label};
use crate::wm::{OutputId, Rect, WindowId};

// --- plugin-owned state types (Phase-2: the screenshot feature's data lives in
// its own module; the field *instances* stay on `BacakState`, the smithay
// handler-data type). ---

/// A queued built-in screenshot. `region` (WM-global logical px) selects a
/// sub-area; `None` captures the whole output.
#[derive(Debug, Clone, Copy)]
pub struct ScreenshotReq {
    pub output: OutputId,
    pub region: Option<Rect>,
    /// When set, capture *only* this window's own pixels (content + SSD bar),
    /// rendered in isolation offscreen — overlapping windows / dock excluded.
    /// Takes precedence over `region`. `None` = output/region capture.
    pub window: Option<WindowId>,
}

/// Live state of an interactive region screenshot (Shift+PrintScreen). `anchor`
/// is the press point and `cur` the current pointer position, both WM-global
/// logical px; the selection is the normalised rectangle between them.
#[derive(Debug, Clone, Copy)]
pub struct RegionShot {
    pub output: OutputId,
    pub anchor: Option<(f64, f64)>,
    pub cur: (f64, f64),
}

impl RegionShot {
    /// The selected rectangle (normalised so w/h are positive), or `None`
    /// before the first press. Clamped to a 1px minimum so a bare click that
    /// didn't drag still yields a valid (tiny) capture rather than failing.
    pub fn rect(&self) -> Option<Rect> {
        let (ax, ay) = self.anchor?;
        let (cx, cy) = self.cur;
        let x = ax.min(cx);
        let y = ay.min(cy);
        let w = (ax - cx).abs().max(1.0);
        let h = (ay - cy).abs().max(1.0);
        Some(Rect::new(x as f32, y as f32, w as f32, h as f32))
    }
}

/// Screenshot options dialog, opened from the Control Center's screenshot
/// button. Lets the user pick the **scope** (whole output vs. the active
/// window) and a **delay** before the capture fires. A modal compositor overlay
/// rebuilt on open; selection toggles mutate the `sel_*` fields in place; "Çek"
/// schedules the grab via `BacakState::pending_shot`.
pub struct ShotDialog {
    pub output: OutputId,
    pub panel: Rect,
    /// Scope pills.
    pub mode_whole: Rect,
    pub mode_window: Rect,
    /// Delay pills, each tagged with its seconds.
    pub delays: Vec<(Rect, u64)>,
    pub capture: Rect,
    pub cancel: Rect,
    /// `false` = whole output, `true` = active window.
    pub sel_window: bool,
    /// Chosen delay in seconds (0 = immediate).
    pub sel_delay: u64,
    pub title: Label,
    pub l_whole: Label,
    pub l_window: Label,
    pub l_delay: Label,
    pub delay_lbls: Vec<Label>,
    pub l_capture: Label,
    pub l_cancel: Label,
}

/// A transient notification banner (toast) — e.g. "screenshot saved → path".
/// Shown top-centre for `TOAST_MS`, fading over the last `TOAST_FADE_MS`.
pub struct Toast {
    pub output: OutputId,
    pub start: Instant,
    pub title: Label,
    pub sub: Label,
    /// Panel size in logical px, derived from the label widths at build time.
    pub w: f32,
    pub h: f32,
}

pub struct ScreenshotPlugin;

impl Plugin for ScreenshotPlugin {
    fn id(&self) -> &'static str {
        "screenshot"
    }

    fn z(&self) -> i32 {
        70
    }

    fn input_z(&self) -> i32 {
        // The modal dialog takes input just below the keyboard.
        90
    }

    fn on_pointer_press(&self, ctx: &mut PluginCtx, gx: f64, gy: f64) -> bool {
        ctx.state().shot_dialog_press(gx as f32, gy as f32)
    }

    fn on_touch_press(&self, ctx: &mut PluginCtx, tx: f32, ty: f32, _slot: i32) -> bool {
        ctx.state().shot_dialog_press(tx, ty)
    }

    fn tick(&self, state: &mut BacakState, _now: Instant) -> bool {
        // Fire a scheduled (delayed) screenshot when its timer elapses.
        state.shot_tick()
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
        crate::render::render_shot_dialog(state, renderer, output, scale, off_x, off_y, out);
        crate::render::render_toast(state, renderer, output, scale, off_x, off_y, out);
    }
}
