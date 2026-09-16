//! Best-effort automatic palm rejection.
//!
//! The touch panel this app was validated against (`CoolTouch(TM) System`)
//! only reports `ABS_MT_POSITION_X/Y` + a tracking id — no
//! `ABS_MT_PRESSURE` and no `ABS_MT_TOUCH_MAJOR/MINOR` (contact size), which
//! is what real palm-rejection algorithms key off. Without that data we
//! cannot reliably distinguish a fingertip from a palm from geometry alone.
//!
//! What we *can* do: a resting palm next to an active pen stroke typically
//! lands as 2+ contact points appearing close together in a very short
//! window (the heel of the hand plus one or more fingers curling against
//! the surface), whereas a deliberate second touch (another student, a
//! deliberate two-finger gesture) tends to either be far away or arrive
//! with clearly separated timing. This module implements that heuristic as
//! a *suppression signal only* — it is not a substitute for the explicit
//! [`crate::toolbar::Tool`] selection, which is the reliable mechanism.

use glam::Vec2;

/// Two touches starting within this many pixels of each other are treated
/// as one physical contact event (e.g. palm heel + curled finger).
const CLUSTER_RADIUS: f32 = 90.0;

/// Two touches starting within this many seconds of each other are treated
/// as simultaneous for clustering purposes.
const CLUSTER_WINDOW_SECS: f64 = 0.12;

/// A touch that appears within [`CLUSTER_RADIUS`] and [`CLUSTER_WINDOW_SECS`]
/// of an already-active Pen-tool touch is flagged as a probable palm contact
/// and should not start a stroke.
pub fn is_probable_palm(
    new_pos: Vec2,
    new_time: f64,
    active_pen_touches: impl Iterator<Item = (Vec2, f64)>,
) -> bool {
    active_pen_touches.into_iter().any(|(pos, start_time)| {
        (new_time - start_time).abs() <= CLUSTER_WINDOW_SECS && new_pos.distance(pos) <= CLUSTER_RADIUS
    })
}
