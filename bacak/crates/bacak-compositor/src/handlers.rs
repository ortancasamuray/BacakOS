//! Wayland protocol handlers — wire [`BacakState`] into Smithay's delegates.
//!
//! Each impl tells Smithay "here's where the protocol's state lives" and, for
//! lifecycle hooks like `new_toplevel`, mirrors the change into the Bacak WM.
//!
//! Compiled only with the `runtime` feature.

#![cfg(feature = "runtime")]

use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::desktop::utils::bbox_from_surface_tree;
use smithay::desktop::{
    find_popup_root_surface, get_popup_toplevel_coords, PopupKeyboardGrab, PopupKind,
    PopupPointerGrab, PopupUngrabStrategy,
};
use smithay::input::pointer::{CursorImageStatus, Focus, GrabStartData};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Client;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::Serial;
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    with_states, CompositorClientState, CompositorHandler, CompositorState,
};
#[cfg(feature = "udev")]
use smithay::wayland::compositor::{add_blocker, add_pre_commit_hook};
#[cfg(feature = "udev")]
use smithay::wayland::drm_syncobj::{DrmSyncobjCachedState, DrmSyncobjHandler, DrmSyncobjState};
#[cfg(feature = "udev")]
use smithay::backend::renderer::sync::Fence;
use smithay::wayland::shell::xdg::decoration::XdgDecorationHandler;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface, XdgShellHandler,
    XdgShellState, XdgToplevelSurfaceData,
};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecoMode;
use smithay::wayland::output::OutputHandler;
use smithay::wayland::selection::data_device::{
    set_data_device_focus, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState,
    ServerDndGrabHandler,
};
use smithay::wayland::selection::primary_selection::{
    set_primary_focus, PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::wayland::selection::{SelectionSource, SelectionTarget};
use std::os::fd::OwnedFd;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};
use smithay::wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState};
use smithay::xwayland::{xwm::XwmId, X11Surface, XWaylandClientData};
use smithay::{
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_fractional_scale,
    delegate_output, delegate_pointer_gestures, delegate_primary_selection, delegate_seat,
    delegate_shm, delegate_viewporter, delegate_xdg_activation, delegate_xdg_decoration,
    delegate_xdg_shell, delegate_xwayland_shell,
};

use crate::grab::{MoveGrab, ResizeGrab};
use crate::state::{BacakState, ClientState};
use crate::wm::{Rect, SnapZone, WinState};

// ---------------------------------------------------------------------------
// wl_compositor
// ---------------------------------------------------------------------------

