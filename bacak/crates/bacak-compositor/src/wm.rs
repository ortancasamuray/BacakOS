//! Window manager — authoritative window state.
//!
//! Every geometry mutation routes through here so multi-monitor and snap
//! invariants hold. The compositor's xdg-shell handler drives this module;
//! the shell crates (panel/dock/launcher) observe state via IPC.
//!
//! ## Output / workspace model
//!
//! - An [`Output`] is a *physical* surface (a monitor). It has `bounds` in
//!   logical pixels and a set of [`Struts`] reserved by chrome (the dock,
//!   the panel, the OSK). `work_area = bounds − struts` is what snap math
//!   and maximise use.
//! - A [`Workspace`] is a *logical* group of windows. Every workspace is
//!   bound to one output; switching workspaces only swaps which workspace
//!   is **active** on that output, never which windows belong where.
//! - A window stores its `workspace`; it is *visible* iff its workspace is
//!   currently active on some output.
//!
//! [`Monitor`] survives as a `Copy` snapshot of `work_area` so the grab
//! code can capture it at the moment a drag starts without holding a lock
//! on the WM.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

pub type WindowId = u64;
pub type WorkspaceId = u32;
pub type OutputId = u32;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && py >= self.y && px <= self.x + self.w && py <= self.y + self.h
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SnapZone {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Maximize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WinState {
    Floating,
    Snapped(SnapZone),
    Maximized,
    Minimized,
    Fullscreen,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    pub id: WindowId,
    pub app: String,
    pub title: String,
    pub geom: Rect,
    pub state: WinState,
    pub workspace: WorkspaceId,
    pub z: u32,
    pub focused: bool,
    /// Set when a client asks for attention (xdg-activation request
    /// the compositor declines to auto-honour) and cleared when the
    /// window next becomes focused. The dock pulses its tile in an
    /// alert accent while this is `true`.
    #[serde(default)]
    pub urgent: bool,
}

/// Reserved-area struts for a single output. The dock contributes a
/// `bottom` strut, the panel a `top` strut, the OSK a transient `bottom`.
/// All measured in logical pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Struts {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

/// Combine every contributor layer's [`Struts`] into the effective
/// reservation an output exposes — per-edge max. The max rule is what
/// matches user intent: dock + OSK both want the bottom; the larger
/// (the OSK while open) is what should actually be reserved, and when
/// it leaves the smaller (the dock baseline) takes back over.
fn compose_struts(layers: Option<&HashMap<String, Struts>>) -> Struts {
    let Some(m) = layers else { return Struts::default() };
    let mut e = Struts::default();
    for s in m.values() {
        if s.top > e.top {
            e.top = s.top;
        }
        if s.right > e.right {
            e.right = s.right;
        }
        if s.bottom > e.bottom {
            e.bottom = s.bottom;
        }
        if s.left > e.left {
            e.left = s.left;
        }
    }
    e
}

/// A physical output (monitor). `bounds` is the full surface in logical
/// pixels; `work_area()` subtracts the struts that chrome has reserved.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Output {
    pub id: OutputId,
    pub bounds: Rect,
    pub struts: Struts,
    /// Compositor render scale for this output (1.0/2.0/3.0 today — bacak does
    /// integer scaling). Mirrored from the backend so the fractional-scale
    /// protocol can advertise a preferred scale without reaching the backend's
    /// smithay `Output`. Defaults to 1.0 until the backend sets it.
    pub scale: f64,
}

impl Output {
    pub fn work_area(&self) -> Rect {
        let b = self.bounds;
        let s = self.struts;
        Rect::new(
            b.x + s.left,
            b.y + s.top,
            (b.w - s.left - s.right).max(0.0),
            (b.h - s.top - s.bottom).max(0.0),
        )
    }
}

/// Logical workspace. `output` is which monitor this workspace is bound to;
/// switching workspaces only ever flips which workspace is *active* on
/// that output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    pub output: OutputId,
}

/// Snapshot of an output's work area. Used by the grab code: the snap math
/// captures the work area at grab-start so a mid-drag resolution change
/// (or a strut update from the OSK) doesn't tear the animation.
///
/// Keep this type `Copy + Send` — `grab.rs` stores it by value.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Monitor {
    pub work_area: Rect,
}

impl From<Output> for Monitor {
    fn from(o: Output) -> Self {
        Monitor { work_area: o.work_area() }
    }
}

/// How keyboard focus follows pointer activity.
///
/// * [`FocusPolicy::ClickToFocus`] — the X11 / macOS default. Pointer
///   motion never changes focus on its own; only a primary-button press on
///   a window's surface promotes it.
/// * [`FocusPolicy::FocusFollowsPointer`] — the classic Unix policy.
///   Whichever window sits under the pointer owns the keyboard. Useful for
///   tiled WMs and stylus-driven tablets where clicking just to type feels
///   wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FocusPolicy {
    #[default]
    ClickToFocus,
    FocusFollowsPointer,
}

#[derive(Debug, Error, Serialize)]
pub enum WmError {
    #[error("window not found: {0}")]
    NotFound(WindowId),
    #[error("workspace not found: {0}")]
    NoWorkspace(WorkspaceId),
    #[error("output not found: {0}")]
    NoOutput(OutputId),
    #[error("cannot remove the only remaining output")]
    LastOutput,
}

pub type Result<T> = std::result::Result<T, WmError>;

// ---------------------------------------------------------------------------
// Snap math
// ---------------------------------------------------------------------------

pub const SNAP_EDGE_PX: f32 = 24.0;

/// Fixed number of virtual desktops (workspaces) per output. The set is
/// created once at `add_output` and never grows — switching clamps into it.
pub const WORKSPACE_COUNT: usize = 4;

pub fn hit_test_snap(px: f32, py: f32, m: Monitor) -> Option<SnapZone> {
    let r = m.work_area;
    let left   = px <= r.x + SNAP_EDGE_PX;
    let right  = px >= r.x + r.w - SNAP_EDGE_PX;
    let top    = py <= r.y + SNAP_EDGE_PX;
    let bottom = py >= r.y + r.h - SNAP_EDGE_PX;

    match (top, bottom, left, right) {
        (true,  false, true,  false) => Some(SnapZone::TopLeft),
        (true,  false, false, true ) => Some(SnapZone::TopRight),
        (false, true,  true,  false) => Some(SnapZone::BottomLeft),
        (false, true,  false, true ) => Some(SnapZone::BottomRight),
        (true,  false, false, false) => Some(SnapZone::Maximize),
        (false, true,  false, false) => Some(SnapZone::Bottom),
        (false, false, true,  false) => Some(SnapZone::Left),
        (false, false, false, true ) => Some(SnapZone::Right),
        _ => None,
    }
}

