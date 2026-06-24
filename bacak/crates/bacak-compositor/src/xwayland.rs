//! XWayland bridge — promote X11 windows into the Bacak WM.
//!
//! Compiled only with the `runtime` feature. The actual `XwmHandler` /
//! `XWaylandShellHandler` impls that Smithay's `X11Wm` event source dispatches
//! to live on the udev backend's loop data (see `udev_runtime.rs`), because
//! that's the calloop data type `X11Wm::start_wm` is parameterised over. Those
//! impls are thin — they call straight into the [`BacakState`] helpers here.
//!
//! ## How an X11 window becomes a Bacak window
//!
//! 1. Xwayland creates the X11 window and, separately, a `wl_surface` it
//!    associates via the `xwayland_shell_v1` protocol (handled on
//!    [`BacakState`] in `handlers.rs`).
//! 2. On `map_window_request` / `surface_associated` we call
//!    [`BacakState::map_x11_surface`]. Once the `wl_surface` is available we
//!    [`WindowManager::open`] a window and insert the surface into the same
//!    `windows: HashMap<WlSurface, WindowId>` bridge xdg toplevels use — so the
//!    render / focus / Overview / Alt-Tab pipeline treats it identically.
//! 3. Unmap / destroy prune both the bridge and the WM entry.
//!
//! Override-redirect surfaces (menus, tooltips, dropdowns) bypass the WM: they
//! position themselves and must never steal focus or appear in the Overview,
//! so they're tracked separately in [`BacakState::x11_override`] and drawn on
//! top at their own absolute geometry by the render path.

#![cfg(feature = "runtime")]

use std::os::fd::OwnedFd;

use smithay::utils::{Logical, Point, Rectangle, Size};
use smithay::wayland::selection::data_device::{
    clear_data_device_selection, current_data_device_selection_userdata,
    request_data_device_client_selection, set_data_device_selection,
};
use smithay::wayland::selection::primary_selection::{
    clear_primary_selection, current_primary_selection_userdata,
    request_primary_client_selection, set_primary_selection,
};
use smithay::wayland::selection::SelectionTarget;
use smithay::xwayland::xwm::{Reorder, ResizeEdge, XwmId};
use smithay::xwayland::{X11Surface, X11Wm, XwmHandler};

use crate::state::BacakState;
use crate::wm::{Rect, WinState, WindowId};

