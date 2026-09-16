//! `zwp_text_input_v3` — a **hand-rolled** server implementation, replacing
//! smithay's built-in one.
//!
//! ## Why not smithay's?
//!
//! Smithay's `text_input` delegate discards *every* text-input request unless an
//! `input-method-v2` instance is bound (`text_input_handle.rs`: "discarding
//! text-input request without IME running"). Bacak's on-screen keyboard is
//! **not** an input-method client — it types via the seat keyboard
//! (`synthesize_chord`) — and a bare Bacak session has no external IME (ibus/
//! fcitx). So under smithay, a client's `enable`/`commit` was thrown away,
//! `active_text_input_id` was never set, and the OSK auto-show could never fire.
//!
//! This module observes text-input directly, with **no IME requirement**:
//! * tracks each client's `zwp_text_input_v3` objects,
//! * sends `enter`/`leave` as the keyboard focus moves ([`BacakState::ti_set_focus`]),
//! * on `commit`, applies the pending `enable` / cursor-rectangle / content-purpose
//!   and drives the OSK directly ([`BacakState::ti_update_osk`]) — show on an
//!   enabled editable field, hide otherwise,
//! * and can `commit_string` literal text (emoji / accents) straight to the
//!   focused field ([`BacakState::ti_commit_string`]).
//!
//! `input-method-v2` + `virtual-keyboard-v1` globals are still advertised for
//! external tools; they simply aren't bridged to this observer (Bacak doesn't
//! need that path).

use smithay::reexports::wayland_protocols::wp::text_input::zv3::server::{
    zwp_text_input_manager_v3::{self, ZwpTextInputManagerV3},
    zwp_text_input_v3::{self, ContentPurpose, ZwpTextInputV3},
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::state::BacakState;
use std::time::{Duration, Instant};

/// How long to wait before actually hiding the OSK after a field deactivates.
/// Deliberately longer than the ~1.2 s enable/disable toggle period some
/// toolkits emit, so a re-activation always lands inside the window and cancels
/// the hide — the keyboard stays put instead of flickering. The cost is a ~1.5 s
/// linger after the user genuinely leaves a field, which is fine (Android does
/// the same).
pub const OSK_HIDE_DEBOUNCE: Duration = Duration::from_millis(1500);

/// Pending (not-yet-committed) text-input state, accumulated between `commit`s.
#[derive(Debug, Default, Clone)]
struct Pending {
    enable: Option<bool>,
    /// Cursor rectangle in surface-local logical coords (x, y, w, h).
    cursor_rect: Option<(i32, i32, i32, i32)>,
    purpose: Option<ContentPurpose>,
}

/// One client `zwp_text_input_v3` object + its committed/pending state.
#[derive(Debug)]
struct Instance {
    obj: ZwpTextInputV3,
    /// `done` serial = number of `commit`s received from the client.
    serial: u32,
    pending: Pending,
    enabled: bool,
    cursor_rect: Option<(i32, i32, i32, i32)>,
    purpose: ContentPurpose,
}

/// All live text-input objects + the surface that currently holds focus.
#[derive(Debug, Default)]
pub struct BacakTextInput {
    focus: Option<WlSurface>,
    instances: Vec<Instance>,
}

impl BacakTextInput {
    /// Whichever instance belonging to the focused client is enabled, if any —
    /// the field the OSK should be open for.
    fn active(&self) -> Option<&Instance> {
        let focus = self.focus.as_ref()?;
        self.instances
            .iter()
            .find(|i| i.enabled && i.obj.id().same_client_as(&focus.id()))
    }

    fn instance_count(&self) -> usize {
        self.instances.len()
    }
    fn enabled_count(&self) -> usize {
        self.instances.iter().filter(|i| i.enabled).count()
    }
}

// ---------------------------------------------------------------------------
// Global + Dispatch
// ---------------------------------------------------------------------------

impl GlobalDispatch<ZwpTextInputManagerV3, ()> for BacakState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpTextInputManagerV3>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwpTextInputManagerV3, ()> for BacakState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZwpTextInputManagerV3,
        request: zwp_text_input_manager_v3::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_text_input_manager_v3::Request::GetTextInput { id, seat: _ } => {
                let obj = data_init.init(id, ());
                // If the requesting client is already focused, it must receive an
                // `enter` immediately so it knows it may enable text-input.
                if let Some(focus) = state.bacak_text_input.focus.clone() {
                    if obj.id().same_client_as(&focus.id()) {
                        obj.enter(&focus);
                    }
                }
                state.bacak_text_input.instances.push(Instance {
                    obj,
                    serial: 0,
                    pending: Pending::default(),
                    enabled: false,
                    cursor_rect: None,
                    purpose: ContentPurpose::Normal,
                });
            }
            zwp_text_input_manager_v3::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<ZwpTextInputV3, ()> for BacakState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwpTextInputV3,
        request: zwp_text_input_v3::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use zwp_text_input_v3::Request;
        let Some(inst) = state
            .bacak_text_input
            .instances
            .iter_mut()
            .find(|i| i.obj == *resource)
        else {
            return;
        };
        match request {
            Request::Enable => inst.pending.enable = Some(true),
            Request::Disable => inst.pending.enable = Some(false),
            Request::SetCursorRectangle { x, y, width, height } => {
                inst.pending.cursor_rect = Some((x, y, width, height));
            }
            Request::SetContentType { hint: _, purpose } => {
                inst.pending.purpose = purpose.into_result().ok();
            }
            Request::SetSurroundingText { .. } | Request::SetTextChangeCause { .. } => {}
            Request::Commit => {
                inst.serial = inst.serial.wrapping_add(1);
                let p = std::mem::take(&mut inst.pending);
                if let Some(en) = p.enable {
                    if inst.enabled != en {
                        tracing::info!(enable = en, "text-input enable toggled");
                    }
                    inst.enabled = en;
                }
                if let Some(cr) = p.cursor_rect {
                    inst.cursor_rect = Some(cr);
                }
                if let Some(pp) = p.purpose {
                    inst.purpose = pp;
                }
                state.ti_update_osk();
            }
            Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, resource: &ZwpTextInputV3, _data: &()) {
        state
            .bacak_text_input
            .instances
            .retain(|i| i.obj != *resource);
        state.ti_update_osk();
    }
}

