//! Live compositor runtime: winit backend + GlesRenderer + wayland socket.
//!
//! Today this module fuses three milestones in one place because they share a
//! single event loop:
//!
//! * **Buffer & Render path** — every committed surface is uploaded through
//!   [`on_commit_buffer_handler`] (see [`crate::handlers`]) and drawn on the
//!   next frame as a [`WaylandSurfaceRenderElement`] against a [`GlesRenderer`].
//! * **Event loop & socket** — a [`ListeningSocket`] is exported as
//!   `WAYLAND_DISPLAY=wayland-bacak-0` and pumped via
//!   [`Display::dispatch_clients`] every frame; frame callbacks fire from
//!   [`send_frames_to_surface_tree`] so animations don't stall.
//! * **Input forwarding** — winit keyboard / pointer / touch input is forwarded
//!   into the compositor seat. Pointer motion drives a hit-test against the
//!   [`crate::wm::WindowManager`] (not the raw surface tree), and the
//!   keyboard follows the pointer **only** when the configured
//!   [`FocusPolicy`] is [`FocusPolicy::FocusFollowsPointer`]. Touch events
//!   feed the [`TouchAggregator`] so multi-finger gestures get classified.
//!
//! Compiled only with the `runtime` feature.

#![cfg(feature = "runtime")]

use std::time::{Duration, Instant};

use anyhow::Result;
use calloop::{EventLoop, LoopHandle};
use smithay::backend::input::{
    AbsolutePositionEvent, ButtonState, Event as _, InputEvent, KeyState, KeyboardKeyEvent,
    PointerButtonEvent, TouchEvent,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Color32F, Frame, Renderer};
use smithay::backend::winit::{self as winit_backend, WinitEvent};
use smithay::input::keyboard::{FilterResult, KeyboardHandle};
use smithay::output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel};
use smithay::input::pointer::{ButtonEvent, MotionEvent, PointerHandle};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Display, DisplayHandle, ListeningSocket};
use smithay::wayland::selection::SelectionTarget;
use smithay::xwayland::{XWayland, XWaylandEvent, X11Wm};
use std::os::fd::OwnedFd;
use smithay::utils::{Rectangle, Serial, SERIAL_COUNTER, Transform};
use smithay::wayland::compositor::{
    with_surface_tree_downward, SurfaceAttributes, TraversalAction,
};
use tracing::{info, warn};
use winit::platform::pump_events::PumpStatus;

use crate::render::{build_styled_elements, BacakElements};
use crate::state::BacakState;
use crate::wm::FocusPolicy;

/// Socket name exported via `$WAYLAND_DISPLAY`. Picked deliberately so it
/// won't collide with stock `wayland-0` on systems that already host a
/// session compositor.
const SOCKET: &str = "wayland-bacak-0";

/// Aegean deep — matches the regreet palette so the empty desktop looks
/// intentional before any client paints.
const CLEAR_COLOR: Color32F = Color32F::new(0.024, 0.165, 0.239, 1.0);

/// `BTN_LEFT` from `linux/input-event-codes.h` — the primary click
/// that activates a dock tile.
const BTN_LEFT: u32 = 0x110;
/// `BTN_RIGHT` from `linux/input-event-codes.h` — opens the dock's
/// per-tile context menu.
const BTN_RIGHT: u32 = 0x111;
/// `BTN_MIDDLE` — middle-click closes the hovered Overview card.
const BTN_MIDDLE: u32 = 0x112;