/// `XwmHandler` lives on [`BacakState`] (the brain). The udev loop data only
/// needs it because `X11Wm::start_wm` is parameterised over the calloop data
/// type — its impl (in `udev_runtime.rs`) forwards straight to these methods.
impl XwmHandler for BacakState {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwm
            .as_mut()
            .expect("xwm_state called before XWayland reported Ready")
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {
        // Created but not yet mapped — nothing to show until map_window_request.
    }

    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        self.map_x11_surface(window);
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.map_x11_override(window);
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.unmap_x11_surface(&window);
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.destroy_x11_surface(&window);
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        self.x11_configure_request(&window, x, y, w, h);
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, Logical>,
        _above: Option<u32>,
    ) {
        self.x11_configure_notify(&window, geometry);
    }

    fn property_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _property: smithay::xwayland::xwm::WmWindowProperty,
    ) {
        self.x11_property_changed(&window);
    }

    fn resize_request(
        &mut self,
        _xwm: XwmId,
        _window: X11Surface,
        _button: u32,
        _edge: ResizeEdge,
    ) {
        // Interactive X11 resize grabs not wired yet (see Overview P-list).
    }

    fn move_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32) {
        // Interactive X11 move grabs not wired yet.
    }

    // --- Selection bridge: X11 → Wayland --------------------------------------
    // The Wayland → X11 direction lives in `SelectionHandler` (handlers.rs).

    /// Gate whether an X client may read the *Wayland*-owned selection. Only
    /// allow it while something holds keyboard focus, so a background X app
    /// can't silently siphon the clipboard.
    fn allow_selection_access(&mut self, _xwm: XwmId, selection: SelectionTarget) -> bool {
        let allowed = self
            .seat
            .get_keyboard()
            .and_then(|kb| kb.current_focus())
            .is_some();
        if crate::clip_debug() {
            tracing::info!(?selection, allowed, "CLIP allow_selection_access (X reads Wayland sel)");
        }
        allowed
    }

    /// An X client took ownership of a selection — publish its mime types as
    /// the compositor-side Wayland selection so Wayland clients can paste it.
    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        use crate::state::SelectionOrigin;
        if crate::clip_debug() {
            tracing::info!(?selection, mimes = ?mime_types, "CLIP X11 took selection → mirroring to Wayland");
        }
        match selection {
            SelectionTarget::Clipboard => set_data_device_selection(
                &self.display_handle,
                &self.seat,
                mime_types,
                SelectionOrigin::X11,
            ),
            SelectionTarget::Primary => set_primary_selection(
                &self.display_handle,
                &self.seat,
                mime_types,
                SelectionOrigin::X11,
            ),
        }
    }

    /// An X client wants to read the *Wayland*-owned selection: stream the
    /// owning Wayland client's data straight into `fd`. No loop handle needed —
    /// the source writes synchronously via the wl_data_source protocol.
    fn send_selection(
        &mut self,
        _xwm: XwmId,
        selection: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
    ) {
        if crate::clip_debug() {
            tracing::info!(?selection, %mime_type, "CLIP X client reading Wayland-owned selection");
        }
        // The two helpers return distinct `SelectionRequestError` types, so log
        // per-arm rather than unifying into one `Result`.
        match selection {
            SelectionTarget::Clipboard => {
                if let Err(err) = request_data_device_client_selection(&self.seat, mime_type, fd) {
                    tracing::warn!(?err, "failed to serve Wayland clipboard to X client");
                }
            }
            SelectionTarget::Primary => {
                if let Err(err) = request_primary_client_selection(&self.seat, mime_type, fd) {
                    tracing::warn!(?err, "failed to serve Wayland primary selection to X client");
                }
            }
        }
    }

    /// An X client dropped its selection. Clear the mirrored Wayland selection,
    /// but only if it's still the compositor-set (X11-originated) one — never
    /// clobber a selection a Wayland client owns. The `userdata` lookup takes a
    /// `RefCell` borrow, so it's dropped (via the `let`) before `clear_*`,
    /// which `borrow_mut`s the same cell.
    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        match selection {
            SelectionTarget::Clipboard => {
                let ours = current_data_device_selection_userdata(&self.seat).is_some();
                if ours {
                    clear_data_device_selection(&self.display_handle, &self.seat);
                }
            }
            SelectionTarget::Primary => {
                let ours = current_primary_selection_userdata(&self.seat).is_some();
                if ours {
                    clear_primary_selection(&self.display_handle, &self.seat);
                }
            }
        }
    }
}

impl BacakState {
    /// The Bacak [`WindowId`] backing an X11 surface, if it's been promoted
    /// into the WM. Matches on the X11 window id so it survives `wl_surface`
    /// churn.
    pub fn x11_window_for(&self, surface: &X11Surface) -> Option<WindowId> {
        let xid = surface.window_id();
        self.x11_windows
            .iter()
            .find_map(|(id, s)| (s.window_id() == xid).then_some(*id))
    }