use smithay::reexports::wayland_server::backend::ClientId;

// ---------------------------------------------------------------------------
// BacakState integration
// ---------------------------------------------------------------------------

impl BacakState {
    /// Retarget text-input focus as the keyboard focus moves. Sends `leave` to
    /// the previously focused client's text-inputs and `enter` to the new one's
    /// (resetting their enabled state — the client re-`enable`s if its widget is
    /// editable). Called from `SeatHandler::focus_changed`.
    pub fn ti_set_focus(&mut self, surface: Option<WlSurface>) {
        if let Some(old) = self.bacak_text_input.focus.take() {
            for inst in &self.bacak_text_input.instances {
                if inst.obj.id().same_client_as(&old.id()) {
                    inst.obj.leave(&old);
                }
            }
        }
        // Reset the enabled flag for every instance of the newly focused client
        // and send it `enter`.
        if let Some(new) = surface.clone() {
            for inst in &mut self.bacak_text_input.instances {
                if inst.obj.id().same_client_as(&new.id()) {
                    inst.enabled = false;
                    inst.pending = Pending::default();
                    inst.obj.enter(&new);
                }
            }
        }
        let app = surface
            .as_ref()
            .and_then(|s| self.window_for(s))
            .and_then(|id| self.wm.get(id).ok())
            .map(|w| w.app)
            .unwrap_or_default();
        tracing::info!(focus = %app, has_focus = surface.is_some(), "ti_set_focus");
        self.bacak_text_input.focus = surface;
        self.ti_update_osk();
    }