/// Bring up the full live runtime: open the socket, attach the winit backend,
/// spin the event loop until the winit window asks to exit.
pub fn run() -> Result<()> {
    let mut display: Display<BacakState> = Display::new()?;
    let mut state = BacakState::new(&display, "bacak-seat0");

    // Bind our own Wayland server socket. If the canonical name is already
    // taken — e.g. a real udev bacak is running and this is a *nested* winit
    // dev instance — fall back to the next free `wayland-bacak-N` so we can
    // still come up instead of dying with "socket name already in use".
    let listener = match ListeningSocket::bind(SOCKET) {
        Ok(s) => s,
        Err(e) => {
            info!(?e, "{SOCKET} unavailable; picking the next free wayland-bacak-N");
            ListeningSocket::bind_auto("wayland-bacak", 1..64)
                .map_err(|e| anyhow::anyhow!("ListeningSocket::bind_auto: {e}"))?
        }
    };
    let socket_name = listener
        .socket_name()
        .and_then(|s| s.to_str())
        .unwrap_or(SOCKET)
        .to_string();
    info!(socket = %socket_name, "wayland listening socket bound");
    // NB: we deliberately do *not* export WAYLAND_DISPLAY yet — winit::init
    // below connects to the *host* compositor using the WAYLAND_DISPLAY we
    // inherited. Overwriting it first would point a nested winit at its own
    // (empty) socket and hang on the init roundtrip. We set it after init.

    // Keyboard / repeat (rate, delay) in ms.
    let keyboard = state
        .seat
        .add_keyboard(Default::default(), 200, 25)
        .map_err(|e| anyhow::anyhow!("seat.add_keyboard: {e}"))?;
    // Pointer — needed so wl_pointer protocol is advertised on the seat and
    // hit-testing has somewhere to send focus enter/leave.
    let pointer = state.seat.add_pointer();

    let (mut backend, mut winit_loop) = winit_backend::init::<GlesRenderer>()
        .map_err(|e| anyhow::anyhow!("winit backend init failed: {e}"))?;

    // Now that winit has connected to the host, export *our* socket so child
    // clients we launch attach to this (nested) compositor, not the host.
    std::env::set_var("WAYLAND_DISPLAY", &socket_name);
    info!(socket = %socket_name, "exported WAYLAND_DISPLAY for child clients");
    // Hand off to the session manager's startup client ($BACAK_STARTUP), e.g.
    // the BDM greeter, now that the (nested) Wayland socket is live.
    crate::launcher::spawn_startup();

    // Advertise zwp_linux_dmabuf_v1 with the winit renderer's import formats
    // (parity with udev — lets GPU clients submit dma-buf instead of rendering
    // black, and makes the global verifiable in a nested session).
    {
        use smithay::backend::renderer::ImportDma;
        let formats = backend.renderer().dmabuf_formats();
        let n = formats.iter().count();
        let global = state
            .dmabuf_state
            .create_global::<BacakState>(&display.handle(), formats);
        state.dmabuf_global = Some(global);
        info!(formats = n, "zwp_linux_dmabuf_v1 advertised (winit)");
    }

    // Advertise a `wl_output` global (parity with the udev backend). Real
    // toolkits — Chromium's Ozone-Wayland, Firefox, GTK/Qt — abort or crash
    // without at least one output. The mode tracks the actual winit window
    // size (winit runs at scale 1, so physical == logical), and the WM's
    // primary output is resized to match so clients lay out in the same space
    // that's actually visible. `output` is held for the whole `run` and its
    // mode is updated on `Resized`.
    let win0 = backend.window_size();
    let (ow, oh) = (win0.w.max(1), win0.h.max(1));
    let primary_output = state.wm.primary_output();
    if let Some(p) = primary_output {
        let _ = state.wm.set_output_bounds(p, crate::wm::Rect::new(0.0, 0.0, ow as f32, oh as f32));
        state.apply_dock_strut();
    }
    let output = Output::new(
        "winit".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Bacak".into(),
            model: "winit".into(),
        },
    );
    let mode0 = OutputMode { size: (ow, oh).into(), refresh: 60_000 };
    output.set_preferred(mode0);
    output.change_current_state(Some(mode0), Some(Transform::Normal), None, Some((0, 0).into()));
    output.create_global::<BacakState>(&display.handle());
    // Register so layer-shell / render / input can reach this output's LayerMap.
    if let Some(p) = primary_output {
        state.register_output(p, output.clone());
    }

    let start_time = state.start_time;
    let mut clients = Vec::new();

    // Live config reload (same watcher the udev backend uses). winit
    // redraws every iteration, so no dirty flag is needed — just swap
    // the values in `state` and the next frame reflects them.
    let config_watch = crate::config::ConfigWatcher::start();
    crate::signals::install();
    crate::launcher::init();

    // Calloop loop hosting the XWayland source + X11 window manager. Unlike the
    // udev backend (whose calloop data is `LoopData`), here `BacakState` is the
    // loop data directly — it already implements `XwmHandler` +
    // `XWaylandShellHandler`, so `X11Wm::start_wm::<BacakState>` needs no
    // wrapper. Pumped non-blocking once per winit frame.
    let mut xwm_loop: EventLoop<'static, BacakState> =
        EventLoop::try_new().map_err(|e| anyhow::anyhow!("xwm EventLoop::try_new: {e}"))?;
    setup_xwayland(&xwm_loop.handle(), &display.handle());
    // Drive X11→Wayland selection transfers on the xwm loop (see X11SelectionSink).
    state.x11_selection_sink = Some(Box::new(WinitX11Sink(xwm_loop.handle())));
    // Tier C accessibility bridge — opt-in (flips the global AT handshake on).
    if std::env::var_os("BACAK_ATSPI").is_some() {
        state.atspi = crate::atspi::AtspiBridge::start();
    }

    info!("entering compositor main loop (winit + wayland)");
    loop {
        // ----- 0. clean-shutdown flush ---------------------------------------
        if crate::signals::shutdown_requested() {
            state.persist_session_now();
            info!("shutdown signal received; exiting");
            return Ok(());
        }

        // ----- 1. drain winit events -----------------------------------------
        let win_size = backend.window_size();
        let status = winit_loop.dispatch_new_events(|event| match event {
            WinitEvent::Resized { size, .. } => {
                // Track the window: resize the WM's primary output and update
                // the advertised wl_output mode so clients re-lay-out to the
                // new size (winit is scale 1 → physical == logical).
                let (w, h) = (size.w.max(1), size.h.max(1));
                if let Some(p) = primary_output {
                    let _ = state
                        .wm
                        .set_output_bounds(p, crate::wm::Rect::new(0.0, 0.0, w as f32, h as f32));
                    state.apply_dock_strut();
                }
                output.change_current_state(
                    Some(OutputMode { size: (w, h).into(), refresh: 60_000 }),
                    None,
                    None,
                    None,
                );
            }
            WinitEvent::Input(ev) => {
                forward_input(&mut state, &keyboard, &pointer, win_size, ev)
            }
            _ => {}
        });

        match status {
            PumpStatus::Continue => {}
            PumpStatus::Exit(_) => {
                info!("winit asked to exit");
                return Ok(());
            }
        }

        // ----- 1b. live config reload --------------------------------------
        if config_watch
            .as_ref()
            .map(|w| w.poll_changed())
            .unwrap_or(false)
        {
            let c = crate::config::CompositorConfig::load();
            tracing::info!(blur = c.blur, "compositor config reloaded");
            state.blur_enabled = c.blur_enabled();
            state.config = c;
            // Dock toggle / height may have changed — re-reserve the
            // bottom strut so snap math tracks the new bar.
            state.apply_dock_strut();
        }

        // ----- 1c. pump XWayland + X11 window manager ----------------------
        // Drives the XWayland `Ready` handshake and, once attached, the X11
        // protocol (map / configure / property events → the Bacak WM). Non-
        // blocking so it never stalls the winit render cadence.
        xwm_loop
            .dispatch(Some(Duration::ZERO), &mut state)
            .map_err(|e| anyhow::anyhow!("xwm dispatch: {e}"))?;

        // ----- 2. tick window animations -----------------------------------
        // This is where post-drag spring physics drives the WM geometry —
        // call it every iteration so the natural ~60 Hz winit cadence gives
        // us a fluid animation without a separate timer.
        let now = Instant::now();
        // Live long-press poll (fires mid-hold). winit redraws every frame, so
        // no explicit redraw request is needed.
        let now_ms = now.saturating_duration_since(state.start_time).as_millis() as u64;
        if let Some(crate::gestures::SelectionGesture::LongPress { x, y }) =
            state.selection_recognizer.tick(now_ms)
        {
            // Altay kendi uzun basış / dosya seçme mantığını yönetir;
            // compositor menüsü gösterilmez.
            // Dokunuş noktasındaki yüzeyin uygulamasını bul
            // (klavye odağına bağlı değil — dokunuşta odak set edilmemiş olabilir)
            let touched_app = state
                .surface_at(x as f64, y as f64)
                .and_then(|(surface, _)| state.window_for(&surface))
                .and_then(|id| state.wm.get(id).ok())
                .map(|w| w.app.clone())
                .unwrap_or_default();
            let touched_is_altay = touched_app.to_ascii_lowercase().contains("altay");
            if !touched_is_altay {
                // Tier A native panel → Tier C AT-SPI foreign selection → Tier B menu.
                if state.text_panel_long_press(x, y) {
                    // handled by the native panel
                } else if state.atspi_select_word_at(x, y) {
                    // handled by AT-SPI over a foreign app
                } else {
                    state.open_selection_menu(x, y);
                }
            }
        }
        // Resync the Tier C overlay when the focused app's selection changed.
        if state.atspi.as_ref().is_some_and(|b| b.take_selection_dirty()) {
            state.atspi_sync_selection();
        }
        // Flush a coalesced Tier C drag point once its debounce elapses.
        state.atspi_drag_tick();
        // Tick the shell plugins (OSK debounced hide, delayed screenshot, …).
        crate::plugins::tick_all(&mut state, now);
        let _ = std::mem::take(&mut state.osk_dirty);
        state.tick_animations(now);
        state.tick_dock_reveal(now);
        state.maybe_persist_session(now);

        // ----- 3. render the frame ------------------------------------------
        let size = backend.window_size();
        let damage: Rectangle<i32, smithay::utils::Physical> = Rectangle::from_size(size);
        {
            let (renderer, mut framebuffer) = backend
                .bind()
                .map_err(|e| anyhow::anyhow!("winit backend bind: {e}"))?;

            // Resolve queued dma-buf imports against the live renderer (reject
            // un-sampleable buffers so clients renegotiate instead of rendering
            // black). Mirrors the udev tick.
            crate::render::process_pending_dmabuf(&mut state, renderer);
            crate::render::process_pending_screencopy(&mut state, renderer);
            crate::render::process_pending_screenshot(&mut state, renderer);

            // Z-ordered, alpha-differentiated, shadow + snap-preview list.
            let elements: Vec<BacakElements> =
                build_styled_elements(&state, renderer, 1);

            let mut frame = renderer
                .render(&mut framebuffer, size, Transform::Flipped180)
                .map_err(|e| anyhow::anyhow!("renderer.render: {e}"))?;
            frame
                .clear(CLEAR_COLOR, &[damage])
                .map_err(|e| anyhow::anyhow!("frame.clear: {e}"))?;
            draw_render_elements(&mut frame, 1.0, &elements, &[damage])
                .map_err(|e| anyhow::anyhow!("draw_render_elements: {e}"))?;
            let _ = frame
                .finish()
                .map_err(|e| anyhow::anyhow!("frame.finish: {e}"))?;

            // ----- 3. frame callbacks ---------------------------------------
            let elapsed_ms = start_time.elapsed().as_millis() as u32;
            for surface in state.xdg_shell_state.toplevel_surfaces() {
                let root = surface.wl_surface();
                send_frames_to_surface_tree(root, elapsed_ms);
                // Popups aren't subsurfaces; send their frame callbacks too or
                // menus/dropdowns stall after first paint (see udev backend).
                for (popup, _) in smithay::desktop::PopupManager::popups_for_surface(root) {
                    send_frames_to_surface_tree(popup.wl_surface(), elapsed_ms);
                }
            }
            // Layer surfaces (panels, OSK, …) + their popups throttle on frames.
            for output in state.outputs.values() {
                for layer in smithay::desktop::layer_map_for_output(output).layers() {
                    let ls = layer.wl_surface();
                    send_frames_to_surface_tree(ls, elapsed_ms);
                    for (popup, _) in smithay::desktop::PopupManager::popups_for_surface(ls) {
                        send_frames_to_surface_tree(popup.wl_surface(), elapsed_ms);
                    }
                }
            }
            // IME candidate popups.
            for popup in &state.ime_popups {
                send_frames_to_surface_tree(popup.wl_surface(), elapsed_ms);
            }
            // XWayland surfaces aren't xdg toplevels, so the loop above skips
            // them — send their frame callbacks explicitly or X11 clients that
            // throttle on wl_frame stall after their first frame.
            for x11 in state.x11_windows.values() {
                if let Some(s) = x11.wl_surface() {
                    send_frames_to_surface_tree(&s, elapsed_ms);
                }
            }
            for x11 in &state.x11_override {
                if let Some(s) = x11.wl_surface() {
                    send_frames_to_surface_tree(&s, elapsed_ms);
                }
            }

            // ----- 4. accept any pending clients ----------------------------
            loop {
                match listener.accept() {
                    Ok(Some(stream)) => {
                        info!("wayland client connected");
                        let client = display
                            .handle()
                            .insert_client(stream, BacakState::new_client_state())
                            .map_err(|e| anyhow::anyhow!("insert_client: {e}"))?;
                        clients.push(client);
                    }
                    Ok(None) => break,
                    Err(e) => {
                        warn!(?e, "listener.accept failed");
                        break;
                    }
                }
            }

            // ----- 5. dispatch + flush wayland events -----------------------
            display.dispatch_clients(&mut state)?;
            display.flush_clients()?;

            // ----- 5b. refresh Overview window snapshots --------------------
            // Parity with the udev backend. Safe here: the window frame is
            // already `finish`ed, so the offscreen captures only touch texture
            // FBOs and `backend.submit` below still presents the window
            // surface. Throttled + self-pruning inside; one-frame-stale
            // snapshots are fine.
            crate::render::capture_window_snapshots(&state, renderer);
        }

        // ----- 6. submit frame ---------------------------------------------
        backend
            .submit(Some(&[damage]))
            .map_err(|e| anyhow::anyhow!("backend.submit: {e}"))?;
    }
}

