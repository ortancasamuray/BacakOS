//! Onboarding plugin — a one-shot modal shown the first time a real user
//! session starts, replaying the same touch-gesture demo as the "Jest
//! Senaryosu" section of `kilavuz/bacakos-kullanim-kilavuzu.html` (drag,
//! two-finger double-tap, three-finger swipe, quad-tap), redrawn natively
//! instead of decoded from a video file — the compositor has no video
//! pipeline, and spawning an external player would break the project's
//! "no separate apps" rule. Dismissing it sets
//! `CompositorConfig::onboarding_shown` and persists it, so it never shows
//! again.
use std::time::Instant;

use smithay::backend::renderer::gles::GlesRenderer;

use super::{Plugin, PluginCtx};
use crate::render::BacakElements;
use crate::state::Label;
use crate::wm::{OutputId, Rect};

/// Total loop length and per-phase length of the gesture demo, shared
/// between the layout builder and the per-frame renderer.
pub const ONBOARDING_PHASE_SECS: f32 = 4.0;
pub const ONBOARDING_TOTAL_SECS: f32 = ONBOARDING_PHASE_SECS * 4.0;

/// Mock "app window" / "terminal" rects as fractions of the stage size —
/// ported 1:1 from the `kilavuz` guide's canvas demo (`WIN_START` etc. in
/// `bacakos.script`'s HTML companion), just expressed relative instead of
/// in fixed 960×540 canvas pixels so they scale with `OnboardingDialog::stage`.
pub const WIN_START_FRAC: (f32, f32, f32, f32) = (0.073, 0.278, 0.396, 0.430);
pub const WIN_END_FRAC: (f32, f32, f32, f32) = (0.490, 0.278, 0.396, 0.430);
pub const WIN_FULL_FRAC: (f32, f32, f32, f32) = (0.027, 0.122, 0.946, 0.774);
pub const TERM_FRAC: (f32, f32, f32, f32) = (0.583, 0.207, 0.344, 0.463);
pub const TERM_W_FRAC: f32 = TERM_FRAC.2;

/// The onboarding dialog's static layout (panel/stage/button rects, the
/// pre-rasterised caption for each of the 4 phases) plus the clock the
/// per-frame renderer plays the demo against.
pub struct OnboardingDialog {
    pub output: OutputId,
    pub start: Instant,
    pub panel: Rect,
    /// The dark "device screen" the gesture demo animates inside.
    pub stage: Rect,
    pub dismiss: Rect,
    pub l_dismiss: Label,
    /// One caption per phase, index = `phase` (0..4). Swapped, not
    /// rebuilt, each frame.
    pub captions: [Label; 4],
    pub l_terminal: Label,
}

pub struct OnboardingPlugin;

impl Plugin for OnboardingPlugin {
    fn id(&self) -> &'static str {
        "onboarding"
    }

    fn z(&self) -> i32 {
        // Above the screenshot dialog (70/90): it should interrupt anything
        // else that could somehow be open the moment a fresh session starts.
        72
    }

    fn input_z(&self) -> i32 {
        92
    }

    fn on_pointer_press(&self, ctx: &mut PluginCtx, gx: f64, gy: f64) -> bool {
        ctx.state().onboarding_dialog_press(gx as f32, gy as f32)
    }

    fn on_touch_press(&self, ctx: &mut PluginCtx, tx: f32, ty: f32, _slot: i32) -> bool {
        ctx.state().onboarding_dialog_press(tx, ty)
    }

    fn render(
        &self,
        state: &crate::state::BacakState,
        renderer: &mut GlesRenderer,
        output: OutputId,
        scale: i32,
        off_x: i32,
        off_y: i32,
        out: &mut Vec<BacakElements>,
    ) {
        crate::render::render_onboarding_dialog(state, renderer, output, scale, off_x, off_y, out);
    }
}
