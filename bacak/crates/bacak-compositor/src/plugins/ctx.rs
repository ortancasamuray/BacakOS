//! `PluginCtx` — the **seam** between a plugin and the compositor.
//!
//! ## Why a facade, not a decoupling
//!
//! Smithay drives input and every Wayland protocol through the *handler data*
//! type — for Bacak that's [`BacakState`]. Anything a plugin does that touches
//! input (synthesise a key, move focus) or a protocol therefore *requires*
//! `&mut BacakState`; a plugin can't escape it. So `PluginCtx` is a thin
//! wrapper over `&mut BacakState` that exposes a **curated, plugin-facing API**
//! (the operations plugins legitimately need) instead of the ~80-field
//! god-object. Plugin handlers take `&mut PluginCtx`, so they're coded against
//! this stable surface — and as each operation gets a proper `ctx` method, the
//! corresponding logic can move out of `BacakState` into the plugin module.
//!
//! For not-yet-curated bits there's [`PluginCtx::state`], a raw escape hatch, so
//! migration is incremental rather than big-bang.

use crate::state::BacakState;
use crate::wm::{OutputId, WindowManager};

/// Curated handle to the compositor passed to plugin input handlers.
pub struct PluginCtx<'a> {
    st: &'a mut BacakState,
}

impl<'a> PluginCtx<'a> {
    pub fn new(st: &'a mut BacakState) -> Self {
        Self { st }
    }

    // --- queries ----------------------------------------------------------

    /// Monotonic milliseconds since compositor start (gesture / double-tap
    /// timing).
    pub fn now_ms(&self) -> u64 {
        self.st.start_time.elapsed().as_millis() as u64
    }

    /// The window manager (Arc-backed — cheap to clone, interior-mutable).
    pub fn wm(&self) -> &WindowManager {
        &self.st.wm
    }

    /// True while a full-screen / modal overlay (overview, menus, screenshot
    /// dialog) is open — other plugins should stand down.
    pub fn overlay_open(&self) -> bool {
        self.st.osk_blocked()
    }

    /// The output the on-screen keyboard is bound to, or the primary.
    pub fn osk_output(&self) -> Option<OutputId> {
        self.st.osk.bound_output().or_else(|| self.st.wm.primary_output())
    }

    // --- actions ----------------------------------------------------------

    /// Inject an evdev modifier+key chord into the focused client (a key, or a
    /// chord like Ctrl+C) through the seat keyboard — reaches every client.
    pub fn synthesize_chord(&mut self, mods_evdev: &[u32], key_evdev: u32) {
        self.st.synthesize_chord(mods_evdev, key_evdev);
    }

    /// Open / toggle the Recent-Apps overview on `out`.
    pub fn toggle_overview(&mut self, out: OutputId) {
        self.st.toggle_overview(out);
    }

    /// Hide the on-screen keyboard.
    pub fn hide_osk(&mut self) {
        self.st.osk_hide();
    }

    // --- escape hatch -----------------------------------------------------

    /// Raw `&mut BacakState` for operations not yet promoted to a `ctx` method.
    /// Each use is a TODO to curate; lets the plugin migration be incremental.
    pub fn state(&mut self) -> &mut BacakState {
        self.st
    }
}