impl CompositorHandler for BacakState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        // Xwayland connects as a wayland client too, but Smithay inserts it
        // with `XWaylandClientData`, not our `ClientState`. Check for it first
        // so X11 surfaces' commits don't panic this lookup.
        if let Some(xwl) = client.get_data::<XWaylandClientData>() {
            return &xwl.compositor_state;
        }
        &client
            .get_data::<ClientState>()
            .expect("every non-Xwayland client is inserted with a ClientState")
            .compositor_state
    }

    #[cfg(feature = "udev")]
    fn new_surface(&mut self, surface: &WlSurface) {
        // Explicit sync (wp_linux_drm_syncobj_v1): a client can attach an
        // *acquire* fence to a commit — the buffer isn't safe to sample until
        // that fence signals. Register a pre-commit hook that, when an acquire
        // point is present and not yet signalled, delays the commit with a
        // blocker until the fence's eventfd fires on the loop. Without this we
        // composite the buffer mid-GPU-render and show BLACK (Vulkan/Skia
        // clients like LibreOffice); GL clients (Firefox) use implicit sync and
        // never set an acquire point, so this is a no-op for them.
        add_pre_commit_hook::<Self, _>(surface, |state, _dh, surface| {
            let Some(loop_h) = state.syncobj_loop.as_ref() else { return };
            let blocker = with_states(surface, |states| {
                let mut cached = states.cached_state.get::<DrmSyncobjCachedState>();
                cached
                    .pending()
                    .acquire_point
                    .as_ref()
                    .filter(|p| !p.is_signaled())
                    .and_then(|p| p.generate_blocker().ok())
            });
            if let Some((blocker, source)) = blocker {
                if let Some(client) = surface.client() {
                    loop_h.insert_sync_source(source, client);
                    add_blocker(surface, blocker);
                }
            }
        });
    }

    fn commit(&mut self, surface: &WlSurface) {
        // Push the freshly-committed buffer into the renderer's tracker so it
        // can mint / refresh the GPU texture on the next frame. Type
        // parameter selects the renderer; everything below is generic over it.
        on_commit_buffer_handler::<Self>(surface);
        self.surface_committed = true;

        if let Some(id) = self.window_for(surface) {
            tracing::trace!(window = id, "surface commit");

            // Sync the WM's window size to the client's actual rendered
            // size. We open windows at a placeholder 640×480 and never
            // dictate a size, so the client picks its own (often much
            // larger). Without this the hit-test rect stayed 640×480 and
            // only the top-left of the window was clickable — right-side
            // toolbar buttons, the close button and centred content
            // buttons all fell outside it. Floating only: a snapped /
            // maximised window's size is the compositor's to dictate.
            if let Ok(w) = self.wm.get(id) {
                if matches!(w.state, crate::wm::WinState::Floating) {
                    // Bounding box of the whole surface tree (root + every
                    // subsurface). `surface_size()` of just the root misses
                    // clients (e.g. Chromium) that render their content into
                    // subsurfaces, which left the hit-test rect too small and
                    // right-side widgets unreachable.
                    let bbox = bbox_from_surface_tree(surface, (0, 0));
                    if bbox.size.w > 0
                        && bbox.size.h > 0
                        && (w.geom.w as i32, w.geom.h as i32) != (bbox.size.w, bbox.size.h)
                    {
                        let _ = self.wm.r#move(
                            id,
                            Rect::new(
                                w.geom.x,
                                w.geom.y,
                                bbox.size.w as f32,
                                bbox.size.h as f32,
                            ),
                        );
                    }
                }
            }

            // Center a dialog over its parent once its real size is known.
            // One-shot: we only do this the first commit where the size is no
            // longer the placeholder, then drop the pending marker.
            if self.dialog_center_pending.contains(&id) {
                if let (Ok(child), Some(&parent_id)) =
                    (self.wm.get(id), self.dialog_parent.get(&id))
                {
                    if let Ok(parent) = self.wm.get(parent_id) {
                        // Skip until the client has committed a real size
                        // (still the 640×480 placeholder ⇒ wait for a later
                        // commit). Heuristic: size differs from the placeholder.
                        let sized = (child.geom.w as i32, child.geom.h as i32)
                            != (640, 480);
                        if sized {
                            let cx = parent.geom.x + (parent.geom.w - child.geom.w) / 2.0;
                            let cy = parent.geom.y + (parent.geom.h - child.geom.h) / 2.0;
                            let _ = self.wm.r#move(
                                id,
                                Rect::new(cx.max(0.0), cy.max(0.0), child.geom.w, child.geom.h),
                            );
                            self.dialog_center_pending.remove(&id);
                        }
                    } else {
                        // Parent gone before we could center — give up.
                        self.dialog_center_pending.remove(&id);
                    }
                }
            }

            // xdg-shell carries title / app_id as double-buffered
            // toplevel state, so a client can change them at any
            // commit (browsers rewrite the title per tab). Mirror the
            // latest into the WM; the switcher label cache keys on the
            // title string and rebuilds itself on the next frame.
            let (app, title) = with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .map(|d| {
                        let a = d.lock().unwrap();
                        (a.app_id.clone(), a.title.clone())
                    })
                    .unwrap_or((None, None))
            });
            // When xdg_toplevel.set_app_id() was never called (e.g. Slint apps),
            // fall back to reading the client binary name from /proc/<pid>/exe so
            // the dock can match it against the pinned app_id (e.g. "bacak-belge").
            let app = app.or_else(|| {
                let pid = surface
                    .client()
                    .and_then(|c| c.get_credentials(&self.display_handle).ok())
                    .map(|cr| cr.pid)?;
                let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
                exe.file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
            });
            let meta_changed =
                self.wm.set_meta(id, app.as_deref(), title.as_deref()).unwrap_or(false);
            // Keep the foreign-toplevel advertisement in sync so taskbars show
            // the live title/app_id (browsers rewrite the title per tab).
            if meta_changed {
                if let (Some(handle), Ok(w)) =
                    (self.foreign_handles.get(&id), self.wm.get(id))
                {
                    handle.send_title(&w.title);
                    handle.send_app_id(&w.app);
                }
                self.ftl_update_title(id);
            }
            // X11 windows carry no XdgToplevelSurfaceData, so the block above
            // is a no-op for them; refresh their WM_CLASS / title straight off
            // the X11 surface instead (no-op for xdg toplevels). WM_CLASS
            // often lands a few commits after the window first maps.
            self.refresh_x11_identity(id);
            // Now that the app id may be known, see if this window
            // matches a saved placement and should jump there.
            self.try_restore_placement(id);
        }

        // Layer surfaces (panels, docks, OSK, wallpaper): on each commit
        // re-arrange the output's layer map (size/anchor/exclusive-zone may have
        // changed) and send the initial configure once so the client knows the
        // size to draw at. Without the configure the layer surface never maps.
        if let Some((oid, output)) = self.output_of_layer(surface) {
            self.arrange_layers(oid, &output);
            let needs_configure = with_states(surface, |states| {
                states
                    .data_map
                    .get::<smithay::wayland::shell::wlr_layer::LayerSurfaceData>()
                    .map(|d| !d.lock().unwrap().initial_configure_sent)
                    .unwrap_or(false)
            });
            if needs_configure {
                let map = smithay::desktop::layer_map_for_output(&output);
                if let Some(layer) = map.layer_for_surface(
                    surface,
                    smithay::desktop::WindowSurfaceType::TOPLEVEL,
                ) {
                    layer.layer_surface().send_configure();
                }
            }
        }

        // Drive the popup state machine (maps a freshly-committed popup against
        // its parent) and send the initial configure once, so xdg_popups — app
        // menus, tooltips, combo-box dropdowns — actually get positioned and
        // mapped. The geometry was set from the positioner in `new_popup`.
        // Without this whole block Wayland app menus never appear.
        self.popups.commit(surface);
        if let Some(PopupKind::Xdg(ref popup)) = self.popups.find_popup(surface) {
            if !popup.is_initial_configure_sent() {
                // Popups don't negotiate a buffer size, so this can't fail in
                // a way we can recover from; log and move on if it does.
                if let Err(err) = popup.send_configure() {
                    tracing::warn!(?err, "failed to send initial popup configure");
                }
            }
        }
    }
}

impl BufferHandler for BacakState {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {
        // Renderer-side textures will be evicted from here.
    }
}

// ---------------------------------------------------------------------------
// zwp_linux_dmabuf_v1 — lets GPU clients submit dma-buf frames instead of
// rendering black on this (otherwise SHM-only) compositor.
// ---------------------------------------------------------------------------

