//! `zwlr_foreign_toplevel_management_v1` — lets external taskbars / docks
//! (waybar's `wlr/taskbar`, etc.) not just *list* open windows but *act* on
//! them: activate, close, minimize/unminimize, maximize/unmaximize, fullscreen.
//!
//! This is the management counterpart to the list-only
//! `ext-foreign-toplevel-list` ([`crate::state::BacakState::foreign_toplevel_list`]).
//! smithay 0.7 has no built-in management protocol, so it's hand-rolled here
//! (like screencopy). Each bound manager gets its own per-window handle; we push
//! title / app_id / state to those handles and route their action requests into
//! the WM. xdg toplevels only for now (X11 windows aren't advertised).

#![cfg(feature = "runtime")]

use smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::state::BacakState;
use crate::wm::{WinState, WindowId};

/// Userdata on a per-window handle: which window it represents.
#[derive(Debug)]
pub struct ForeignToplevelHandleData {
    pub window: WindowId,
}

/// The wlr `state` event is an array of u32 state values (native-endian).
/// Values: maximized=0, minimized=1, activated=2, fullscreen=3.
fn state_array(state: &BacakState, id: WindowId) -> Vec<u8> {
    let mut v: Vec<u32> = Vec::new();
    if let Ok(w) = state.wm.get(id) {
        if w.focused {
            v.push(2); // activated
        }
        match w.state {
            WinState::Maximized | WinState::Snapped(_) => v.push(0),
            WinState::Minimized => v.push(1),
            WinState::Fullscreen => v.push(3),
            WinState::Floating => {}
        }
    }
    v.iter().flat_map(|s| s.to_ne_bytes()).collect()
}

impl BacakState {
    /// Create + announce a handle for `id` on a single manager, with its current
    /// title / app_id / state, then `done`.
    fn ftl_announce_to(&mut self, manager: &ZwlrForeignToplevelManagerV1, id: WindowId) {
        let Some(client) = manager.client() else { return };
        let dh = self.display_handle.clone();
        let Ok(handle) = client
            .create_resource::<ZwlrForeignToplevelHandleV1, ForeignToplevelHandleData, BacakState>(
                &dh,
                manager.version(),
                ForeignToplevelHandleData { window: id },
            )
        else {
            return;
        };
        manager.toplevel(&handle);
        if let Ok(w) = self.wm.get(id) {
            handle.title(w.title.clone());
            handle.app_id(w.app.clone());
        }
        handle.state(state_array(self, id));
        handle.done();
        self.ftl_handles.entry(id).or_default().push(handle);
    }

    /// Announce a newly-mapped window to every bound manager.
    pub fn ftl_announce_window(&mut self, id: WindowId) {
        if self.ftl_managers.is_empty() {
            return;
        }
        let managers = self.ftl_managers.clone();
        for m in &managers {
            self.ftl_announce_to(m, id);
        }
    }

    /// Push the live title / app_id to all of `id`'s handles.
    pub fn ftl_update_title(&self, id: WindowId) {
        let Some(handles) = self.ftl_handles.get(&id) else { return };
        let Ok(w) = self.wm.get(id) else { return };
        for h in handles {
            h.title(w.title.clone());
            h.app_id(w.app.clone());
            h.done();
        }
    }

    /// Push the current state array to all of `id`'s handles.
    pub fn ftl_update_state(&self, id: WindowId) {
        let Some(handles) = self.ftl_handles.get(&id) else { return };
        let arr = state_array(self, id);
        for h in handles {
            h.state(arr.clone());
            h.done();
        }
    }

    /// State changed somewhere (focus/min/max). Refresh every window's handles —
    /// cheap (few windows) and keeps the "activated" flag exclusive.
    pub fn ftl_refresh_all_state(&self) {
        if self.ftl_handles.is_empty() {
            return;
        }
        let ids: Vec<WindowId> = self.ftl_handles.keys().copied().collect();
        for id in ids {
            self.ftl_update_state(id);
        }
    }

    /// Window closed: tell every handle and forget them.
    pub fn ftl_closed(&mut self, id: WindowId) {
        if let Some(handles) = self.ftl_handles.remove(&id) {
            for h in handles {
                h.closed();
            }
        }
    }

    /// Taskbar asked to activate a window: restore if minimized, focus + raise +
    /// give keyboard focus, then refresh state so the bar shows it active.
    fn ftl_activate(&mut self, id: WindowId) {
        if matches!(self.wm.get(id).map(|w| w.state), Ok(WinState::Minimized)) {
            self.restore_window_animated(id);
        }
        let _ = self.wm.focus(id);
        if let Some(s) = self.surface_for_window(id) {
            self.set_keyboard_focus(Some(s));
        }
        self.ftl_refresh_all_state();
    }
}

impl GlobalDispatch<ZwlrForeignToplevelManagerV1, ()> for BacakState {
    fn bind(
        state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrForeignToplevelManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        state.ftl_managers.push(manager.clone());
        // Announce every existing window to the freshly-bound manager.
        let ids: Vec<WindowId> = state.wm.all_windows().iter().map(|w| w.id).collect();
        for id in ids {
            state.ftl_announce_to(&manager, id);
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for BacakState {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &ZwlrForeignToplevelManagerV1,
        request: zwlr_foreign_toplevel_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Request::Stop = request {
            manager.finished();
            state.ftl_managers.retain(|m| m != manager);
        }
    }
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ForeignToplevelHandleData> for BacakState {
    fn request(
        state: &mut Self,
        _client: &Client,
        handle: &ZwlrForeignToplevelHandleV1,
        request: zwlr_foreign_toplevel_handle_v1::Request,
        data: &ForeignToplevelHandleData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Request;
        let id = data.window;
        let is = |s: WinState| matches!(state.wm.get(id).map(|w| w.state), Ok(w) if w == s);
        match request {
            Request::Activate { seat: _ } => state.ftl_activate(id),
            Request::Close => state.close_window(id),
            Request::SetMinimized => {
                state.minimize_window_animated(id);
                state.ftl_refresh_all_state();
            }
            Request::UnsetMinimized => {
                state.restore_window_animated(id);
                state.ftl_refresh_all_state();
            }
            Request::SetMaximized => {
                if !is(WinState::Maximized) {
                    state.toggle_maximize(id);
                }
                state.ftl_refresh_all_state();
            }
            Request::UnsetMaximized => {
                if is(WinState::Maximized) {
                    state.toggle_maximize(id);
                }
                state.ftl_refresh_all_state();
            }
            Request::SetFullscreen { output: _ } => {
                if let Some(m) = state.wm.monitor_for_window(id) {
                    let _ = state.wm.fullscreen(id, m.work_area);
                    state.ftl_refresh_all_state();
                }
            }
            Request::UnsetFullscreen => {
                // No dedicated un-fullscreen in the WM; restoring focus state is
                // the best we do without tracking pre-fullscreen geometry here.
                state.ftl_refresh_all_state();
            }
            Request::SetRectangle { .. } => {
                // Minimise-animation target hint; we animate to the dock instead.
            }
            Request::Destroy => {
                if let Some(handles) = state.ftl_handles.get_mut(&id) {
                    handles.retain(|h| h != handle);
                }
            }
            _ => {}
        }
    }
}
