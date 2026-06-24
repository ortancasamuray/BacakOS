//! bacak-dock — bottom application dock.
//!
//! Layer-shell anchored to the bottom edge. Hosts pinned launchers and live
//! running-window indicators sourced from the compositor's WM state.
//!
//! Build with `--features gtk` to compile the live surface.

use anyhow::Result;
use bacak_shell::{init_tracing, Surface};

fn main() -> Result<()> {
    init_tracing(Surface::Dock);

    #[cfg(feature = "gtk")]
    {
        gtk_dock::run()
    }

    #[cfg(not(feature = "gtk"))]
    {
        tracing::info!("gtk feature disabled — rebuild with `--features gtk` to render");
        Ok(())
    }
}

#[cfg(feature = "gtk")]
mod gtk_dock {
    use anyhow::Result;
    use gtk4::prelude::*;
    use gtk4::{Application, ApplicationWindow, Box, Label, Orientation};
    use gtk4_layer_shell::{Edge, Layer, LayerShell};

    pub fn run() -> Result<()> {
        let app = Application::builder().application_id("os.bacak.Dock").build();
        app.connect_activate(build_ui);
        app.run();
        Ok(())
    }

    fn build_ui(app: &Application) {
        let content = Box::new(Orientation::Horizontal, 12);
        content.append(&Label::new(Some("⌂")));
        content.append(&Label::new(Some("✉")));
        content.append(&Label::new(Some("⚙")));

        let window = ApplicationWindow::builder()
            .application(app)
            .default_height(64)
            .title("Bacak Dock")
            .child(&content)
            .build();

        window.init_layer_shell();
        window.set_layer(Layer::Top);
        window.set_anchor(Edge::Bottom, true);
        window.set_margin(Edge::Bottom, 16);
        window.present();
    }
}
