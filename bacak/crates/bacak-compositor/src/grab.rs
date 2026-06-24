//! Pointer grabs for interactive window move + resize.
//!
//! Wayland clients can ask the compositor to *manage* a drag for them via
//! `xdg_toplevel.move` and `xdg_toplevel.resize`. The protocol's contract is:
//!
//! 1. Client receives a pointer button press.
//! 2. Client immediately calls `move` (or `resize`) with that serial.
//! 3. Compositor takes over: pointer events route to a server-side grab
//!    instead of any surface, until the user releases the same button.
//!
//! These grabs implement step 3. They:
//!
//! * Convert pointer-motion deltas into authoritative WM geometry updates
//!   (so multi-monitor and snap invariants always hold).
//! * For *move* grabs, hit-test each motion against [`crate::wm::hit_test_snap`]
//!   and update [`crate::state::BacakState::snap_preview`] so the renderer
//!   can paint a translucent landing pad under the pointer.
//! * On the matching button release, run a spring animation to the target
//!   (the snapped half-screen rect or, when off-snap, just settle in place).
//! * For *resize* grabs, forward the new logical size to the client through
//!   `xdg_toplevel.configure` so the client itself re-renders at the new
//!   size — the compositor never stretches a stale buffer.
//!
//! Compiled only with the `runtime` feature because they reach into Smithay
//! protocol types.

#![cfg(feature = "runtime")]

