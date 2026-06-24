//! bacak-compositor — Wayland compositor entry point for Bacak OS.
//!
//! # Build profiles
//!
//! The binary supports three increasingly heavy modes, selected at compile
//! time via cargo features and dispatched at run time via `$BACAK_BACKEND`:
//!
//! | feature flags                  | backend                       | use case             |
//! |--------------------------------|-------------------------------|----------------------|
//! | *(none)*                       | skeleton only (no socket)     | fast `cargo check`   |
//! | `--features runtime`           | winit (nested Wayland window) | dev on a host WM     |
//! | `--features udev`              | libseat + DRM/KMS native      | real Bacak session   |
//!
//! ```bash
//! # Skeleton (fast, no display server)
//! cargo run -p bacak-compositor
//!
//! # Nested live compositor (winit). Default when `runtime` is on.
//! cargo run -p bacak-compositor --features runtime
//!
//! # Native session host (libseat + DRM/KMS + libinput).
//! cargo run -p bacak-compositor --features udev
//! BACAK_BACKEND=udev cargo run -p bacak-compositor --features udev
//!
//! # Force the winit dev backend even when the udev feature is compiled in:
//! BACAK_BACKEND=winit cargo run -p bacak-compositor --features udev
//! ```

use anyhow::Result;
use bacak_compositor::{input, wm};
use tracing::info;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    info!("bacak-compositor v{} starting", env!("CARGO_PKG_VERSION"));

    // Bring up the headless state — this is the same state the runtime will
    // manipulate once the Smithay event loop is wired in.
    let monitor = wm::Monitor { work_area: wm::Rect::new(0.0, 0.0, 1920.0, 1080.0) };
    let wm = wm::WindowManager::new(monitor);
    let osk = input::OskController::new(input::OskConfig::default());

    info!("WM initialized: workspace={}, snap edge={}px", wm.active_workspace(), wm::SNAP_EDGE_PX);
    info!("OSK initialized: state={:?}", osk.state());

    #[cfg(any(feature = "runtime", feature = "udev"))]
    {
        let backend = select_backend();
        info!(?backend, "handing off to live runtime");
        run_backend(backend)
    }

    #[cfg(not(any(feature = "runtime", feature = "udev")))]
    {
        let _ = (wm, osk);
        info!(
            "runtime feature disabled — rebuild with `--features runtime` (winit dev) \
             or `--features udev` (native DRM session). Exiting cleanly."
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Backend selection
// ---------------------------------------------------------------------------

/// Which live event loop should drive this process.
#[cfg(any(feature = "runtime", feature = "udev"))]
#[derive(Debug, Clone, Copy)]
enum Backend {
    /// Smithay's winit backend — nested in a host Wayland/X11 session,
    /// using a window for both display and input.
    Winit,
    /// Native DRM/KMS session via libseat + libudev + libinput. Required
    /// for a "Bacak from boot" experience; needs the `udev` cargo feature.
    Udev,
}

/// Choose a backend based on `$BACAK_BACKEND` first, then a sensible default
/// for the compiled feature set. Unknown values fall back to the default and
/// are logged.
#[cfg(any(feature = "runtime", feature = "udev"))]
fn select_backend() -> Backend {
    match std::env::var("BACAK_BACKEND").ok().as_deref() {
        Some("udev") => Backend::Udev,
        Some("winit") => Backend::Winit,
        Some(other) => {
            tracing::warn!(value = other, "unknown BACAK_BACKEND; falling back to default");
            default_backend()
        }
        None => default_backend(),
    }
}

/// Default when the env var isn't set: prefer the native DRM session when
/// the `udev` feature was compiled in, otherwise the winit dev backend.
#[cfg(feature = "udev")]
fn default_backend() -> Backend {
    Backend::Udev
}

#[cfg(all(feature = "runtime", not(feature = "udev")))]
fn default_backend() -> Backend {
    Backend::Winit
}

/// Dispatch into the selected backend's `run()`. The match arms are gated by
/// feature flags so an unselected backend doesn't drag its dependencies into
/// the link.
#[cfg(any(feature = "runtime", feature = "udev"))]
fn run_backend(backend: Backend) -> Result<()> {
    match backend {
        #[cfg(feature = "runtime")]
        Backend::Winit => bacak_compositor::runtime::run(),

        #[cfg(not(feature = "runtime"))]
        Backend::Winit => Err(anyhow::anyhow!(
            "winit backend requested but `runtime` cargo feature is disabled"
        )),

        #[cfg(feature = "udev")]
        Backend::Udev => bacak_compositor::udev_runtime::run(),

        #[cfg(not(feature = "udev"))]
        Backend::Udev => Err(anyhow::anyhow!(
            "udev backend requested but `udev` cargo feature is disabled"
        )),
    }
}