impl DmabufHandler for BacakState {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        // DIAGNOSTIC (black-render hunt): log the negotiated (format, modifier)
        // of every client buffer. The buffer already matched an *advertised*
        // texture format (smithay rejects others before we get here), yet some
        // apps still render black — meaning EGL reports the pair as
        // texture-importable but the GlesRenderer fails the real per-frame
        // import. Compare what LibreOffice/Skia picks (black) against what
        // OnlyOffice picks (renders fine) to find the offending modifier.
        let format = smithay::backend::allocator::Buffer::format(&dmabuf);
        let size = smithay::backend::allocator::Buffer::size(&dmabuf);
        tracing::info!(
            fourcc = ?format.code,
            modifier = ?format.modifier,
            width = size.w,
            height = size.h,
            planes = dmabuf.num_planes(),
            "dmabuf import requested (optimistic-accept)"
        );

        // Queue for a real test-import. The renderer lives in the backend, not
        // on `BacakState`, so we can't validate here — but optimistically
        // accepting let buffers through whose (format, modifier) the
        // `GlesRenderer` can advertise as EGL-importable yet fails to actually
        // sample, which renders BLACK (LibreOffice/Skia). The render tick drains
        // this queue (`process_pending_dmabuf`), test-imports into the live
        // renderer, and resolves the notifier `successful`/`failed` so a client
        // whose buffer we can't sample renegotiates a format we can.
        self.pending_dmabuf.push((dmabuf, notifier));
    }
}
delegate_dmabuf!(BacakState);

#[cfg(feature = "udev")]
impl DrmSyncobjHandler for BacakState {
    fn drm_syncobj_state(&mut self) -> Option<&mut DrmSyncobjState> {
        self.syncobj_state.as_mut()
    }
}
#[cfg(feature = "udev")]
smithay::delegate_drm_syncobj!(BacakState);

// ---------------------------------------------------------------------------
// wl_shm
// ---------------------------------------------------------------------------

impl ShmHandler for BacakState {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

// ---------------------------------------------------------------------------
// xdg_shell — the surface ↔ window bridge lives here
// ---------------------------------------------------------------------------

impl BacakState {
    /// Flip/slide/resize a popup so it stays inside its output's work area,
    /// per the client's `xdg_positioner` constraint-adjustment. Without this a
    /// menu or combo-box dropdown opened near a screen edge spills off-screen
    /// and reads as "invisible". Best-effort: if the popup's root toplevel
    /// isn't a tracked window (or has no output) we leave the geometry as the
    /// client asked. Reads `state.positioner` set just before the call.
    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let kind = PopupKind::Xdg(popup.clone());
        let Ok(root) = find_popup_root_surface(&kind) else { return };
        let Some(win_id) = self.window_for(&root) else { return };
        let Ok(win) = self.wm.get(win_id) else { return };
        let Some(monitor) = self.wm.monitor_for_window(win_id) else { return };

        // The toplevel's *window-geometry* origin in global logical coords:
        // bacak draws the root surface at `win.geom` (surface (0,0)); the
        // window geometry (content) sits `geo_loc` into it (CSD shadow margin).
        let geo_loc = with_states(&root, |states| {
            states
                .cached_state
                .get::<SurfaceCachedState>()
                .current()
                .geometry
                .map(|g| g.loc)
                .unwrap_or_default()
        });

        // The allowed area = the output work area, expressed relative to *this*
        // popup's parent-geometry origin — the coordinate space the positioner
        // anchors in. Subtract the toplevel geometry origin and the accumulated
        // ancestor-popup offsets (`get_popup_toplevel_coords`). Mirrors anvil.
        let wa = monitor.work_area;
        let mut target = smithay::utils::Rectangle::<i32, smithay::utils::Logical>::new(
            smithay::utils::Point::from((wa.x as i32, wa.y as i32)),
            smithay::utils::Size::from((wa.w as i32, wa.h as i32)),
        );
        target.loc -= smithay::utils::Point::from((win.geom.x as i32, win.geom.y as i32));
        target.loc -= geo_loc;
        target.loc -= get_popup_toplevel_coords(&kind);

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

impl XdgShellHandler for BacakState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // A client just produced a new top-level window. Register it with the
        // Bacak WM and remember which WM id corresponds to this WlSurface so
        // later commits / focus changes can be routed back.
        let wl_surface = surface.wl_surface().clone();
        let app = String::new();
        let title = String::new();
        let geom = Rect::new(120.0, 120.0, 640.0, 480.0);
        let id = self.wm.open(app, title, geom);
        self.windows.insert(wl_surface.clone(), id);

        // Transient/dialog handling: if the client set a parent
        // (`xdg_toplevel.set_parent`), record child→parent so we can center the
        // dialog over its parent once its real size is known (in `commit`) and
        // keep it stacked above. A dialog left at the placeholder corner reads
        // as "didn't appear", which is one of the reported symptoms.
        if let Some(parent_surface) = surface.parent() {
            if let Some(parent_id) = self.window_for(&parent_surface) {
                self.dialog_parent.insert(id, parent_id);
                self.dialog_center_pending.insert(id);
            }
        }