    /// Promote a normal (non-override-redirect) X11 surface into the WM.
    /// Idempotent, and a no-op until Xwayland has associated a `wl_surface`
    /// (we get called again from `surface_associated` once it has).
    pub fn map_x11_surface(&mut self, surface: X11Surface) {
        if surface.is_override_redirect() {
            return self.map_x11_override(surface);
        }
        // Tell Xwayland to actually map the window; harmless if already mapped.
        let _ = surface.set_mapped(true);

        // Already promoted? Refresh its identity instead of bailing — WM_CLASS
        // and the title commonly arrive a beat after the first map, and this
        // is one of the points we get re-invoked (surface association).
        if let Some(id) = self.x11_window_for(&surface) {
            let (app, title) = x11_identity(&surface);
            let _ = self.wm.set_meta(id, Some(&app), Some(&title));
            return;
        }
        let Some(wl) = surface.wl_surface() else {
            return; // wait for xwayland_shell association
        };

        let (app, title) = x11_identity(&surface);
        let geo = surface.geometry();
        let geom = Rect::new(
            geo.loc.x as f32,
            geo.loc.y as f32,
            geo.size.w.max(1) as f32,
            geo.size.h.max(1) as f32,
        );

        let id = self.wm.open(app, title, geom);
        self.windows.insert(wl, id);
        self.x11_windows.insert(id, surface.clone());
        let _ = surface.set_activated(true);

        // Transient/modal dialog: if this X11 window is `WM_TRANSIENT_FOR`
        // another mapped window, record child→parent so it stays stacked above
        // it (see `raise_child_dialogs`) — otherwise an X11 app's "Save
        // changes?" prompt hides behind its window and the app can't be closed.
        // Mirrors the xdg_toplevel `set_parent` path in `new_toplevel`.
        if let Some(parent_xid) = surface.is_transient_for() {
            if let Some(parent_id) = self
                .x11_windows
                .iter()
                .find_map(|(wid, s)| (s.window_id() == parent_xid).then_some(*wid))
            {
                self.dialog_parent.insert(id, parent_id);
            }
        }
        // X11 has no CSD concept: the WM decorates by default; a window opts
        // out via `_MOTIF_WM_HINTS` (borderless / draws-its-own). Note Smithay's
        // `is_decorated()` is really "borderless requested" (true iff MOTIF
        // decorations == 0), so we decorate when it's *false*. Re-synced on a
        // later MotifHints `property_notify` (the hint can arrive after map).
        if !surface.is_decorated() {
            self.decorated.insert(id);
        }

        // Push the WM's resolved geometry back so Xwayland positions the X11
        // window where we placed it (open() may snap / clamp to the work area).
        if let Ok(win) = self.wm.get(id) {
            let _ = surface.configure(Some(rect_to_x11(win.geom)));
        }

        let s = self.surface_for_window(id);
        self.set_keyboard_focus(s);

        // Advertise the X11 window to foreign-toplevel taskbars (list +
        // management), same as xdg toplevels. Title/app_id refine later via
        // refresh_x11_identity once WM_CLASS arrives.
        if let Ok(w) = self.wm.get(id) {
            let handle = self
                .foreign_toplevel_list
                .new_toplevel::<BacakState>(w.title.clone(), w.app.clone());
            self.foreign_handles.insert(id, handle);
        }
        self.ftl_announce_window(id);
    }

    /// Track an override-redirect surface for drawing. Never enters the WM.
    pub fn map_x11_override(&mut self, surface: X11Surface) {
        let xid = surface.window_id();
        if !self.x11_override.iter().any(|s| s.window_id() == xid) {
            self.x11_override.push(surface);
        }
    }

    /// An X11 window was unmapped (hidden but not destroyed) — drop it from
    /// the WM / override list. A later remap re-promotes it.
    pub fn unmap_x11_surface(&mut self, surface: &X11Surface) {
        if let Some(id) = self.x11_window_for(surface) {
            self.x11_windows.remove(&id);
            self.windows.retain(|_, v| *v != id);
            let _ = self.wm.close(id);
            // Withdraw from foreign-toplevel taskbars (list + management).
            if let Some(handle) = self.foreign_handles.remove(&id) {
                self.foreign_toplevel_list.remove_toplevel(&handle);
            }
            self.ftl_closed(id);
            self.focus_history.forget(id);
            self.decorated.remove(&id);
            self.evict_label(id);
            if self.title_drag.map(|d| d.id) == Some(id) {
                self.title_drag = None;
            }
            // Confirms a pending dismiss (or prunes an externally-closed card).
            self.overview_window_closed(id);
        }
        let xid = surface.window_id();
        self.x11_override.retain(|s| s.window_id() != xid);
    }

    /// Push window `id`'s current WM geometry back to its X11 surface (Xwayland
    /// needs the position for input mapping; unlike xdg clients, X11 windows are
    /// positioned by the WM). Used after a server-side title-bar move. No-op for
    /// non-X11 windows.
    pub fn x11_push_geometry(&self, id: WindowId) {
        if let Some(surface) = self.x11_windows.get(&id) {
            if let Ok(win) = self.wm.get(id) {
                let _ = surface.configure(Some(rect_to_x11(win.geom)));
            }
        }
    }

    /// Apply a maximise state + geometry to an X11 window (the X11 equivalent of
    /// the xdg configure in `toggle_maximize`). No-op for non-X11 windows.
    pub fn x11_apply_maximized(&self, id: WindowId, maximized: bool, geom: Rect) {
        if let Some(surface) = self.x11_windows.get(&id) {
            let _ = surface.set_maximized(maximized);
            let _ = surface.configure(Some(rect_to_x11(geom)));
        }
    }