    /// Open or close the OSK to match the current text-input state: open for an
    /// enabled, non-blocked editable field on its window's output; close
    /// otherwise. Marks the frame dirty on any visibility change so the backend
    /// re-renders.
    pub fn ti_update_osk(&mut self) {
        let was_visible = self.osk.is_visible();

        // Snapshot what we need before mutably borrowing the OSK.
        let target = self.bacak_text_input.active().map(|i| (i.purpose, i.cursor_rect));
        let focus = self.bacak_text_input.focus.clone();
        let want_visible = target.map_or(false, |(p, _)| !osk_purpose_blocked(p));
        tracing::debug!(
            want_visible,
            instances = self.bacak_text_input.instance_count(),
            enabled = self.bacak_text_input.enabled_count(),
            "ti_update_osk"
        );

        match target {
            Some((purpose, cursor_rect)) if !osk_purpose_blocked(purpose) => {
                // Field active → cancel any pending hide and (re)open.
                self.osk_hide_at = None;
                if !self.osk.is_visible() {
                    if let Some(surface) = focus {
                        let (output, mut rect) = self.osk_field_geometry(&surface);
                        // Prefer the precise cursor rectangle (surface-local →
                        // global) when the client supplied one.
                        if let Some((cx, cy, cw, ch)) = cursor_rect {
                            rect = OskRect {
                                x: rect.x + cx as f32,
                                y: rect.y + cy as f32,
                                w: cw.max(1) as f32,
                                h: ch.max(1) as f32,
                            };
                        }
                        if let Some(out) = output {
                            self.osk.bind(self.wm.clone(), out);
                        }
                        let field = FocusedField {
                            rect,
                            mode: InputMode::Text,
                            has_hw_keyboard: false,
                        };
                        self.osk.open_for(field);
                        let _ = self.osk.confirm_open();
                        self.osk.auto_position(rect);
                        // Program the seat to the keyboard's active layout
                        // (Turkish-F by default) so keys type the right glyphs.
                        self.apply_osk_xkb();
                    }
                }
            }
            _ => {
                // Field inactive → don't close immediately; arm the debounce.
                // A re-activation before it fires cancels it (above). The
                // per-frame tick (`osk_tick_hide`) performs the actual close.
                if self.osk.is_visible() && self.osk_hide_at.is_none() {
                    self.osk_hide_at = Some(Instant::now() + OSK_HIDE_DEBOUNCE);
                }
            }
        }

        if self.osk.is_visible() != was_visible {
            self.osk_dirty = true;
            tracing::info!(
                visible = self.osk.is_visible(),
                "OSK visibility changed (text-input)"
            );
        }
    }

    /// Per-frame: fire a debounced hide once its deadline passes and the field
    /// is still inactive. Returns `true` if it closed the OSK (caller redraws).
    pub fn osk_tick_hide(&mut self) -> bool {
        let Some(deadline) = self.osk_hide_at else { return false };
        if Instant::now() < deadline {
            return false;
        }
        self.osk_hide_at = None;
        // Re-check: a field may have re-activated without clearing (defensive).
        if self.bacak_text_input.active().is_none() && self.osk.is_visible() {
            self.osk.close();
            let _ = self.osk.confirm_close();
            tracing::info!("OSK hidden (debounce elapsed)");
            return true;
        }
        false
    }

    /// Commit a literal string (emoji / accented alternate) directly to the
    /// focused, enabled text-input field via `commit_string` + `done`. Returns
    /// `true` if a field accepted it. Protocol-correct: the client splices the
    /// glyph at its cursor, no clipboard involved.
    pub fn ti_commit_string(&mut self, text: &str) -> bool {
        let focus = match self.bacak_text_input.focus.clone() {
            Some(f) => f,
            None => return false,
        };
        for inst in &mut self.bacak_text_input.instances {
            if inst.enabled && inst.obj.id().same_client_as(&focus.id()) {
                inst.obj.commit_string(Some(text.to_string()));
                inst.obj.done(inst.serial);
                return true;
            }
        }
        false
    }
}

/// Policy gate: suppress the keyboard for password / PIN fields when the policy
/// disables it. Default policy allows them (returns `false`); flip this to
/// honour a "no OSK for passwords" setting.
fn osk_purpose_blocked(purpose: ContentPurpose) -> bool {
    let _ = purpose; // matches Password / Pin when a policy is added
    false
}

use crate::input::{FocusedField, InputMode, OskRect};