        // Mark the surface as activated and send the initial configure so the
        // client picks a buffer size. Geometry refinement happens after the
        // first commit, once the client tells us its preferred size.
        surface.with_pending_state(|s| {
            s.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();

        // Advertise the window to external taskbars (ext-foreign-toplevel-list).
        // Title/app_id are filled in later commits via `set_meta`, so start
        // empty and update when known.
        let handle = self
            .foreign_toplevel_list
            .new_toplevel::<BacakState>(String::new(), String::new());
        self.foreign_handles.insert(id, handle);
        // Also announce to wlr foreign-toplevel *management* clients (taskbars
        // that can activate/close/minimise the window).
        self.ftl_announce_window(id);

        // Give the new toplevel keyboard focus. `wm.open` already raised it to
        // the top and set WM focus, but Smithay keyboard focus is separate —
        // without this a freshly-opened window (especially a modal dialog) gets
        // no key events until the user clicks it, so dialogs feel dead.
        self.set_keyboard_focus(Some(wl_surface));

        tracing::info!(window = id, "xdg toplevel mapped");
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        // Stamp the positioner into the popup's pending state, then unconstrain
        // it against the output work area (flip/slide/resize so a menu near a
        // screen edge stays on-screen instead of spilling off). The initial
        // configure that flushes this is sent from `commit` once the popup
        // first commits. track_popup wires it to its parent so `commit` can map
        // it and the render path can find it.
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        if crate::popup_debug() {
            let requested = surface.with_pending_state(|s| s.positioner.get_geometry());
            let placed = surface.with_pending_state(|s| s.geometry);
            let root = find_popup_root_surface(&PopupKind::Xdg(surface.clone())).ok();
            let win = root.as_ref().and_then(|r| self.window_for(r));
            tracing::info!(
                ?win,
                requested = ?(requested.loc, requested.size),
                placed = ?(placed.loc, placed.size),
                flipped = (requested.loc != placed.loc || requested.size != placed.size),
                "POPUP new_popup: tracked + unconstrained"
            );
        }
        if let Err(err) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            tracing::warn!(?err, "failed to track popup");
        }
    }