    /// An X11 window was destroyed — same cleanup as unmap.
    pub fn destroy_x11_surface(&mut self, surface: &X11Surface) {
        self.unmap_x11_surface(surface);
    }

    /// Honour an X11 client's configure request: ack it back to Xwayland and
    /// mirror the new geometry into the WM (floating windows only — the
    /// compositor dictates snapped / maximised geometry).
    pub fn x11_configure_request(
        &mut self,
        surface: &X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
    ) {
        let cur = surface.geometry();
        let nx = x.unwrap_or(cur.loc.x);
        let ny = y.unwrap_or(cur.loc.y);
        let nw = w.map(|v| v as i32).unwrap_or(cur.size.w).max(1);
        let nh = h.map(|v| v as i32).unwrap_or(cur.size.h).max(1);
        let rect = Rectangle::new(Point::from((nx, ny)), Size::from((nw, nh)));
        let _ = surface.configure(Some(rect));
        self.x11_sync_geometry(surface, nx, ny, nw, nh);
    }

    /// An X11 window reported its own new geometry (it moved/resized itself,
    /// common for override-redirect popups). Mirror promoted windows into the
    /// WM; override-redirect ones are read live at draw time so need nothing.
    pub fn x11_configure_notify(&mut self, surface: &X11Surface, geometry: Rectangle<i32, Logical>) {
        self.x11_sync_geometry(
            surface,
            geometry.loc.x,
            geometry.loc.y,
            geometry.size.w.max(1),
            geometry.size.h.max(1),
        );
    }

    /// A property changed (`property_notify`) — refresh the WM title/class and
    /// re-evaluate the decoration request (the MotifHints can land after map).
    pub fn x11_property_changed(&mut self, surface: &X11Surface) {
        if let Some(id) = self.x11_window_for(surface) {
            let (app, title) = x11_identity(surface);
            let _ = self.wm.set_meta(id, Some(&app), Some(&title));
            // `is_decorated()` is "borderless requested" → decorate when false.
            if surface.is_decorated() {
                self.decorated.remove(&id);
            } else {
                self.decorated.insert(id);
            }
        }
    }

    /// Re-read an X11 window's WM_CLASS / title and push it into the WM; a
    /// no-op for non-X11 windows. Driven from the compositor commit handler:
    /// X11 clients routinely set WM_CLASS a few commits *after* the window
    /// first maps and carry no double-buffered title state for the commit
    /// path to read, so this is the reliable catch-all for `app=""`.
    pub fn refresh_x11_identity(&mut self, id: WindowId) {
        let Some((app, title)) = self.x11_windows.get(&id).map(x11_identity) else {
            return;
        };
        let changed = self.wm.set_meta(id, Some(&app), Some(&title)).unwrap_or(false);
        if changed {
            // Push the upgraded WM_CLASS / title to foreign-toplevel taskbars.
            if let Some(handle) = self.foreign_handles.get(&id) {
                handle.send_title(&title);
                handle.send_app_id(&app);
            }
            self.ftl_update_title(id);
        }
    }

    fn x11_sync_geometry(&mut self, surface: &X11Surface, x: i32, y: i32, w: i32, h: i32) {
        let Some(id) = self.x11_window_for(surface) else { return };
        let Ok(win) = self.wm.get(id) else { return };
        if matches!(win.state, WinState::Floating) {
            let _ = self
                .wm
                .r#move(id, Rect::new(x as f32, y as f32, w as f32, h as f32));
        }
    }
}

/// An X11 surface's `(app_id, title)`.
///
/// The app id prefers the WM_CLASS class, then the instance, then — as a last
/// resort — the window title, so that a client which never sets WM_CLASS at
/// all (e.g. `glxgears`, which only calls `XSetStandardProperties`) still gets
/// a stable, non-empty identity for dock grouping and icon resolution instead
/// of collapsing into an empty `app=""` bucket. The title falls back to the
/// app id symmetrically so a window always has *some* label.
///
/// WM_CLASS commonly lands a few commits / a `PropertyNotify` after the first
/// map; [`WindowManager::set_meta`] ignores empty incoming values, so each
/// refresh (map / property_notify / commit) upgrades a title-derived id to the
/// real class once it arrives without ever clobbering a good value with "".
fn x11_identity(surface: &X11Surface) -> (String, String) {
    let class = surface.class();
    let instance = surface.instance();
    let title = surface.title();

    let app = if !class.is_empty() {
        class
    } else if !instance.is_empty() {
        instance
    } else {
        title.clone()
    };
    let title = if title.is_empty() { app.clone() } else { title };
    (app, title)
}

