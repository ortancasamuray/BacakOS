//! Uzakel pairing plugin — a Control Center tile + QR panel that turns the
//! `uzakel-daemon`'s current pairing PIN into a scannable code, so pairing
//! the Android app doesn't require typing a 6-digit PIN and an IP address
//! by hand. Enabled when `/usr/share/uzakel/plugins/uzakel.plugin` is
//! installed (provided by the `uzakel-daemon` package — see
//! `../../../../uzakel/daemon`), matching every other optional Control
//! Center section's manifest-gating convention.
use smithay::backend::renderer::gles::GlesRenderer;

use super::{Plugin, PluginCtx};
use crate::render::BacakElements;
use crate::state::BacakState;
use crate::wm::OutputId;

pub const MANIFEST: &str = "/usr/share/bacak/plugins/uzakel.plugin";

pub struct UzakelPlugin;

impl Plugin for UzakelPlugin {
    fn id(&self) -> &'static str {
        "uzakel"
    }

    fn z(&self) -> i32 {
        62
    }

    fn input_z(&self) -> i32 {
        74
    }

    fn enabled(&self, _state: &BacakState) -> bool {
        std::path::Path::new(MANIFEST).exists()
    }

    fn on_pointer_press(&self, ctx: &mut PluginCtx, gx: f64, gy: f64) -> bool {
        ctx.state().uzakel_panel_press(gx as f32, gy as f32)
    }

    fn on_touch_press(&self, ctx: &mut PluginCtx, tx: f32, ty: f32, _slot: i32) -> bool {
        ctx.state().uzakel_panel_press(tx, ty)
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
        crate::render::render_uzakel_panel(state, renderer, output, scale, off_x, off_y, out);
    }
}