    fn move_request(
        &mut self,
        surface: ToplevelSurface,
        seat: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat,
        serial: Serial,
    ) {
        // Resolve the WlSeat back to our typed Seat handle so we can fetch
        // the pointer that initiated the click.
        let seat = match Seat::<BacakState>::from_resource(&seat) {
            Some(s) => s,
            None => {
                tracing::warn!("move_request: unknown WlSeat");
                return;
            }
        };
        let pointer = match seat.get_pointer() {
            Some(p) => p,
            None => return,
        };
        let Some(window_id) = self.window_for(surface.wl_surface()) else { return };
        let Ok(window) = self.wm.get(window_id) else { return };
        // Snap math must follow the window across outputs — if the user
        // dragged Firefox to the secondary monitor before flinging it
        // back, the snap zones live on whichever output the window
        // currently inhabits.
        let Some(monitor) = self.wm.monitor_for_window(window_id) else { return };

        // Build GrabStartData from the current pointer position. We don't
        // require a strict serial match — clients sometimes get the serial
        // wrong, and refusing the grab feels broken to the user. The serial
        // is still threaded through to Smithay so it can de-dup re-entries.
        let start = GrabStartData::<BacakState> {
            focus: None,
            button: 0x110, // BTN_LEFT — the only button we currently grant
            location: self.pointer_position.into(),
        };
        let initial_window_pos = (window.geom.x as f64, window.geom.y as f64).into();
        let grab = MoveGrab::new(
            start,
            window_id,
            initial_window_pos,
            (window.geom.w, window.geom.h),
            monitor,
        );
        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let seat = match Seat::<BacakState>::from_resource(&seat) {
            Some(s) => s,
            None => return,
        };
        let pointer = match seat.get_pointer() {
            Some(p) => p,
            None => return,
        };
        let Some(window_id) = self.window_for(surface.wl_surface()) else { return };
        let Ok(window) = self.wm.get(window_id) else { return };

        let start = GrabStartData::<BacakState> {
            focus: None,
            button: 0x110,
            location: self.pointer_position.into(),
        };
        let grab = ResizeGrab::new(start, window_id, window.geom, edges, surface);
        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    fn grab(
        &mut self,
        surface: PopupSurface,
        seat: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat,
        serial: Serial,
    ) {
        // The "menu grab": while a menu is up, the pointer and keyboard are
        // grabbed by the popup chain so a click/keypress outside it dismisses
        // the whole chain (and routes input to the right submenu). Without it
        // GTK/Qt menus either don't open or close again immediately.
        let Some(seat) = Seat::<BacakState>::from_resource(&seat) else {
            return;
        };
        let kind = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&kind) else {
            tracing::warn!("popup grab: no root surface");
            return;
        };
        // Our KeyboardFocus/PointerFocus are both WlSurface, so the grab root
        // is just the root surface.
        let mut grab = match self.popups.grab_popup(root, kind, &seat, serial) {
            Ok(g) => g,
            Err(err) => {
                tracing::warn!(?err, "failed to grab popup");
                return;
            }
        };

        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed()
                && !(keyboard.has_grab(serial)
                    || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }

        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed()
                && !(pointer.has_grab(serial)
                    || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }

        if crate::popup_debug() {
            tracing::info!("POPUP grab: menu pointer+keyboard grab installed");
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        // The client wants to move an already-mapped popup (e.g. a submenu
        // re-anchoring). Restamp the geometry from the new positioner and ack
        // with the same token so the client knows which request we applied.
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // Mirror surface destruction into the WM so the window list stays
        // accurate. We close on the WM side too, ignoring errors because the
        // mapping is the single source of truth.
        let wl_surface = surface.wl_surface();
        if let Some(id) = self.windows.remove(wl_surface) {
            // Remember the workspace before close so the auto-focus path
            // can promote a sibling on the same workspace afterwards —
            // otherwise the user is left with no keyboard target.
            let ws = self.wm.get(id).ok().map(|w| w.workspace);
            let _ = self.wm.close(id);
            self.focus_history.forget(id);
            self.evict_label(id);
            self.placement_done.remove(&id);
            self.dialog_parent.remove(&id);
            self.dialog_center_pending.remove(&id);
            self.dialog_parent.retain(|_, parent| *parent != id);
            // Withdraw the foreign-toplevel advertisement (list + management).
            if let Some(handle) = self.foreign_handles.remove(&id) {
                self.foreign_toplevel_list.remove_toplevel(&handle);
            }
            self.ftl_closed(id);
            self.decorated.remove(&id);
            if self.title_drag.map(|d| d.id) == Some(id) {
                self.title_drag = None;
            }
            // Confirms a pending dismiss (or prunes an externally-closed card).
            self.overview_window_closed(id);
            if let Some(ws) = ws {
                self.auto_focus_workspace(ws);
            }
            tracing::info!(window = id, "xdg toplevel destroyed");
        }
    }

    // Maximise / fullscreen requests. Clients send these when the user
    // double-clicks a CSD title bar or hits the maximise/fullscreen
    // control — we honour them so the behaviour is uniform across every
    // app (we draw no server-side decorations, so the client owns the
    // title bar and its double-click). We resize the WM window AND send
    // the client a configure carrying the new size + state so it
    // repaints to fill the area.
    fn maximize_request(&mut self, surface: ToplevelSurface) {
        let Some(id) = self.window_for(surface.wl_surface()) else { return };
        let Some(monitor) = self.wm.monitor_for_window(id) else { return };
        // Remember where to return to (only when coming from a normal state).
        if let Ok(w) = self.wm.get(id) {
            if matches!(w.state, WinState::Floating | WinState::Snapped(_)) {
                self.maximize_restore.insert(id, w.geom);
            }
        }
        let area = monitor.work_area;
        let _ = self.wm.snap(id, SnapZone::Maximize);
        // Server-side decorated windows reserve the title-bar strip at the top
        // of the work area, so the bar (drawn above the content) stays on-screen
        // instead of overlapping the panel / going off the top edge.
        let bar = if self.decorated.contains(&id) {
            crate::decoration::BAR_H
        } else {
            0.0
        };
        let ch = (area.h - bar).max(1.0);
        if bar > 0.0 {
            let _ = self.wm.set_geom(id, Rect::new(area.x, area.y + bar, area.w, ch));
        }
        surface.with_pending_state(|s| {
            s.states.set(xdg_toplevel::State::Maximized);
            s.size = Some((area.w as i32, ch as i32).into());
        });
        surface.send_configure();
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        let Some(id) = self.window_for(surface.wl_surface()) else { return };
        let restore = self.maximize_restore.remove(&id);
        surface.with_pending_state(|s| {
            s.states.unset(xdg_toplevel::State::Maximized);
            s.size = restore.map(|r| (r.w as i32, r.h as i32).into());
        });
        surface.send_configure();
        if let Some(r) = restore {
            let _ = self.wm.r#move(id, r);
        }
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, _output: Option<WlOutput>) {
        let Some(id) = self.window_for(surface.wl_surface()) else { return };
        let Some(output) = self.wm.output_for_window(id) else { return };
        if let Ok(w) = self.wm.get(id) {
            if matches!(w.state, WinState::Floating | WinState::Snapped(_)) {
                self.maximize_restore.insert(id, w.geom);
            }
        }
        let b = output.bounds;
        let _ = self.wm.fullscreen(id, b);
        surface.with_pending_state(|s| {
            s.states.set(xdg_toplevel::State::Fullscreen);
            s.size = Some((b.w as i32, b.h as i32).into());
        });
        surface.send_configure();
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        let Some(id) = self.window_for(surface.wl_surface()) else { return };
        let restore = self.maximize_restore.remove(&id);
        surface.with_pending_state(|s| {
            s.states.unset(xdg_toplevel::State::Fullscreen);
            s.size = restore.map(|r| (r.w as i32, r.h as i32).into());
        });
        surface.send_configure();
        if let Some(r) = restore {
            let _ = self.wm.r#move(id, r);
        }
    }
}

// ---------------------------------------------------------------------------
// wl_seat
// ---------------------------------------------------------------------------

impl SeatHandler for BacakState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        // Keep WM focus in sync with the keyboard focus that Smithay just
        // applied. Unknown surfaces (subsurfaces, popups) are simply skipped.
        // Also feed the MRU history; the promote call is a no-op while an
        // alt+tab cycle is in progress, so cycle steps don't pollute it.
        if let Some(surface) = focused {
            if let Some(id) = self.window_for(surface) {
                // In FocusFollowsPointer mode the motion handler sets
                // `suppress_focus_raise` so that pointer drift over an older
                // window doesn't bury a newly-opened app (e.g. bacak-belge
                // opened from Altay). Skip z-order raise and MRU promotion;
                // clipboard/text-input/foreign-toplevel updates still happen.
                if !self.suppress_focus_raise {
                    let _ = self.wm.focus(id);
                    self.focus_history.promote(id);
                    self.raise_child_dialogs(id);
                }
            }
        }

