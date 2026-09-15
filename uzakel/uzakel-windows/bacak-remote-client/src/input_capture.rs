//! Translates local `winit` input into wire [`InputEvent`]s.
//!
//! Pointer motion is sent as relative deltas (matching what the host's
//! `enigo::Coordinate::Rel` injection expects, and how `uzakel/uzakel-android/android`'s
//! `TrackpadView` already does it) computed in `main.rs` from consecutive
//! `WindowEvent::CursorMoved` positions — **not** `DeviceEvent::MouseMotion`,
//! which winit's Wayland backend never emits (it reports raw per-axis
//! `DeviceEvent::Motion` events instead); relying on `MouseMotion` alone
//! left pointer motion silently dead under `bacak-compositor` until this was
//! found via real-hardware testing (see the workspace README's real-machine
//! testing notes).
//!
//! Touch coordinates are normalized against the *window's* current size, on
//! the assumption the render surface shows the full remote screen at
//! whatever local size/aspect the window happens to be — no letterboxing yet
//! (see workspace README's "known gaps").

use bacak_remote_proto::{InputEvent, PointerButton};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, Touch, TouchPhase};

pub fn map_mouse_button(button: MouseButton) -> Option<PointerButton> {
    match button {
        MouseButton::Left => Some(PointerButton::Left),
        MouseButton::Right => Some(PointerButton::Right),
        MouseButton::Middle => Some(PointerButton::Middle),
        _ => None,
    }
}

pub fn mouse_button_event(button: MouseButton, state: ElementState) -> Option<InputEvent> {
    let button = map_mouse_button(button)?;
    Some(InputEvent::PointerButton { button, pressed: state == ElementState::Pressed })
}

pub fn scroll_event(delta: MouseScrollDelta) -> InputEvent {
    let (dx, dy) = match delta {
        MouseScrollDelta::LineDelta(x, y) => (x, y),
        // Pixel deltas (trackpads) come in much finer-grained; scale down to
        // roughly line-equivalent so both input types feel similar on the host.
        MouseScrollDelta::PixelDelta(pos) => (pos.x as f32 / 40.0, pos.y as f32 / 40.0),
    };
    InputEvent::PointerScroll { dx, dy }
}

pub fn touch_event(touch: Touch, window_width: f64, window_height: f64) -> InputEvent {
    let x = (touch.location.x / window_width.max(1.0)) as f32;
    let y = (touch.location.y / window_height.max(1.0)) as f32;
    let finger_id = touch.id as u32;
    match touch.phase {
        TouchPhase::Started => InputEvent::TouchDown { finger_id, x, y },
        TouchPhase::Moved => InputEvent::TouchMotion { finger_id, x, y },
        TouchPhase::Ended | TouchPhase::Cancelled => InputEvent::TouchUp { finger_id },
    }
}
