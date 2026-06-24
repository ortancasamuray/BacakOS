//! Bacak OS — Shell surfaces.
//!
//! Three GTK4 binaries layer on top of the compositor via wlr-layer-shell:
//!
//! * `bacak-panel`    — top bar (clock, status, indicators).
//! * `bacak-dock`     — bottom dock (running apps, pinned launchers).
//! * `bacak-launcher` — full-screen app launcher.
//!
//! Each binary is small and self-contained so the compositor can spawn and
//! restart them independently. Shared types and helpers live here.

use serde::{Deserialize, Serialize};

/// Identifies which surface a binary represents — useful for logging and
/// the future cross-binary IPC layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Surface {
    Panel,
    Dock,
    Launcher,
}

impl Surface {
    pub fn name(self) -> &'static str {
        match self {
            Surface::Panel => "bacak-panel",
            Surface::Dock => "bacak-dock",
            Surface::Launcher => "bacak-launcher",
        }
    }
}

/// Helper invoked from each bin's `main` to keep startup logging uniform.
pub fn init_tracing(surface: Surface) {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    tracing::info!(
        "{} v{} starting",
        surface.name(),
        env!("CARGO_PKG_VERSION"),
    );
}