/// Spawn an Xwayland server and, on `Ready`, attach an [`X11Wm`] so X11
/// clients map as ordinary Bacak windows in the winit (nested) backend too.
/// `BacakState` is the calloop loop data, so `start_wm` parameterises over it
/// directly. Best-effort: a missing `Xwayland` binary is logged and the
/// session continues Wayland-only.
fn setup_xwayland(handle: &LoopHandle<'static, BacakState>, dh: &DisplayHandle) {
    let (xwayland, client) = match XWayland::spawn(
        dh,
        None,
        std::iter::empty::<(String, String)>(),
        true,
        std::process::Stdio::null(),
        std::process::Stdio::null(),
        |_| {},
    ) {
        Ok(v) => v,
        Err(e) => {
            warn!("XWayland::spawn failed ({e}); continuing Wayland-only");
            return;
        }
    };

    let wm_handle = handle.clone();
    let res = handle.insert_source(xwayland, move |event, _, state: &mut BacakState| match event {
        XWaylandEvent::Ready { x11_socket, display_number } => {
            match X11Wm::start_wm(wm_handle.clone(), x11_socket, client.clone()) {
                Ok(wm) => {
                    state.xwm = Some(wm);
                    std::env::set_var("DISPLAY", format!(":{display_number}"));
                    info!(display = display_number, "XWayland ready (winit); X11 WM attached");
                }
                Err(e) => warn!("failed to attach X11 window manager: {e}"),
            }
        }
        XWaylandEvent::Error => warn!("XWayland failed to start"),
    });
    if let Err(e) = res {
        warn!("failed to insert XWayland source into the event loop: {e}");
    }
}

