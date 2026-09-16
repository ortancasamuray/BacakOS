//! bacak-launcher — full-screen application launcher.
//!
//! Toggled from `bacak-panel`. Layer-shell anchored to all four edges with an
//! overlay layer so it sits above running windows. The grid is populated from
//! XDG desktop entries today; later it will pull from a Bacak app registry.
//!
//! Build with `--features gtk` to compile the live surface.

use anyhow::Result;
use bacak_shell::{init_tracing, Surface};

fn main() -> Result<()> {
    init_tracing(Surface::Launcher);

    #[cfg(feature = "gtk")]
    {
        gtk_launcher::run()
    }

    #[cfg(not(feature = "gtk"))]
    {
        tracing::info!("gtk feature disabled — rebuild with `--features gtk` to render");
        Ok(())
    }
}

#[cfg(feature = "gtk")]
mod gtk_launcher {
    use anyhow::Result;
    use gtk4::prelude::*;
    use gtk4::{Application, ApplicationWindow, Label};
    use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

    pub fn run() -> Result<()> {
        let app = Application::builder().application_id("os.bacak.Launcher").build();
        app.connect_activate(build_ui);
        app.run();
        Ok(())
    }

    fn build_ui(app: &Application) {
        let window = ApplicationWindow::builder()
            .application(app)
            .title("Bacak Launcher")
            .child(&Label::new(Some("⌘ Bacak Launcher (skeleton)")))
            .build();

        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.present();
    }
}
