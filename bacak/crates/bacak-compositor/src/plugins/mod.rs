//! Compositor **shell plugins** — the modular UX layer.
//!
//! Each self-contained UI feature (the dock, the on-screen keyboard, the app
//! launcher, the Control Center, screenshots…) is a [`Plugin`]: a uniform
//! `render` / `tick` / lifecycle interface, registered in [`PLUGINS`] and
//! dispatched by the compositor's render + frame loops. This replaces the
//! hard-coded `render_dock(); render_osk(); …` calls with an ordered registry,
//! so features layer by their `z()` and can be added/removed in one place.
//!
//! ## Why compile-time, in-tree plugins
//!
//! A Wayland compositor can't expose a *stable ABI* safely, so dynamic
//! (`dlopen`) plugins would be unsound. These are compile-time trait objects.
//! For now a plugin is a **stateless dispatcher**: the per-feature state still
//! lives on [`BacakState`] (which lets [`PLUGINS`] be a `static` with no borrow
//! conflicts). Migrating each plugin's *state + input handlers* out of the
//! god-object and into its own module is the next phase (see the crate's
//! plugin notes); the trait below is already shaped for it.
//!
//! ## What is a plugin vs. core
//!
//! **Plugins** (shell / UX): dock, keyboard, apps-menu, control-center,
//! screenshot — and, to migrate next, overview ("recents"), text-selection.
//! **Core** (not plugins): the window manager, the GLES render pipeline,
//! seat/input routing + focus, every Wayland protocol handler, XWayland, output
//! hotplug/session and config — the compositor itself.

use std::time::Instant;

use smithay::backend::renderer::gles::GlesRenderer;

use crate::render::BacakElements;
use crate::state::BacakState;
use crate::wm::OutputId;

pub mod apps_menu;
pub mod audio;
pub mod control_center;
pub mod ctx;
pub mod desktop_settings;
pub mod dock;
pub mod gestures;
pub mod keyboard;
pub mod network;
pub mod onboarding;
pub mod overview;
pub mod remote_desktop;
pub mod screenshot;
pub mod selection;
pub mod uzakel;

pub use ctx::PluginCtx;

/// A compositor shell plugin. Implementors are zero-sized dispatchers; their
/// state lives on [`BacakState`].
pub trait Plugin: Sync {
    /// Stable identifier (also used in logs / tests).
    fn id(&self) -> &'static str;

    /// Render-layer priority: higher renders on top. (Input priority is
    /// separate — see [`input_z`](Plugin::input_z) — because the overview, say,
    /// renders top-most yet takes input *after* the keyboard.)
    fn z(&self) -> i32;

    /// Input priority: higher gets first refusal on a press. Defaults to the
    /// render `z()`; override when input order differs from visual order.
    fn input_z(&self) -> i32 {
        self.z()
    }

    /// Whether the plugin is active under the current config.
    fn enabled(&self, _state: &BacakState) -> bool {
        true
    }

    /// Handle a pointer/touch press at global coords through the [`PluginCtx`]
    /// seam. Returns `true` if the plugin consumed it. This is the input
    /// interface plugins migrate their handlers onto (away from raw
    /// `BacakState` methods); dispatched by [`pointer_press`].
    fn on_pointer_press(&self, _ctx: &mut PluginCtx, _gx: f64, _gy: f64) -> bool {
        false
    }

    /// Handle a touch-down at global coords. Like [`on_pointer_press`] but with
    /// the touch `slot`, so a plugin that starts a drag can record it (in its
    /// own `*_touch_slot` field) for the slot's later motion/up. Returns `true`
    /// if consumed.
    fn on_touch_press(&self, _ctx: &mut PluginCtx, _tx: f32, _ty: f32, _slot: i32) -> bool {
        false
    }

    /// Pointer moved (button up). A plugin owning the pointer (an OSK title
    /// drag, hovering the OSK, a selection drag) consumes it → `true`, and the
    /// motion is *not* delivered to a client. (In-flight dock/overview/title
    /// drags that *also* let the client see motion stay in the backends.)
    fn on_pointer_motion(&self, _ctx: &mut PluginCtx, _gx: f64, _gy: f64) -> bool {
        false
    }