        // Move the clipboard / primary-selection focus to the newly focused
        // client. Smithay's keyboard `set_focus` does NOT do this — the data
        // device offers the current selection only to the *data-device-focused*
        // client, so without this a freshly-focused app (e.g. gedit) is never
        // offered the clipboard and Ctrl+V pastes nothing across apps. (This is
        // the "no system-wide clipboard" bug.) The selection is re-offered only
        // when the focus actually changes, per `set_data_device_focus`.
        let client = focused.and_then(|s| self.display_handle.get_client(s.id()).ok());
        set_data_device_focus(&self.display_handle, seat, client.clone());
        set_primary_focus(&self.display_handle, seat, client);

        // Retarget Bacak's text-input observer to the new keyboard focus: send
        // `leave`/`enter` so the focused client may enable text-input, which is
        // what drives the on-screen keyboard's auto-show.
        self.ti_set_focus(focused.cloned());

        // Reflect the new "activated" state to wlr foreign-toplevel taskbars.
        self.ftl_refresh_all_state();
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        // Remember what the focused client wants the cursor to look like;
        // the render path turns this into the actual on-screen cursor
        // (client surface + hotspot, hidden, or our default arrow).
        self.cursor_status = image;
    }
}

// ---------------------------------------------------------------------------
// wl_output — OutputManagerState drives the protocol; we only need the
// (empty) handler so `delegate_output!` is satisfied. The default
// `output_bound` is fine — we don't track per-client output binds.
// ---------------------------------------------------------------------------

impl OutputHandler for BacakState {}

// ---------------------------------------------------------------------------
// wl_data_device_manager — clipboard + drag-and-drop. No custom selection
// or DnD policy yet, so the grab handlers take their trait defaults; only
// the state getter is required. Advertising this global is what lets
// Chromium's Ozone-Wayland backend initialise at all.
// ---------------------------------------------------------------------------

impl SelectionHandler for BacakState {
    type SelectionUserData = crate::state::SelectionOrigin;

    /// A Wayland client took ownership of a selection (CLIPBOARD or PRIMARY).
    /// Mirror it onto the X11 side so XWayland clients can paste from it. The
    /// reverse direction (X11 sets the selection) is driven from `XwmHandler`
    /// in `xwayland.rs`; the two never loop because compositor-set selections
    /// (those originating from X11) don't re-enter this callback.
    fn new_selection(&mut self, ty: SelectionTarget, source: Option<SelectionSource>, _seat: Seat<Self>) {
        if crate::clip_debug() {
            tracing::info!(
                ?ty,
                mimes = ?source.as_ref().map(|s| s.mime_types()),
                "CLIP Wayland took selection → mirroring to X11"
            );
        }
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(err) = xwm.new_selection(ty, source.map(|s| s.mime_types())) {
                tracing::warn!(?err, ?ty, "failed to advertise Wayland selection to XWayland");
            }
        }
    }

    /// A Wayland client wants to read a selection the compositor owns. Two
    /// origins: an X11-mirrored selection is streamed by the X11Wm on the
    /// backend loop; compositor-copied native text is written straight to `fd`.
    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        user_data: &crate::state::SelectionOrigin,
    ) {
        if crate::clip_debug() {
            tracing::info!(?ty, %mime_type, origin = ?user_data, "CLIP Wayland client reading compositor-owned selection");
        }
        match user_data {
            crate::state::SelectionOrigin::X11 => {
                // Disjoint field borrows: `xwm` is the manager, `sink` carries
                // the backend loop handle it needs (see `X11SelectionSink`).
                let Some(xwm) = self.xwm.as_mut() else { return };
                let Some(sink) = self.x11_selection_sink.as_ref() else {
                    tracing::warn!("no X11 selection sink wired; cannot serve X11 selection");
                    return;
                };
                sink.send(xwm, ty, mime_type, fd);
            }
            crate::state::SelectionOrigin::NativeText => {
                // The compositor-owned selection is either text or a screenshot
                // PNG; branch on the requested mime. Only the mimes we advertised
                // are requestable, so an image selection can't ask for text.
                use std::io::Write;
                let mut file = std::fs::File::from(fd);
                if mime_type.starts_with("image/") {
                    if let Some(png) = self.clipboard_image.as_ref() {
                        tracing::info!(?ty, %mime_type, bytes = png.len(), "clipboard: client requested screenshot image");
                        if let Err(err) = file.write_all(png) {
                            tracing::debug!(?err, "client closed the image selection pipe early");
                        }
                    }
                } else if let Some(text) = self.clipboard_text.as_ref() {
                    tracing::info!(?ty, %mime_type, bytes = text.len(), "atspi/clipboard: client requested compositor text selection");
                    if let Err(err) = file.write_all(text.as_bytes()) {
                        tracing::debug!(?err, "client closed the selection pipe early");
                    }
                }
            }
        }
    }
}

impl DataDeviceHandler for BacakState {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl PrimarySelectionHandler for BacakState {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

impl ClientDndGrabHandler for BacakState {}
impl ServerDndGrabHandler for BacakState {}

// xdg-decoration: negotiate server- vs client-side decorations. Policy:
// default to server-side (the compositor draws a uniform title bar, which also
// gives apps that draw no decorations of their own something to move/close);
// honour an explicit client-side request (GTK et al. that prefer their own
// CSD). The `decorated` set drives the render + title-bar input.
impl XdgDecorationHandler for BacakState {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| s.decoration_mode = Some(DecoMode::ServerSide));
        toplevel.send_configure();
        if let Some(id) = self.window_for(toplevel.wl_surface()) {
            self.decorated.insert(id);
        }
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecoMode) {
        let server = mode != DecoMode::ClientSide;
        let applied = if server { DecoMode::ServerSide } else { DecoMode::ClientSide };
        toplevel.with_pending_state(|s| s.decoration_mode = Some(applied));
        toplevel.send_configure();
        if let Some(id) = self.window_for(toplevel.wl_surface()) {
            if server {
                self.decorated.insert(id);
            } else {
                self.decorated.remove(&id);
            }
        }
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| s.decoration_mode = Some(DecoMode::ServerSide));
        toplevel.send_configure();
        if let Some(id) = self.window_for(toplevel.wl_surface()) {
            self.decorated.insert(id);
        }
    }
}

