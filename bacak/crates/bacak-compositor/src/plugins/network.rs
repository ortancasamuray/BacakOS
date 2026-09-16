//! Network settings plugin — WiFi, Bluetooth, Ethernet panels in the
//! Control Center. Enabled when `/usr/share/bacak/plugins/network.plugin`
//! is installed (provided by the `bacak-plugin-network` package).
use smithay::backend::renderer::gles::GlesRenderer;

use super::{Plugin, PluginCtx};
use crate::render::BacakElements;
use crate::state::BacakState;
use crate::wm::OutputId;

const MANIFEST: &str = "/usr/share/bacak/plugins/network.plugin";

pub struct NetworkPlugin;

impl Plugin for NetworkPlugin {
    fn id(&self) -> &'static str {
        "network"
    }

    fn z(&self) -> i32 {
        61
    }

    fn input_z(&self) -> i32 {
        73
    }

    fn enabled(&self, _state: &BacakState) -> bool {
        std::path::Path::new(MANIFEST).exists()
    }

    fn on_pointer_press(&self, ctx: &mut PluginCtx, gx: f64, gy: f64) -> bool {
        let s = ctx.state();
        s.wifi_panel_press(gx as f32, gy as f32) || s.bt_panel_press(gx as f32, gy as f32)
    }

    fn on_touch_press(&self, ctx: &mut PluginCtx, tx: f32, ty: f32, _slot: i32) -> bool {
        let s = ctx.state();
        s.wifi_panel_press(tx, ty) || s.bt_panel_press(tx, ty)
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
        crate::render::render_wifi_panel(state, renderer, output, scale, off_x, off_y, out);
        crate::render::render_bt_panel(state, renderer, output, scale, off_x, off_y, out);
    }
}
