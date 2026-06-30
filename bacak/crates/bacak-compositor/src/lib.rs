//! bacak-compositor — library surface.
//!
//! Exposes the headless pieces of the compositor so other crates (the CLI,
//! tests, future debugging tools) can drive WM and OSK state without pulling
//! in a Wayland session host.
//!
//! The Smithay-driven runtime lives only in the binary entry point, guarded
//! by the `runtime` feature.

pub mod animation;
pub mod carousel;
pub mod config;
pub mod decoration;
pub mod focus;
pub mod session;
pub mod hotplug;
pub mod input;
pub mod keyboard;
#[cfg(feature = "runtime")]
pub mod emoji;
#[cfg(feature = "runtime")]
pub mod text_input;
pub mod gestures;
pub mod wm;

#[cfg(feature = "runtime")]
pub mod text;

#[cfg(feature = "runtime")]
pub mod icons;

#[cfg(feature = "runtime")]
pub mod blur;

#[cfg(feature = "runtime")]
pub mod signals;

#[cfg(feature = "runtime")]
pub mod launcher;

#[cfg(feature = "runtime")]
pub mod controls;
pub mod bluetooth;

#[cfg(feature = "runtime")]
pub mod state;

#[cfg(feature = "runtime")]
pub mod handlers;

#[cfg(feature = "runtime")]
pub mod xwayland;

#[cfg(feature = "runtime")]
pub mod grab;

#[cfg(feature = "runtime")]
pub mod selection;

#[cfg(feature = "runtime")]
pub mod atspi;

#[cfg(feature = "runtime")]
pub mod screencopy;

#[cfg(feature = "runtime")]
pub mod foreign_toplevel;

#[cfg(feature = "runtime")]
pub mod render;

/// Shell plugins (dock, keyboard, apps-menu, control-center, screenshot…) — the
/// modular UX layer dispatched by the render/frame loops. See [`plugins`].
#[cfg(feature = "runtime")]
pub mod plugins;

#[cfg(feature = "runtime")]
pub mod runtime;

#[cfg(feature = "udev")]
pub mod udev_runtime;

/// Whether `BACAK_DEBUG_POPUP` is set — gates verbose, per-event tracing of the
/// xdg_popup lifecycle (track/unconstrain geometry, menu grab, the `surface_at`
/// popup hit-test, and the render pass). Off by default and cached so the
/// per-click / per-frame hot paths pay one `getenv` for the whole process.
///
/// This is the host-free verification substrate for popups: a live nested or
/// GDM run with `BACAK_DEBUG_POPUP=1` emits the geometry/routing facts that
/// can't be eyeballed from this headless dev shell (see the verification notes).
pub fn popup_debug() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("BACAK_DEBUG_POPUP").is_some())
}

/// Whether `BACAK_DEBUG_CLIP` is set — gates verbose tracing of the
/// clipboard / selection path: the floating-menu Copy/Paste actions
/// (`SELECTION …`) and the Wayland↔XWayland clipboard bridge (`CLIP …`). Off by
/// default and cached. The primary-selection bridge log fires on *every*
/// drag-select, so this stays quiet in daily use and is flipped on only to
/// diagnose a copy/paste regression.
pub fn clip_debug() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("BACAK_DEBUG_CLIP").is_some())
}