// xwayland_shell_v1 — Xwayland associates each X11 window's wl_surface with a
// serial through this protocol. The X11Wm matches that serial against the
// `WL_SURFACE_SERIAL` atom on the X11 window to bridge the two. The actual
// promotion into the Bacak WM happens in `xwayland::map_x11_surface`, driven
// by the `XwmHandler` impl on the udev loop data.
impl XWaylandShellHandler for BacakState {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }

    fn surface_associated(&mut self, _xwm: XwmId, _wl_surface: WlSurface, surface: X11Surface) {
        // The wl_surface is now bound to the X11 window. If the client has
        // already asked to map, promote it into the WM now (idempotent with
        // the `map_window_request` path, since association/map race).
        self.map_x11_surface(surface);
    }
}

// ---------------------------------------------------------------------------
// Delegate macros — forward protocol traffic to the impls above.
// ---------------------------------------------------------------------------

delegate_compositor!(BacakState);
delegate_xdg_shell!(BacakState);
delegate_shm!(BacakState);
delegate_seat!(BacakState);
delegate_xdg_activation!(BacakState);
delegate_output!(BacakState);
delegate_data_device!(BacakState);
delegate_primary_selection!(BacakState);
delegate_pointer_gestures!(BacakState);
delegate_xwayland_shell!(BacakState);
delegate_xdg_decoration!(BacakState);
delegate_viewporter!(BacakState);
delegate_fractional_scale!(BacakState);
smithay::delegate_foreign_toplevel_list!(BacakState);

impl smithay::wayland::foreign_toplevel_list::ForeignToplevelListHandler for BacakState {
    fn foreign_toplevel_list_state(
        &mut self,
    ) -> &mut smithay::wayland::foreign_toplevel_list::ForeignToplevelListState {
        &mut self.foreign_toplevel_list
    }
}

// ---------------------------------------------------------------------------
// text-input-v3 + input-method-v2
//
// text-input is fully driven by smithay (it routes a focused editable field's
// events to the active input-method). For input-method we only need to satisfy
// the handler: give the IME its parent (focused window) geometry so it could
// place a candidate popup near the cursor. The IME's candidate popup itself is
// not rendered yet — text injection (commit_string) works without it, which is
// what on-screen keyboards (wvkbd) and basic IME use. CJK candidate visuals are
// a follow-up (would render the input-method PopupSurface like an xdg popup).
// ---------------------------------------------------------------------------

// text-input-v3 is hand-rolled (see crate::text_input) — no smithay delegate.
smithay::delegate_input_method_manager!(BacakState);
smithay::delegate_virtual_keyboard_manager!(BacakState);

impl smithay::wayland::input_method::InputMethodHandler for BacakState {
    fn new_popup(&mut self, surface: smithay::wayland::input_method::PopupSurface) {
        // IME candidate popup (CJK etc.) — track it; the render path draws it
        // near the text cursor.
        self.ime_popups.push(surface);
    }

    fn dismiss_popup(&mut self, surface: smithay::wayland::input_method::PopupSurface) {
        self.ime_popups
            .retain(|p| p.wl_surface() != surface.wl_surface());
    }

    fn popup_repositioned(&mut self, _surface: smithay::wayland::input_method::PopupSurface) {
        // Location is read fresh at render time, so nothing to cache here.
    }

