//! bacak-panel — top status bar.
//!
//! Layer-shell anchored to the top edge. Displays clock, wifi/bt/audio
//! indicators sourced from `bacak-services::device`, and an app menu trigger.
//!
//! Build with `--features gtk` to compile the live surface.

use anyhow::Result;
use bacak_shell::{init_tracing, Surface};

fn main() -> Result<()> {
    init_tracing(Surface::Panel);

    #[cfg(feature = "gtk")]
    {
        gtk_panel::run()
    }

    #[cfg(not(feature = "gtk"))]
    {
        tracing::info!("gtk feature disabled — rebuild with `--features gtk` to render");
        Ok(())
    }
}

#[cfg(feature = "gtk")]
mod gtk_panel {
    use anyhow::Result;
    use gtk4::prelude::*;
    use gtk4::{Application, ApplicationWindow, Label};
    use gtk4_layer_shell::{Edge, Layer, LayerShell};

    pub fn run() -> Result<()> {
        let app = Application::builder().application_id("os.bacak.Panel").build();
        app.connect_activate(build_ui);
        app.run();
        Ok(())
    }

    fn build_ui(app: &Application) {
        let window = ApplicationWindow::builder()
            .application(app)
            .default_height(32)
            .title("Bacak Panel")
            .child(&Label::new(Some("Bacak OS")))
            .build();

        window.init_layer_shell();
        window.set_layer(Layer::Top);
        window.set_anchor(Edge::Top, true);
        window.set_anchor(Edge::Left, true);
        window.set_anchor(Edge::Right, true);
        window.auto_exclusive_zone_enable();
        window.present();
    }
}