    /// Continue a drag while the pointer button is held. Returns `true` if a
    /// plugin owns the drag and consumed it.
    fn on_pointer_release(&self, _ctx: &mut PluginCtx, _gx: f64, _gy: f64) -> bool {
        false
    }

    /// Continue a touch drag (the owning plugin matches `slot`). `true` if
    /// consumed.
    fn on_touch_motion(&self, _ctx: &mut PluginCtx, _tx: f32, _ty: f32, _slot: i32) -> bool {
        false
    }

    /// Finish a touch drag (the owning plugin matches `slot`, settles + clears
    /// its slot). `true` if consumed.
    fn on_touch_up(&self, _ctx: &mut PluginCtx, _slot: i32) -> bool {
        false
    }

    /// React to a recognised multi-finger / long-press gesture (workspace
    /// switch, task overview, …). Returns `true` if handled (the caller then
    /// redraws). Multi-finger recognition itself (the touch aggregator) is
    /// low-level input plumbing and stays in the backends.
    fn on_gesture(&self, _ctx: &mut PluginCtx, _gesture: crate::input::Gesture) -> bool {
        false
    }

    /// Per-frame update. Returns `true` if it changed something that needs a
    /// redraw (e.g. an animation step, a debounced hide firing).
    fn tick(&self, _state: &mut BacakState, _now: Instant) -> bool {
        false
    }

    /// Contribute this plugin's render elements for `output`. Signature matches
    /// the existing `render_*` helpers exactly, so a plugin is a thin wrapper.
    #[allow(clippy::too_many_arguments)]
    fn render(
        &self,
        _state: &BacakState,
        _renderer: &mut GlesRenderer,
        _output: OutputId,
        _scale: i32,
        _off_x: i32,
        _off_y: i32,
        _out: &mut Vec<BacakElements>,
    ) {
    }
}

/// The registered shell plugins. Order here is irrelevant — [`render_overlays`]
/// layers them by `z()` — but it's the one place to add/remove a feature.
pub static PLUGINS: &[&dyn Plugin] = &[
    &selection::SelectionPlugin,
    &overview::OverviewPlugin,
    &onboarding::OnboardingPlugin,
    &screenshot::ScreenshotPlugin,
    &apps_menu::AppsMenuPlugin,
    &control_center::ControlCenterPlugin,
    &network::NetworkPlugin,
    &audio::AudioPlugin,
    &desktop_settings::DesktopSettingsPlugin,
    &uzakel::UzakelPlugin,
    &remote_desktop::RemoteDesktopPlugin,
    &keyboard::KeyboardPlugin,
    &dock::DockPlugin,
    &gestures::GesturePlugin,
];

/// Render every enabled plugin onto `out`, **top-most first** (highest `z()`
/// pushed first — Bacak's element list is top-first). Called from
/// `build_output_frame` in place of the hard-coded overlay block.
#[allow(clippy::too_many_arguments)]
pub fn render_overlays(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output: OutputId,
    scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let mut active: Vec<&&dyn Plugin> = PLUGINS.iter().filter(|p| p.enabled(state)).collect();
    active.sort_by_key(|p| std::cmp::Reverse(p.z()));
    for p in active {
        p.render(state, renderer, output, scale, off_x, off_y, out);
    }
}

/// Dispatch a pointer/touch press through the plugins by **input priority**
/// (highest `input_z()` first), via a [`PluginCtx`]. Returns `true` once a
/// plugin consumes it. This is the registry's input path; the backends' input
/// handlers migrate onto it as each feature's handler moves behind `ctx`. (The
/// non-plugin selection/AT-SPI/title handlers are interleaved in the current
/// chains, so full wiring lands once those are plugins too.)
pub fn pointer_press(state: &mut BacakState, gx: f64, gy: f64) -> Option<&'static str> {
    let mut order: Vec<&&dyn Plugin> = PLUGINS.iter().collect();
    order.sort_by_key(|p| std::cmp::Reverse(p.input_z()));
    for p in order {
        if !p.enabled(state) {
            continue;
        }
        let mut ctx = PluginCtx::new(state);
        if p.on_pointer_press(&mut ctx, gx, gy) {
            return Some(p.id());
        }
    }
    None
}