    fn parent_geometry(
        &self,
        parent: &WlSurface,
    ) -> smithay::utils::Rectangle<i32, smithay::utils::Logical> {
        // Place the IME popup relative to the focused text field's window.
        self.window_for(parent)
            .and_then(|id| self.wm.get(id).ok())
            .map(|w| {
                smithay::utils::Rectangle::new(
                    (w.geom.x as i32, w.geom.y as i32).into(),
                    (w.geom.w as i32, w.geom.h as i32).into(),
                )
            })
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// wlr-layer-shell — panels, docks, wallpaper, notifiers, on-screen keyboards
//
// Layer surfaces are owned by per-output `LayerMap`s (in the smithay Output's
// user_data); we reach them via `state.outputs` (registered by the backend).
// On map/commit we `arrange()` the map and mirror its exclusive zones into the
// WM "layer-shell" strut layer so windows/maximize avoid panels. Render
// (background/bottom below windows, top/overlay above) and input hit-testing
// live in render.rs / state.rs::surface_at.
// ---------------------------------------------------------------------------

smithay::delegate_single_pixel_buffer!(BacakState);
smithay::delegate_content_type!(BacakState);
smithay::delegate_presentation!(BacakState);
smithay::delegate_relative_pointer!(BacakState);
smithay::delegate_pointer_constraints!(BacakState);
smithay::delegate_tablet_manager!(BacakState);
smithay::delegate_security_context!(BacakState);
smithay::delegate_layer_shell!(BacakState);

impl smithay::wayland::security_context::SecurityContextHandler for BacakState {
    fn context_created(
        &mut self,
        source: smithay::wayland::security_context::SecurityContextListenerSource,
        security_context: smithay::wayland::security_context::SecurityContext,
    ) {
        // Queue the restricted listener; the udev loop inserts it (it owns the
        // calloop handle) and tags clients connecting through it.
        self.pending_security_listeners.push((source, security_context));
    }
}

impl smithay::wayland::tablet_manager::TabletSeatHandler for BacakState {
    // A client may set a custom cursor image for a tool; we keep our own
    // cursor, so accept-and-ignore.
    fn tablet_tool_image(
        &mut self,
        _tool: &smithay::backend::input::TabletToolDescriptor,
        _image: CursorImageStatus,
    ) {
    }
}

impl smithay::wayland::pointer_constraints::PointerConstraintsHandler for BacakState {
    fn new_constraint(
        &mut self,
        surface: &WlSurface,
        pointer: &smithay::input::pointer::PointerHandle<Self>,
    ) {
        // Activate the lock/confine immediately if the pointer is already over
        // the requesting surface (the usual case: click into a game, it locks).
        if self.surface_under_pointer().as_ref() == Some(surface) {
            smithay::wayland::pointer_constraints::with_pointer_constraint(
                surface,
                pointer,
                |c| {
                    if let Some(c) = c {
                        c.activate();
                    }
                },
            );
        }
    }

    fn cursor_position_hint(
        &mut self,
        _surface: &WlSurface,
        _pointer: &smithay::input::pointer::PointerHandle<Self>,
        _location: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) {
        // A locked client suggests where to show the cursor on unlock. We hide
        // the cursor during lock and re-derive position on unlock, so no-op.
    }
}

impl smithay::wayland::shell::wlr_layer::WlrLayerShellHandler for BacakState {
    fn shell_state(&mut self) -> &mut smithay::wayland::shell::wlr_layer::WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: smithay::wayland::shell::wlr_layer::LayerSurface,
        wl_output: Option<WlOutput>,
        _layer: smithay::wayland::shell::wlr_layer::Layer,
        namespace: String,
    ) {
        let Some((oid, output)) = self.resolve_layer_output(wl_output) else {
            tracing::warn!("layer surface with no resolvable output; dropping");
            return;
        };
        let ls = smithay::desktop::LayerSurface::new(surface, namespace);
        {
            let mut map = smithay::desktop::layer_map_for_output(&output);
            if let Err(err) = map.map_layer(&ls) {
                tracing::warn!(?err, "failed to map layer surface");
                return;
            }
        }
        // Arrange + publish struts now; the initial configure is sent on the
        // first commit (the client then attaches a buffer at the chosen size).
        self.arrange_layers(oid, &output);
    }

    fn new_popup(
        &mut self,
        _parent: smithay::wayland::shell::wlr_layer::LayerSurface,
        popup: smithay::wayland::shell::xdg::PopupSurface,
    ) {
        // A popup parented to a layer surface (e.g. a panel menu). Track it so
        // it configures and grabs like an xdg popup. Rendering layer-parented
        // popups is a follow-up (our popup render walks toplevels only).
        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_geometry();
        });
        if let Err(err) = self.popups.track_popup(PopupKind::Xdg(popup)) {
            tracing::warn!(?err, "failed to track layer popup");
        }
    }

    fn layer_destroyed(&mut self, surface: smithay::wayland::shell::wlr_layer::LayerSurface) {
        if let Some((oid, output)) = self.output_of_layer(surface.wl_surface()) {
            {
                let mut map = smithay::desktop::layer_map_for_output(&output);
                let victim = map
                    .layers()
                    .find(|l| l.wl_surface() == surface.wl_surface())
                    .cloned();
                if let Some(l) = victim {
                    map.unmap_layer(&l);
                }
            }
            self.arrange_layers(oid, &output);
        }
    }
}

// ---------------------------------------------------------------------------
// wp_fractional_scale_v1
//
// When a client creates a fractional-scale object for a surface, tell it the
// scale we'll render that surface at, so HiDPI toolkits render crisply instead
// of guessing from the integer wl_output scale. bacak does integer scaling, so
// we resolve the surface's window → its output → that output's scale; for a
// surface not yet mapped to a window we fall back to the primary output.
// (viewporter needs no handler — render honours the viewport automatically.)
// ---------------------------------------------------------------------------

impl smithay::wayland::fractional_scale::FractionalScaleHandler for BacakState {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let scale = self
            .window_for(&surface)
            .and_then(|id| self.wm.output_for_window(id))
            .map(|o| o.id)
            .or_else(|| self.wm.primary_output())
            .map(|oid| self.wm.output_scale(oid))
            .unwrap_or(1.0);
        with_states(&surface, |states| {
            smithay::wayland::fractional_scale::with_fractional_scale(states, |fs| {
                fs.set_preferred_scale(scale);
            });
        });
    }
}

// ---------------------------------------------------------------------------
// xdg-activation v1
//
// A client asks the compositor to "activate" one of its surfaces (e.g. a
// pop-up notification window). Auto-honouring this would let any client
// steal focus, so the user-respecting policy is: if the request is for a
// window that's already focused, ignore it (already there); otherwise mark
// that window urgent so the dock pulses its tile until the user looks. The
// token is removed either way so it can't be reused.
// ---------------------------------------------------------------------------

impl XdgActivationHandler for BacakState {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        _token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        if let Some(id) = self.window_for(&surface) {
            let is_focused =
                self.wm.get(id).map(|w| w.focused).unwrap_or(false);
            if !is_focused {
                let _ = self.wm.set_urgent(id, true);
            }
        }
        self.xdg_activation.remove_token(&token);
    }
}