/// Bacak `Rect` (f32, logical) → the integer `Rectangle` Xwayland's
/// `configure` expects.
fn rect_to_x11(r: Rect) -> Rectangle<i32, Logical> {
    Rectangle::new(
        Point::from((r.x as i32, r.y as i32)),
        Size::from((r.w.max(1.0) as i32, r.h.max(1.0) as i32)),
    )
}

#[cfg(test)]
mod tests {
    //! Headless XWayland smoke tests. These need only the `Xwayland` binary
    //! and `XDG_RUNTIME_DIR` — no DRM, no seat, no GPU — so they run anywhere
    //! the dev headers are present. `BacakState` provides the real protocol
    //! handlers (wl_compositor / wl_shm / xwayland_shell / XwmHandler), so the
    //! path under test is the production one, not a mock.
    use std::time::{Duration, Instant};

    use smithay::reexports::calloop::EventLoop;
    use smithay::reexports::wayland_server::Display;
    use smithay::xwayland::{XWayland, XWaylandEvent, X11Wm};

    use crate::state::BacakState;

    fn xwayland_present() -> bool {
        std::process::Command::new("Xwayland")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Pump the wayland display + calloop until `done` or the deadline.
    fn pump(
        ev: &mut EventLoop<'static, BacakState>,
        display: &mut Display<BacakState>,
        state: &mut BacakState,
        deadline: Duration,
        mut done: impl FnMut(&BacakState) -> bool,
    ) {
        let start = Instant::now();
        while !done(state) && start.elapsed() < deadline {
            ev.dispatch(Some(Duration::from_millis(50)), state).unwrap();
            display.dispatch_clients(state).unwrap();
            display.flush_clients().unwrap();
        }
    }

    /// Spawn Xwayland against a real `Display<BacakState>`, attach an `X11Wm`
    /// on `Ready`, and pump until it's live. Returns the still-running loop /
    /// display / state so a test can go on to drive X11 clients. `DISPLAY` is
    /// set to the spawned server. Asserts the WM attaches.
    fn xwayland_session() -> (
        EventLoop<'static, BacakState>,
        Display<BacakState>,
        BacakState,
    ) {
        let mut display: Display<BacakState> = Display::new().unwrap();
        let mut state = BacakState::new(&display, "xwm-test-seat");
        let mut ev: EventLoop<'static, BacakState> = EventLoop::try_new().unwrap();

        let (xwayland, client) = XWayland::spawn(
            &display.handle(),
            None,
            std::iter::empty::<(String, String)>(),
            true,
            std::process::Stdio::null(),
            std::process::Stdio::null(),
            |_| {},
        )
        .expect("XWayland::spawn");

        let wm_handle = ev.handle();
        ev.handle()
            .insert_source(xwayland, move |event, _, st: &mut BacakState| {
                if let XWaylandEvent::Ready { x11_socket, display_number } = event {
                    st.xwm = Some(
                        X11Wm::start_wm(wm_handle.clone(), x11_socket, client.clone())
                            .expect("start_wm"),
                    );
                    std::env::set_var("DISPLAY", format!(":{display_number}"));
                }
            })
            .unwrap();

        pump(&mut ev, &mut display, &mut state, Duration::from_secs(15), |s| {
            s.xwm.is_some()
        });
        assert!(
            state.xwm.is_some(),
            "Xwayland never reported Ready / X11Wm did not attach"
        );
        (ev, display, state)
    }

    /// Spawn Xwayland and confirm an `X11Wm` attaches. Deterministic.
    #[test]
    fn xwayland_spawns_and_attaches_wm() {
        if !xwayland_present() {
            eprintln!("skipping: no Xwayland binary");
            return;
        }
        let (_ev, _display, state) = xwayland_session();
        assert!(state.xwm.is_some());
    }

    /// End-to-end: spawn Xwayland, then launch a real X11 client (`glxgears`)
    /// and confirm it maps into the Bacak WM with a non-empty identity.
    /// glxgears never sets WM_CLASS (only WM_NAME), so this exercises the
    /// `x11_identity` title fallback. Ignored by default — GLX availability is
    /// environment-dependent. Run with:
    ///   cargo test -p bacak-compositor --features udev -- --ignored --nocapture
    #[test]
    #[ignore]
    fn x11_client_maps_into_wm() {
        if !xwayland_present() {
            eprintln!("skipping: no Xwayland binary");
            return;
        }
        let (mut ev, mut display, mut state) = xwayland_session();
        let display_var = std::env::var("DISPLAY").expect("DISPLAY set on Ready");

        let mut child = std::process::Command::new("glxgears")
            .env("DISPLAY", &display_var)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn glxgears");

        pump(&mut ev, &mut display, &mut state, Duration::from_secs(10), |s| {
            s.wm
                .all_windows()
                .first()
                .map(|w| !w.app.is_empty())
                .unwrap_or(false)
        });

        let windows = state.wm.all_windows();
        eprintln!("WM tracked {} window(s) after launching glxgears:", windows.len());
        for w in &windows {
            eprintln!("  app={:?} title={:?} geom={}x{}", w.app, w.title, w.geom.w, w.geom.h);
        }
        let _ = child.kill();
        assert!(!windows.is_empty(), "glxgears did not map into the Bacak WM");
        assert!(
            windows.iter().all(|w| !w.app.is_empty()),
            "an X11 window has no usable app identity (app=\"\") — hardening failed"
        );
    }

    /// Rigorous identity test: connect to our own Xwayland as a plain X11
    /// client, create a window that *does* set `WM_CLASS`, and confirm the
    /// class — not the title fallback — lands in the Bacak WM's `app` field.
    /// This covers the capture path glxgears can't (it never sets WM_CLASS).
    /// Ignored by default (drives a real X11 connection). Run with:
    ///   cargo test -p bacak-compositor --features udev -- --ignored --nocapture
    #[test]
    #[ignore]
    fn x11_wm_class_is_captured() {
        use smithay::reexports::x11rb::connection::Connection;
        use smithay::reexports::x11rb::protocol::xproto::{
            AtomEnum, ConnectionExt as _, CreateWindowAux, PropMode, WindowClass,
        };
        use smithay::reexports::x11rb::rust_connection::RustConnection;
        use smithay::reexports::x11rb::wrapper::ConnectionExt as _;

        if !xwayland_present() {
            eprintln!("skipping: no Xwayland binary");
            return;
        }
        let (mut ev, mut display, mut state) = xwayland_session();
        let display_var = std::env::var("DISPLAY").expect("DISPLAY set on Ready");

        let (conn, screen_num) = match RustConnection::connect(Some(&display_var)) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skipping: cannot connect to Xwayland ({e})");
                return;
            }
        };
        let screen = conn.setup().roots[screen_num].clone();
        let win = conn.generate_id().unwrap();
        conn.create_window(
            screen.root_depth,
            win,
            screen.root,
            0,
            0,
            400,
            300,
            0,
            WindowClass::INPUT_OUTPUT,
            screen.root_visual,
            &CreateWindowAux::new(),
        )
        .unwrap();
        // ICCCM WM_CLASS is "instance\0class\0"; Smithay maps the 2nd field to
        // `class()`, which `x11_identity` prefers for the app id.
        conn.change_property8(
            PropMode::REPLACE,
            win,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            b"bacaktest\0BacakTestClient\0",
        )
        .unwrap();
        conn.map_window(win).unwrap();
        conn.flush().unwrap();

        pump(&mut ev, &mut display, &mut state, Duration::from_secs(10), |s| {
            s.wm.all_windows().iter().any(|w| w.app == "BacakTestClient")
        });

        let windows = state.wm.all_windows();
        eprintln!("WM tracked {} window(s):", windows.len());
        for w in &windows {
            eprintln!("  app={:?} title={:?}", w.app, w.title);
        }
        assert!(
            windows.iter().any(|w| w.app == "BacakTestClient"),
            "WM_CLASS class was not captured into the WM app id"
        );
        // The window set no `_MOTIF_WM_HINTS`, so the WM decorates it
        // server-side (X11 default). Verifies the X11 SSD marking path.
        let id = windows
            .iter()
            .find(|w| w.app == "BacakTestClient")
            .map(|w| w.id);
        assert!(
            id.is_some_and(|id| state.decorated.contains(&id)),
            "X11 window with no MOTIF opt-out should be server-side decorated"
        );
    }
}
