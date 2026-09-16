//! "Uzak Masaüstü" plugin — thin dispatcher over `crate::remote_desktop`
//! (state + network + rendering all live there; see its module doc for why).
use smithay::backend::renderer::gles::GlesRenderer;

use super::{Plugin, PluginCtx};
use crate::render::BacakElements;
use crate::state::BacakState;
use crate::wm::OutputId;

pub struct RemoteDesktopPlugin;

impl Plugin for RemoteDesktopPlugin {
    fn id(&self) -> &'static str {
        "remote_desktop"
    }

    fn z(&self) -> i32 {
        63 // just above the Uzakel QR panel — both are modal-ish overlays
    }

    fn input_z(&self) -> i32 {
        75
    }

    fn enabled(&self, state: &BacakState) -> bool {
        state.remote_desktop_panel.is_some()
    }

    fn on_pointer_press(&self, ctx: &mut PluginCtx, gx: f64, gy: f64) -> bool {
        ctx.state().remote_desktop_press(gx as f32, gy as f32)
    }

    fn on_pointer_motion(&self, ctx: &mut PluginCtx, gx: f64, gy: f64) -> bool {
        ctx.state().remote_desktop_pointer_motion(gx as f32, gy as f32)
    }

    fn on_pointer_release(&self, ctx: &mut PluginCtx, gx: f64, gy: f64) -> bool {
        ctx.state().remote_desktop_pointer_release(gx as f32, gy as f32)
    }

    fn on_touch_press(&self, ctx: &mut PluginCtx, tx: f32, ty: f32, slot: i32) -> bool {
        ctx.state().remote_desktop_touch_press(tx, ty, slot)
    }

    fn on_touch_motion(&self, ctx: &mut PluginCtx, tx: f32, ty: f32, slot: i32) -> bool {
        let st = ctx.state();
        if st.remote_desktop_touch_slot() == Some(slot) {
            st.remote_desktop_touch_motion(tx, ty)
        } else {
            false
        }
    }

    fn on_touch_up(&self, ctx: &mut PluginCtx, slot: i32) -> bool {
        let st = ctx.state();
        if st.remote_desktop_touch_slot() == Some(slot) {
            st.remote_desktop_touch_up()
        } else {
            false
        }
    }

    fn tick(&self, state: &mut BacakState, _now: std::time::Instant) -> bool {
        state.remote_desktop_tick()
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
        crate::remote_desktop::render_remote_desktop_panel(state, renderer, output, scale, off_x, off_y, out);
    }
}
