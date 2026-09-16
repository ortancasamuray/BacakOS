//! Audio settings plugin — output volume, microphone volume and device
//! selection panels in the Control Center. Enabled when
//! `/usr/share/bacak/plugins/audio.plugin` is installed
//! (provided by the `bacak-plugin-audio` package).
use smithay::backend::renderer::gles::GlesRenderer;

use super::{Plugin, PluginCtx};
use crate::render::BacakElements;
use crate::state::BacakState;
use crate::wm::OutputId;

const MANIFEST: &str = "/usr/share/bacak/plugins/audio.plugin";

pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn id(&self) -> &'static str {
        "audio"
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
        let s = ctx.state();
        s.audio_panel_press(gx as f32, gy as f32) || s.mic_panel_press(gx as f32, gy as f32)
    }

    fn on_touch_press(&self, ctx: &mut PluginCtx, tx: f32, ty: f32, _slot: i32) -> bool {
        let s = ctx.state();
        s.audio_panel_press(tx, ty) || s.mic_panel_press(tx, ty)
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
        crate::render::render_audio_panel(state, renderer, output, scale, off_x, off_y, out);
        crate::render::render_mic_panel(state, renderer, output, scale, off_x, off_y, out);
    }
}