pub fn rect_for_zone(zone: SnapZone, m: Monitor) -> Rect {
    let r = m.work_area;
    let (hw, hh) = (r.w / 2.0, r.h / 2.0);
    match zone {
        SnapZone::Left        => Rect::new(r.x,        r.y,        hw, r.h),
        SnapZone::Right       => Rect::new(r.x + hw,   r.y,        hw, r.h),
        SnapZone::Top         => Rect::new(r.x,        r.y,        r.w, hh),
        SnapZone::Bottom      => Rect::new(r.x,        r.y + hh,   r.w, hh),
        SnapZone::TopLeft     => Rect::new(r.x,        r.y,        hw, hh),
        SnapZone::TopRight    => Rect::new(r.x + hw,   r.y,        hw, hh),
        SnapZone::BottomLeft  => Rect::new(r.x,        r.y + hh,   hw, hh),
        SnapZone::BottomRight => Rect::new(r.x + hw,   r.y + hh,   hw, hh),
        SnapZone::Maximize    => r,
    }
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct WindowManager {
    inner: Arc<WmInner>,
}

#[derive(Default)]
struct WmInner {
    next_window_id: AtomicU64,
    next_z: AtomicU64,
    next_workspace_id: AtomicU32,
    next_output_id: AtomicU32,
    windows: RwLock<HashMap<WindowId, Window>>,
    outputs: RwLock<HashMap<OutputId, Output>>,
    workspaces: RwLock<HashMap<WorkspaceId, Workspace>>,
    /// For every output, which of its workspaces is currently displayed.
    active_per_output: RwLock<HashMap<OutputId, WorkspaceId>>,
    primary_output: RwLock<Option<OutputId>>,
    /// Per-output struts split into named layers (dock / osk / panel /
    /// the legacy `"manual"` slot for [`set_struts`]). The composed
    /// effective struts (per-edge max across layers) is what callers
    /// see via [`Output::struts`] — recomputed on every layer write.
    /// Layers let an edge swap on one contributor (e.g. dock) not
    /// trample another's still-live reservation (e.g. OSK).
    strut_layers: RwLock<HashMap<OutputId, HashMap<String, Struts>>>,
}

impl WindowManager {
    /// Boot the WM with a single default monitor. A workspace `0` is
    /// created on output `0` and made active — this matches the
    /// single-monitor session that early callers (CLI demo, headless
    /// tests) assume.
    pub fn new(default_monitor: Monitor) -> Self {
        let inner = WmInner::default();
        let wm = Self { inner: Arc::new(inner) };
        let oid = wm.add_output(default_monitor.work_area);
        debug_assert_eq!(oid, 0);
        // `add_output` already created and activated workspace 0 on it.
        wm
    }

    // ----- outputs -----------------------------------------------------

    /// Register a new output with the given bounds (logical pixels, zero
    /// struts) and seed it with the fixed set of [`WORKSPACE_COUNT`]
    /// workspaces, the first made active. Returns the new [`OutputId`]. The
    /// first output added also becomes the primary.
    pub fn add_output(&self, bounds: Rect) -> OutputId {
        let id = self.inner.next_output_id.fetch_add(1, Ordering::Relaxed);
        let output = Output { id, bounds, struts: Struts::default(), scale: 1.0 };
        self.inner.outputs.write().insert(id, output);

        // Seed the fixed set of virtual desktops; the first is made active.
        let ws_id = self.add_workspace_inner(id, "Workspace 1".to_string());
        self.inner.active_per_output.write().insert(id, ws_id);
        for n in 2..=WORKSPACE_COUNT {
            self.add_workspace_inner(id, format!("Workspace {n}"));
        }

        let mut primary = self.inner.primary_output.write();
        if primary.is_none() {
            *primary = Some(id);
        }
        id
    }

    /// Mirror a backend-chosen render scale onto the WM output (for the
    /// fractional-scale protocol). No-op if the output is unknown.
    pub fn set_output_scale(&self, id: OutputId, scale: f64) {
        if let Some(o) = self.inner.outputs.write().get_mut(&id) {
            o.scale = scale.max(1.0);
        }
    }

    /// Render scale of `id`, or `1.0` if unknown.
    pub fn output_scale(&self, id: OutputId) -> f64 {
        self.inner
            .outputs
            .read()
            .get(&id)
            .map(|o| o.scale)
            .unwrap_or(1.0)
    }

    /// Remove an output. All its workspaces migrate to the current
    /// primary output (windows on those workspaces keep their workspace
    /// id, but the workspace itself is reparented). Fails if this is the
    /// only output, or if the requested id is the primary while there is
    /// no other output to promote.
    pub fn remove_output(&self, id: OutputId) -> Result<()> {
        let mut outputs = self.inner.outputs.write();
        if !outputs.contains_key(&id) {
            return Err(WmError::NoOutput(id));
        }
        if outputs.len() <= 1 {
            return Err(WmError::LastOutput);
        }

        // Decide where orphaned workspaces should land.
        let mut primary_guard = self.inner.primary_output.write();
        let was_primary = matches!(*primary_guard, Some(p) if p == id);
        let new_primary = if was_primary {
            outputs
                .keys()
                .copied()
                .find(|o| *o != id)
                .expect("checked outputs.len() > 1 above")
        } else {
            primary_guard.expect("primary must be set once any output exists")
        };

        // Reparent workspaces.
        let mut workspaces = self.inner.workspaces.write();
        for ws in workspaces.values_mut() {
            if ws.output == id {
                ws.output = new_primary;
            }
        }

        // Drop the active-pointer for this output. (We don't auto-promote
        // its previously-active workspace onto the new home — the caller
        // can do that explicitly with `switch_workspace_on`.)
        self.inner.active_per_output.write().remove(&id);

        outputs.remove(&id);
        if was_primary {
            *primary_guard = Some(new_primary);
        }
        Ok(())
    }

    pub fn output(&self, id: OutputId) -> Option<Output> {
        self.inner.outputs.read().get(&id).copied()
    }

    pub fn outputs(&self) -> Vec<Output> {
        let mut v: Vec<Output> = self.inner.outputs.read().values().copied().collect();
        v.sort_by_key(|o| o.id);
        v
    }

    pub fn primary_output(&self) -> Option<OutputId> {
        *self.inner.primary_output.read()
    }

    pub fn set_primary_output(&self, id: OutputId) -> Result<()> {
        if !self.inner.outputs.read().contains_key(&id) {
            return Err(WmError::NoOutput(id));
        }
        *self.inner.primary_output.write() = Some(id);
        Ok(())
    }

    /// Set the dock/OSK/panel/etc. contribution for an output under
    /// `layer`. The effective struts (what
    /// [`work_area`](Output::work_area) and snap math see) is the
    /// per-edge max across every layer for that output. A second call
    /// with the same `layer` replaces that contribution; the others
    /// (e.g. a live OSK) are left intact.
    pub fn set_strut_layer(
        &self,
        id: OutputId,
        layer: &str,
        struts: Struts,
    ) -> Result<()> {
        if !self.inner.outputs.read().contains_key(&id) {
            return Err(WmError::NoOutput(id));
        }
        let mut layers = self.inner.strut_layers.write();
        layers.entry(id).or_default().insert(layer.to_string(), struts);
        let composed = compose_struts(layers.get(&id).map(|m| m as _));
        drop(layers);
        let mut outputs = self.inner.outputs.write();
        if let Some(o) = outputs.get_mut(&id) {
            o.struts = composed;
        }
        Ok(())
    }

    /// Drop `layer`'s contribution to an output's struts (e.g. OSK
    /// closes, panel hides). The other layers' values remain — the
    /// effective struts simply recompose without it.
    pub fn clear_strut_layer(&self, id: OutputId, layer: &str) -> Result<()> {
        if !self.inner.outputs.read().contains_key(&id) {
            return Err(WmError::NoOutput(id));
        }
        let mut layers = self.inner.strut_layers.write();
        if let Some(m) = layers.get_mut(&id) {
            m.remove(layer);
        }
        let composed = compose_struts(layers.get(&id).map(|m| m as _));
        drop(layers);
        let mut outputs = self.inner.outputs.write();
        if let Some(o) = outputs.get_mut(&id) {
            o.struts = composed;
        }
        Ok(())
    }

    /// Backward-compatible whole-struts setter — writes into the
    /// `"manual"` layer so older callers (tests, ad-hoc tools) keep
    /// working unchanged. Pass `Struts::default()` to clear it.
    pub fn set_struts(&self, id: OutputId, struts: Struts) -> Result<()> {
        self.set_strut_layer(id, "manual", struts)
    }

    /// Move/resize an output's physical bounds (e.g. monitor hot-plug
    /// resolution change). Existing windows are *not* re-fitted — the
    /// session layer is expected to re-snap them if it wants to.
    pub fn set_output_bounds(&self, id: OutputId, bounds: Rect) -> Result<()> {
        let mut outputs = self.inner.outputs.write();
        let o = outputs.get_mut(&id).ok_or(WmError::NoOutput(id))?;
        o.bounds = bounds;
        Ok(())
    }

    // ----- workspaces --------------------------------------------------

    /// Create a new workspace on the given output. The first workspace
    /// on an output is created automatically by `add_output`; call this
    /// to add more.
    pub fn add_workspace(&self, output: OutputId, name: impl Into<String>) -> Result<WorkspaceId> {
        if !self.inner.outputs.read().contains_key(&output) {
            return Err(WmError::NoOutput(output));
        }
        Ok(self.add_workspace_inner(output, name.into()))
    }

    fn add_workspace_inner(&self, output: OutputId, name: String) -> WorkspaceId {
        let id = self.inner.next_workspace_id.fetch_add(1, Ordering::Relaxed);
        self.inner
            .workspaces
            .write()
            .insert(id, Workspace { id, name, output });
        id
    }

    pub fn workspace(&self, id: WorkspaceId) -> Option<Workspace> {
        self.inner.workspaces.read().get(&id).cloned()
    }

    pub fn workspaces(&self) -> Vec<Workspace> {
        let mut v: Vec<Workspace> = self.inner.workspaces.read().values().cloned().collect();
        v.sort_by_key(|w| w.id);
        v
    }

    pub fn workspaces_for(&self, output: OutputId) -> Vec<Workspace> {
        let mut v: Vec<Workspace> = self
            .inner
            .workspaces
            .read()
            .values()
            .filter(|w| w.output == output)
            .cloned()
            .collect();
        v.sort_by_key(|w| w.id);
        v
    }

    /// Move a workspace to a different output. The workspace's windows
    /// follow — their workspace id doesn't change, only the workspace
    /// itself is reparented. If the workspace was active on its old
    /// output, that output's active workspace falls back to any other
    /// workspace still living there (or `None` if there are none).
    pub fn move_workspace_to_output(&self, ws: WorkspaceId, output: OutputId) -> Result<()> {
        if !self.inner.outputs.read().contains_key(&output) {
            return Err(WmError::NoOutput(output));
        }
        let old_output;
        {
            let mut workspaces = self.inner.workspaces.write();
            let w = workspaces.get_mut(&ws).ok_or(WmError::NoWorkspace(ws))?;
            old_output = w.output;
            if old_output == output {
                return Ok(());
            }
            w.output = output;
        }

        // If `ws` was active on its old output, pick a replacement.
        let mut active = self.inner.active_per_output.write();
        if matches!(active.get(&old_output), Some(a) if *a == ws) {
            let workspaces = self.inner.workspaces.read();
            let replacement = workspaces
                .values()
                .filter(|w| w.output == old_output)
                .map(|w| w.id)
                .min();
            match replacement {
                Some(r) => {
                    active.insert(old_output, r);
                }
                None => {
                    active.remove(&old_output);
                }
            }
        }
        Ok(())
    }

    /// Which workspace is currently displayed on `output`.
    pub fn active_workspace_on(&self, output: OutputId) -> Option<WorkspaceId> {
        self.inner.active_per_output.read().get(&output).copied()
    }

    /// Convenience: active workspace on the primary output. Returns `0`
    /// if no primary or no active is set yet — preserves the old
    /// single-monitor contract for callers that haven't been updated.
    pub fn active_workspace(&self) -> WorkspaceId {
        self.primary_output()
            .and_then(|p| self.active_workspace_on(p))
            .unwrap_or(0)
    }

    /// Switch the workspace shown on `output`. The workspace must
    /// already be bound to that output.
    pub fn switch_workspace_on(&self, output: OutputId, ws: WorkspaceId) -> Result<()> {
        let workspaces = self.inner.workspaces.read();
        let w = workspaces.get(&ws).ok_or(WmError::NoWorkspace(ws))?;
        if w.output != output {
            return Err(WmError::NoOutput(output));
        }
        if !self.inner.outputs.read().contains_key(&output) {
            return Err(WmError::NoOutput(output));
        }
        drop(workspaces);
        self.inner.active_per_output.write().insert(output, ws);
        Ok(())
    }

    /// Switch a workspace into view on whichever output it lives on.
    /// Equivalent to `switch_workspace_on(workspace.output, ws)`.
    pub fn switch_workspace(&self, ws: WorkspaceId) -> Result<()> {
        let w = self
            .inner
            .workspaces
            .read()
            .get(&ws)
            .cloned()
            .ok_or(WmError::NoWorkspace(ws))?;
        self.switch_workspace_on(w.output, ws)
    }

    // ----- windows -----------------------------------------------------

    /// Find the workspace a new window should land on. Honours the
    /// active workspace of the primary output; if for some reason
    /// nothing is active yet, falls back to the lowest workspace id.
    fn default_target_workspace(&self) -> WorkspaceId {
        if let Some(primary) = self.primary_output() {
            if let Some(ws) = self.active_workspace_on(primary) {
                return ws;
            }
        }
        self.inner
            .workspaces
            .read()
            .keys()
            .copied()
            .min()
            .unwrap_or(0)
    }

    pub fn open(&self, app: impl Into<String>, title: impl Into<String>, geom: Rect) -> WindowId {
        let id = self.inner.next_window_id.fetch_add(1, Ordering::Relaxed) + 1;
        let z  = self.next_z();
        let ws = self.default_target_workspace();
        let w = Window {
            id,
            app: app.into(),
            title: title.into(),
            geom,
            state: WinState::Floating,
            workspace: ws,
            z: z as u32,
            focused: false,
            urgent: false,
        };
        self.inner.windows.write().insert(id, w);
        self.focus(id).ok();
        id
    }

    /// Update a window's app id / title. Empty incoming values are
    /// ignored (clients often commit a title before an app id or vice
    /// versa; we don't want a transient empty string to wipe a good
    /// one). Returns `true` iff something actually changed — callers
    /// use this to skip needless redraws / cache rebuilds.
    pub fn set_meta(
        &self,
        id: WindowId,
        app: Option<&str>,
        title: Option<&str>,
    ) -> Result<bool> {
        let mut g = self.inner.windows.write();
        let w = g.get_mut(&id).ok_or(WmError::NotFound(id))?;
        let mut changed = false;
        if let Some(app) = app {
            if !app.is_empty() && w.app != app {
                w.app = app.to_string();
                changed = true;
            }
        }
        if let Some(title) = title {
            if !title.is_empty() && w.title != title {
                w.title = title.to_string();
                changed = true;
            }
        }
        // If app_id was never received, derive one from the window title so the
        // dock can look up an icon (e.g. "Altay" → "altay", "Firefox ESR" → "firefox").
        if w.app.is_empty() && !w.title.is_empty() {
            let derived = w.title
                .split_whitespace()
                .next()
                .unwrap_or(&w.title)
                .to_lowercase();
            if !derived.is_empty() {
                w.app = derived;
                changed = true;
            }
        }
        Ok(changed)
    }

    pub fn close(&self, id: WindowId) -> Result<()> {
        self.inner
            .windows
            .write()
            .remove(&id)
            .map(|_| ())
            .ok_or(WmError::NotFound(id))
    }

    pub fn get(&self, id: WindowId) -> Result<Window> {
        self.inner
            .windows
            .read()
            .get(&id)
            .cloned()
            .ok_or(WmError::NotFound(id))
    }

    /// Move a window to a different workspace. The window's geometry is
    /// left untouched — the caller may want to re-snap it against the
    /// destination output's work area.
    pub fn move_window_to_workspace(&self, id: WindowId, ws: WorkspaceId) -> Result<()> {
        if !self.inner.workspaces.read().contains_key(&ws) {
            return Err(WmError::NoWorkspace(ws));
        }
        let mut windows = self.inner.windows.write();
        let w = windows.get_mut(&id).ok_or(WmError::NotFound(id))?;
        w.workspace = ws;
        if w.focused {
            // Moving a focused window to an off-screen workspace shouldn't
            // leave keyboard focus dangling.
            w.focused = false;
        }
        Ok(())
    }

    pub fn list_active(&self) -> Vec<Window> {
        let ws = self.active_workspace();
        let g = self.inner.windows.read();
        let mut v: Vec<Window> = g.values().filter(|w| w.workspace == ws).cloned().collect();
        v.sort_by_key(|w| w.z);
        v
    }

    /// Windows on a specific workspace, ascending z. Visibility (whether
    /// the workspace is currently active anywhere) is not considered —
    /// callers like the slide renderer need to draw an outgoing
    /// workspace's windows even after its output stopped marking it
    /// active.
    pub fn windows_on_workspace(&self, ws: WorkspaceId) -> Vec<Window> {
        let g = self.inner.windows.read();
        let mut v: Vec<Window> = g.values().filter(|w| w.workspace == ws).cloned().collect();
        v.sort_by_key(|w| w.z);
        v
    }

    /// Every window, all workspaces/outputs, ascending z. Used by
    /// session persistence to snapshot the full layout.
    pub fn all_windows(&self) -> Vec<Window> {
        let g = self.inner.windows.read();
        let mut v: Vec<Window> = g.values().cloned().collect();
        v.sort_by_key(|w| w.z);
        v
    }

    /// All windows whose workspace is currently active on *some* output.
    /// This is the set the renderer should actually draw on a
    /// multi-monitor session.
    pub fn list_visible(&self) -> Vec<Window> {
        let active: std::collections::HashSet<WorkspaceId> =
            self.inner.active_per_output.read().values().copied().collect();
        let g = self.inner.windows.read();
        let mut v: Vec<Window> = g
            .values()
            .filter(|w| active.contains(&w.workspace))
            .cloned()
            .collect();
        v.sort_by_key(|w| w.z);
        v
    }

    /// Topmost window under a *compositor-global* point, taking the
    /// active workspace of the output that owns that point. Returns
    /// `None` if the point lands on bare desktop, in the void between
    /// outputs, or on a minimized window — the cursor should fall
    /// through to the wallpaper in all three cases.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<WindowId> {
        let output = self.output_at(x, y)?;
        let ws = self.active_workspace_on(output)?;
        let g = self.inner.windows.read();
        g.values()
            .filter(|w| w.workspace == ws)
            .filter(|w| !matches!(w.state, WinState::Minimized))
            .filter(|w| w.geom.contains(x, y))
            .max_by_key(|w| w.z)
            .map(|w| w.id)
    }

    // ----- multi-output pointer geometry -------------------------------

    /// Axis-aligned bounding box of every output, in WM-global logical
    /// pixels. The cursor is clamped to this rect, and tablets /
    /// touchscreens project their `[0,1]` device coords onto it. For a
    /// single-output session this is just that output's bounds; on
    /// multi-output the hull may include voids (L-shaped layouts) — the
    /// cursor can pass through them, which is the same behaviour every
    /// stitched-display WM has.
    pub fn desktop_bounds(&self) -> Rect {
        let outs = self.inner.outputs.read();
        if outs.is_empty() {
            return Rect::default();
        }
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for o in outs.values() {
            min_x = min_x.min(o.bounds.x);
            min_y = min_y.min(o.bounds.y);
            max_x = max_x.max(o.bounds.x + o.bounds.w);
            max_y = max_y.max(o.bounds.y + o.bounds.h);
        }
        Rect::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }

    /// The output that contains `(x, y)`, if any. Seams between outputs
    /// are half-open on the right/bottom edges so the boundary point
    /// `(1920, 0)` belongs unambiguously to the output that starts at
    /// `x=1920`, not the one that ends there.
    pub fn output_at(&self, x: f32, y: f32) -> Option<OutputId> {
        // Sort by id for deterministic results — HashMap iteration is
        // arbitrary, and the hit-test contract should be stable.
        let outs_guard = self.inner.outputs.read();
        let mut outs: Vec<&Output> = outs_guard.values().collect();
        outs.sort_by_key(|o| o.id);
        for o in outs {
            let b = o.bounds;
            if x >= b.x && y >= b.y && x < b.x + b.w && y < b.y + b.h {
                return Some(o.id);
            }
        }
        None
    }

    /// Clamp a global pointer position to the desktop bounding rect so
    /// relative-pointer motion never escapes the visible desktop. The
    /// returned point may still land in a void between outputs on
    /// non-rectangular layouts — that's fine; `output_at` will then
    /// return `None` and hit-tests fall through to the wallpaper.
    pub fn clamp_to_desktop(&self, x: f32, y: f32) -> (f32, f32) {
        let db = self.desktop_bounds();
        let cx = x.clamp(db.x, db.x + db.w);
        let cy = y.clamp(db.y, db.y + db.h);
        (cx, cy)
    }

    pub fn r#move(&self, id: WindowId, geom: Rect) -> Result<()> {
        let mut g = self.inner.windows.write();
        let w = g.get_mut(&id).ok_or(WmError::NotFound(id))?;
        w.geom = geom;
        if matches!(w.state, WinState::Snapped(_) | WinState::Maximized) {
            w.state = WinState::Floating;
        }
        Ok(())
    }

    /// Set window geometry without changing the window state. Used when the
    /// compositor needs to fine-tune the geometry of a snapped/maximized window
    /// (e.g. reserving the title-bar strip) without losing the Maximized state.
    pub fn set_geom(&self, id: WindowId, geom: Rect) -> Result<()> {
        let mut g = self.inner.windows.write();
        let w = g.get_mut(&id).ok_or(WmError::NotFound(id))?;
        w.geom = geom;
        Ok(())
    }

    pub fn snap(&self, id: WindowId, zone: SnapZone) -> Result<Rect> {
        // Resolve the window's output (via its workspace) first; then
        // re-take the windows lock as a writer to mutate. Holding both at
        // once would deadlock on the same RwLock if the read guard
        // outlives the write.
        let monitor = self
            .monitor_for_window(id)
            .ok_or(WmError::NotFound(id))?;
        let r = rect_for_zone(zone, monitor);

        let mut g = self.inner.windows.write();
        let w = g.get_mut(&id).ok_or(WmError::NotFound(id))?;
        w.geom = r;
        w.state = if zone == SnapZone::Maximize {
            WinState::Maximized
        } else {
            WinState::Snapped(zone)
        };
        Ok(r)
    }

    pub fn minimize(&self, id: WindowId) -> Result<()> {
        let mut g = self.inner.windows.write();
        let w = g.get_mut(&id).ok_or(WmError::NotFound(id))?;
        w.state = WinState::Minimized;
        w.focused = false;
        Ok(())
    }

    /// Put a window into fullscreen at `bounds` (the full output, struts
    /// included — unlike [`snap`](Self::snap) with `Maximize`, which uses
    /// the work area). The caller restores the prior geometry on exit.
    pub fn fullscreen(&self, id: WindowId, bounds: Rect) -> Result<()> {
        let mut g = self.inner.windows.write();
        let w = g.get_mut(&id).ok_or(WmError::NotFound(id))?;
        w.geom = bounds;
        w.state = WinState::Fullscreen;
        Ok(())
    }

    /// Topmost non-minimized window on `ws`, or `None` if the workspace
    /// has nothing focusable. Used by the auto-focus path after a window
    /// is moved or closed so keyboard input never falls into a void
    /// the user can't escape from with a click.
    pub fn next_focus_candidate(&self, ws: WorkspaceId) -> Option<WindowId> {
        let g = self.inner.windows.read();
        g.values()
            .filter(|w| w.workspace == ws)
            .filter(|w| !matches!(w.state, WinState::Minimized))
            .max_by_key(|w| w.z)
            .map(|w| w.id)
    }

    /// Topmost *minimized* window on `ws`, or `None`. `z` reflects the
    /// last-raised order, so the highest-z minimized window is a decent
    /// "most recently used before it was hidden" pick for a
    /// keyboard-driven un-minimise.
    pub fn topmost_minimized_on(&self, ws: WorkspaceId) -> Option<WindowId> {
        let g = self.inner.windows.read();
        g.values()
            .filter(|w| w.workspace == ws)
            .filter(|w| matches!(w.state, WinState::Minimized))
            .max_by_key(|w| w.z)
            .map(|w| w.id)
    }

    /// Clear `focused` on every window. Used when the auto-focus path
    /// has no candidate (e.g. switching to an empty workspace) — without
    /// this, the previously-focused window on the old workspace would
    /// silently keep its flag and confuse downstream consumers that
    /// peek at WM state for "who has keyboard focus right now".
    pub fn clear_focus(&self) {
        let mut g = self.inner.windows.write();
        for w in g.values_mut() {
            w.focused = false;
        }
    }

    pub fn focus(&self, id: WindowId) -> Result<()> {
        let z = self.next_z() as u32;
        let mut g = self.inner.windows.write();
        if !g.contains_key(&id) {
            return Err(WmError::NotFound(id));
        }
        for w in g.values_mut() {
            w.focused = false;
        }
        let w = g.get_mut(&id).unwrap();
        w.focused = true;
        // Focus is the canonical "the user looked at this" signal,
        // so it clears any pending urgency.
        w.urgent = false;
        w.z = z;
        if matches!(w.state, WinState::Minimized) {
            w.state = WinState::Floating;
        }
        Ok(())
    }

    /// Raise `id` to the top of the stack (fresh highest z) WITHOUT changing
    /// which window is focused. Used to keep transient/modal dialogs stacked
    /// above their parent when the parent is raised — otherwise a "Save
    /// changes?" dialog hides behind its window and the app can't be closed.
    pub fn raise(&self, id: WindowId) -> Result<()> {
        let z = self.next_z() as u32;
        let mut g = self.inner.windows.write();
        let w = g.get_mut(&id).ok_or(WmError::NotFound(id))?;
        w.z = z;
        Ok(())
    }

    /// Mark `id` urgent (or clear it). No-op when the window is the
    /// currently-focused one — focusing already clears urgency, and a
    /// client shouldn't be able to "request attention" for the window
    /// the user is actively in.
    pub fn set_urgent(&self, id: WindowId, flag: bool) -> Result<()> {
        let mut g = self.inner.windows.write();
        let w = g.get_mut(&id).ok_or(WmError::NotFound(id))?;
        if w.focused && flag {
            return Ok(());
        }
        w.urgent = flag;
        Ok(())
    }

    // ----- monitor lookups for grabs -----------------------------------

    /// The [`Output`] that hosts `window`'s workspace, or `None` if the
    /// window doesn't exist (or its workspace has been orphaned).
    pub fn output_for_window(&self, id: WindowId) -> Option<Output> {
        let ws = self.inner.windows.read().get(&id)?.workspace;
        let output_id = self.inner.workspaces.read().get(&ws)?.output;
        self.inner.outputs.read().get(&output_id).copied()
    }

    /// Snap-math snapshot for a window's output. Use this when starting
    /// a grab so the work area is captured once and survives mid-drag
    /// strut changes.
    pub fn monitor_for_window(&self, id: WindowId) -> Option<Monitor> {
        self.output_for_window(id).map(Monitor::from)
    }

    /// Snap-math snapshot for the primary output's current work area —
    /// used by callers that don't have a specific window in hand (e.g.
    /// launching apps, the wallpaper drop target).
    pub fn active_monitor(&self) -> Option<Monitor> {
        let primary = self.primary_output()?;
        self.inner.outputs.read().get(&primary).copied().map(Monitor::from)
    }

    /// Snap-math snapshot for a specific workspace's output. Useful for
    /// pre-flight checks before moving a window to that workspace.
    pub fn monitor(&self, ws: WorkspaceId) -> Option<Monitor> {
        let output_id = self.inner.workspaces.read().get(&ws)?.output;
        self.inner.outputs.read().get(&output_id).copied().map(Monitor::from)
    }

    fn next_z(&self) -> u64 {
        self.inner.next_z.fetch_add(1, Ordering::Relaxed) + 1
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Monitor {
        Monitor { work_area: Rect::new(0.0, 0.0, 1440.0, 800.0) }
    }

    #[test]
    fn hit_test_finds_left_edge() {
        let m = screen();
        assert_eq!(hit_test_snap(2.0, 400.0, m), Some(SnapZone::Left));
        assert_eq!(hit_test_snap(720.0, 2.0, m), Some(SnapZone::Maximize));
        assert_eq!(hit_test_snap(2.0, 2.0, m), Some(SnapZone::TopLeft));
        assert_eq!(hit_test_snap(700.0, 400.0, m), None);
    }

    #[test]
    fn rect_for_zone_left_half() {
        let m = screen();
        let r = rect_for_zone(SnapZone::Left, m);
        assert_eq!(r, Rect::new(0.0, 0.0, 720.0, 800.0));
        assert_eq!(rect_for_zone(SnapZone::Maximize, m), m.work_area);
    }

    #[test]
    fn open_focus_snap_close() {
        let wm = WindowManager::new(screen());
        let a = wm.open("firefox", "FF", Rect::new(100.0, 100.0, 800.0, 600.0));
        let b = wm.open("files",   "Files", Rect::new(140.0, 140.0, 800.0, 600.0));

        let wa = wm.get(a).unwrap();
        let wb = wm.get(b).unwrap();
        assert!(wb.z > wa.z);
        assert!(wb.focused && !wa.focused);

        wm.focus(a).unwrap();
        assert!(wm.get(a).unwrap().focused);

        let r = wm.snap(a, SnapZone::Left).unwrap();
        assert_eq!(wm.get(a).unwrap().geom, r);
        assert_eq!(wm.get(a).unwrap().state, WinState::Snapped(SnapZone::Left));

        wm.close(b).unwrap();
        assert!(matches!(wm.get(b), Err(WmError::NotFound(_))));
    }

    #[test]
    fn manual_move_unsnaps() {
        let wm = WindowManager::new(screen());
        let id = wm.open("x", "x", Rect::new(0.0, 0.0, 200.0, 200.0));
        wm.snap(id, SnapZone::Right).unwrap();
        wm.r#move(id, Rect::new(100.0, 100.0, 400.0, 300.0)).unwrap();
        assert_eq!(wm.get(id).unwrap().state, WinState::Floating);
    }

    #[test]
    fn hit_test_picks_topmost_under_pointer() {
        let wm = WindowManager::new(screen());
        let a = wm.open("a", "A", Rect::new(0.0,   0.0, 400.0, 400.0));
        let b = wm.open("b", "B", Rect::new(100.0, 100.0, 400.0, 400.0));
        // `b` was opened last → it has the higher z, so it wins over `a` on
        // the overlapping region.
        assert_eq!(wm.hit_test(200.0, 200.0), Some(b));
        // Outside `b`, inside `a`.
        assert_eq!(wm.hit_test(50.0,  50.0),  Some(a));
        // Bare wallpaper.
        assert_eq!(wm.hit_test(1000.0, 700.0), None);
    }

    #[test]
    fn hit_test_skips_minimized() {
        let wm = WindowManager::new(screen());
        let a = wm.open("a", "A", Rect::new(0.0, 0.0, 400.0, 400.0));
        wm.minimize(a).unwrap();
        assert_eq!(wm.hit_test(200.0, 200.0), None);
    }

    // -- multi-output / workspace ------------------------------------------

    #[test]
    fn struts_shrink_work_area() {
        let wm = WindowManager::new(screen());
        let primary = wm.primary_output().unwrap();
        wm.set_struts(primary, Struts { bottom: 64.0, ..Default::default() })
            .unwrap();
        let m = wm.active_monitor().unwrap();
        assert_eq!(m.work_area, Rect::new(0.0, 0.0, 1440.0, 800.0 - 64.0));
    }

    #[test]
    fn snap_left_after_dock_strut_avoids_dock_band() {
        let wm = WindowManager::new(screen());
        let primary = wm.primary_output().unwrap();
        wm.set_struts(primary, Struts { bottom: 64.0, ..Default::default() })
            .unwrap();
        let id = wm.open("x", "x", Rect::new(0.0, 0.0, 200.0, 200.0));
        let r = wm.snap(id, SnapZone::Left).unwrap();
        // Half-screen left, but the bottom 64 px belong to the dock.
        assert_eq!(r, Rect::new(0.0, 0.0, 720.0, 800.0 - 64.0));
    }

    #[test]
    fn add_second_output_yields_independent_workspaces() {
        let wm = WindowManager::new(screen());
        let secondary = wm.add_output(Rect::new(1440.0, 0.0, 1920.0, 1080.0));

        // Each output got its own fixed set of workspaces, with disjoint ids.
        let wss_primary = wm.workspaces_for(wm.primary_output().unwrap());
        let wss_secondary = wm.workspaces_for(secondary);
        assert_eq!(wss_primary.len(), WORKSPACE_COUNT);
        assert_eq!(wss_secondary.len(), WORKSPACE_COUNT);
        let overlap = wss_primary
            .iter()
            .any(|p| wss_secondary.iter().any(|s| s.id == p.id));
        assert!(!overlap, "workspace ids must not be shared across outputs");

        // Active workspace per output is that output's first seed.
        let mut sec_sorted = wss_secondary.clone();
        sec_sorted.sort_by_key(|w| w.id);
        assert_eq!(wm.active_workspace_on(secondary), Some(sec_sorted[0].id));
    }

    #[test]
    fn switch_workspace_per_output_is_independent() {
        let wm = WindowManager::new(screen());
        let primary = wm.primary_output().unwrap();
        let ws_b = wm.add_workspace(primary, "B").unwrap();
        let ws_a = wm.active_workspace_on(primary).unwrap();

        // Open a window on the active workspace, then switch.
        let id = wm.open("x", "x", Rect::new(10.0, 10.0, 100.0, 100.0));
        assert_eq!(wm.get(id).unwrap().workspace, ws_a);

        wm.switch_workspace_on(primary, ws_b).unwrap();
        // The window still belongs to ws_a; list_active() now hides it.
        assert!(wm.list_active().iter().all(|w| w.id != id));
        assert_eq!(wm.get(id).unwrap().workspace, ws_a);
    }

    #[test]
    fn move_window_across_workspaces() {
        let wm = WindowManager::new(screen());
        let primary = wm.primary_output().unwrap();
        let ws_b = wm.add_workspace(primary, "B").unwrap();
        let id = wm.open("x", "x", Rect::new(10.0, 10.0, 100.0, 100.0));

        wm.move_window_to_workspace(id, ws_b).unwrap();
        assert_eq!(wm.get(id).unwrap().workspace, ws_b);
        // The window is no longer on the active workspace.
        assert!(wm.list_active().iter().all(|w| w.id != id));
        // Switching to ws_b brings it back into view.
        wm.switch_workspace_on(primary, ws_b).unwrap();
        assert!(wm.list_active().iter().any(|w| w.id == id));
    }

    #[test]
    fn monitor_for_window_follows_workspace_to_other_output() {
        let wm = WindowManager::new(screen());
        let secondary = wm.add_output(Rect::new(1440.0, 0.0, 1920.0, 1080.0));
        let ws_secondary = wm.active_workspace_on(secondary).unwrap();

        let id = wm.open("x", "x", Rect::new(10.0, 10.0, 100.0, 100.0));
        // Sanity: window's monitor matches the primary work area.
        assert_eq!(wm.monitor_for_window(id).unwrap().work_area, screen().work_area);

        wm.move_window_to_workspace(id, ws_secondary).unwrap();
        // Now snap math for this window resolves to the secondary output.
        let m = wm.monitor_for_window(id).unwrap();
        assert_eq!(m.work_area, Rect::new(1440.0, 0.0, 1920.0, 1080.0));
    }

    #[test]
    fn snap_uses_window_output_not_primary() {
        let wm = WindowManager::new(screen());
        let secondary = wm.add_output(Rect::new(1440.0, 0.0, 1920.0, 1080.0));
        let ws_secondary = wm.active_workspace_on(secondary).unwrap();

        let id = wm.open("x", "x", Rect::new(10.0, 10.0, 100.0, 100.0));
        wm.move_window_to_workspace(id, ws_secondary).unwrap();
        let r = wm.snap(id, SnapZone::Right).unwrap();
        // Right-half of the *secondary* output, not the primary.
        assert_eq!(r, Rect::new(1440.0 + 960.0, 0.0, 960.0, 1080.0));
    }

    #[test]
    fn move_workspace_to_other_output() {
        let wm = WindowManager::new(screen());
        let secondary = wm.add_output(Rect::new(1440.0, 0.0, 1920.0, 1080.0));
        let primary = wm.primary_output().unwrap();
        let ws_b = wm.add_workspace(primary, "B").unwrap();

        let id = wm.open("x", "x", Rect::new(10.0, 10.0, 100.0, 100.0));
        wm.move_window_to_workspace(id, ws_b).unwrap();
        // Initially ws_b lives on the primary.
        assert_eq!(wm.workspace(ws_b).unwrap().output, primary);

        wm.move_workspace_to_output(ws_b, secondary).unwrap();
        assert_eq!(wm.workspace(ws_b).unwrap().output, secondary);
        // The window followed automatically — its snap output is now secondary.
        let m = wm.monitor_for_window(id).unwrap();
        assert_eq!(m.work_area.x, 1440.0);
    }

    #[test]
    fn remove_output_reparents_workspaces() {
        let wm = WindowManager::new(screen());
        let secondary = wm.add_output(Rect::new(1440.0, 0.0, 1920.0, 1080.0));
        let primary = wm.primary_output().unwrap();
        let ws_secondary = wm.active_workspace_on(secondary).unwrap();

        wm.remove_output(secondary).unwrap();
        // Workspace migrated to primary.
        assert_eq!(wm.workspace(ws_secondary).unwrap().output, primary);
        // Output is gone.
        assert!(wm.output(secondary).is_none());
        // Active per-output map cleared for the removed id.
        assert!(wm.active_workspace_on(secondary).is_none());
    }

    #[test]
    fn cannot_remove_last_output() {
        let wm = WindowManager::new(screen());
        let primary = wm.primary_output().unwrap();
        assert!(matches!(
            wm.remove_output(primary),
            Err(WmError::LastOutput)
        ));
    }

    #[test]
    fn desktop_bounds_is_aabb_of_outputs() {
        let wm = WindowManager::new(screen());
        // Single output → desktop equals its bounds.
        assert_eq!(wm.desktop_bounds(), Rect::new(0.0, 0.0, 1440.0, 800.0));

        let _secondary = wm.add_output(Rect::new(1440.0, 0.0, 1920.0, 1080.0));
        // Side-by-side: x = 0..(1440+1920) = 3360, y = 0..max(800, 1080) = 1080.
        assert_eq!(
            wm.desktop_bounds(),
            Rect::new(0.0, 0.0, 1440.0 + 1920.0, 1080.0)
        );
    }

    #[test]
    fn output_at_picks_correct_output_with_half_open_seam() {
        let wm = WindowManager::new(screen());
        let primary = wm.primary_output().unwrap();
        let secondary = wm.add_output(Rect::new(1440.0, 0.0, 1920.0, 1080.0));

        assert_eq!(wm.output_at(0.0, 0.0), Some(primary));
        assert_eq!(wm.output_at(700.0, 400.0), Some(primary));
        // Right edge belongs to the next output (half-open seam).
        assert_eq!(wm.output_at(1440.0, 100.0), Some(secondary));
        assert_eq!(wm.output_at(2000.0, 500.0), Some(secondary));
        // Below both outputs — void.
        assert_eq!(wm.output_at(700.0, 2000.0), None);
    }

    #[test]
    fn clamp_keeps_pointer_inside_desktop() {
        let wm = WindowManager::new(screen());
        wm.add_output(Rect::new(1440.0, 0.0, 1920.0, 1080.0));
        // Below desktop on Y, in range on X — clamp Y to max y.
        assert_eq!(wm.clamp_to_desktop(100.0, 2000.0), (100.0, 1080.0));
        // Way off to the right — clamp X to max x.
        assert_eq!(wm.clamp_to_desktop(9999.0, 100.0), (1440.0 + 1920.0, 100.0));
        // Inside — pass-through.
        assert_eq!(wm.clamp_to_desktop(100.0, 100.0), (100.0, 100.0));
    }

    #[test]
    fn hit_test_finds_window_on_secondary_output() {
        let wm = WindowManager::new(screen());
        let secondary = wm.add_output(Rect::new(1440.0, 0.0, 1920.0, 1080.0));
        let ws_secondary = wm.active_workspace_on(secondary).unwrap();

        // Window on the secondary output (in global coords).
        let id = wm.open("x", "x", Rect::new(1500.0, 100.0, 400.0, 300.0));
        wm.move_window_to_workspace(id, ws_secondary).unwrap();
        // Point sits inside that window on the secondary monitor.
        assert_eq!(wm.hit_test(1700.0, 200.0), Some(id));
        // Same Y, but on the primary monitor — no window there.
        assert_eq!(wm.hit_test(700.0, 200.0), None);
        // Void below outputs.
        assert_eq!(wm.hit_test(2000.0, 2000.0), None);
    }

    #[test]
    fn next_focus_candidate_picks_topmost_non_minimized() {
        let wm = WindowManager::new(screen());
        let ws = wm.active_workspace();
        let a = wm.open("a", "A", Rect::new(0.0, 0.0, 100.0, 100.0));
        let b = wm.open("b", "B", Rect::new(0.0, 0.0, 100.0, 100.0));
        // `b` opened second → higher z.
        assert_eq!(wm.next_focus_candidate(ws), Some(b));
        // Minimize `b` and the candidate falls back to `a`.
        wm.minimize(b).unwrap();
        assert_eq!(wm.next_focus_candidate(ws), Some(a));
        // Minimize `a` too → no candidate.
        wm.minimize(a).unwrap();
        assert_eq!(wm.next_focus_candidate(ws), None);
    }

    #[test]
    fn strut_layers_compose_per_edge_max() {
        let wm = WindowManager::new(screen());
        let p = wm.primary_output().unwrap();
        wm.set_strut_layer(
            p,
            "dock",
            Struts { bottom: 56.0, ..Default::default() },
        )
        .unwrap();
        wm.set_strut_layer(
            p,
            "osk",
            Struts { bottom: 300.0, ..Default::default() },
        )
        .unwrap();
        // Max wins: OSK while open eats the bottom.
        assert_eq!(wm.output(p).unwrap().struts.bottom, 300.0);
        // Clearing the OSK layer falls back to the dock baseline,
        // *without* needing a save/restore on the OSK's part.
        wm.clear_strut_layer(p, "osk").unwrap();
        assert_eq!(wm.output(p).unwrap().struts.bottom, 56.0);
    }

    #[test]
    fn edge_swap_does_not_clobber_other_layers() {
        // The Phase-13 caveat: an edge swap of the dock used to wipe
        // the bottom strut even when the OSK was holding it. With
        // layers, the dock only writes its own layer and the OSK's
        // value survives.
        let wm = WindowManager::new(screen());
        let p = wm.primary_output().unwrap();
        wm.set_strut_layer(
            p,
            "osk",
            Struts { bottom: 300.0, ..Default::default() },
        )
        .unwrap();
        // Dock starts on the bottom edge…
        wm.set_strut_layer(
            p,
            "dock",
            Struts { bottom: 56.0, ..Default::default() },
        )
        .unwrap();
        // …then swaps to the top. Only the dock's layer flips.
        wm.set_strut_layer(
            p,
            "dock",
            Struts { top: 56.0, ..Default::default() },
        )
        .unwrap();
        let s = wm.output(p).unwrap().struts;
        assert_eq!(s.top, 56.0); // dock moved up
        assert_eq!(s.bottom, 300.0); // OSK still reserved at the bottom
    }

    #[test]
    fn set_urgent_marks_then_focus_clears_it() {
        let wm = WindowManager::new(screen());
        let a = wm.open("a", "A", Rect::new(0.0, 0.0, 100.0, 100.0));
        let b = wm.open("b", "B", Rect::new(0.0, 0.0, 100.0, 100.0));
        // `b` opened last → focused. Marking `a` urgent works...
        wm.set_urgent(a, true).unwrap();
        assert!(wm.get(a).unwrap().urgent);
        // ...but marking the *focused* window urgent is a no-op
        // (a client shouldn't bug the window the user is in).
        wm.set_urgent(b, true).unwrap();
        assert!(!wm.get(b).unwrap().urgent);
        // Focusing `a` clears its urgency (the user looked).
        wm.focus(a).unwrap();
        assert!(!wm.get(a).unwrap().urgent);
    }

    #[test]
    fn set_meta_updates_only_on_real_change() {
        let wm = WindowManager::new(screen());
        let id = wm.open("", "", Rect::new(0.0, 0.0, 100.0, 100.0));

        // First real values → changed.
        assert!(wm.set_meta(id, Some("firefox"), Some("Mozilla")).unwrap());
        let w = wm.get(id).unwrap();
        assert_eq!(w.app, "firefox");
        assert_eq!(w.title, "Mozilla");

        // Same values again → no change.
        assert!(!wm.set_meta(id, Some("firefox"), Some("Mozilla")).unwrap());

        // Empty strings must not wipe good values.
        assert!(!wm.set_meta(id, Some(""), Some("")).unwrap());
        assert_eq!(wm.get(id).unwrap().title, "Mozilla");

        // Title-only update (browser tab switch).
        assert!(wm.set_meta(id, None, Some("GitHub")).unwrap());
        assert_eq!(wm.get(id).unwrap().title, "GitHub");
        assert_eq!(wm.get(id).unwrap().app, "firefox");

        // Unknown window → error.
        assert!(matches!(
            wm.set_meta(9999, Some("x"), None),
            Err(WmError::NotFound(_))
        ));
    }

    #[test]
    fn clear_focus_resets_every_window() {
        let wm = WindowManager::new(screen());
        let a = wm.open("a", "A", Rect::new(0.0, 0.0, 100.0, 100.0));
        let _b = wm.open("b", "B", Rect::new(0.0, 0.0, 100.0, 100.0));
        // `b` is focused (last opened); confirm, then clear.
        wm.focus(a).unwrap();
        assert!(wm.get(a).unwrap().focused);
        wm.clear_focus();
        assert!(!wm.get(a).unwrap().focused);
    }

    #[test]
    fn list_visible_spans_all_active_workspaces() {
        let wm = WindowManager::new(screen());
        let secondary = wm.add_output(Rect::new(1440.0, 0.0, 1920.0, 1080.0));
        let ws_secondary = wm.active_workspace_on(secondary).unwrap();

        let a = wm.open("a", "A", Rect::new(0.0, 0.0, 100.0, 100.0));
        let b = wm.open("b", "B", Rect::new(0.0, 0.0, 100.0, 100.0));
        wm.move_window_to_workspace(b, ws_secondary).unwrap();

        let ids: Vec<WindowId> = wm.list_visible().iter().map(|w| w.id).collect();
        assert!(ids.contains(&a));
        assert!(ids.contains(&b));
    }
}