/// Dispatch a touch-down through the plugins by input priority. Each plugin
/// records its own drag slot in `on_touch_press`; the returned id lets the
/// caller redraw. (Critical orderings — keyboard before selection — are kept
/// via `input_z`; the rest of the overlays are mutually exclusive so their
/// relative order is moot.)
pub fn touch_press(state: &mut BacakState, tx: f32, ty: f32, slot: i32) -> Option<&'static str> {
    let mut order: Vec<&&dyn Plugin> = PLUGINS.iter().collect();
    order.sort_by_key(|p| std::cmp::Reverse(p.input_z()));
    for p in order {
        if !p.enabled(state) {
            continue;
        }
        let mut ctx = PluginCtx::new(state);
        if p.on_touch_press(&mut ctx, tx, ty, slot) {
            return Some(p.id());
        }
    }
    None
}

/// Dispatch pointer motion (button up) — a plugin owning the pointer (OSK
/// drag/hover, selection drag) consumes it. `true` if consumed (don't deliver
/// to a client).
pub fn pointer_motion(state: &mut BacakState, gx: f64, gy: f64) -> bool {
    dispatch(state, |p, ctx| p.on_pointer_motion(ctx, gx, gy))
}

/// Dispatch a pointer-button release through the plugins (the one owning the
/// drag settles + clears its flag). `true` if consumed.
pub fn pointer_release(state: &mut BacakState, gx: f64, gy: f64) -> bool {
    dispatch(state, |p, ctx| p.on_pointer_release(ctx, gx, gy))
}

/// Dispatch a touch-motion (the plugin owning `slot` continues its drag).
pub fn touch_motion(state: &mut BacakState, tx: f32, ty: f32, slot: i32) -> bool {
    dispatch(state, |p, ctx| p.on_touch_motion(ctx, tx, ty, slot))
}

/// Dispatch a touch-up (the plugin owning `slot` settles its drag + clears it).
pub fn touch_up(state: &mut BacakState, slot: i32) -> bool {
    dispatch(state, |p, ctx| p.on_touch_up(ctx, slot))
}

/// Dispatch a recognised gesture to the plugin that handles it. Returns `true`
/// if handled (caller redraws).
pub fn dispatch_gesture(state: &mut BacakState, gesture: crate::input::Gesture) -> bool {
    dispatch(state, |p, ctx| p.on_gesture(ctx, gesture))
}

/// Shared input dispatch: try each enabled plugin by input priority, building a
/// `PluginCtx` per call; stop at the first consumer.
fn dispatch(
    state: &mut BacakState,
    mut f: impl FnMut(&dyn Plugin, &mut PluginCtx) -> bool,
) -> bool {
    let mut order: Vec<&&dyn Plugin> = PLUGINS.iter().collect();
    order.sort_by_key(|p| std::cmp::Reverse(p.input_z()));
    for p in order {
        if !p.enabled(state) {
            continue;
        }
        let mut ctx = PluginCtx::new(state);
        if f(*p, &mut ctx) {
            return true;
        }
    }
    false
}

/// Tick every plugin once per frame. Returns `true` if any reported a change
/// that needs a redraw.
pub fn tick_all(state: &mut BacakState, now: Instant) -> bool {
    let mut dirty = false;
    for p in PLUGINS {
        dirty |= p.tick(state, now);
    }
    dirty
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_ids_unique_and_layered() {
        let mut ids: Vec<&str> = PLUGINS.iter().map(|p| p.id()).collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate plugin id");
        // Keyboard must layer above the dock (the bottom overlap bug fix), and
        // the Overview ("recents") is the top-most overlay (covers everything).
        let z = |id: &str| PLUGINS.iter().find(|p| p.id() == id).unwrap().z();
        assert!(z("keyboard") > z("dock"));
        assert!(z("apps_menu") > z("keyboard"));
        assert!(z("overview") >= z("apps_menu"));
        // Input priority differs from render z: the keyboard gets a press first
        // even though the overview renders on top of it.
        let iz = |id: &str| PLUGINS.iter().find(|p| p.id() == id).unwrap().input_z();
        // Press order: keyboard → selection → screenshot → overview → menus → dock.
        assert!(iz("keyboard") > iz("selection"));
        assert!(iz("selection") > iz("screenshot"));
        assert!(iz("screenshot") > iz("overview"));
        assert!(iz("overview") > iz("dock"));
    }
}