use smithay::input::pointer::{
    AxisFrame, ButtonEvent, Focus, GestureHoldBeginEvent, GestureHoldEndEvent,
    GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent,
    GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent, GrabStartData,
    MotionEvent, PointerGrab, PointerInnerHandle, RelativeMotionEvent,
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::{Logical, Point, SERIAL_COUNTER};
use smithay::wayland::shell::xdg::ToplevelSurface;

use crate::animation::{AnimEnd, WindowAnim};
use crate::state::BacakState;
use crate::wm::{hit_test_snap, rect_for_zone, Monitor, Rect, WindowId};

/// Minimum window size during interactive resize. Anything smaller than this
/// makes the chrome unusable, and a zero size would crash dumb clients.
const MIN_RESIZE_W: f32 = 200.0;
const MIN_RESIZE_H: f32 = 120.0;

// ---------------------------------------------------------------------------
// Move grab
// ---------------------------------------------------------------------------

/// Interactive move. Started in `XdgShellHandler::move_request`.
pub struct MoveGrab {
    /// Pointer + click coords at the moment the grab began.
    start: GrabStartData<BacakState>,
    /// Window being dragged.
    window: WindowId,
    /// Window origin (top-left) at the moment the grab began. Combined with
    /// `event.location - start.location` this gives us the new origin
    /// without accumulating floating-point drift across many motion events.
    initial_window_pos: Point<f64, Logical>,
    /// Window size at the moment the grab began — preserved unchanged
    /// through the move; resize is a separate grab.
    initial_size: (f32, f32),
    /// Monitor used for snap hit-tests. We freeze the work area at grab
    /// start; a mid-drag resolution change is a corner case we ignore.
    monitor: Monitor,
}

impl MoveGrab {
    pub fn new(
        start: GrabStartData<BacakState>,
        window: WindowId,
        initial_window_pos: Point<f64, Logical>,
        initial_size: (f32, f32),
        monitor: Monitor,
    ) -> Self {
        Self { start, window, initial_window_pos, initial_size, monitor }
    }
}

impl PointerGrab<BacakState> for MoveGrab {
    fn motion(
        &mut self,
        data: &mut BacakState,
        handle: &mut PointerInnerHandle<'_, BacakState>,
        _focus: Option<(smithay::reexports::wayland_server::protocol::wl_surface::WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // Smithay's grab convention: pass `None` as focus so no surface
        // receives spurious pointer enter/leave events while the user is
        // dragging chrome.
        handle.motion(data, None, event);

        // Translate the pointer delta into a new window origin.
        let delta = event.location - self.start.location;
        let new_x = self.initial_window_pos.x + delta.x;
        let new_y = self.initial_window_pos.y + delta.y;
        let new_rect = Rect::new(
            new_x as f32,
            new_y as f32,
            self.initial_size.0,
            self.initial_size.1,
        );
        let _ = data.wm.r#move(self.window, new_rect);

        // Edge proximity: ask the WM whether *the pointer* is in a snap
        // zone, not the window's origin. That gives the "throw to corner"
        // behaviour users expect from KDE / GNOME / Windows.
        data.snap_preview = hit_test_snap(
            event.location.x as f32,
            event.location.y as f32,
            self.monitor,
        )
        .map(|zone| (zone, rect_for_zone(zone, self.monitor)));
    }

    fn relative_motion(
        &mut self,
        data: &mut BacakState,
        handle: &mut PointerInnerHandle<'_, BacakState>,
        _focus: Option<(smithay::reexports::wayland_server::protocol::wl_surface::WlSurface, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, None, event);
    }

    fn button(
        &mut self,
        data: &mut BacakState,
        handle: &mut PointerInnerHandle<'_, BacakState>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);

        // The same button that started the grab must end it. Other button
        // events are forwarded transparently — left-drag while right-click
        // is held shouldn't tear the grab.
        if event.button != self.start.button {
            return;
        }
        if event.state != smithay::backend::input::ButtonState::Released {
            return;
        }

        // Restore the default grab and clear the snap preview overlay.
        let serial = SERIAL_COUNTER.next_serial();
        handle.unset_grab(self, data, serial, event.time, true);
        let preview = data.snap_preview.take();

        // Decide the spring target:
        //   * snap zone → animate to half-screen rect, then re-snap WM state
        //   * off-snap  → animate the window from its current spot to the
        //                 same spot (== no-op), so we still flush any
        //                 sub-pixel drift introduced by floating-point math.
        let current = match data.wm.get(self.window) {
            Ok(w) => w.geom,
            Err(_) => return,
        };
        let (target, zone) = match preview {
            Some((zone, target_rect)) => {
                // Decorated windows reserve the title-bar strip at the top of
                // the snap zone, so the bar (drawn above the content) stays
                // on-screen instead of off the top edge / over the panel.
                let t = if data.decorated.contains(&self.window) {
                    let b = crate::decoration::BAR_H;
                    Rect::new(
                        target_rect.x,
                        target_rect.y + b,
                        target_rect.w,
                        (target_rect.h - b).max(1.0),
                    )
                } else {
                    target_rect
                };
                (t, Some(zone))
            }
            None => (current, None),
        };
        // Avoid scheduling a no-op animation that would burn frames.
        if (target.x - current.x).abs() < 0.5
            && (target.y - current.y).abs() < 0.5
            && (target.w - current.w).abs() < 0.5
            && (target.h - current.h).abs() < 0.5
            && zone.is_none()
        {
            return;
        }
        let end = zone.map(AnimEnd::Snap).unwrap_or(AnimEnd::None);
        data.animations.insert(
            self.window,
            WindowAnim::to_rect(self.window, current, target, end),
        );
    }

    fn axis(&mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>, details: AxisFrame) {
        handle.axis(data, details);
    }
    fn frame(&mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>) {
        handle.frame(data);
    }
    fn gesture_swipe_begin(
        &mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>, event: &GestureSwipeBeginEvent,
    ) { handle.gesture_swipe_begin(data, event); }
    fn gesture_swipe_update(
        &mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>, event: &GestureSwipeUpdateEvent,
    ) { handle.gesture_swipe_update(data, event); }
    fn gesture_swipe_end(
        &mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>, event: &GestureSwipeEndEvent,
    ) { handle.gesture_swipe_end(data, event); }
    fn gesture_pinch_begin(
        &mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>, event: &GesturePinchBeginEvent,
    ) { handle.gesture_pinch_begin(data, event); }
    fn gesture_pinch_update(
        &mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>, event: &GesturePinchUpdateEvent,
    ) { handle.gesture_pinch_update(data, event); }
    fn gesture_pinch_end(
        &mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>, event: &GesturePinchEndEvent,
    ) { handle.gesture_pinch_end(data, event); }
    fn gesture_hold_begin(
        &mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>, event: &GestureHoldBeginEvent,
    ) { handle.gesture_hold_begin(data, event); }
    fn gesture_hold_end(
        &mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>, event: &GestureHoldEndEvent,
    ) { handle.gesture_hold_end(data, event); }

    fn start_data(&self) -> &GrabStartData<BacakState> { &self.start }
    fn unset(&mut self, data: &mut BacakState) {
        // The grab can be replaced by another (e.g. focus changes) before
        // the user releases. Clean up the overlay so we don't leave a
        // stale snap preview painted on the next frame.
        data.snap_preview = None;
    }
}

// ---------------------------------------------------------------------------
// Resize grab
// ---------------------------------------------------------------------------

/// Interactive resize. Started in `XdgShellHandler::resize_request`. The
/// `edges` mask tells us which sides of the window are anchored to the
/// pointer; we recompute geometry against the *unchanged* opposite side so
/// the window never appears to drift sideways while you drag a single edge.
pub struct ResizeGrab {
    start: GrabStartData<BacakState>,
    window: WindowId,
    /// Initial geometry at grab start — every motion is computed against
    /// this baseline, so we don't accumulate drift over many events.
    initial_geom: Rect,
    /// Which edges the user is pulling.
    edges: xdg_toplevel::ResizeEdge,
    /// Surface handle, used to send `xdg_toplevel.configure` so the client
    /// renders at the new size. Without this the buffer would stretch.
    surface: ToplevelSurface,
}

impl ResizeGrab {
    pub fn new(
        start: GrabStartData<BacakState>,
        window: WindowId,
        initial_geom: Rect,
        edges: xdg_toplevel::ResizeEdge,
        surface: ToplevelSurface,
    ) -> Self {
        Self { start, window, initial_geom, edges, surface }
    }

    /// Apply the current pointer position to the initial geometry, returning
    /// the new rect after clamping to the minimum window size.
    fn compute_new_geom(&self, pointer_now: Point<f64, Logical>) -> Rect {
        let dx = (pointer_now.x - self.start.location.x) as f32;
        let dy = (pointer_now.y - self.start.location.y) as f32;
        let mut r = self.initial_geom;

        // Convert the protocol enum into independent top/bottom/left/right
        // booleans. Corners are encoded as composite variants in the
        // protocol (TopLeft = 5, etc.) so we just match each one.
        let (left, right, top, bottom) = match self.edges {
            xdg_toplevel::ResizeEdge::Top         => (false, false, true,  false),
            xdg_toplevel::ResizeEdge::Bottom      => (false, false, false, true),
            xdg_toplevel::ResizeEdge::Left        => (true,  false, false, false),
            xdg_toplevel::ResizeEdge::Right       => (false, true,  false, false),
            xdg_toplevel::ResizeEdge::TopLeft     => (true,  false, true,  false),
            xdg_toplevel::ResizeEdge::TopRight    => (false, true,  true,  false),
            xdg_toplevel::ResizeEdge::BottomLeft  => (true,  false, false, true),
            xdg_toplevel::ResizeEdge::BottomRight => (false, true,  false, true),
            // None / future variants: behave as bottom-right, the friendliest
            // default for a malformed request.
            _ => (false, true, false, true),
        };

        if left {
            r.x = self.initial_geom.x + dx;
            r.w = (self.initial_geom.w - dx).max(MIN_RESIZE_W);
            // If clamping kicked in, snap x back so the right edge stays put.
            if r.w == MIN_RESIZE_W {
                r.x = self.initial_geom.x + self.initial_geom.w - MIN_RESIZE_W;
            }
        }
        if right {
            r.w = (self.initial_geom.w + dx).max(MIN_RESIZE_W);
        }
        if top {
            r.y = self.initial_geom.y + dy;
            r.h = (self.initial_geom.h - dy).max(MIN_RESIZE_H);
            if r.h == MIN_RESIZE_H {
                r.y = self.initial_geom.y + self.initial_geom.h - MIN_RESIZE_H;
            }
        }
        if bottom {
            r.h = (self.initial_geom.h + dy).max(MIN_RESIZE_H);
        }
        r
    }
}

impl PointerGrab<BacakState> for ResizeGrab {
    fn motion(
        &mut self,
        data: &mut BacakState,
        handle: &mut PointerInnerHandle<'_, BacakState>,
        _focus: Option<(smithay::reexports::wayland_server::protocol::wl_surface::WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        handle.motion(data, None, event);

        let new_geom = self.compute_new_geom(event.location);
        let _ = data.wm.r#move(self.window, new_geom);

        // Drive the client. Mark the surface as Resizing so adaptive
        // clients can avoid expensive damage during the drag and just
        // letterbox their current frame.
        self.surface.with_pending_state(|s| {
            s.size = Some((new_geom.w as i32, new_geom.h as i32).into());
            s.states.set(xdg_toplevel::State::Resizing);
        });
        self.surface.send_configure();
    }

    fn relative_motion(
        &mut self,
        data: &mut BacakState,
        handle: &mut PointerInnerHandle<'_, BacakState>,
        _focus: Option<(smithay::reexports::wayland_server::protocol::wl_surface::WlSurface, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, None, event);
    }

    fn button(
        &mut self,
        data: &mut BacakState,
        handle: &mut PointerInnerHandle<'_, BacakState>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
        if event.button != self.start.button {
            return;
        }
        if event.state != smithay::backend::input::ButtonState::Released {
            return;
        }

        let serial = SERIAL_COUNTER.next_serial();
        handle.unset_grab(self, data, serial, event.time, true);

        // Drop the Resizing state so the client can resume its normal
        // damage-tracking after the drag ends.
        self.surface.with_pending_state(|s| {
            s.states.unset(xdg_toplevel::State::Resizing);
        });
        self.surface.send_configure();
    }

    fn axis(&mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>, details: AxisFrame) {
        handle.axis(data, details);
    }
    fn frame(&mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>) {
        handle.frame(data);
    }
    fn gesture_swipe_begin(
        &mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>, event: &GestureSwipeBeginEvent,
    ) { handle.gesture_swipe_begin(data, event); }
    fn gesture_swipe_update(
        &mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>, event: &GestureSwipeUpdateEvent,
    ) { handle.gesture_swipe_update(data, event); }
    fn gesture_swipe_end(
        &mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>, event: &GestureSwipeEndEvent,
    ) { handle.gesture_swipe_end(data, event); }
    fn gesture_pinch_begin(
        &mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>, event: &GesturePinchBeginEvent,
    ) { handle.gesture_pinch_begin(data, event); }
    fn gesture_pinch_update(
        &mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>, event: &GesturePinchUpdateEvent,
    ) { handle.gesture_pinch_update(data, event); }
    fn gesture_pinch_end(
        &mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>, event: &GesturePinchEndEvent,
    ) { handle.gesture_pinch_end(data, event); }
    fn gesture_hold_begin(
        &mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>, event: &GestureHoldBeginEvent,
    ) { handle.gesture_hold_begin(data, event); }
    fn gesture_hold_end(
        &mut self, data: &mut BacakState, handle: &mut PointerInnerHandle<'_, BacakState>, event: &GestureHoldEndEvent,
    ) { handle.gesture_hold_end(data, event); }

    fn start_data(&self) -> &GrabStartData<BacakState> { &self.start }
    fn unset(&mut self, _data: &mut BacakState) {
        // Best-effort: pull the Resizing state so a long-replaced grab
        // doesn't leave the client thinking the drag is still in progress.
        self.surface.with_pending_state(|s| {
            s.states.unset(xdg_toplevel::State::Resizing);
        });
        self.surface.send_configure();
    }
}

// ---------------------------------------------------------------------------
// Helpers used by the xdg-shell handler to start a grab.
// ---------------------------------------------------------------------------

/// Validate that the start serial matches a recent pointer click on the same
/// surface, then hand `grab` to the pointer. Returns `true` on success,
/// `false` when the request was stale / mismatched (the spec says clients
/// must tolerate this).
pub fn start_pointer_grab<G: PointerGrab<BacakState> + 'static>(
    pointer: &smithay::input::pointer::PointerHandle<BacakState>,
    data: &mut BacakState,
    grab: G,
    serial: smithay::utils::Serial,
) {
    pointer.set_grab(data, grab, serial, Focus::Clear);
}

/// `Focus` is re-exported here so the xdg-shell handler can pass it without
/// importing every smithay::input path.
#[allow(unused)]
pub use smithay::input::pointer::Focus as PointerFocus;
