//! Keyboard plugin — the on-screen keyboard (OSK). Dispatches to the OSK state
//! (`osk_*` on [`BacakState`], the [`OskController`](crate::input::OskController)
//! engine and `crate::keyboard`) and `render_osk`.
use std::time::Instant;

use smithay::backend::renderer::gles::GlesRenderer;

use super::{Plugin, PluginCtx};
use crate::render::BacakElements;
use crate::state::BacakState;
use crate::wm::OutputId;

pub struct KeyboardPlugin;

impl Plugin for KeyboardPlugin {
    fn id(&self) -> &'static str {
        "keyboard"
    }

    fn z(&self) -> i32 {
        // Above the dock (bottom overlap), below modal menus.
        40
    }

    fn input_z(&self) -> i32 {
        // The keyboard gets first refusal on a press (topmost chrome at the
        // bottom edge), even though its render layer sits below the menus.
        100
    }

    fn on_pointer_press(&self, ctx: &mut PluginCtx, gx: f64, gy: f64) -> bool {
        ctx.state().osk_press_global(gx, gy)
    }

    fn on_touch_press(&self, ctx: &mut PluginCtx, tx: f32, ty: f32, slot: i32) -> bool {
        let st = ctx.state();
        if st.osk_press_global(tx as f64, ty as f64) {
            // Track the slot so the finger's motion (drag) / up (release) route
            // back to the OSK.
            st.osk_touch_slot = Some(slot);
            true
        } else {
            false
        }
    }

    fn on_pointer_motion(&self, ctx: &mut PluginCtx, gx: f64, gy: f64) -> bool {
        let st = ctx.state();
        if st.osk_pointer_down {
            // A title-strip drag owns motion.
            st.osk_motion_global(gx, gy);
            true
        } else if st.osk_hit_test(gx, gy) {
            // Hovering the OSK overlay → capture the pointer (clear focus behind).
            st.osk_hover_capture(gx, gy);
            true
        } else {
            false
        }
    }

    fn on_pointer_release(&self, ctx: &mut PluginCtx, _gx: f64, _gy: f64) -> bool {
        let st = ctx.state();
        if st.osk_pointer_down {
            st.osk_release_global();
            st.osk_pointer_down = false;
            true
        } else {
            false
        }
    }

    fn on_touch_motion(&self, ctx: &mut PluginCtx, tx: f32, ty: f32, slot: i32) -> bool {
        let st = ctx.state();
        if st.osk_touch_slot == Some(slot) {
            st.osk_motion_global(tx as f64, ty as f64);
            true
        } else {
            false
        }
    }

    fn on_touch_up(&self, ctx: &mut PluginCtx, slot: i32) -> bool {
        let st = ctx.state();
        if st.osk_touch_slot == Some(slot) {
            st.osk_release_global();
            st.osk_touch_slot = None;
            true
        } else {
            false
        }
    }

    fn tick(&self, state: &mut BacakState, _now: Instant) -> bool {
        // Fire the debounced auto-hide when its deadline passes.
        state.osk_tick_hide()
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
        crate::render::render_osk(state, renderer, output, scale, off_x, off_y, out);
    }
}