/// winit backend's [`crate::state::X11SelectionSink`]. Here the xwm loop's data
/// type *is* `BacakState`, so the handle is `LoopHandle<'static, BacakState>`.
struct WinitX11Sink(LoopHandle<'static, BacakState>);

impl crate::state::X11SelectionSink for WinitX11Sink {
    fn send(&self, xwm: &mut X11Wm, selection: SelectionTarget, mime_type: String, fd: OwnedFd) {
        if let Err(err) = xwm.send_selection::<BacakState>(selection, mime_type, fd, self.0.clone()) {
            warn!(?err, "failed to stream X11 selection to Wayland client");
        }
    }
}

/// Forward a single winit `InputEvent` into the Bacak state. The flow is:
///
/// 1. **Keyboard** — forwarded straight to the seat keyboard.
/// 2. **Pointer motion** — converted to logical coords, used to hit-test
///    the WM, and the result drives the pointer's focus (and the
///    keyboard's, when [`FocusPolicy::FocusFollowsPointer`] is on).
/// 3. **Pointer button** — relayed to the seat pointer. On press, if the
///    policy is [`FocusPolicy::ClickToFocus`], the surface under the cursor
///    is promoted to keyboard focus.
/// 4. **Touch** — routed through [`crate::input::TouchAggregator`]; the
///    emitted gesture is logged for now (the WM gesture dispatcher hooks
///    into the same struct via [`BacakState::touch_aggregator`]).
fn forward_input(
    state: &mut BacakState,
    keyboard: &KeyboardHandle<BacakState>,
    pointer: &PointerHandle<BacakState>,
    win_size: smithay::utils::Size<i32, smithay::utils::Physical>,
    ev: InputEvent<winit_backend::WinitInput>,
) {
    match ev {
        InputEvent::Keyboard { event } => {
            let serial = SERIAL_COUNTER.next_serial();
            let key_state = event.state();
            keyboard.input::<(), _>(
                state,
                event.key_code(),
                key_state,
                serial,
                event.time_msec(),
                |state, mods, keysym| {
                    // Bare Super tap → toggle the dock (parity with udev). Armed
                    // on a lone Super press, disarmed by any chord key, fired on
                    // Super release (forwarded, not intercepted — no stuck mod).
                    {
                        const KEY_SUPER_L: u32 = 0xffeb;
                        const KEY_SUPER_R: u32 = 0xffec;
                        let this = keysym.raw_latin_sym_or_raw_current_sym().map(|s| s.raw());
                        let is_super = matches!(this, Some(KEY_SUPER_L) | Some(KEY_SUPER_R));
                        match (key_state, is_super) {
                            (KeyState::Pressed, true) => state.super_tap_armed = true,
                            (KeyState::Pressed, false) => state.super_tap_armed = false,
                            (KeyState::Released, true) => {
                                if state.super_tap_armed {
                                    state.toggle_dock();
                                }
                                state.super_tap_armed = false;
                            }
                            (KeyState::Released, false) => {}
                        }
                    }
                    // Key-press only; releases and everything not handled below
                    // falls through to the focused client. Parity with udev.
                    if key_state != KeyState::Pressed {
                        return FilterResult::Forward;
                    }
                    let raw = keysym.raw_latin_sym_or_raw_current_sym().map(|s| s.raw());
                    // Escape dismisses the selection menu / text panel.
                    if raw == Some(0xff1b)
                        && (state.floating_menu.is_some()
                            || state.text_panel.is_some()
                            || state.atspi_selection.is_some())
                    {
                        state.close_selection_menu();
                        state.close_text_panel();
                        state.clear_atspi_selection();
                        return FilterResult::Intercept(());
                    }
                    // Super+T → toggle the Tier A native text panel.
                    if mods.logo && !mods.ctrl && !mods.alt && raw == Some(0x74) {
                        if let Some(out) = state.wm.primary_output() {
                            state.toggle_text_panel(out);
                        }
                        return FilterResult::Intercept(());
                    }
                    // Recent-Apps Overview nav.
                    if state.overview.is_some() {
                        match raw {
                            Some(0xff51) => {
                                state.overview_wheel(-1); // Left
                                return FilterResult::Intercept(());
                            }
                            Some(0xff53) => {
                                state.overview_wheel(1); // Right
                                return FilterResult::Intercept(());
                            }
                            Some(0xff09) | Some(0xfe20) => {
                                state.overview_wheel(if mods.shift { -1 } else { 1 }); // Tab
                                return FilterResult::Intercept(());
                            }
                            Some(0xff0d) | Some(0xff8d) => {
                                state.overview_activate_selected();
                                return FilterResult::Intercept(());
                            }
                            Some(0xff1b) => {
                                state.close_overview();
                                return FilterResult::Intercept(());
                            }
                            _ => return FilterResult::Forward,
                        }
                    }
                    // Applications menu search (plain keys only).
                    if state.apps_menu.is_some() && !mods.ctrl && !mods.logo && !mods.alt {
                        match raw {
                            Some(0xff1b) => {
                                state.close_apps_menu();
                                return FilterResult::Intercept(());
                            }
                            Some(0xff08) => {
                                state.apps_menu_backspace();
                                return FilterResult::Intercept(());
                            }
                            Some(0xff0d) | Some(0xff8d) => {
                                state.apps_menu_enter();
                                return FilterResult::Intercept(());
                            }
                            _ => {
                                let utf = smithay::input::keyboard::xkb::keysym_to_utf8(
                                    keysym.modified_sym(),
                                );
                                if let Some(c) = utf.chars().next() {
                                    if !c.is_control() {
                                        state.apps_menu_type(c);
                                        return FilterResult::Intercept(());
                                    }
                                }
                            }
                        }
                    }
                    // Ctrl+Alt+Left/Right → previous / next workspace.
                    if mods.ctrl && mods.alt && !mods.logo {
                        let delta = match raw {
                            Some(0xff51) => Some(-1),
                            Some(0xff53) => Some(1),
                            _ => None,
                        };
                        if let Some(delta) = delta {
                            if let Some(out) = state.wm.primary_output() {
                                let _ = state.switch_workspace_relative(out, delta);
                            }
                            return FilterResult::Intercept(());
                        }
                    }
                    // Wi-Fi picker text entry (password / static IP) from a
                    // physical keyboard.
                    if state.wifi_text_active() && !mods.ctrl && !mods.logo && !mods.alt {
                        if let Some(raw) = raw {
                            let utf = smithay::input::keyboard::xkb::keysym_to_utf8(
                                keysym.modified_sym(),
                            );
                            if state.wifi_physical_key(raw, &utf) {
                                return FilterResult::Intercept(());
                            }
                        }
                    }
                    // Bluetooth PIN / passkey entry from a physical keyboard.
                    if state.bt_pin_active() && !mods.ctrl && !mods.logo && !mods.alt {
                        if let Some(raw) = raw {
                            let utf = smithay::input::keyboard::xkb::keysym_to_utf8(
                                keysym.modified_sym(),
                            );
                            if state.bt_pin_key(raw, &utf) {
                                return FilterResult::Intercept(());
                            }
                        }
                    }
                    FilterResult::Forward
                },
            );
        }

        InputEvent::PointerMotionAbsolute { event } => {
            let pos = event.position_transformed(win_size.to_logical(1));
            state.pointer_position = (pos.x, pos.y);
            // Plugins owning the pointer (OSK title-strip drag / OSK hover-
            // capture / selection drag) consume motion. See `crate::plugins`.
            if crate::plugins::pointer_motion(state, pos.x, pos.y) {
                return;
            }
            // Update any in-flight dock / overview drag — cheap no-op otherwise.
            state.dock_pointer_motion(pos.x as f32, pos.y as f32);
            state.overview_pointer_motion(pos.x as f32, pos.y as f32);
            state.title_pointer_motion(pos.x as f32, pos.y as f32);

            // Hit-test the WM, not the raw surface tree: the WM is the
            // single source of truth for geometry / z-order, and skipping
            // it would let minimized or off-workspace surfaces pull focus.
            // Real surface origin (not (0,0)) so the client gets correct
            // surface-local coordinates and its widgets respond to clicks.
            let focus_pair = state.surface_under_pointer_with_loc();
            let focus_target: Option<WlSurface> = focus_pair.as_ref().map(|(s, _)| s.clone());
            let serial = SERIAL_COUNTER.next_serial();
            let time = event.time_msec();

            pointer.motion(
                state,
                focus_pair,
                &MotionEvent { location: pos, serial, time },
            );
            pointer.frame(state);

            if state.focus_policy == FocusPolicy::FocusFollowsPointer {
                // set_focus is idempotent — Smithay dedupes leave/enter
                // events when the target is unchanged, so calling it on
                // every motion is cheap.
                // suppress_focus_raise: pointer drift must not bury a
                // newly-opened window (e.g. bacak-belge opened from Altay).
                state.suppress_focus_raise = true;
                keyboard.set_focus(state, focus_target, serial);
                state.suppress_focus_raise = false;
            }
        }

        InputEvent::PointerButton { event } => {
            let serial = SERIAL_COUNTER.next_serial();
            let button = event.button_code();
            let bstate = event.state();

            // Compositor dock owns left-clicks that land on a tile.
            // Press registers (and may start a pinned-tile drag);
            // release commits the drag (or fires the deferred click).
            // Either way the event is swallowed so it never reaches a
            // client or the click-to-focus fallback.
            let (px, py) =
                (state.pointer_position.0 as f32, state.pointer_position.1 as f32);
            // Right-press opens (or dismisses) the dock context menu;
            // left-press first consults an open menu, then falls
            // through to the normal press/drag/release flow.
            // Right-click → dock menu on a tile, else our text action menu
            // (Copy/Paste/…) at the pointer. Both edges swallowed.
            if button == BTN_RIGHT {
                if bstate == ButtonState::Pressed && !state.dock_right_press(px, py) {
                    // Only (re)focus if there's a window under the cursor — never
                    // clear focus, or synthesised Copy/Paste keys go nowhere.
                    if let Some(target) = state.surface_under_pointer() {
                        keyboard.set_focus(state, Some(target), serial);
                    }
                    state.open_text_context_menu(px, py);
                }
                return;
            }
            // Middle-click closes the hovered Overview card (parity with udev).
            if button == BTN_MIDDLE
                && bstate == ButtonState::Pressed
                && state.overview_middle_click(px, py)
            {
                return;
            }
            if button == BTN_LEFT {
                match bstate {
                    ButtonState::Pressed => {
                        // Shell plugins get first refusal (keyboard → selection →
                        // screenshot → overview → menus → dock), by input
                        // priority. See `crate::plugins`.
                        match crate::plugins::pointer_press(state, px as f64, py as f64) {
                            Some("keyboard") => {
                                state.osk_pointer_down = true;
                                return;
                            }
                            Some(_) => return,
                            None => {}
                        }
                        // Core: window-decoration title-bar move (not a plugin).
                        if state.title_press(px, py) {
                            return;
                        }
                    }
                    ButtonState::Released => {
                        // A plugin owning the drag (OSK strip, selection,
                        // overview, dock) settles it.
                        if crate::plugins::pointer_release(state, px as f64, py as f64) {
                            return;
                        }
                        // Core: window-decoration title-bar move release.
                        if state.title_release() {
                            return;
                        }
                    }
                }
            }

            pointer.button(
                state,
                &ButtonEvent {
                    button,
                    state: bstate,
                    serial,
                    time: event.time_msec(),
                },
            );
            pointer.frame(state);

            if bstate == ButtonState::Pressed
                && state.focus_policy == FocusPolicy::ClickToFocus
            {
                let target = state.surface_under_pointer();
                keyboard.set_focus(state, target, serial);
            }
        }

        InputEvent::TouchDown { event } => {
            let pos = event.position_transformed(win_size.to_logical(1));
            let slot = touch_slot_id(event.slot());
            let (tx, ty) = (pos.x as f32, pos.y as f32);
            // Shell plugins get first refusal on the touch, dispatched by input
            // priority (keyboard → selection → screenshot → menus → overview →
            // dock); each records its own drag slot. See `crate::plugins`.
            if crate::plugins::touch_press(state, tx, ty, slot).is_some() {
                return;
            }
            state.touch_aggregator.down(
                slot,
                tx,
                ty,
                event.time_msec() as u64,
            );
            // Single-finger selection recogniser (long-press → action menu),
            // on the start-time clock to match the per-frame tick poll.
            if state.touch_aggregator.fingers() == 1 {
                let now_ms = state.start_time.elapsed().as_millis() as u64;
                state.selection_recognizer.down(tx, ty, now_ms);
            } else {
                state.selection_recognizer.cancel();
            }
        }

        InputEvent::TouchMotion { event } => {
            let pos = event.position_transformed(win_size.to_logical(1));
            let slot = touch_slot_id(event.slot());
            // A plugin owning a drag continues it (see `crate::plugins`).
            if crate::plugins::touch_motion(state, pos.x as f32, pos.y as f32, slot) {
                return;
            }
            state
                .touch_aggregator
                .motion(slot, pos.x as f32, pos.y as f32);
            state.selection_recognizer.motion(pos.x as f32, pos.y as f32);
        }

        InputEvent::TouchUp { event } => {
            let slot = touch_slot_id(event.slot());
            // A plugin owning the drag finishes it + clears its slot.
            if crate::plugins::touch_up(state, slot) {
                return;
            }
            // Lifted finger is no longer a long-press candidate (mirrors the
            // udev path — without this the tick would fire a stale long-press).
            state.selection_recognizer.cancel();
            if let Some(gesture) = state
                .touch_aggregator
                .up(slot, event.time_msec() as u64)
            {
                // Route through the gestures plugin (winit redraws each frame,
                // so no explicit redraw needed). See `crate::plugins`.
                crate::plugins::dispatch_gesture(state, gesture);
            }
        }

        InputEvent::PointerAxis { event } => {
            // Wheel drives the carousel while the Overview is open (parity with
            // the udev backend); otherwise left for the client-scroll path.
            use smithay::backend::input::{Axis, PointerAxisEvent};
            if state.overview.is_some() {
                let v120 = event
                    .amount_v120(Axis::Vertical)
                    .or_else(|| event.amount_v120(Axis::Horizontal));
                let cont = event
                    .amount(Axis::Vertical)
                    .or_else(|| event.amount(Axis::Horizontal));
                let delta = crate::state::axis_notches(v120, cont);
                if delta != 0 {
                    state.overview_wheel(delta);
                }
            } else if state.apps_menu.is_some() {
                // Apps menu open → the wheel scrolls its grid.
                let v120 = event
                    .amount_v120(Axis::Vertical)
                    .or_else(|| event.amount_v120(Axis::Horizontal));
                let cont = event
                    .amount(Axis::Vertical)
                    .or_else(|| event.amount(Axis::Horizontal));
                let delta = crate::state::axis_notches(v120, cont);
                if delta != 0 {
                    state.apps_menu_scroll(delta);
                }
            } else {
                // Overview closed → forward the scroll to the focused client.
                let h = (
                    event.amount(Axis::Horizontal),
                    event.amount_v120(Axis::Horizontal),
                    event.relative_direction(Axis::Horizontal),
                );
                let v = (
                    event.amount(Axis::Vertical),
                    event.amount_v120(Axis::Vertical),
                    event.relative_direction(Axis::Vertical),
                );
                crate::state::forward_axis(
                    state,
                    pointer,
                    event.time_msec(),
                    event.source(),
                    h,
                    v,
                );
            }
        }

        InputEvent::TouchCancel { .. } => {
            if state.dock_touch_slot.is_some() {
                state.dock_touch_cancel();
            }
            if state.overview_touch_slot.is_some() {
                state.overview_touch_slot = None;
                state.overview_drag = None;
            }
            state.touch_aggregator.cancel();
            state.selection_recognizer.cancel();
        }

        _ => {}
    }
}

/// Smithay's `TouchSlot` is opaque; the aggregator wants a plain integer
/// key so it can live in a `HashMap`. The slot number is per-device and
/// stable for the duration of a gesture, which is exactly what we need.
/// Smithay provides `From<TouchSlot> for i32` (an absent slot maps to -1).
fn touch_slot_id(slot: smithay::backend::input::TouchSlot) -> i32 {
    slot.into()
}

/// Walk every surface in `surface`'s tree and fire any pending frame
/// callbacks. Without this, clients drawing on `wl_callback` (most of them)
/// stop after their first commit.
pub fn send_frames_to_surface_tree(surface: &WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_surf, states, &()| {
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
}

// Re-export `Serial` so callers in sibling modules can compute serials
// against the same counter without re-importing the smithay path.
#[allow(dead_code)]
pub(crate) fn next_serial() -> Serial {
    SERIAL_COUNTER.next_serial()
}
