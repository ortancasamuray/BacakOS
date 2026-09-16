//! Gestures plugin — routes recognised multi-finger / long-press gestures to
//! shell actions (workspace switch, task overview, …). The recognition itself
//! (the touch aggregator + arbiter, the long-press recogniser) is low-level
//! input plumbing in the backends; this plugin owns only the **policy** — what
//! each [`Gesture`] does.
//!
//! It renders nothing and consumes no presses; it only implements `on_gesture`.
use super::{Plugin, PluginCtx};
use crate::input::{Gesture, SwipeDir};

pub struct GesturePlugin;

impl Plugin for GesturePlugin {
    fn id(&self) -> &'static str {
        "gestures"
    }

    fn z(&self) -> i32 {
        // Renders nothing; keep it out of the way.
        -10
    }

    fn input_z(&self) -> i32 {
        -10
    }

    fn on_gesture(&self, ctx: &mut PluginCtx, gesture: Gesture) -> bool {
        match gesture {
            Gesture::SwitchWorkspace { direction } => {
                let Some(output) = ctx.wm().primary_output() else { return false };
                // Swipe fingers left ⇒ advance (show what's to the right); right
                // ⇒ previous. Sign matches the slide delta.
                let delta = match direction {
                    SwipeDir::Left => 1,
                    SwipeDir::Right => -1,
                };
                match ctx.state().switch_workspace_relative(output, delta) {
                    Ok(()) => true,
                    Err(err) => {
                        tracing::warn!(?err, output, ?direction, "switch_workspace_relative failed");
                        false
                    }
                }
            }
            Gesture::TaskOverview => {
                // Three-finger swipe up → toggle the Recent-Apps overview.
                let Some(output) = ctx.wm().primary_output() else { return false };
                ctx.toggle_overview(output);
                true
            }
            Gesture::SecondaryTap => {
                // Four 2-finger taps in a row (mirrors the mouse LEFT+RIGHT
                // double chord-press) → open a terminal.
                ctx.state().secondary_tap_event();
                true
            }
            // ShowDesktop / LongPress / RevealDock are recognised but not yet
            // routed (need shell surfaces / WM actions not built).
            other => {
                tracing::debug!(?other, "gesture recognised but not yet routed");
                false
            }
        }
    }
}
