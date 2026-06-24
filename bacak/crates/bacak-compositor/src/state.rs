//! Compositor-wide and per-client state.
//!
//! Smithay's protocol implementations are blanket impls over a single user
//! type — [`BacakState`] holds every wayland protocol's server-side state
//! object plus the Bacak-specific WM and OSK controllers. Per-client state
//! (the parts that the protocol layer wants pinned next to the client itself)
//! lives in [`ClientState`].
//!
//! # The surface ↔ window bridge
//!
//! Smithay tracks `WlSurface` objects; the Bacak WM tracks abstract
//! [`wm::WindowId`]s and never sees a wayland surface directly. The bridge is
//! a `HashMap<WlSurface, WindowId>` carried inside [`BacakState`]: every time
//! a new xdg-shell toplevel arrives we [`WindowManager::open`] a corresponding
//! WM window and remember the mapping. Surface destruction prunes both sides.
//!
//! This module compiles only with the `runtime` feature, so a plain
//! `cargo check` on the workspace doesn't pull the Smithay tree.

#![cfg(feature = "runtime")]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::input::{AxisRelativeDirection, AxisSource};
use smithay::desktop::PopupManager;
use smithay::input::pointer::{AxisFrame, CursorImageStatus, PointerHandle};
use smithay::input::{Seat, SeatState};
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Display, DisplayHandle};
use smithay::utils::{Transform, SERIAL_COUNTER};
use smithay::wayland::compositor::{CompositorClientState, CompositorState};
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::xdg_activation::XdgActivationState;
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufState};
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::pointer_gestures::PointerGesturesState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::selection::SelectionTarget;
use smithay::wayland::shm::ShmState;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shell::xdg::ToplevelSurface;
use smithay::wayland::xwayland_shell::XWaylandShellState;
use smithay::xwayland::{X11Surface, X11Wm};

use std::os::fd::OwnedFd;

/// Backend-erased bridge for transferring an X11-owned selection to a Wayland
/// client that requested it. [`X11Wm::send_selection`] needs the calloop
/// `LoopHandle` of the loop the X11Wm runs on, and that loop's data type
/// differs per backend (`LoopData` on udev, `BacakState` on winit) — so the
/// concrete handle can't live on [`BacakState`] directly. Each backend boxes a
/// small impl that closes over its own handle; [`BacakState`] only sees this
/// trait object and never the loop type.
pub trait X11SelectionSink {
    /// Stream the active X11 `selection` for `mime_type` into `fd` (async,
    /// driven on the backend's event loop). `xwm` is the live window manager,
    /// passed in because it lives on [`BacakState`], not the sink.
    fn send(&self, xwm: &mut X11Wm, selection: SelectionTarget, mime_type: String, fd: OwnedFd);
}

/// Backend-erased bridge for inserting an explicit-sync acquire fence
/// ([`DrmSyncPointSource`](smithay::wayland::drm_syncobj::DrmSyncPointSource))
/// into the compositor's event loop. The pre-commit hook only has `&mut
/// BacakState`, but the source must be inserted into the calloop loop whose data
/// type is backend-specific (`LoopData` on udev) — same problem as
/// [`X11SelectionSink`]. The backend boxes an impl closing over its `LoopHandle`;
/// when the fence fires it clears the client's commit blockers
/// (`blocker_cleared`) so the delayed commit proceeds. See the popup/clipboard
/// pattern; without explicit sync, GPU clients that present with a syncobj fence
/// (Vulkan/Skia — LibreOffice) get sampled before their render finishes → black.
#[cfg(feature = "udev")]
pub trait SyncobjLoopHandle {
    /// Insert `source` into the loop; on signal, clear `client`'s commit blockers.
    fn insert_sync_source(
        &self,
        source: smithay::wayland::drm_syncobj::DrmSyncPointSource,
        client: smithay::reexports::wayland_server::Client,
    );
}

use crate::animation::{AnimEnd, SlideAnim, Spring, WindowAnim};
use crate::carousel::{self, DragAxis};
use crate::config::DockEdge;
use crate::focus::FocusHistory;
use crate::gestures::{SelectionGesture, SelectionRecognizer, TwoFingerOut, TwoFingerRecognizer};
use crate::selection::NativeText;
use crate::input::{
    FocusedField, InputMode, OskConfig, OskController, OskPress, OskRect, OskTextPress,
    TouchAggregator, TouchArbiter,
};
use crate::text::TextRenderer;
use crate::wm::{
    FocusPolicy, Monitor, OutputId, Rect, SnapZone, Struts, WinState, WindowId, WindowManager,
    WmError, WorkspaceId,
};

/// One cached, GPU-resident task-switcher label. `MemoryRenderBuffer`
/// caches its imported texture internally per renderer context, so
/// keeping the *same* instance alive across frames is what actually
/// avoids the per-frame re-upload — re-creating it from a slice every
/// frame would not. `title` is the string the buffer was rasterised
/// for; a mismatch (the client changed its title) forces a rebuild.
pub struct LabelCacheEntry {
    pub title: String,
    pub buffer: MemoryRenderBuffer,
    /// Rasterised pixel height — kept so the renderer can vertically
    /// centre the label on a cache hit without re-measuring.
    pub height: usize,
}

/// A decoded, GPU-cached app icon. Keyed by `app_id`, shared across
/// every window of that app. The native PNG dimensions are kept so
/// the renderer can fit it into the tile band at a uniform display
/// size without re-measuring.
pub struct IconCacheEntry {
    pub buffer: MemoryRenderBuffer,
    pub w: u32,
    pub h: u32,
}

/// A frozen GPU snapshot of a window, captured off the live surface tree while
/// the window was mapped. The Overview draws this for cards whose window is no
/// longer mapped (minimised / on another workspace) so they show a real
/// preview instead of just the app icon. `w`/`h` are the captured texture's
/// pixel size; `captured` drives the per-window refresh throttle.
pub struct WindowSnapshot {
    pub tex: GlesTexture,
    pub w: i32,
    pub h: i32,
    pub captured: Instant,
}


/// Live 3-finger touchpad workspace swipe (udev only). Created on
/// `GestureSwipeBegin` with three fingers, fed by `GestureSwipeUpdate`
/// deltas, resolved on `GestureSwipeEnd`. See
/// [`BacakState::ws_swipe_begin`].
#[derive(Debug, Clone, Copy)]
pub struct WsSwipe {
    pub output: OutputId,
    /// Accumulated horizontal finger delta in px. Negative = swipe left
    /// (reveal the workspace to the right, i.e. "next").
    pub accum: f32,
    /// Output width in px, used to map `accum` onto unit progress.
    pub width: f32,
    /// EMA of recent horizontal velocity (px/frame-ish) for fling decisions.
    pub vel: f32,
    /// `true` once the start threshold was crossed and a real neighbour
    /// slide was created (or an edge bounce armed). Until then a release
    /// is a no-op (a stray micro-swipe shouldn't switch workspaces).
    pub armed: bool,
}

/// Edge rubber-band spring (see [`BacakState::ws_bounce`]).
#[derive(Debug, Clone, Copy)]
pub struct WsBounce {
    pub output: OutputId,
    /// Horizontal pixel offset of the active workspace; springs back to 0.
    pub offset: Spring,
}

/// Fade state for the alt+tab switcher overlay. `alpha` is a spring in
/// `[0, 1]` driving the overlay's opacity. While the cycle is live the
/// renderer reads candidates straight from [`FocusHistory`]; once the
/// user commits/cancels, the cycle is torn down immediately, so we
/// stash the last `(candidates, selected)` in `frozen` and keep
/// drawing that until the fade-out settles.
pub struct SwitcherFade {
    pub output: OutputId,
    pub alpha: Spring,
    pub closing: bool,
    pub frozen: Option<(Vec<WindowId>, Option<WindowId>)>,
}








impl FloatingMenu {
    /// Index of the button under `(px, py)`, if any (WM-global logical px).
    pub fn item_at(&self, px: f32, py: f32) -> Option<usize> {
        self.buttons.iter().position(|b| {
            px >= b.x && px <= b.x + b.w && py >= b.y && py <= b.y + b.h
        })
    }

    /// True if `(px, py)` is anywhere on the menu plaque.
    pub fn contains(&self, px: f32, py: f32) -> bool {
        let r = self.rect;
        px >= r.x && px <= r.x + r.w && py >= r.y && py <= r.y + r.h
    }
}


impl TextPanel {
    /// Convert WM-global → text-local coordinates.
    pub fn local(&self, px: f32, py: f32) -> (f32, f32) {
        (px - self.text_origin.0, py - self.text_origin.1)
    }
    /// True if `(px, py)` is on the panel plaque.
    pub fn contains(&self, px: f32, py: f32) -> bool {
        let r = self.rect;
        px >= r.x && px <= r.x + r.w && py >= r.y && py <= r.y + r.h
    }
    /// True if `(px, py)` is over the text content area.
    pub fn in_text(&self, px: f32, py: f32) -> bool {
        let (ox, oy) = self.text_origin;
        let (w, h) = self.text_size;
        px >= ox && px <= ox + w && py >= oy && py <= oy + h
    }
    /// WM-global position of a text-local point (for anchoring the menu).
    pub fn to_global(&self, local: (f32, f32)) -> (f32, f32) {
        (self.text_origin.0 + local.0, self.text_origin.1 + local.1)
    }
}


impl AtspiSelection {
    /// Ordered `(start, end)` offsets.
    pub fn range(&self) -> (i32, i32) {
        (self.anchor.min(self.focus), self.anchor.max(self.focus))
    }
}







impl AppsMenu {
    /// On-screen rect of filtered item `i`, accounting for the pixel
    /// scroll offset. May lie partly (or fully) outside the viewport; the
    /// renderer clips it to [`grid_rect`].
    pub fn item_rect(&self, i: usize) -> Rect {
        let row = i / self.cols.max(1);
        let col = i % self.cols.max(1);
        Rect::new(
            self.grid_x + col as f32 * self.cell,
            self.grid_y + row as f32 * self.cell - self.scroll.pos as f32,
            self.cell,
            self.cell,
        )
    }
    /// The scroll viewport: cells are clipped to this rectangle.
    pub fn grid_rect(&self) -> Rect {
        Rect::new(self.grid_x, self.grid_y, self.cols as f32 * self.cell, self.grid_h)
    }
    /// Whether item `i` is at least partly inside the viewport (so it's
    /// worth emitting — fully-scrolled-away cells are skipped).
    pub fn cell_visible(&self, i: usize) -> bool {
        let r = self.item_rect(i);
        r.y + r.h > self.grid_y && r.y < self.grid_y + self.grid_h
    }
    /// Whether `(px, py)` lands on a genuinely visible part of cell `i`
    /// (inside both the cell and the clip viewport) — used for tap/click
    /// hit-testing so a clipped sliver isn't tappable where it's hidden.
    pub fn cell_hit(&self, i: usize, px: f32, py: f32) -> bool {
        py >= self.grid_y
            && py <= self.grid_y + self.grid_h
            && self.item_rect(i).contains(px, py)
    }
    pub fn total_rows(&self) -> usize {
        self.items.len().div_ceil(self.cols.max(1))
    }
    /// Maximum scroll offset in pixels (content height past the viewport).
    pub fn max_scroll_y(&self) -> f32 {
        (self.total_rows() as f32 * self.cell - self.grid_h).max(0.0)
    }
}





/// Pre-rasterised label buffer (glyphs + size), reused across the overlays.
pub type Label = Option<(MemoryRenderBuffer, usize, usize)>;

// Each shell feature's data types live in its plugin module (Phase-2: the
// plugin owns its types); re-exported here so the field decls / constructors
// below — which stay on `BacakState` (smithay's handler data) — are unchanged.
pub use crate::plugins::apps_menu::{AppCategoryTab, AppMenuItem, AppsMenu, AppsMenuDrag, AppsTouch};
pub use crate::plugins::control_center::{
    AudioAction, AudioPanel, AudioRow,
    BtAction, BtDevice, BtDialogKind, BtPanel, BtRow, CcAction, CcKind, CcSnapshot, CcTile,
    ControlCenter, StaticEntry, WifiAction, WifiMsg, WifiPanel, WifiRow,
};
pub use crate::plugins::overview::{DismissAnim, DismissKind, Overview, OverviewCard, OverviewDrag};
pub use crate::plugins::dock::{
    DockDrag, DockEntry, DockMenu, DockMenuAction, DockMenuItem, DockTooltip,
};
pub use crate::plugins::screenshot::{RegionShot, ScreenshotReq, ShotDialog, Toast};
pub use crate::plugins::desktop_settings::{
    DsAction, DsMode, DsResult, DsRow, DesktopSettingsPanel, FbEntry, FileBrowserPanel,
};
pub use crate::plugins::selection::{
    AtspiSelection, FloatingMenu, FloatingMenuItem, SelectionAction, SelectionOrigin, TextPanel,
};



/// In-flight server-side-decoration title-bar move drag (compositor-driven,
/// like [`DockDrag`] / [`OverviewDrag`] — not a Smithay pointer grab). The
/// pointer-to-window offset is captured at press so the window tracks the
/// cursor 1:1.
#[derive(Clone, Copy)]
pub struct TitleDrag {
    pub id: WindowId,
    pub grab_dx: f32,
    pub grab_dy: f32,
}




/// Rasterise one label line into a `MemoryRenderBuffer` (or `None` when
/// there's no font or the text is empty). Free function so callers borrow
/// `self.text` for just the call, leaving `self` free to mutate after.
/// Supersample factor for all UI label rasterisation. We rasterise glyphs
/// at `SS×` the requested point size into a buffer tagged with
/// `buffer_scale = SS`, so the buffer's *logical* size stays at the
/// requested px (layout maths is unchanged) while it carries SS× the
/// pixels. Smithay then maps logical→physical by the live output scale
/// ([`smithay`] `memory.rs`: `physical = logical × output_scale`), so the
/// same buffer is pixel-crisp on a scale-1 output (super-sampled, then
/// box-downscaled) *and* on a HiDPI scale-2 output (1:1). The label
/// dimensions returned are logical, so callers keep positioning in
/// logical pixels.
pub(crate) const LABEL_SUPERSAMPLE: i32 = 2;

fn cc_rasterize(
    text: Option<&TextRenderer>,
    s: &str,
    px: f32,
    color: [u8; 3],
    max_w: usize,
) -> Option<(MemoryRenderBuffer, usize, usize)> {
    let ss = LABEL_SUPERSAMPLE;
    let (rgba, w, h) = text?.rasterize_line(s, px * ss as f32, color, max_w * ss as usize)?;
    let buf = MemoryRenderBuffer::from_slice(
        &rgba,
        smithay::backend::allocator::Fourcc::Abgr8888,
        (w as i32, h as i32),
        ss,
        smithay::utils::Transform::Normal,
        None,
    );
    // Report LOGICAL dimensions (physical buffer / supersample) so every
    // caller's centring / truncation maths stays in logical pixels.
    Some((buf, w / ss as usize, h / ss as usize))
}

/// Reduce a pointer-axis (wheel / touchpad) event to a discrete `-1 / 0 / +1`
/// carousel step. `v120` is the high-res wheel detent amount (one notch → one
/// step); `cont` is the continuous amount, thresholded for touchpads. Positive
/// (scroll down / right) advances. The backends extract the axis values (the
/// trait methods are generic over the backend) and pass them here so the
/// sign / threshold policy is shared.
pub fn axis_notches(v120: Option<f64>, cont: Option<f64>) -> i32 {
    if let Some(d) = v120 {
        if d != 0.0 {
            return d.signum() as i32;
        }
    }
    let c = cont.unwrap_or(0.0);
    if c.abs() > 8.0 {
        c.signum() as i32
    } else {
        0
    }
}

/// Forward a pointer-axis (scroll) event to the focused client. Called by both
/// backends when the Overview is closed (when it's open the wheel drives the
/// carousel instead). The backends extract the per-axis `(amount, v120,
/// relative_direction)` — `PointerAxisEvent` is generic over the backend — and
/// this assembles them into a `wl_pointer` axis frame: `value` for legacy
/// clients, `v120` for high-res wheels, and a `stop` to terminate finger
/// (touchpad) scroll sequences.
#[allow(clippy::too_many_arguments)]
pub fn forward_axis(
    state: &mut BacakState,
    pointer: &PointerHandle<BacakState>,
    time_ms: u32,
    source: AxisSource,
    h: (Option<f64>, Option<f64>, AxisRelativeDirection),
    v: (Option<f64>, Option<f64>, AxisRelativeDirection),
) {
    use smithay::backend::input::Axis;
    let mut frame = AxisFrame::new(time_ms).source(source);
    for (axis, (amount, v120, rel)) in [(Axis::Horizontal, h), (Axis::Vertical, v)] {
        if amount.is_some() || v120.is_some() {
            frame = frame.relative_direction(axis, rel);
        }
        if let Some(d) = v120 {
            frame = frame.v120(axis, d as i32);
        }
        if let Some(a) = amount {
            frame = frame.value(axis, a);
        } else if source == AxisSource::Finger {
            // Finger scroll guarantees a 0-value stop to end the sequence.
            frame = frame.stop(axis);
        }
    }
    pointer.axis(state, frame);
    pointer.frame(state);
}

/// Per-client server state. Smithay's protocol delegates read this back when
/// dispatching events, so any wayland-server protocol that wants client-local
/// state pins it on `Arc<ClientState>` and we hand the same Arc to every
/// `display.insert_client(...)` call.
#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
    /// Set for clients connected through a `wp_security_context` listener
    /// (sandboxed apps). Used to exclude them from re-binding the
    /// security-context manager (a sandbox must not create nested contexts).
    pub security_context: Option<smithay::wayland::security_context::SecurityContext>,
}

impl ClientData for ClientState {
    fn initialized(&self, _: ClientId) {
        tracing::debug!("wayland client initialized");
    }
    fn disconnected(&self, _: ClientId, reason: DisconnectReason) {
        tracing::debug!(?reason, "wayland client disconnected");
    }
}

/// The compositor's single source of truth.
///
/// Holds:
/// * Smithay protocol state objects (`compositor_state`, `xdg_shell_state`,
///   `shm_state`, `seat_state`, the default `seat`).
/// * Bacak's headless controllers ([`WindowManager`], [`OskController`]).
/// * The surface → WM-window mapping that keeps the two worlds in sync.
///
/// One instance is constructed in `main` and threaded through every protocol
/// dispatch — Smithay's delegate macros assume the value behind `&mut Self`.
pub struct BacakState {
    // --- Smithay protocol state -------------------------------------------
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    /// Tracks xdg_popups (app menus, tooltips, combo boxes) against their
    /// parent toplevel. Drives mapping / initial configure (in `commit`),
    /// the menu pointer+keyboard grab (in `XdgShellHandler::grab`), and is
    /// walked at render time so popups are actually drawn. Without it
    /// Wayland app menus never appear.
    pub popups: PopupManager,
    /// xdg-activation v1 protocol state. A client requesting activation
    /// of one of its surfaces lands in
    /// [`XdgActivationHandler::request_activation`]; rather than
    /// honour the focus steal we mark the window urgent and let the
    /// dock pulse its tile until the user looks.
    pub xdg_activation: XdgActivationState,
    pub shm_state: ShmState,
    pub seat_state: SeatState<Self>,
    pub seat: Seat<Self>,
    /// `wl_output` + `zxdg_output_manager_v1` advertisement. Without a
    /// `wl_output` global, real toolkits (Chromium's Ozone-Wayland,
    /// Firefox, GTK/Qt) abort or crash right after connecting.
    pub output_manager_state: OutputManagerState,
    /// `wl_data_device_manager` — clipboard + drag-and-drop. Chromium's
    /// Ozone-Wayland refuses to start without it.
    pub data_device_state: DataDeviceState,
    /// `zwp_primary_selection_device_manager_v1` — the X11-style "primary"
    /// selection (highlight-to-copy, middle-click-to-paste). Distinct from the
    /// CLIPBOARD that `data_device_state` serves; foot/GTK/Qt all expect it.
    pub primary_selection_state: PrimarySelectionState,
    /// Backend-supplied bridge for streaming an X11-owned selection to a
    /// Wayland client (see [`X11SelectionSink`]). `None` until a backend with
    /// XWayland wires it; clipboard then works Wayland-only.
    pub x11_selection_sink: Option<Box<dyn X11SelectionSink>>,
    /// `wp_linux_drm_syncobj_v1` explicit-sync state. `None` on backends without
    /// a DRM device (winit) or where the device lacks syncobj-eventfd support;
    /// the udev backend sets it so GPU clients (Vulkan/Skia — LibreOffice) that
    /// present with an acquire fence are composited only after their render
    /// finishes, instead of being sampled early and rendering black.
    #[cfg(feature = "udev")]
    pub syncobj_state: Option<smithay::wayland::drm_syncobj::DrmSyncobjState>,
    /// Backend-supplied bridge to insert an explicit-sync acquire fence into the
    /// event loop (see [`SyncobjLoopHandle`]). `None` until a backend wires it.
    #[cfg(feature = "udev")]
    pub syncobj_loop: Option<Box<dyn SyncobjLoopHandle>>,
    /// `zwp_pointer_gestures_v1` — advertises touchpad pinch/swipe/hold so
    /// clients (Firefox, Chromium) get native pinch-to-zoom. The compositor
    /// only *forwards* the gesture; the app does the actual content zoom.
    pub pointer_gestures: PointerGesturesState,
    /// `zwp_linux_dmabuf_v1` — lets GPU clients (LibreOffice Skia/GL, GTK GL)
    /// submit dma-buf frames instead of rendering black on this otherwise
    /// SHM-only compositor. The state lives here; the *global* is created by a
    /// backend once its renderer's import formats are known (`dmabuf_global`).
    pub dmabuf_state: DmabufState,
    /// The advertised dmabuf global, kept alive for the session. `None` until
    /// a backend with a renderer creates it (currently the udev backend).
    pub dmabuf_global: Option<DmabufGlobal>,
    /// dma-buf imports awaiting a real test-import. The `DmabufHandler` runs on
    /// `BacakState` and has no renderer, so it can't validate a buffer; it
    /// queues `(buffer, notifier)` here and the backend render tick drains it
    /// (`process_pending_dmabuf`), test-importing into the live `GlesRenderer`
    /// and resolving the notifier `successful`/`failed`. Replaces the old
    /// optimistic-accept that let un-sampleable formats through → black render.
    pub pending_dmabuf: Vec<(
        smithay::backend::allocator::dmabuf::Dmabuf,
        smithay::wayland::dmabuf::ImportNotifier,
    )>,
    /// `wp_viewporter` — lets clients crop/scale a buffer into a different
    /// surface size. Render already honours the viewport (smithay applies it in
    /// `render_elements_from_surface_tree`); wiring the global is all that's
    /// needed. Video players and some toolkits require it.
    pub viewporter_state: smithay::wayland::viewporter::ViewporterState,
    /// `wp_fractional_scale_v1` — advertises a per-surface preferred scale so
    /// HiDPI clients render crisply at the compositor's scale instead of
    /// guessing from the integer `wl_output` scale. Paired with viewporter.
    pub fractional_scale_state:
        smithay::wayland::fractional_scale::FractionalScaleManagerState,
    /// `ext-foreign-toplevel-list-v1` — lets external taskbars/docks enumerate
    /// open windows (title + app_id + identifier). List-only: the wlr
    /// *management* extension (activate/close/minimize) is not implemented.
    pub foreign_toplevel_list:
        smithay::wayland::foreign_toplevel_list::ForeignToplevelListState,
    /// Per-window foreign-toplevel handle, so we can update its title/app_id on
    /// change and withdraw it on close. xdg toplevels only for now (X11 windows
    /// aren't advertised yet).
    pub foreign_handles: std::collections::HashMap<
        WindowId,
        smithay::wayland::foreign_toplevel_list::ForeignToplevelHandle,
    >,
    /// `zwp_text_input_v3` — apps advertise an editable text field. Bacak uses
    /// its **own** hand-rolled handler (see [`crate::text_input`]) instead of
    /// smithay's, because smithay discards all text-input requests unless an
    /// external IME is bound — which would make the on-screen keyboard's
    /// auto-show impossible. This observer drives the OSK directly.
    pub bacak_text_input: crate::text_input::BacakTextInput,
    /// Set when the OSK's visibility toggles outside the per-frame tick (e.g.
    /// from a text-input `commit` during client dispatch); the backend loop
    /// forces a redraw and clears it.
    pub osk_dirty: bool,
    /// Set by the commit handler whenever any surface commits new content;
    /// the backend render loop forces a redraw and clears it.
    pub surface_committed: bool,
    /// Debounce for *hiding* the OSK: when a field deactivates we don't close
    /// immediately but arm this deadline; if a field re-activates before it
    /// elapses the close is cancelled. Stops the rapid show/hide flicker some
    /// toolkits cause by toggling text-input enable/disable. The per-frame tick
    /// fires the close once `Instant::now()` passes it.
    pub osk_hide_at: Option<Instant>,
    /// `zwp_input_method_v2` — an IME / OSK connects here and injects text
    /// (commit_string) into the focused text-input client. Enables on-screen
    /// keyboards (wvkbd) and IMEs (ibus/fcitx5). The IME's own candidate popup
    /// is tracked but not yet rendered (CJK candidate visuals are a follow-up;
    /// text injection works without it).
    pub input_method_manager_state:
        smithay::wayland::input_method::InputMethodManagerState,
    /// `zwp_virtual_keyboard_v1` — lets a client (an external OSK, an
    /// accessibility tool, or a remote-input bridge) inject keymap + key
    /// events straight into the seat. Bacak's *own* compositor-drawn keyboard
    /// types via the seat keyboard directly ([`synthesize_chord`]); this global
    /// is what makes third-party virtual keyboards work too.
    pub virtual_keyboard_manager_state:
        smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState,
    /// `wlr-layer-shell` — panels, docks, wallpaper, notification daemons,
    /// on-screen keyboards. Layer surfaces are owned by per-output
    /// `LayerMap`s (in the smithay `Output`'s user_data); this is just the
    /// global. See `outputs` for how the handler reaches the maps.
    pub layer_shell_state: smithay::wayland::shell::wlr_layer::WlrLayerShellState,
    /// Smithay `Output` objects keyed by WM `OutputId`, registered by the
    /// backend. Needed so protocol handlers (layer-shell) and the render/input
    /// paths — all of which run on `BacakState` — can reach
    /// `layer_map_for_output`. The backend's own `Output` lives in its render
    /// target; these are cheap clones (Output is Arc-backed).
    pub outputs: std::collections::HashMap<OutputId, smithay::output::Output>,
    /// `zwlr_screencopy_v1` capture requests awaiting fulfilment. The Dispatch
    /// handlers (no renderer) validate the client buffer and queue here; the
    /// backend render tick (`render::process_pending_screencopy`) renders the
    /// output offscreen, reads it back, copies into the client's SHM buffer,
    /// and fires `ready`. SHM + full-output/region; no dmabuf/cursor overlay.
    pub pending_screencopy: Vec<crate::screencopy::ScreencopyRequest>,
    /// A built-in screenshot is requested (PrintScreen = whole output;
    /// Shift+PrintScreen drag = a region). Set by input handlers (no renderer);
    /// the backend render tick renders the output offscreen, reads it back, and
    /// writes a PNG to disk (`render::process_pending_screenshot`). Cleared once
    /// taken.
    pub pending_screenshot: Option<ScreenshotReq>,
    /// Active interactive region screenshot (Shift+PrintScreen). While `Some`,
    /// the compositor swallows pointer input and draws a selection rectangle;
    /// the drag's release queues a [`ScreenshotReq`] with the chosen region and
    /// clears this. `anchor` is `None` until the first press.
    pub region_shot: Option<RegionShot>,
    /// Window-pick screenshot mode (Alt+PrintScreen) is active. While true, the
    /// compositor swallows pointer input, highlights the window under the
    /// pointer, and a left-click captures that window (its content + SSD title
    /// bar) — see [`window_shot_rect`](Self::window_shot_rect). Escape / a click
    /// on empty desktop cancels.
    pub window_pick: bool,
    /// Brief white "camera flash" shown after a screenshot is taken: the output
    /// it covers + the `Instant` it started. Set *after* the capture (so it's
    /// never in the saved image) and faded out over [`FLASH_MS`] by
    /// `tick_animations`; drawn topmost in `build_output_frame`.
    pub flash: Option<(OutputId, std::time::Instant)>,
    /// Active transient notification banner (e.g. "screenshot saved").
    pub toast: Option<Toast>,
    /// `wp_single_pixel_buffer` — 1×1 solid-colour buffers (GTK4 backgrounds,
    /// solid fills) without an SHM pool. No handler; render path treats it as a
    /// normal buffer.
    pub single_pixel_buffer_state:
        smithay::wayland::single_pixel_buffer::SinglePixelBufferState,
    /// `wp_content_type_v1` — a client hints whether a surface is video / game
    /// (for tearing / VRR decisions). We just accept the hint (stored in
    /// surface cached state); no behaviour change yet.
    pub content_type_state: smithay::wayland::content_type::ContentTypeState,
    /// `wp_presentation` — lets clients learn when their frame actually hit the
    /// screen (mpv, smooth animation). Global advertised against CLOCK_MONOTONIC.
    pub presentation_state: smithay::wayland::presentation::PresentationState,
    /// `zwp_relative_pointer_v1` — unaccelerated motion deltas for games / 3D
    /// (FPS mouselook). Deltas are emitted from the udev PointerMotion handler.
    pub relative_pointer_manager_state:
        smithay::wayland::relative_pointer::RelativePointerManagerState,
    /// `zwp_pointer_constraints_v1` — pointer lock / confine for games. Locked
    /// constraints freeze the cursor (the client navigates via relative motion);
    /// confine is accepted but not yet enforced.
    pub pointer_constraints_state:
        smithay::wayland::pointer_constraints::PointerConstraintsState,
    /// `zwlr_foreign_toplevel_manager_v1` — bound taskbar/dock managers. Each
    /// gets its own handle per window (in `ftl_handles`). The *management*
    /// protocol (activate/close/minimize/maximize/fullscreen), complementing the
    /// list-only ext protocol (`foreign_toplevel_list`). See foreign_toplevel.rs.
    pub ftl_managers: Vec<smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1>,
    /// Per-window wlr foreign-toplevel handles (one per bound manager). Title /
    /// app_id / state are pushed to these; `closed` on window destroy.
    pub ftl_handles: std::collections::HashMap<WindowId, Vec<smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1>>,
    /// `zwp_tablet_manager_v2` — graphics tablets / styluses. Tablets and tools
    /// are tracked on the seat's tablet-seat; the udev input loop forwards tool
    /// proximity / motion / pressure / tilt / tip / buttons to clients.
    pub tablet_manager_state: smithay::wayland::tablet_manager::TabletManagerState,
    /// `wp_security_context_v1` — a sandbox manager (Flatpak/portal) creates a
    /// restricted listening socket; clients connecting through it are tagged
    /// (see `ClientState::security_context`). The listener source is queued here
    /// and inserted into the udev event loop (where the loop handle lives).
    pub security_context_state: smithay::wayland::security_context::SecurityContextState,
    pub pending_security_listeners: Vec<(
        smithay::wayland::security_context::SecurityContextListenerSource,
        smithay::wayland::security_context::SecurityContext,
    )>,
    /// Active IME candidate popups (input-method-v2). Rendered on top near the
    /// text cursor. Usually 0–1; a Vec tolerates odd IMEs.
    pub ime_popups: Vec<smithay::wayland::input_method::PopupSurface>,

    // --- Bacak-specific state ---------------------------------------------
    pub display_handle: DisplayHandle,
    pub wm: WindowManager,
    pub osk: OskController,
    pub start_time: Instant,

    // --- Bridge between Wayland surfaces and Bacak window ids --------------
    /// Maps each live xdg-shell toplevel `WlSurface` to a Bacak `WindowId`.
    /// Entries are inserted in [`XdgShellHandler::new_toplevel`] and removed
    /// when the surface is destroyed / role-cleared.
    pub windows: HashMap<WlSurface, WindowId>,

    // --- XWayland ----------------------------------------------------------
    /// `xwayland_shell_v1` global state — Xwayland associates each X11
    /// window's `wl_surface` with a serial through this protocol. The
    /// global is created unconditionally; only Xwayland ever binds it.
    pub xwayland_shell_state: XWaylandShellState,
    /// `zxdg_decoration_manager_v1` — clients negotiate server- vs client-side
    /// decorations through this. Our policy (see `XdgDecorationHandler`)
    /// defaults to server-side, so the compositor draws a uniform title bar.
    pub xdg_decoration_state: XdgDecorationState,
    /// Windows currently using server-side decorations (compositor draws their
    /// title bar). Negotiated via the decoration protocol; pruned on close.
    pub decorated: std::collections::HashSet<WindowId>,
    /// In-flight server-side title-bar move drag.
    pub title_drag: Option<TitleDrag>,
    /// Last title-bar press `(window, when, x, y)` for double-click → maximise.
    pub last_title_click: Option<(WindowId, Instant, f32, f32)>,
    /// Title-bar text cache, keyed by `WindowId` + title string (separate from
    /// the switcher's `label_cache` because the decoration rasterises at its
    /// own size / width, so sharing one cache would thrash both).
    pub deco_label_cache: Mutex<HashMap<WindowId, LabelCacheEntry>>,
    /// The live X11 window manager, set once the spawned Xwayland server
    /// signals `Ready`. `None` until then (and on backends that don't wire
    /// XWayland, e.g. the winit dev backend).
    pub xwm: Option<X11Wm>,
    /// Back-reference from a Bacak [`WindowId`] to the X11 surface that
    /// backs it, so geometry/close/activate can be pushed back to Xwayland.
    /// Only normal (non-override-redirect) windows live here — they're the
    /// ones promoted into [`WindowManager`] like any xdg toplevel.
    pub x11_windows: HashMap<WindowId, X11Surface>,
    /// Override-redirect X11 surfaces (menus, tooltips, combo dropdowns).
    /// They bypass the WM — never focused, never in the Overview — but are
    /// drawn on top at their own absolute geometry by the render path.
    pub x11_override: Vec<X11Surface>,

    // --- Interactive state -------------------------------------------------
    /// Last known pointer position in compositor-logical coordinates. Updated
    /// on every pointer motion event so click handlers can hit-test without
    /// re-reading the seat.
    pub pointer_position: (f64, f64),

    /// What the focused client wants the cursor to look like (set via
    /// [`SeatHandler::cursor_image`]). The render path turns this into
    /// the on-screen cursor: the client's own surface offset by its
    /// hotspot, hidden, or our built-in arrow for `Named`/default.
    pub cursor_status: CursorImageStatus,

    /// Whether keyboard focus chases the pointer or waits for a click. The
    /// active policy is read by the pointer event handler in `runtime.rs`.
    pub focus_policy: FocusPolicy,

    /// Stateful aggregator that fuses per-slot libinput touch events into
    /// higher-level [`crate::input::Gesture`] decisions.
    pub touch_aggregator: TouchAggregator,
    /// Touch arbitration: routes free touches to the client (single finger) or
    /// claims them as a compositor gesture (≥2 fingers), cancelling the
    /// client's touch. See [`TouchArbiter`].
    pub touch_arbiter: TouchArbiter,
    /// Two-finger window gesture recogniser (move / double-tap-fullscreen).
    pub two_finger: TwoFingerRecognizer,
    /// The window a two-finger gesture is acting on, picked under the centroid
    /// when both fingers land.
    two_finger_target: Option<WindowId>,
    /// That window's geometry at the moment the two-finger drag began, so the
    /// move is an absolute `start + delta` (no per-event drift).
    two_finger_win_start: Rect,

    // --- Animation + drag-preview state -----------------------------------
    /// Per-window spring animations driving post-drop / snap motion. Indexed
    /// by [`WindowId`]; one entry per actively-animating window. When the
    /// animation settles, the entry is removed and (if `commit_zone` is set)
    /// the WM is informed via [`WindowManager::snap`].
    pub animations: std::collections::HashMap<WindowId, WindowAnim>,

    /// Per-output workspace-slide transitions. The WM's `active_workspace_on`
    /// has already been switched the instant the slide starts; this map
    /// only carries the visual offset that lets the renderer paint both
    /// the outgoing and incoming workspaces gliding past each other.
    pub workspace_slides: std::collections::HashMap<OutputId, SlideAnim>,

    /// In-progress 3-finger touchpad workspace swipe (udev only). While
    /// `Some` and `armed`, the finger drives `workspace_slides[output]`'s
    /// progress directly and `tick_animations` does **not** step that
    /// slide — release commits (spring → 1.0) or cancels (spring → 0.0,
    /// active restored to `prev_ws` once it settles). See
    /// [`BacakState::ws_swipe_begin`].
    pub ws_swipe: Option<WsSwipe>,

    /// Edge rubber-band: when a swipe pushes past the first / last
    /// workspace there's no neighbour to slide to, so the active
    /// workspace shifts a little in the swipe direction and springs
    /// back. Keyed by output; `offset` is a horizontal pixel offset the
    /// renderer adds to the active workspace when no slide is running.
    pub ws_bounce: Option<WsBounce>,

    /// While a [`crate::grab::MoveGrab`] is active, the WM's snap-edge
    /// hit-test result for the *current pointer position*. The render path
    /// draws a translucent rectangle over this region so the user knows
    /// where the window will land if they release here.
    pub snap_preview: Option<(SnapZone, Rect)>,

    /// Timestamp of the last animation tick. Used to compute `dt` for the
    /// spring integrator; the loop driver should pass `Instant::now()` into
    /// [`BacakState::tick_animations`] every frame.
    pub last_anim_tick: Instant,

    /// MRU focus history + alt+tab cycle state. The runtime keyboard
    /// filter drives the cycle through [`BacakState::alt_tab_start`]
    /// and friends; every other focus path (clicks, workspace
    /// switches, programmatic) feeds the MRU via
    /// [`crate::handlers`]'s `focus_changed` mirror.
    pub focus_history: FocusHistory,

    /// Fade animation for the alt+tab switcher overlay. Outlives the
    /// `focus_history` cycle so the overlay can fade *out* after the
    /// user commits or cancels — at which point the cycle's candidate
    /// list is already gone, hence the captured `frozen` snapshot.
    pub switcher_fade: Option<SwitcherFade>,

    /// Effective glassmorphism-blur setting, resolved once at startup
    /// from the persistent config (with the `BACAK_BLUR` env override).
    /// The udev render path reads this instead of probing the env
    /// every frame.
    pub blur_enabled: bool,

    /// Loaded, sanitised compositor tunables (blur radius, shadow
    /// ramp, dock slot). Read by the render path.
    pub config: crate::config::CompositorConfig,

    /// Debounced session persistence: last time we wrote
    /// `session.json`, and the JSON we wrote (so a periodic save is
    /// skipped when nothing changed).
    pub last_session_save: Instant,
    pub last_session_json: String,

    /// Phase-2 restore: saved window placements not yet matched to a
    /// live client. The first time a window's app id is known, a
    /// matching record is consumed and its geometry applied.
    pub pending_placements: Vec<crate::session::WindowRec>,
    /// Windows already considered for placement (one-shot per window).
    pub placement_done: std::collections::HashSet<WindowId>,
    /// Transient/dialog child → parent window. Set in `new_toplevel` from
    /// `xdg_toplevel.set_parent`; used to center a dialog over its parent and
    /// keep it stacked above it. (Without centering, GTK/Qt dialogs open at the
    /// placeholder corner and read as "not appearing".)
    pub dialog_parent: std::collections::HashMap<WindowId, WindowId>,
    /// Dialogs awaiting a one-shot center-over-parent, performed in `commit`
    /// once the real size is known. Drained per window.
    pub dialog_center_pending: std::collections::HashSet<WindowId>,

    /// Lazily-loaded UI font for overlay labels. `None` when no system
    /// font could be found — the renderer then falls back to
    /// label-less coloured tiles.
    pub text: Option<TextRenderer>,

    /// Dedicated font for the on-screen keyboard, with full coverage of its
    /// symbol keys (⇧ ↵ ⌫ arrows …) which the default UI font (Liberation Sans)
    /// lacks. Loaded from DejaVu Sans; falls back to [`Self::text`] when no
    /// symbol font is installed.
    pub osk_font: Option<TextRenderer>,

    /// Colour-emoji font (Noto Color Emoji, CBDT PNG strikes) for the OSK emoji
    /// page — `fontdue` can't render colour emoji. `None` when none is
    /// installed (emoji stay monochrome). See [`crate::emoji`].
    pub emoji_font: Option<crate::emoji::EmojiFont>,

    /// The close-button "×" glyph, rasterised once at startup (it's identical
    /// for every server-side title bar). `None` when there's no font or the
    /// glyph is missing — the red chip alone then signals "close".
    pub deco_close_glyph: Option<(MemoryRenderBuffer, usize, usize)>,

    /// `WindowId → rasterised label`. Behind a `Mutex` so the render
    /// path can refresh it through a `&BacakState` without threading
    /// `&mut` through every render helper. Single-threaded in practice
    /// (the compositor renders on one thread), so the lock never
    /// contends. Entries are evicted when their window is destroyed.
    pub label_cache: Mutex<HashMap<WindowId, LabelCacheEntry>>,

    /// `app_id → resolved icon`. The `Option` distinguishes "found and
    /// uploaded" from "looked, nothing usable" — the latter stops us
    /// re-walking the filesystem every frame. Keyed by app id (not
    /// window), so it's bounded by the number of distinct apps and
    /// never needs eviction.
    pub icon_cache: Mutex<HashMap<String, Option<IconCacheEntry>>>,
    /// Frozen window previews for the Overview (captured by the udev render
    /// loop while windows are mapped). Interior-mutable like `icon_cache` so
    /// the render path can refresh it behind `&BacakState`.
    pub snapshots: Mutex<HashMap<WindowId, WindowSnapshot>>,

    /// `app_id → moment we last spawned it from a pinned tile`. Guards
    /// the launcher against spam-clicks: a not-running pin won't
    /// re-spawn while a launch is still pending (until its window maps
    /// — at which point the slot binds a window and this is moot — or
    /// [`LAUNCH_DEBOUNCE`] elapses for a launch that never showed).
    /// Bounded by the pin count; pruned on every dock click.
    pub pending_launches: HashMap<String, Instant>,

    /// One-slot GPU cache for the dock hover tooltip (see
    /// [`DockTooltip`]). `None` until the first tile is hovered;
    /// rebuilt only when the hovered label's text changes, so a
    /// motionless hover doesn't re-upload every frame.
    pub dock_tooltip: Mutex<Option<DockTooltip>>,

    /// Per-output auto-hide reveal animations. Each spring's `pos` is
    /// that output's dock offset in px (`0` = fully shown, the
    /// hidden-offset for that output = fully tucked below its bottom
    /// edge). Lazy: entries are created on first
    /// [`tick_dock_reveal`](Self::tick_dock_reveal). Only meaningful
    /// when `config.dock_autohide`; otherwise every spring stays at
    /// `0`. Per-output so revealing on monitor B doesn't disturb the
    /// settled state on monitor A.
    pub dock_reveal: HashMap<OutputId, Spring>,

    /// In-flight drag-to-reorder of a pinned dock tile. `None` while
    /// idle. Set on a left press over a pinned slot, promoted from
    /// `started=false` to `true` once the pointer crosses
    /// [`DOCK_DRAG_THRESHOLD`], committed (and cleared) on release.
    pub dock_drag: Option<DockDrag>,

    /// The touch slot, if any, that currently owns a dock interaction.
    /// Set on a [`TouchDown`](smithay::backend::input::InputEvent)
    /// landing on a tile so subsequent motion/up for the same slot
    /// route to [`dock_pointer_motion`](Self::dock_pointer_motion) /
    /// [`dock_release`](Self::dock_release); other touches keep going
    /// to the touch aggregator. Cleared on up / cancel.
    pub dock_touch_slot: Option<i32>,

    /// Most recent dock-tile hover: `(output, slot_index, since)`.
    /// Updated by [`dock_pointer_motion`](Self::dock_pointer_motion).
    /// `Some` while the pointer sits on a dock tile; once `since`
    /// crosses [`DOCK_HOVER_DWELL`] the renderer promotes the text
    /// tooltip to a live window thumbnail (for slots bound to a
    /// running window).
    pub dock_hover_started: Option<(OutputId, usize, Instant)>,

    /// Open right-click context menu, if any. Set by
    /// [`dock_right_press`](Self::dock_right_press) on a tile;
    /// cleared by an item click, an off-menu click, or an Esc press.
    pub dock_menu: Option<DockMenu>,

    /// Android-style floating action menu (Copy/Paste/…), shown after a
    /// long-press on the focused client. `None` when dismissed.
    pub floating_menu: Option<FloatingMenu>,
    /// Text-selection magnifier (loupe) anchor in WM-global logical px, while an
    /// after-long-press drag is in progress. `Some` → the renderer draws a
    /// zoomed crop of the scene above this point. Set on long-press, followed on
    /// drag, cleared on touch-up/cancel (see the udev touch handlers).
    pub loupe: Option<(f32, f32)>,
    /// Slot of the current single-finger touch sequence, so a long-press can
    /// hand the rest of the drag to the emulated-pointer (`touch_pointer_slot`)
    /// path for mouse-emulated text selection. Cleared on touch-up/cancel.
    pub single_touch_slot: Option<i32>,
    /// A mouse-emulated text selection (long-press → drag) is in progress over a
    /// foreign window. On touch-up we pop the Android-style Copy/Paste menu above
    /// the selection. Cleared on touch-up/cancel.
    pub mouse_select: bool,
    /// The window the floating menu was opened over (right-click / long-press
    /// target). Copy/Paste re-assert keyboard focus to this window's surface
    /// just before synthesising the clipboard chord — otherwise the synthesised
    /// Ctrl+(Shift+)C/V lands nowhere if focus drifted (the right-click target
    /// is an XWayland window, focus was cleared, etc.). Cleared with the menu.
    pub selection_menu_target: Option<WindowId>,
    /// Single-finger selection gesture recogniser feeding [`floating_menu`].
    /// Driven from the touch handlers + the per-frame tick.
    pub selection_recognizer: SelectionRecognizer,

    /// The Tier A compositor-native text panel, when open (Super+T).
    pub text_panel: Option<TextPanel>,
    /// Text the compositor currently owns on the clipboard (set by
    /// [`copy_text_to_clipboard`](Self::copy_text_to_clipboard)); written to a
    /// requesting client's fd by `SelectionHandler::send_selection`.
    pub clipboard_text: Option<String>,
    /// PNG bytes the compositor currently owns on the clipboard as `image/png`
    /// (a screenshot copied via [`copy_image_to_clipboard`]); served to a
    /// requesting client by `SelectionHandler::send_selection`.
    pub clipboard_image: Option<Vec<u8>>,

    /// Tier C accessibility bridge (AT-SPI2), when started (`BACAK_ATSPI=1`).
    /// Lets foreign apps that expose a11y be selected accurately; `None`
    /// otherwise. Started by the backend, not [`new`](Self::new), so it stays
    /// out of tests and headless sessions.
    pub atspi: Option<crate::atspi::AtspiBridge>,
    /// Tier C selection overlay drawn over a foreign app (AT-SPI geometry).
    pub atspi_selection: Option<AtspiSelection>,
    /// The AT-SPI text accessible (+ window origin) under a right-click context
    /// menu, so Copy/Paste act on it directly via AT-SPI (read selection / caret
    /// insert) instead of synthesising keys. Cleared when the menu closes.
    pub atspi_menu_ctx: Option<(crate::atspi::AccRef, (f32, f32))>,

    /// Open applications grid menu, if any (dock's apps button).
    pub apps_menu: Option<AppsMenu>,

    /// Open Control Center, if any (dock's gear button).
    pub control_center: Option<ControlCenter>,
    /// Receiver for the Control Center's background state fetch. `Some` until the
    /// snapshot lands (drained in [`Self::tick_animations`]).
    pub cc_rx: Option<std::sync::mpsc::Receiver<CcSnapshot>>,
    /// Native Wi-Fi picker, if open (tapping the Control Center's Wi-Fi tile).
    pub wifi_panel: Option<WifiPanel>,
    /// Native Bluetooth picker, if open (tapping the Control Center's BT tile).
    pub bt_panel: Option<BtPanel>,
    /// Audio output device picker, if open (tapping the Control Center's audio tile).
    pub audio_panel: Option<AudioPanel>,
    /// Microphone input device picker, if open (tapping the Control Center's mic tile).
    pub mic_panel: Option<AudioPanel>,
    /// Persistent `bluetoothctl` coprocess, started when the BT panel first opens
    /// and kept alive for the session (so pairings/agent prompts work).
    pub btctl: Option<crate::bluetooth::BtCtl>,
    /// Last passkey we auto-confirmed (dedup: the prompt is re-emitted many times
    /// per second; answer each distinct passkey once). Survives panel rebuilds.
    pub bt_last_pk: Option<String>,
    /// MACs BlueZ has bonded (queried on open / after pair / forget). Lets a
    /// freshly-scanned already-paired device show in the "paired" section instead
    /// of offering a (failing) re-pair.
    pub bt_paired: std::collections::HashSet<String>,
    /// Receiver for the background Wi-Fi connect thread's result. `Some` only
    /// while a connect is in flight; drained in [`Self::tick_animations`].
    pub wifi_rx: Option<std::sync::mpsc::Receiver<WifiMsg>>,
    /// Desktop Settings panel (opened from the Control Center).
    pub desktop_settings: Option<DesktopSettingsPanel>,
    /// Receiver for desktop-settings background worker results.
    pub ds_rx: Option<std::sync::mpsc::Receiver<DsResult>>,
    /// File browser overlay (opened from Desktop Settings "Resim Yolu" row).
    pub file_browser: Option<FileBrowserPanel>,
    /// Screenshot options dialog (scope + delay), opened from the Control
    /// Center's screenshot button.
    pub shot_dialog: Option<ShotDialog>,
    /// A scheduled (delayed) screenshot: fire the request once `Instant` passes.
    /// Set by the screenshot dialog's "Çek"; the per-frame tick promotes it to
    /// [`pending_screenshot`](Self::pending_screenshot) when due.
    pub pending_shot: Option<(Instant, ScreenshotReq)>,

    /// Open Android-style Overview, if any (dock's recents button or the
    /// three-finger swipe-up gesture).
    pub overview: Option<Overview>,

    /// In-flight Overview card drag (flick-to-close / tap-to-switch).
    pub overview_drag: Option<OverviewDrag>,

    /// Touch slot that owns the active Overview card drag, so its
    /// motion/up route to the card instead of the gesture aggregator.
    pub overview_touch_slot: Option<i32>,

    /// Touch slot that landed on the on-screen keyboard (a key tap or a
    /// title-strip drag). Its motion/up route to the OSK (drag / release)
    /// instead of the gesture aggregator or a client behind it.
    pub osk_touch_slot: Option<i32>,
    /// Left mouse button is held down on the OSK (a key press-and-hold or a
    /// title-strip drag). Its motion drives the drag and its release ends it,
    /// so the click never leaks to a client.
    pub osk_pointer_down: bool,

    /// In-flight single-finger drag-scroll of the apps menu grid.
    pub apps_menu_drag: Option<AppsMenuDrag>,
    /// Touch slot that owns that drag, so its motion/up route to the
    /// grid scroll instead of the gesture aggregator / a client.
    pub apps_menu_touch_slot: Option<i32>,

    /// Touch slot currently emulating the pointer for an X11 (XWayland)
    /// window. XWayland only forwards `wl_touch` as XI2 touch, which core-
    /// pointer-only X11 apps (e.g. OpenBoard) ignore — so we synthesise a
    /// left-button pointer drag from the first finger on an X11 surface,
    /// making touch behave like the (working) mouse. Wayland clients keep
    /// native `wl_touch` so in-app multitouch / pinch still works.
    pub touch_pointer_slot: Option<i32>,

    /// Software display brightness in `[MIN_BRIGHTNESS, 1.0]`. There's no
    /// hardware backlight on this machine, so the renderer dims the whole
    /// output with a translucent black overlay scaled by `1.0 - brightness`.
    pub brightness: f32,

    /// Dark-mode flag toggled from the Control Center. Tints the
    /// compositor's own chrome (panels/cards); clients are unaffected.
    pub dark_mode: bool,

    /// Decoded wallpaper image pixels (ABGR8, output-size), cached per
    /// logical resolution.  Rebuilt when the path or output size changes.
    pub wallpaper_cache: std::collections::HashMap<(u32, u32), smithay::backend::renderer::element::memory::MemoryRenderBuffer>,

    /// Saved pre-maximise/fullscreen geometry per window, so the
    /// unmaximise / unfullscreen path can restore the floating size
    /// and position.
    pub maximize_restore: HashMap<WindowId, Rect>,
    /// Independent tick clock for [`dock_reveal`](Self::dock_reveal) so
    /// its `dt` isn't consumed by [`tick_animations`](Self::tick_animations).
    pub last_reveal_tick: Instant,
    /// Manual "keep the dock up" override from the floating launcher button
    /// (or the Super tap). When `true` the window-presence auto-hide is
    /// overridden and the dock stays revealed. Cleared when a new window
    /// appears (so "open an app → dock hides") and when toggled off — see
    /// [`tick_dock_reveal`](Self::tick_dock_reveal) and [`toggle_dock`](Self::toggle_dock).
    pub dock_force_shown: bool,
    /// Last tick's count of visible (non-minimised) windows. A rising edge
    /// clears [`dock_force_shown`](Self::dock_force_shown) so launching /
    /// restoring an app tucks the dock away again.
    pub dock_prev_visible_count: usize,
    /// Bare-Super-tap tracking: set on a lone Super press, cleared the instant
    /// any other key joins the chord (so Super+digit etc. still work). A Super
    /// *release* while still set toggles the dock. The release is forwarded
    /// normally (not intercepted) so clients never see a stuck modifier.
    pub super_tap_armed: bool,
}

/// Pointer hot-zone (px) at the very bottom of the primary output that
/// reveals an auto-hidden dock.
pub const DOCK_REVEAL_EDGE: f32 = 2.0;

/// Pointer travel (px) required to promote a held click on a pinned
/// dock tile into a drag-reorder, vs. a plain click on release.
pub const DOCK_DRAG_THRESHOLD: f32 = 6.0;

/// How long the pointer must dwell on a dock tile before its
/// text tooltip is upgraded to a live window thumbnail. Short
/// sweeps across the bar never trigger the thumbnail.
pub const DOCK_HOVER_DWELL: Duration = Duration::from_millis(400);

/// Width (px) of the right-click context-menu plaque.
/// Sentinel `DockEntry::app` for the dock's applications-menu button.
/// Control-char prefix so it can never collide with a real app id.
pub const APPS_BUTTON_APP: &str = "\u{1}bacak:apps";

/// Sentinel `DockEntry::app` for the dock's Control-Center button.
/// Same control-char prefix scheme as [`APPS_BUTTON_APP`].
pub const SETTINGS_BUTTON_APP: &str = "\u{1}bacak:settings";

/// Sentinel `DockEntry::app` for the dock's Recents/Overview button.
pub const RECENTS_BUTTON_APP: &str = "\u{1}bacak:recents";

/// Upward travel (px) a card must be dragged before release closes its
/// window; a smaller move that stays within [`OVERVIEW_TAP_SLOP`] of the
/// press is treated as a tap (switch to that app) instead.
pub const OVERVIEW_CLOSE_DIST: f32 = 110.0;
/// Upward fling speed (px/s) that dismisses a card regardless of distance —
/// the kinetic "flick to dismiss" from Android Recents.
pub const DISMISS_VEL: f32 = 900.0;
/// How long to wait for a window to actually close after the request before
/// treating it as refused (card springs back + error pulse). Generous so a
/// slow-but-cooperative client isn't flagged; a hung/prompting one is.
pub const DISMISS_CONFIRM_TIMEOUT: Duration = Duration::from_millis(1800);
/// Duration of the "couldn't close" red pulse on a refused card.
pub const DISMISS_ERROR_MS: u128 = 700;
/// Horizontal travel (px) a 3-finger touchpad swipe must cover before it
/// "arms" and starts driving a workspace slide. Filters out micro-jitter
/// and lets a near-vertical 3-finger gesture stay an overview swipe.
pub const WS_SWIPE_START_PX: f32 = 40.0;
/// Fraction of the output width a swipe must drag the slide to before a
/// release commits the switch (otherwise it springs back).
pub const WS_SWIPE_COMMIT_FRAC: f32 = 0.4;
/// Per-frame horizontal velocity (px) above which a release commits the
/// switch regardless of distance — a quick flick still pages over.
pub const WS_SWIPE_FLING_VEL: f32 = 6.0;
/// Maximum edge rubber-band offset (px) when swiping past the first / last
/// workspace.
pub const WS_BOUNCE_MAX: f32 = 70.0;
/// Movement (px) under which a press→release counts as a tap, not a drag.
pub const OVERVIEW_TAP_SLOP: f32 = 10.0;

/// Applications grid menu geometry.
pub const APPS_MENU_CELL: f32 = 112.0;
pub const APPS_MENU_PAD: f32 = 18.0;
pub const APPS_MENU_LABEL_PX: f32 = 14.0;
/// Height of the search bar at the top of the apps menu.
pub const APPS_MENU_SEARCH_H: f32 = 44.0;
/// Fling projection time (s): on release, the scroll spring targets
/// `pos + velocity * this`. Tuned to the spring's natural period
/// (≈ `1/sqrt(stiffness)`) so a flick coasts to rest with minimal
/// overshoot rather than springing back.
pub const APPS_MENU_FLING_SECS: f64 = 0.067;
/// Width of the category sidebar on the left of the apps menu.
pub const APPS_MENU_SIDEBAR_W: f32 = 176.0;
/// Per-row height of a category entry in the sidebar.
pub const APPS_MENU_CAT_ROW_H: f32 = 38.0;

/// Software-brightness floor: the dim overlay never goes fully black, so
/// the user can always see the slider to bring it back up.
pub const MIN_BRIGHTNESS: f32 = 0.15;

/// Duration of the post-screenshot "camera flash" white overlay.
pub const FLASH_MS: u64 = 170;

/// How long a toast banner stays on screen, and the fade-out tail.
pub const TOAST_MS: u64 = 4000;
pub const TOAST_FADE_MS: u64 = 500;

/// Horizontal padding (px) of a Control-Center slider's track inside its
/// tile. Shared by the hit-test (click → level) and the renderer (level
/// → fill) so the thumb lands exactly where it's drawn.
pub const CC_SLIDER_INSET: f32 = 16.0;

pub const DOCK_MENU_W: f32 = 184.0;
/// Per-item row height (px); the plaque grows in height with the
/// number of items.
pub const DOCK_MENU_ROW_H: f32 = 26.0;
/// Padding (px) between the plaque edge and the first/last row.
pub const DOCK_MENU_PAD: f32 = 6.0;

// --- Floating selection menu (Android-style action bar) -------------------
/// Touch-friendly button height for the floating action menu (tall enough for
/// an icon stacked above the label).
pub const SEL_MENU_H: f32 = 60.0;
/// Icon glyph size (logical px) drawn above each menu label.
pub const SEL_MENU_ICON_PX: f32 = 24.0;
/// Width of the reveal (eye) button at the right end of the Wi-Fi password box.
pub const WIFI_EYE_W: f32 = 52.0;
/// Width of the gear (settings) button on the connected-network row.
pub const WIFI_GEAR_W: f32 = 44.0;

/// Placeholder shown in the static-IP box for each step.
fn static_field_hint(step: u8) -> &'static str {
    match step {
        0 => "IP/prefix (örn. 192.168.1.50/24)",
        1 => "Ağ geçidi (örn. 192.168.1.1)",
        _ => "DNS (örn. 8.8.8.8)",
    }
}

/// Title for each static-IP step.
fn static_field_title(step: u8) -> &'static str {
    match step {
        0 => "Statik IP · IP adresi",
        1 => "Statik IP · Ağ geçidi",
        _ => "Statik IP · DNS",
    }
}
/// Gap between the touch point and the menu (it floats *above* the finger so
/// the finger doesn't cover it).
pub const SEL_MENU_GAP: f32 = 14.0;
/// Horizontal padding inside each button (left + right of the label).
pub const SEL_MENU_BTN_PAD: f32 = 16.0;
/// Approximate label advance per character at [`SEL_MENU_LABEL_PX`]; used to
/// size buttons in the pure layout so the hit-test needs no font. The renderer
/// centres the rasterised label inside this rect, so small drift is invisible.
pub const SEL_MENU_CHAR_W: f32 = 8.5;
/// Label point size for menu buttons.
pub const SEL_MENU_LABEL_PX: f32 = 15.0;

// --- Tier A text panel ----------------------------------------------------
/// Inner padding between the panel plaque and its text.
pub const SEL_PANEL_PAD: f32 = 28.0;
/// Body font size + line height for the text panel.
pub const SEL_PANEL_FONT_PX: f32 = 20.0;
pub const SEL_PANEL_LINE_H: f32 = 30.0;
/// Tap radius for grabbing a selection handle.
pub const SEL_HANDLE_HIT: f32 = 26.0;
/// On-screen radius of a selection handle's knob.
pub const SEL_HANDLE_R: f32 = 9.0;
/// Minimum interval between AT-SPI drag round-trips (`SetSelection` + extents),
/// so a fast finger/pointer drag doesn't flood the app over D-Bus. ~30 Hz.
pub const ATSPI_DRAG_DEBOUNCE_MS: u64 = 33;
/// Panel background + body text colours.
pub const SEL_PANEL_TEXT_RGB: [u8; 3] = [232, 236, 242];
/// Demo text shown when the panel opens (proves the full selection UX without
/// needing a file picker / real document yet).
pub const SEL_PANEL_DEMO: &str = "Bacak native text selection.\n\nTap to place a caret, double-tap a word, triple-tap the line. Long-press starts a selection and opens the action menu. Drag the round handles to grow or shrink it, then Copy lands the text on the Wayland clipboard — paste it into any app.";


/// How long a pinned launch is considered "in flight". Past this, a
/// click re-launches (the previous attempt is assumed to have failed
/// or the app to have exited) and the "starting" cue stops.
pub const LAUNCH_DEBOUNCE: Duration = Duration::from_secs(8);

impl BacakState {
    /// Build a fresh state, wiring every Smithay protocol state object against
    /// the provided display.
    ///
    /// The default monitor is `1920×1080` until the runtime learns the real
    /// output geometry from the chosen backend (winit / udev / drm).
    pub fn new(display: &Display<Self>, seat_name: &str) -> Self {
        let dh = display.handle();

        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let xdg_activation = XdgActivationState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&dh);
        let pointer_gestures = PointerGesturesState::new::<Self>(&dh);
        let dmabuf_state = DmabufState::new();
        let xwayland_shell_state = XWaylandShellState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let viewporter_state =
            smithay::wayland::viewporter::ViewporterState::new::<Self>(&dh);
        let fractional_scale_state =
            smithay::wayland::fractional_scale::FractionalScaleManagerState::new::<Self>(&dh);
        let foreign_toplevel_list =
            smithay::wayland::foreign_toplevel_list::ForeignToplevelListState::new::<Self>(&dh);
        // Bacak's own zwp_text_input_manager_v3 global (see text_input.rs).
        dh.create_global::<Self, smithay::reexports::wayland_protocols::wp::text_input::zv3::server::zwp_text_input_manager_v3::ZwpTextInputManagerV3, ()>(1, ());
        // Allow any client to act as the input-method (no sandboxing yet).
        let input_method_manager_state =
            smithay::wayland::input_method::InputMethodManagerState::new::<Self, _>(
                &dh,
                |_client| true,
            );
        let virtual_keyboard_manager_state =
            smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState::new::<Self, _>(
                &dh,
                |_client| true,
            );
        let layer_shell_state =
            smithay::wayland::shell::wlr_layer::WlrLayerShellState::new::<Self>(&dh);
        // wlr-screencopy global (v3). Manual protocol; see screencopy.rs.
        dh.create_global::<Self, smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1, ()>(3, ());
        let single_pixel_buffer_state =
            smithay::wayland::single_pixel_buffer::SinglePixelBufferState::new::<Self>(&dh);
        let content_type_state =
            smithay::wayland::content_type::ContentTypeState::new::<Self>(&dh);
        // CLOCK_MONOTONIC = 1 (Linux); presentation timestamps use it.
        let presentation_state =
            smithay::wayland::presentation::PresentationState::new::<Self>(&dh, 1);
        let relative_pointer_manager_state =
            smithay::wayland::relative_pointer::RelativePointerManagerState::new::<Self>(&dh);
        let pointer_constraints_state =
            smithay::wayland::pointer_constraints::PointerConstraintsState::new::<Self>(&dh);
        // wlr foreign-toplevel *management* global (v3). Manual; see foreign_toplevel.rs.
        dh.create_global::<Self, smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1, ()>(3, ());
        let tablet_manager_state =
            smithay::wayland::tablet_manager::TabletManagerState::new::<Self>(&dh);
        // Filter MUST exclude clients that came in through a security context, so
        // a sandboxed app can't create nested (escaping) contexts.
        let security_context_state =
            smithay::wayland::security_context::SecurityContextState::new::<Self, _>(&dh, |client| {
                client
                    .get_data::<ClientState>()
                    .map_or(true, |s| s.security_context.is_none())
            });
        let mut seat_state = SeatState::<Self>::new();
        let seat = seat_state.new_wl_seat(&dh, seat_name);

        let monitor = Monitor {
            work_area: Rect::new(0.0, 0.0, 1920.0, 1080.0),
        };
        let wm = WindowManager::new(monitor);
        let osk = OskController::new(OskConfig::default());
        // Reserve-area invariant: when the OSK opens, the bottom strip of
        // the primary output is reserved so the focused window's viewport
        // shrinks above the keyboard instead of being painted over.
        // `WindowManager::new` always sets a primary; the expect is a
        // debug-only sanity check.
        let primary = wm
            .primary_output()
            .expect("WindowManager::new always sets a primary output");
        osk.bind(wm.clone(), primary);

        let now = Instant::now();
        let config = crate::config::CompositorConfig::load();
        let text = TextRenderer::load();
        let osk_font = TextRenderer::load_symbol();
        let emoji_font = crate::emoji::EmojiFont::load();
        // Build the close "×" once; falls back to the bare chip if the font
        // lacks the glyph (cc_rasterize → None).
        let deco_close_glyph =
            cc_rasterize(text.as_ref(), "\u{00d7}", 17.0, [238, 240, 245], 64);
        let mut s = Self {
            compositor_state,
            xdg_shell_state,
            popups: PopupManager::default(),
            pending_dmabuf: Vec::new(),
            viewporter_state,
            fractional_scale_state,
            foreign_toplevel_list,
            foreign_handles: std::collections::HashMap::new(),
            bacak_text_input: crate::text_input::BacakTextInput::default(),
            osk_dirty: false,
            surface_committed: false,
            osk_hide_at: None,
            input_method_manager_state,
            virtual_keyboard_manager_state,
            layer_shell_state,
            outputs: std::collections::HashMap::new(),
            pending_screencopy: Vec::new(),
            pending_screenshot: None,
            region_shot: None,
            window_pick: false,
            flash: None,
            toast: None,
            single_pixel_buffer_state,
            content_type_state,
            presentation_state,
            relative_pointer_manager_state,
            pointer_constraints_state,
            ftl_managers: Vec::new(),
            ftl_handles: std::collections::HashMap::new(),
            tablet_manager_state,
            security_context_state,
            pending_security_listeners: Vec::new(),
            ime_popups: Vec::new(),
            xdg_activation,
            shm_state,
            seat_state,
            seat,
            output_manager_state,
            data_device_state,
            primary_selection_state,
            x11_selection_sink: None,
            #[cfg(feature = "udev")]
            syncobj_state: None,
            #[cfg(feature = "udev")]
            syncobj_loop: None,
            pointer_gestures,
            dmabuf_state,
            dmabuf_global: None,
            display_handle: dh,
            wm,
            osk,
            start_time: now,
            windows: HashMap::new(),
            xwayland_shell_state,
            xdg_decoration_state,
            decorated: std::collections::HashSet::new(),
            title_drag: None,
            last_title_click: None,
            deco_label_cache: Mutex::new(HashMap::new()),
            xwm: None,
            x11_windows: HashMap::new(),
            x11_override: Vec::new(),
            pointer_position: (0.0, 0.0),
            cursor_status: CursorImageStatus::default_named(),
            focus_policy: FocusPolicy::default(),
            touch_aggregator: TouchAggregator::new(),
            touch_arbiter: TouchArbiter::new(),
            two_finger: TwoFingerRecognizer::new(),
            two_finger_target: None,
            two_finger_win_start: Rect::new(0.0, 0.0, 0.0, 0.0),
            animations: std::collections::HashMap::new(),
            workspace_slides: std::collections::HashMap::new(),
            ws_swipe: None,
            ws_bounce: None,
            snap_preview: None,
            last_anim_tick: now,
            focus_history: FocusHistory::new(),
            switcher_fade: None,
            blur_enabled: config.blur_enabled(),
            config,
            last_session_save: now,
            last_session_json: String::new(),
            pending_placements: Vec::new(),
            placement_done: std::collections::HashSet::new(),
            dialog_parent: std::collections::HashMap::new(),
            dialog_center_pending: std::collections::HashSet::new(),
            text,
            osk_font,
            emoji_font,
            deco_close_glyph,
            label_cache: Mutex::new(HashMap::new()),
            icon_cache: Mutex::new(HashMap::new()),
            snapshots: Mutex::new(HashMap::new()),
            pending_launches: HashMap::new(),
            dock_tooltip: Mutex::new(None),
            dock_reveal: HashMap::new(),
            last_reveal_tick: now,
            dock_force_shown: false,
            dock_prev_visible_count: 0,
            super_tap_armed: false,
            dock_drag: None,
            dock_touch_slot: None,
            dock_hover_started: None,
            dock_menu: None,
            floating_menu: None,
            loupe: None,
            single_touch_slot: None,
            mouse_select: false,
            selection_menu_target: None,
            selection_recognizer: SelectionRecognizer::new(),
            text_panel: None,
            clipboard_text: None,
            clipboard_image: None,
            atspi: None,
            atspi_selection: None,
            atspi_menu_ctx: None,
            apps_menu: None,
            control_center: None,
            cc_rx: None,
            wifi_panel: None,
            bt_panel: None,
            audio_panel: None,
            mic_panel: None,
            btctl: None,
            bt_last_pk: None,
            bt_paired: std::collections::HashSet::new(),
            wifi_rx: None,
            desktop_settings: None,
            ds_rx: None,
            file_browser: None,
            shot_dialog: None,
            pending_shot: None,
            overview: None,
            overview_drag: None,
            overview_touch_slot: None,
            osk_touch_slot: None,
            osk_pointer_down: false,
            apps_menu_drag: None,
            apps_menu_touch_slot: None,
            touch_pointer_slot: None,
            brightness: 1.0,
            dark_mode: true,
            maximize_restore: HashMap::new(),
            wallpaper_cache: HashMap::new(),
        };
        // Reserve the dock's bottom strut before anything else can
        // touch struts, so the OSK save/restore composes on top of it.
        s.apply_dock_strut();
        // Bring the workspace topology back the way the user left it.
        s.restore_session();
        s
    }

    /// Restore the primary output's workspace topology from
    /// `session.json` and stage the saved window placements so they're
    /// applied (Phase 2) as each client's app id becomes known.
    fn restore_session(&mut self) {
        let snap = crate::session::SessionSnapshot::load();
        // Stage placements regardless of workspace count — a fresh
        // profile with `workspace_count == 0` may still have windows.
        self.pending_placements = snap.windows.clone();
        // The workspace set is fixed (WORKSPACE_COUNT, seeded at add_output) —
        // we don't grow/shrink from the session, only restore which one was
        // active (clamped into the fixed set).
        let Some(out) = self.wm.primary_output() else { return };
        let mut wss = self.wm.workspaces_for(out);
        wss.sort_by_key(|w| w.id);
        let idx = snap.primary.active_index.min(wss.len().saturating_sub(1));
        if let Some(ws) = wss.get(idx) {
            let _ = self.wm.switch_workspace_on(out, ws.id);
        }
    }

    /// Build the current snapshot from the WM.
    fn session_snapshot(&self) -> crate::session::SessionSnapshot {
        use crate::session::{OutputSession, SessionSnapshot, WindowRec};
        let primary = match self.wm.primary_output() {
            Some(out) => {
                let mut wss = self.wm.workspaces_for(out);
                wss.sort_by_key(|w| w.id);
                let active = self.wm.active_workspace_on(out);
                let active_index = active
                    .and_then(|a| wss.iter().position(|w| w.id == a))
                    .unwrap_or(0);
                OutputSession { workspace_count: wss.len(), active_index }
            }
            None => OutputSession::default(),
        };
        let windows = self
            .wm
            .all_windows()
            .into_iter()
            .map(|w| {
                // Workspace slot = position of this window's workspace
                // in its output's id-sorted list (0 if unresolvable).
                let workspace_index = self
                    .wm
                    .output_for_window(w.id)
                    .map(|o| {
                        let mut wss = self.wm.workspaces_for(o.id);
                        wss.sort_by_key(|x| x.id);
                        wss.iter().position(|x| x.id == w.workspace).unwrap_or(0)
                    })
                    .unwrap_or(0);
                WindowRec {
                    app: w.app,
                    title: w.title,
                    x: w.geom.x,
                    y: w.geom.y,
                    w: w.geom.w,
                    h: w.geom.h,
                    workspace_index,
                }
            })
            .collect();
        SessionSnapshot { primary, windows }
    }

    /// Debounced session save: at most every 5 s, and only when the
    /// snapshot actually changed since the last write. Called every
    /// frame from the runtime tick.
    pub fn maybe_persist_session(&mut self, now: Instant) {
        if now.saturating_duration_since(self.last_session_save).as_secs() < 5 {
            return;
        }
        self.last_session_save = now;
        let snap = self.session_snapshot();
        let json = serde_json::to_string(&snap).unwrap_or_default();
        if json == self.last_session_json {
            return; // unchanged — skip the write
        }
        match snap.save() {
            Ok(()) => self.last_session_json = json,
            Err(e) => tracing::warn!(?e, "session.json save failed"),
        }
    }

    /// Unconditional session flush — no debounce, no skip. Called from
    /// the runtime loops when a shutdown signal arrives so the last
    /// few seconds of layout aren't lost to the periodic-save window.
    pub fn persist_session_now(&self) {
        let snap = self.session_snapshot();
        match snap.save() {
            Ok(()) => tracing::info!("session flushed on shutdown"),
            Err(e) => tracing::warn!(?e, "final session save failed"),
        }
    }

    /// Phase-2 placement restore. Called from the commit handler once
    /// per window: the first time the window's app id is known, a
    /// matching saved record (if any) is consumed and its geometry
    /// applied — so a relaunched app reopens where it was. One-shot
    /// per window; windows with no saved match keep the default
    /// cascade.
    pub fn try_restore_placement(&mut self, id: WindowId) {
        if self.placement_done.contains(&id) {
            return;
        }
        let Ok(win) = self.wm.get(id) else { return };
        if win.app.trim().is_empty() {
            // App id not set yet — retry on a later commit.
            return;
        }
        self.placement_done.insert(id);
        let Some(rec) = crate::session::take_placement(
            &mut self.pending_placements,
            &win.app,
            &win.title,
        ) else {
            return;
        };

        // Send it back to its saved workspace (by slot index on the
        // window's output), then restore its geometry. Order matters:
        // move_window_to_workspace may clear focus, and r#move just
        // sets geom — neither cares which ran first, but doing the
        // workspace move first keeps the window off-screen during the
        // geom set if it's not on the active workspace.
        if let Some(out) = self.wm.output_for_window(id).map(|o| o.id) {
            let mut wss = self.wm.workspaces_for(out);
            wss.sort_by_key(|w| w.id);
            if let Some(ws) = wss.get(rec.workspace_index) {
                let _ = self.wm.move_window_to_workspace(id, ws.id);
            }
        }
        let _ = self.wm.r#move(id, Rect::new(rec.x, rec.y, rec.w, rec.h));
    }

    /// Drop a window's cached switcher label. Called on destroy so the
    /// cache doesn't accumulate dead entries over a long session.
    pub fn evict_label(&self, id: WindowId) {
        self.label_cache.lock().remove(&id);
        self.deco_label_cache.lock().remove(&id);
    }

    /// Reverse lookup: given a Bacak `WindowId`, return the wayland surface
    /// that backs it (if it's still alive). Cost is O(n_windows) — fine for
    /// the dozens of windows a desktop session typically holds; revisit
    /// with a second map if profiling ever shows it hot.
    pub fn surface_for_window(&self, id: WindowId) -> Option<WlSurface> {
        self.windows
            .iter()
            .find_map(|(s, wid)| if *wid == id { Some(s.clone()) } else { None })
    }

    /// Hit-test the current pointer position against the WM and return the
    /// surface whose window sits topmost there. Wallpaper / dead space
    /// returns `None`.
    pub fn surface_under_pointer(&self) -> Option<WlSurface> {
        let (x, y) = self.pointer_position;
        let id = self.wm.hit_test(x as f32, y as f32)?;
        self.surface_for_window(id)
    }

    /// Like [`surface_under_pointer`](Self::surface_under_pointer) but
    /// also returns the surface's origin in WM-global logical
    /// coordinates. [`PointerHandle::motion`] takes
    /// `(surface, surface_location)` and subtracts the latter from the
    /// event location to derive surface-local coordinates. Passing the
    /// real window origin (instead of `(0, 0)`) is what makes clicks
    /// land on the right widget inside the client — otherwise every
    /// client receives the *global* pointer position and mis-hits.
    pub fn surface_under_pointer_with_loc(
        &self,
    ) -> Option<(WlSurface, smithay::utils::Point<f64, smithay::utils::Logical>)> {
        let (x, y) = self.pointer_position;
        self.surface_at(x, y)
    }

    /// The (sub)surface at WM-global logical `(x, y)` plus that surface's
    /// origin — the `(surface, surface_location)` pair the pointer and
    /// touch handles want. Shared so pointer-motion focus and touch-event
    /// routing hit-test identically.
    pub fn surface_at(
        &self,
        x: f64,
        y: f64,
    ) -> Option<(WlSurface, smithay::utils::Point<f64, smithay::utils::Logical>)> {
        let point = smithay::utils::Point::<f64, smithay::utils::Logical>::from((x, y));

        // Popups (app menus, dropdowns) render on top of every window, so they
        // must be hit-tested *first* — otherwise a click over a menu item lands
        // on the window behind it and the popup grab, which trusts this focus,
        // treats the menu as "clicked outside" (the item never fires, the menu
        // just closes). Check windows top-to-bottom; for each, walk its popup
        // surface trees at the exact origins the render path draws them
        // (`win_root + geo_loc + popup_offset - popup.geometry().loc`).
        let mut visible = self.wm.list_visible();
        visible.sort_by_key(|w| std::cmp::Reverse(w.z));
        for w in &visible {
            let Some(root) = self.surface_for_window(w.id) else { continue };
            let geo_loc = smithay::wayland::compositor::with_states(&root, |states| {
                states
                    .cached_state
                    .get::<smithay::wayland::shell::xdg::SurfaceCachedState>()
                    .current()
                    .geometry
                    .map(|g| g.loc)
                    .unwrap_or_default()
            });
            for (popup, popup_offset) in PopupManager::popups_for_surface(&root) {
                let off = geo_loc + popup_offset - popup.geometry().loc;
                let p_origin = smithay::utils::Point::<i32, smithay::utils::Logical>::from((
                    w.geom.x as i32 + off.x,
                    w.geom.y as i32 + off.y,
                ));
                if let Some((surface, loc)) = smithay::desktop::utils::under_from_surface_tree(
                    popup.wl_surface(),
                    point,
                    p_origin,
                    smithay::desktop::WindowSurfaceType::ALL,
                ) {
                    if crate::popup_debug() {
                        tracing::info!(
                            at = ?(x, y),
                            window = w.id,
                            popup_origin = ?(p_origin.x, p_origin.y),
                            "POPUP surface_at: hit POPUP surface (routed to menu, not window behind)"
                        );
                    }
                    return Some((surface, smithay::utils::Point::from((loc.x as f64, loc.y as f64))));
                }
            }
        }

        // Overlay + Top layer-shell surfaces sit above the app windows (panels,
        // notifiers, a focused launcher), so they're hit-tested before windows.
        use smithay::wayland::shell::wlr_layer::Layer;
        if let Some(hit) = self.layer_surface_at(x, y, &[Layer::Overlay, Layer::Top]) {
            return Some(hit);
        }

        // App windows.
        if let Some(id) = self.wm.hit_test(x as f32, y as f32) {
            if let Some(root) = self.surface_for_window(id) {
                if let Ok(w) = self.wm.get(id) {
                    // Walk the surface tree to the exact (sub)surface under the
                    // pointer and return *its* origin, not the toplevel's.
                    // Clients like Chromium render their UI into subsurfaces;
                    // delivering to the root surface with the toplevel origin
                    // left those widgets unreachable. `point` and `win_origin`
                    // are both WM-global logical, so the returned location is too.
                    let win_origin = (w.geom.x as i32, w.geom.y as i32);
                    if let Some((surface, loc)) = smithay::desktop::utils::under_from_surface_tree(
                        &root,
                        point,
                        win_origin,
                        smithay::desktop::WindowSurfaceType::ALL,
                    ) {
                        return Some((
                            surface,
                            smithay::utils::Point::from((loc.x as f64, loc.y as f64)),
                        ));
                    }
                }
            }
        }

        // Bottom + Background layer-shell surfaces sit behind the windows
        // (wallpaper, below-window panels) — the last thing under the pointer.
        self.layer_surface_at(x, y, &[Layer::Bottom, Layer::Background])
    }

    /// Whether the topmost window at WM-global logical `(x, y)` is an X11
    /// (XWayland) window — used to decide whether a touch there should be
    /// pointer-emulated (see [`touch_pointer_slot`](Self::touch_pointer_slot)).
    pub fn hit_window_is_x11(&self, x: f32, y: f32) -> bool {
        self.wm
            .hit_test(x, y)
            .map(|id| self.x11_windows.contains_key(&id))
            .unwrap_or(false)
    }

    /// The output currently under the pointer, with a primary fallback
    /// for points in the void between outputs and for the very first
    /// frame before the pointer has moved (initial `(0, 0)` lands inside
    /// primary on the canonical layout, but a quirky setup could place
    /// primary elsewhere). Used to route keyboard shortcuts so e.g.
    /// `Super+1` acts on the monitor the user is currently looking at.
    pub fn pointer_output(&self) -> Option<OutputId> {
        let (x, y) = self.pointer_position;
        self.wm
            .output_at(x as f32, y as f32)
            .or_else(|| self.wm.primary_output())
    }

    /// Promote a window on `ws` to focus so keyboard input never falls
    /// into a void — after a workspace switch, window move, or window
    /// close. Picks the topmost non-minimized window on `ws`; if there
    /// is nothing focusable, clears keyboard focus globally so the
    /// previously-focused window doesn't keep eating input from
    /// off-screen. Returns the new focus id, if any.
    pub fn auto_focus_workspace(&mut self, ws: WorkspaceId) -> Option<WindowId> {
        match self.wm.next_focus_candidate(ws) {
            Some(id) => {
                // `wm.focus` re-stamps z and clears focused on every
                // other window in one go — exactly what we want.
                let _ = self.wm.focus(id);
                let surface = self.surface_for_window(id);
                self.set_keyboard_focus(surface);
                Some(id)
            }
            None => {
                self.wm.clear_focus();
                self.set_keyboard_focus(None);
                None
            }
        }
    }

    /// Keep transient/modal dialogs stacked above their parent: after a window
    /// is raised (focus / focus-follows-pointer), bump every child dialog's z so
    /// it stays on top, recursing for dialog-of-dialog chains. Without this a
    /// "Save changes?" dialog hides behind its window — the user can't reach it
    /// and so can't close the app ([[bacak-ssd-decorations]] dialog handling).
    pub(crate) fn raise_child_dialogs(&mut self, parent: WindowId) {
        // Snapshot direct children first so the recursive `&mut self` calls
        // don't alias the `dialog_parent` borrow.
        let children: Vec<WindowId> = self
            .dialog_parent
            .iter()
            .filter(|(_, p)| **p == parent)
            .map(|(c, _)| *c)
            .collect();
        for child in children {
            let _ = self.wm.raise(child);
            self.raise_child_dialogs(child);
        }
    }

    /// The WM-global logical rect a window-pick screenshot would capture: the
    /// window's content plus its server-side title bar (height `BAR_H` directly
    /// above the content) when decorated. Shared by the pick-mode highlight
    /// overlay and the capture so they agree on the bounds.
    pub fn window_shot_rect(&self, id: WindowId) -> Option<Rect> {
        let w = self.wm.get(id).ok()?;
        Some(if self.decorated.contains(&id) {
            Rect::new(
                w.geom.x,
                w.geom.y - crate::decoration::BAR_H,
                w.geom.w,
                w.geom.h + crate::decoration::BAR_H,
            )
        } else {
            w.geom
        })
    }

    /// Current alpha of the post-screenshot camera flash on `output`, or `None`
    /// when there's no flash there or it has faded out. Bright instantly, then
    /// eases out over [`FLASH_MS`] ((1-t)² tail).
    pub fn flash_alpha(&self, output: OutputId) -> Option<f32> {
        let (out, start) = self.flash?;
        if out != output {
            return None;
        }
        let t = start.elapsed().as_millis() as f32 / FLASH_MS as f32;
        if t >= 1.0 {
            return None;
        }
        Some(0.6 * (1.0 - t) * (1.0 - t))
    }

    /// Forward a focus change to Smithay's keyboard handle. Pulled out
    /// because both branches of [`auto_focus_workspace`] need it and the
    /// `&mut self` plumbing through `kb.set_focus` reads better in one
    /// place.
    pub(crate) fn set_keyboard_focus(&mut self, surface: Option<WlSurface>) {
        // `get_keyboard` clones a cheap Arc handle; the original `seat`
        // borrow is dropped before we call `set_focus`, so we can pass
        // `&mut self` without aliasing.
        let Some(kb) = self.seat.get_keyboard() else { return };
        let serial = SERIAL_COUNTER.next_serial();
        kb.set_focus(self, surface, serial);
    }

    // ----- wlr-layer-shell helpers --------------------------------------

    /// Register a backend smithay `Output` so layer-shell / render / input can
    /// reach `layer_map_for_output`. Called by each backend when it brings an
    /// output up. Idempotent.
    pub fn register_output(&mut self, id: OutputId, output: smithay::output::Output) {
        self.outputs.insert(id, output);
    }

    /// Resolve a client-requested `wl_output` (or `None` → primary) to our
    /// `(OutputId, Output)` pair.
    pub fn resolve_layer_output(
        &self,
        wl: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) -> Option<(OutputId, smithay::output::Output)> {
        if let Some(wl) = wl {
            if let Some(o) = smithay::output::Output::from_resource(&wl) {
                if let Some((id, out)) = self.outputs.iter().find(|(_, ro)| **ro == o) {
                    return Some((*id, out.clone()));
                }
            }
        }
        let pid = self.wm.primary_output()?;
        self.outputs.get(&pid).map(|o| (pid, o.clone()))
    }

    /// The `(OutputId, Output)` whose `LayerMap` currently holds `surface`.
    pub fn output_of_layer(
        &self,
        surface: &WlSurface,
    ) -> Option<(OutputId, smithay::output::Output)> {
        self.outputs.iter().find_map(|(id, o)| {
            let has = smithay::desktop::layer_map_for_output(o)
                .layer_for_surface(surface, smithay::desktop::WindowSurfaceType::ALL)
                .is_some();
            has.then(|| (*id, o.clone()))
        })
    }

    /// Re-arrange an output's layer map and mirror its exclusive zones into the
    /// WM as the `"layer-shell"` strut layer, so window placement / maximize
    /// avoid panels and docks. `non_exclusive_zone` is output-local logical.
    pub fn arrange_layers(&self, oid: OutputId, output: &smithay::output::Output) {
        let nz = {
            let mut map = smithay::desktop::layer_map_for_output(output);
            map.arrange();
            map.non_exclusive_zone()
        };
        let Some(b) = self.wm.output(oid) else { return };
        let (w, h) = (b.bounds.w as i32, b.bounds.h as i32);
        let struts = crate::wm::Struts {
            left: nz.loc.x.max(0) as f32,
            top: nz.loc.y.max(0) as f32,
            right: (w - (nz.loc.x + nz.size.w)).max(0) as f32,
            bottom: (h - (nz.loc.y + nz.size.h)).max(0) as f32,
        };
        let _ = self.wm.set_strut_layer(oid, "layer-shell", struts);
    }

    /// Hit-test the layer surfaces in `want` at WM-global logical `(x, y)`,
    /// returning the `(surface, global-origin)` pair like [`Self::surface_at`].
    /// Used by `surface_at` to route input to panels/OSK (overlay+top sit above
    /// windows, bottom+background below).
    fn layer_surface_at(
        &self,
        x: f64,
        y: f64,
        want: &[smithay::wayland::shell::wlr_layer::Layer],
    ) -> Option<(WlSurface, smithay::utils::Point<f64, smithay::utils::Logical>)> {
        let oid = self.wm.output_at(x as f32, y as f32)?;
        let ob = self.wm.output(oid)?;
        let (offx, offy) = (ob.bounds.x as f64, ob.bounds.y as f64);
        let output = self.outputs.get(&oid)?;
        let map = smithay::desktop::layer_map_for_output(output);
        let lp = smithay::utils::Point::<f64, smithay::utils::Logical>::from((x - offx, y - offy));
        for layer_kind in want {
            if let Some(layer) = map.layer_under(*layer_kind, lp) {
                let geo = map.layer_geometry(layer).unwrap_or_default();
                let rel = lp - geo.loc.to_f64();
                if let Some((surf, so)) =
                    layer.surface_under(rel, smithay::desktop::WindowSurfaceType::ALL)
                {
                    return Some((
                        surf,
                        smithay::utils::Point::from((
                            offx + geo.loc.x as f64 + so.x as f64,
                            offy + geo.loc.y as f64 + so.y as f64,
                        )),
                    ));
                }
            }
        }
        None
    }

    // ----- alt+tab cycle ------------------------------------------------

    /// Snapshot of currently-focusable windows for an alt+tab cycle:
    /// every visible window that isn't minimized, ordered by
    /// `focus_history`'s MRU-then-unseen rule.
    fn alt_tab_candidates(&self) -> Vec<WindowId> {
        let visible: Vec<WindowId> = self
            .wm
            .list_visible()
            .into_iter()
            .filter(|w| !matches!(w.state, WinState::Minimized))
            .map(|w| w.id)
            .collect();
        self.focus_history.cycle_order(&visible)
    }

    /// Begin an alt+tab cycle. Returns `Some(target)` after focusing
    /// the next window, or `None` if there's nothing meaningful to
    /// cycle to (one or zero windows visible).
    pub fn alt_tab_start(&mut self, reverse: bool) -> Option<WindowId> {
        // Pin the overlay to the screen the user is looking at *now*.
        // Captured once so a pointer drift while Alt is held doesn't
        // make the switcher hop monitors mid-cycle.
        let output = self.pointer_output()?;
        let candidates = self.alt_tab_candidates();
        let target = self
            .focus_history
            .start_cycle(candidates, reverse, output)?;
        self.focus_for_cycle(target);
        // Fade the overlay in. Re-opening during a fade-out keeps the
        // current alpha for continuity (no flash back to 0).
        let start = self
            .switcher_fade
            .as_ref()
            .map(|f| f.alpha.pos)
            .unwrap_or(0.0);
        self.switcher_fade = Some(SwitcherFade {
            output,
            alpha: Spring::settle_to(start, 1.0),
            closing: false,
            frozen: None,
        });
        Some(target)
    }

    /// Capture the cycle's tiles and start the fade-out. Must run
    /// *before* the `FocusHistory` cycle is cleared, otherwise the
    /// snapshot is empty.
    fn begin_switcher_fade_out(&mut self) {
        let frozen = self
            .focus_history
            .cycle_candidates()
            .map(|c| (c.to_vec(), self.focus_history.current_cycle_window()));
        if let Some(f) = self.switcher_fade.as_mut() {
            f.frozen = frozen;
            f.closing = true;
            f.alpha.retarget(0.0);
        }
    }

    /// Step the active cycle. Returns the newly-previewed window, or
    /// `None` when no cycle is active.
    pub fn alt_tab_advance(&mut self, reverse: bool) -> Option<WindowId> {
        let target = self.focus_history.advance_cycle(reverse)?;
        self.focus_for_cycle(target);
        Some(target)
    }

    /// End the cycle and promote whichever window the user landed on
    /// to the MRU front. Called on Alt release.
    pub fn alt_tab_commit(&mut self) {
        self.begin_switcher_fade_out();
        self.focus_history.commit_cycle();
    }

    /// End the cycle and restore the window that had focus when it
    /// began. Called on Escape during a cycle.
    pub fn alt_tab_cancel(&mut self) {
        self.begin_switcher_fade_out();
        if let Some(prev) = self.focus_history.cancel_cycle() {
            // The cycle is now off, so the focus_changed mirror will
            // promote `prev` to the MRU front — which is exactly the
            // right thing for "cancel back to where you started".
            self.focus_for_cycle(prev);
        }
    }

    /// Apply focus during a cycle step. Skips the MRU push because
    /// `focus_history.promote` short-circuits while a cycle is active,
    /// and the `focus_changed` mirror in [`crate::handlers`] also
    /// defers to that gate.
    fn focus_for_cycle(&mut self, id: WindowId) {
        if self.wm.get(id).is_err() {
            return;
        }
        let _ = self.wm.focus(id);
        let surface = self.surface_for_window(id);
        self.set_keyboard_focus(surface);
    }

    /// The dock's slots on the primary output: pinned launchers first
    /// (in `config.dock_pinned` order — each bound to its topmost
    /// running window if the app is open, otherwise a launcher),
    /// followed by every other running window on the primary's active
    /// workspace (z order) that no pin already represents. Laid out as
    /// a centred row of square tiles along the bottom band.
    ///
    /// Empty when the dock is disabled, there's no primary, or there's
    /// nothing to show (no pins and no windows). The single source of
    /// truth shared by the renderer, click routing, and the
    /// minimise/restore animation target so they always agree.
    pub fn dock_tiles(&self) -> Vec<DockEntry> {
        match self.wm.primary_output() {
            Some(out) => self.dock_tiles_for(out),
            None => Vec::new(),
        }
    }

    /// The dock entries for a *specific* output. Pins are shared
    /// across outputs (they show on every dock), but each output's
    /// pinned slot binds the topmost running window of that app on
    /// *its own* active workspace — so the same pinned tile can be a
    /// launcher on one monitor and a window switcher on another.
    /// Non-pinned slots are the remaining running windows on that
    /// output's active workspace.
    pub fn dock_tiles_for(&self, out: OutputId) -> Vec<DockEntry> {
        if !self.config.dock {
            return Vec::new();
        }
        let Some(o) = self.wm.output(out) else { return Vec::new() };
        let Some(ws) = self.wm.active_workspace_on(out) else { return Vec::new() };
        let wins = self.wm.windows_on_workspace(ws); // ascending z

        let mut entries: Vec<DockEntry> = Vec::new();
        let mut bound: Vec<WindowId> = Vec::new();

        // 1. Pinned apps, in config order. Bind the topmost (highest z
        //    = last in the ascending list) running window of that app
        //    so clicking foregrounds it; with none running it's a pure
        //    launcher slot.
        for app in &self.config.dock_pinned {
            let win = wins
                .iter()
                .rev()
                .find(|w| &w.app == app)
                .map(|w| w.id);
            if let Some(id) = win {
                bound.push(id);
            }
            entries.push(DockEntry {
                rect: Rect::new(0.0, 0.0, 0.0, 0.0),
                app: app.clone(),
                window: win,
                pinned: true,
            });
        }

        // 2. Remaining running windows (any not already bound to a
        //    pin), ascending z, as regular non-pinned slots.
        for w in &wins {
            if bound.contains(&w.id) {
                continue;
            }
            entries.push(DockEntry {
                rect: Rect::new(0.0, 0.0, 0.0, 0.0),
                app: w.app.clone(),
                window: Some(w.id),
                pinned: false,
            });
        }

        // Applications-menu button. Kept *last in the entries vec* so it
        // never shifts the pinned-slot indices (the first
        // `dock_pinned.len()` entries) that the drag/reorder/target math
        // relies on — but the layout below draws it in the *leading* slot,
        // so it appears first (left on bottom/top docks, top on side docks).
        entries.push(DockEntry {
            rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            app: APPS_BUTTON_APP.to_string(),
            window: None,
            pinned: false,
        });

        // Recents/Overview button, then the Control-Center (gear) button.
        // Like the apps button these are kept last in the entries vec so
        // they never shift the pinned-slot indices; the layout draws them
        // in the *trailing* visual slots (recents, then settings rightmost).
        entries.push(DockEntry {
            rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            app: RECENTS_BUTTON_APP.to_string(),
            window: None,
            pinned: false,
        });
        entries.push(DockEntry {
            rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            app: SETTINGS_BUTTON_APP.to_string(),
            window: None,
            pinned: false,
        });

        if entries.is_empty() {
            return Vec::new();
        }

        let h = self.config.dock_height;
        let pad = (h * 0.18).max(2.0);
        let side = (h - 2.0 * pad).max(8.0);
        let gap = pad;
        let n = entries.len() as f32;
        let run = n * side + (n - 1.0).max(0.0) * gap;
        let b = o.bounds;
        let step = side + gap;
        // Visual slot remap: the three trailing sentinel entries are the
        // apps button (drawn in slot 0, leading edge), then recents and
        // settings drawn in the last two slots (trailing edge); every
        // regular tile sits one slot in from the leading edge. Entry
        // *order* is untouched, so all index-based click/drag math still
        // sees pins as the first `dock_pinned.len()` entries.
        let count = entries.len();
        let settings_idx = count.saturating_sub(1);
        let recents_idx = count.saturating_sub(2);
        let apps_idx = count.saturating_sub(3);
        let slot = |i: usize| {
            if i == apps_idx {
                0
            } else if i == recents_idx {
                count - 2
            } else if i == settings_idx {
                count - 1
            } else {
                i + 1
            }
        };
        // Centre the tile row along the dock's long axis: start it half the
        // leftover space in from the leading edge, so the bar sits centred on
        // the screen (clamped to `pad` so a too-wide row still starts on-screen).
        match self.config.dock_edge {
            DockEdge::Bottom => {
                let x0 = b.x + ((b.w - run) / 2.0).max(pad);
                let y = b.y + b.h - h + pad;
                for (i, e) in entries.iter_mut().enumerate() {
                    e.rect = Rect::new(x0 + slot(i) as f32 * step, y, side, side);
                }
            }
            DockEdge::Top => {
                let x0 = b.x + ((b.w - run) / 2.0).max(pad);
                let y = b.y + pad;
                for (i, e) in entries.iter_mut().enumerate() {
                    e.rect = Rect::new(x0 + slot(i) as f32 * step, y, side, side);
                }
            }
            DockEdge::Left => {
                let x = b.x + pad;
                let y0 = b.y + ((b.h - run) / 2.0).max(pad);
                for (i, e) in entries.iter_mut().enumerate() {
                    e.rect = Rect::new(x, y0 + slot(i) as f32 * step, side, side);
                }
            }
            DockEdge::Right => {
                let x = b.x + b.w - h + pad;
                let y0 = b.y + ((b.h - run) / 2.0).max(pad);
                for (i, e) in entries.iter_mut().enumerate() {
                    e.rect = Rect::new(x, y0 + slot(i) as f32 * step, side, side);
                }
            }
        }
        entries
    }

    /// The dock panel's on-screen rect (the icon row grown by the
    /// layout padding), at its *shown* position — the reveal offset is
    /// applied separately by the renderer. `None` when there are no
    /// tiles. Shared by the renderer and the auto-hide reveal logic so
    /// the bar's bounds are defined in exactly one place.
    pub fn dock_panel_rect(&self) -> Option<Rect> {
        self.dock_panel_rect_for(self.wm.primary_output()?)
    }

    /// Per-output panel rect — same idea, computed from that output's
    /// tile row.
    pub fn dock_panel_rect_for(&self, out: OutputId) -> Option<Rect> {
        let tiles = self.dock_tiles_for(out);
        if tiles.is_empty() {
            return None;
        }
        // Span the full tile row from its visual min to max corner. The
        // sentinel slot-remap means the leading/trailing tiles are *not*
        // the first/last entries, so derive the bounds from every rect.
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for t in &tiles {
            min_x = min_x.min(t.rect.x);
            min_y = min_y.min(t.rect.y);
            max_x = max_x.max(t.rect.x + t.rect.w);
            max_y = max_y.max(t.rect.y + t.rect.h);
        }
        let pad = (self.config.dock_height * 0.18).max(2.0);
        Some(Rect::new(
            min_x - pad,
            min_y - pad,
            (max_x - min_x) + 2.0 * pad,
            (max_y - min_y) + 2.0 * pad,
        ))
    }

    /// Offset magnitude (px) that fully tucks `out`'s panel past its
    /// nearest screen edge (plus a few px so its shadow doesn't peek).
    /// Always a positive distance; the renderer applies the sign / axis
    /// based on `config.dock_edge`. `None` when that output has no
    /// panel.
    fn dock_hidden_offset_for(&self, out: OutputId) -> Option<f32> {
        let o = self.wm.output(out)?;
        let p = self.dock_panel_rect_for(out)?;
        let b = o.bounds;
        Some(match self.config.dock_edge {
            DockEdge::Bottom => (b.y + b.h - p.y) + 4.0,
            DockEdge::Top => (p.y + p.h - b.y) + 4.0,
            DockEdge::Left => (p.x + p.w - b.x) + 4.0,
            DockEdge::Right => (b.x + b.w - p.x) + 4.0,
        })
    }

    /// True once `out`'s auto-hidden dock has fully settled off-screen
    /// — the renderer skips drawing the bar on that output entirely.
    pub fn dock_fully_hidden_for(&self, out: OutputId) -> bool {
        if !(self.config.dock && self.config.dock_autohide) {
            return false;
        }
        match (self.dock_hidden_offset_for(out), self.dock_reveal.get(&out)) {
            (Some(h), Some(s)) => s.pos >= h as f64 - 0.5,
            _ => false,
        }
    }

    /// Current reveal offset (px) for `out`'s dock — what the renderer
    /// subtracts from `off_y`. `0` when there's no spring yet (a brand
    /// new output starts fully shown then slides on first tick).
    pub fn dock_reveal_pos(&self, out: OutputId) -> f32 {
        self.dock_reveal.get(&out).map(|s| s.pos as f32).unwrap_or(0.0)
    }

    /// Per-output reveal decision for the **window-presence** auto-hide model
    /// (2026-05-27): the dock is shown only when nothing competes for the
    /// screen — i.e. there's no mapped, non-minimised window on this output's
    /// active workspace — or when the user has explicitly summoned it with the
    /// floating launcher button / Super tap ([`dock_force_shown`]). A context
    /// menu on this output also keeps it up so the menu isn't orphaned.
    /// (Replaced the earlier pointer-at-edge hover reveal.)
    fn dock_reveal_should_show_for(&self, out: OutputId) -> bool {
        if self.dock_force_shown {
            return true;
        }
        if matches!(&self.dock_menu, Some(m) if m.output == out) {
            return true;
        }
        !self.has_visible_window_on(out)
    }

    /// Any mapped, non-minimised window on `out`'s active workspace — the
    /// signal that drives the window-presence dock auto-hide.
    fn has_visible_window_on(&self, out: OutputId) -> bool {
        let Some(ws) = self.wm.active_workspace_on(out) else {
            return false;
        };
        self.wm
            .windows_on_workspace(ws)
            .iter()
            .any(|w| !matches!(w.state, WinState::Minimized))
    }

    /// Total visible (non-minimised) windows across every output's active
    /// workspace. A rising edge of this clears [`dock_force_shown`].
    fn visible_window_count(&self) -> usize {
        self.wm
            .outputs()
            .iter()
            .filter_map(|o| self.wm.active_workspace_on(o.id))
            .flat_map(|ws| self.wm.windows_on_workspace(ws))
            .filter(|w| !matches!(w.state, WinState::Minimized))
            .count()
    }

    /// Toggle the dock's manual "stay shown" override (floating launcher
    /// button / Super tap). With windows present this summons or re-hides the
    /// dock; with the desktop empty the dock is already shown so it's a no-op.
    pub fn toggle_dock(&mut self) {
        self.dock_force_shown = !self.dock_force_shown;
    }

    /// The always-visible floating launcher button — a small square pinned to
    /// the dock's leading corner (bottom-left for a bottom dock) that toggles
    /// the dock. Present only when the dock can hide (`dock` + `dock_autohide`);
    /// `None` otherwise. Anchored to the screen edge — it does NOT slide with
    /// the dock, so it stays hittable while the bar is tucked away.
    pub fn dock_floating_button_rect(&self, out: OutputId) -> Option<Rect> {
        if !(self.config.dock && self.config.dock_autohide) {
            return None;
        }
        let o = self.wm.output(out)?;
        let b = o.bounds;
        let h = self.config.dock_height;
        let pad = (h * 0.18).max(2.0);
        let side = (h - 2.0 * pad).max(8.0);
        let (x, y) = match self.config.dock_edge {
            DockEdge::Bottom => (b.x + pad, b.y + b.h - h + pad),
            DockEdge::Top => (b.x + pad, b.y + pad),
            DockEdge::Left => (b.x + pad, b.y + pad),
            DockEdge::Right => (b.x + b.w - h + pad, b.y + pad),
        };
        Some(Rect::new(x, y, side, side))
    }

    /// Hit-test the floating launcher button across all outputs; toggles the
    /// dock and returns `true` (so the backend swallows the click) on a hit.
    pub fn dock_floating_button_press(&mut self, px: f32, py: f32) -> bool {
        let hit = self
            .wm
            .outputs()
            .iter()
            .any(|o| self.dock_floating_button_rect(o.id).is_some_and(|r| r.contains(px, py)));
        if hit {
            self.toggle_dock();
        }
        hit
    }

    /// Drive every output's auto-hide reveal spring. Returns `true`
    /// while *any* of them is still moving — udev keeps `needs_redraw`
    /// armed off this. With auto-hide off (or the dock off) each
    /// spring's target is pinned at `0` so a previously-hidden bar
    /// slides back the instant the setting flips.
    pub fn tick_dock_reveal(&mut self, now: Instant) -> bool {
        let dt = now
            .saturating_duration_since(self.last_reveal_tick)
            .as_secs_f64();
        self.last_reveal_tick = now;
        let autohide = self.config.dock && self.config.dock_autohide;

        // "Open an app → dock hides": a rising edge in the visible-window
        // count drops the manual override so launching / restoring a window
        // re-tucks a summoned dock.
        let vis = self.visible_window_count();
        if vis > self.dock_prev_visible_count {
            self.dock_force_shown = false;
        }
        self.dock_prev_visible_count = vis;

        // Decide every output's target first (borrows `&self` for the
        // panel/edge math), then mutate the spring map.
        let outputs = self.wm.outputs();
        let targets: Vec<(OutputId, f64)> = outputs
            .iter()
            .map(|o| {
                let target = if !autohide || self.dock_reveal_should_show_for(o.id)
                {
                    0.0
                } else {
                    self.dock_hidden_offset_for(o.id).unwrap_or(0.0) as f64
                };
                (o.id, target)
            })
            .collect();

        let mut moving = false;
        for (id, target) in targets {
            let s = self
                .dock_reveal
                .entry(id)
                .or_insert_with(|| Spring::settle_to(0.0, 0.0));
            s.retarget(target);
            if s.step(dt) {
                moving = true;
            }
        }
        // Forget springs for outputs that no longer exist (hot-unplug).
        let alive: std::collections::HashSet<OutputId> =
            self.wm.outputs().into_iter().map(|o| o.id).collect();
        self.dock_reveal.retain(|id, _| alive.contains(id));
        moving
    }

    /// The dock landing slot for `id`. With the dock enabled this is
    /// the window's own tile in the bar; otherwise the legacy
    /// bottom-centre stand-in slot.
    fn dock_slot_for(&self, id: WindowId) -> Option<Rect> {
        if self.config.dock {
            // Each output has its own dock now, so look up the
            // window's own output rather than the primary.
            let o = self.wm.output_for_window(id)?;
            if let Some(e) = self
                .dock_tiles_for(o.id)
                .into_iter()
                .find(|e| e.window == Some(id))
            {
                return Some(e.rect);
            }
            // Window not in this output's tile list (e.g. minimised
            // already) — fall back to that bar's centre so it still
            // flies there.
            let side = (self.config.dock_height * 0.64).max(8.0);
            return Some(Rect::new(
                o.bounds.x + o.bounds.w / 2.0 - side / 2.0,
                o.bounds.y + o.bounds.h - self.config.dock_height
                    + (self.config.dock_height - side) / 2.0,
                side,
                side,
            ));
        }
        let sw = self.config.dock_slot_w;
        let sh = self.config.dock_slot_h;
        let wa = self.wm.output_for_window(id)?.work_area();
        Some(Rect::new(
            wa.x + wa.w / 2.0 - sw / 2.0,
            wa.y + wa.h - sh,
            sw,
            sh,
        ))
    }

    /// Reserve (or release) the compositor dock's strut on *every*
    /// output, on the configured edge. Called at startup, after a
    /// config hot-reload, and after hot-plug so snap math / `work_area`
    /// exclude each monitor's bar. An auto-hiding dock reserves no
    /// space anywhere — it floats over content on demand. The other
    /// three edges' dock-contribution struts are cleared so an edge
    /// swap (e.g. bottom → top) doesn't leave a stale reservation.
    pub fn apply_dock_strut(&self) {
        let v = if self.config.dock && !self.config.dock_autohide {
            self.config.dock_height
        } else {
            0.0
        };
        // Build the dock's *own* layer — zero on the three edges it
        // doesn't occupy, `v` on the one it does. Other contributors
        // (the OSK, panels, manual test struts) live on their own
        // layers and compose around this via per-edge max, so an edge
        // swap here can never trample a live OSK reservation.
        let mut layer = Struts::default();
        match self.config.dock_edge {
            DockEdge::Bottom => layer.bottom = v,
            DockEdge::Top => layer.top = v,
            DockEdge::Left => layer.left = v,
            DockEdge::Right => layer.right = v,
        }
        for o in self.wm.outputs() {
            let _ = self.wm.set_strut_layer(o.id, "dock", layer);
        }
    }

    /// Left-press entry point for the dock — paired with
    /// [`dock_release`](Self::dock_release). Non-pinned tiles activate
    /// immediately (preserves the snappy press feel). Pinned tiles
    /// only *register* the press, so the release can decide between a
    /// plain click (activate) and a drag-reorder. Returns `true` when
    /// the press was consumed so the backend skips the normal
    /// click-to-focus path.
    ///
    /// Activation semantics (whichever path runs):
    /// - Running window: toggle. The already-focused, visible window
    ///   minimises; everything else restores / focuses via
    ///   [`restore_window_animated`].
    /// - A minimised or mid-(un)minimise window always restores —
    ///   never re-hide something the user is trying to bring back.
    /// - A pinned slot whose app isn't running launches it (Phase 3
    ///   debounce applies).
    pub fn dock_press(&mut self, px: f32, py: f32) -> bool {
        if !self.config.dock {
            return false;
        }
        let Some(out) = self.wm.output_at(px, py) else { return false };
        // Don't intercept presses through a fully-hidden bar — the
        // user should be able to click the bottom of the screen
        // normally while the dock waits to reveal.
        if self.dock_fully_hidden_for(out) {
            return false;
        }
        let tiles = self.dock_tiles_for(out);
        let Some((i, entry)) = tiles
            .iter()
            .enumerate()
            .find(|(_, e)| {
                let r = e.rect;
                px >= r.x && px <= r.x + r.w && py >= r.y && py <= r.y + r.h
            })
            .map(|(i, e)| (i, e.clone()))
        else {
            return false;
        };

        // Every dock press *may* become a drag — defer activation to
        // release, where we know whether the pointer crossed the
        // threshold and where it landed. Pinned-tile drags reorder or
        // unpin; non-pinned drags pin the app to the bar. A press +
        // release without motion still fires the activate path below
        // ([`dock_release`]), so the click case feels just as snappy
        // as before drag existed.
        self.dock_drag = Some(DockDrag {
            output: out,
            from_idx: i,
            start_x: px,
            start_y: py,
            current_x: px,
            current_y: py,
            started: false,
            source_was_pinned: entry.pinned,
            source_app: entry.app.clone(),
        });
        true
    }

    /// Track motion while a dock press is held: promote a pending
    /// drag to "started" once the input crosses [`DOCK_DRAG_THRESHOLD`]
    /// from its press origin. Cheap and no-op when no drag is pending,
    /// so backends can call it on every pointer or touch motion event.
    pub fn dock_pointer_motion(&mut self, px: f32, py: f32) {
        // Update the hover-dwell timer. We do this on every motion
        // because that's the only signal we get; once the pointer
        // stops, the renderer reads `dock_thumbnail_ready` against the
        // stored `since`.
        let new_hover = self.wm.output_at(px, py).and_then(|out| {
            self.dock_tiles_for(out)
                .iter()
                .position(|e| {
                    let r = e.rect;
                    px >= r.x && px <= r.x + r.w && py >= r.y && py <= r.y + r.h
                })
                .map(|i| (out, i))
        });
        match (self.dock_hover_started.as_ref(), new_hover) {
            (Some((o, i, _)), Some((no, ni))) if *o == no && *i == ni => {
                // Same tile — keep the existing `since`.
            }
            (_, Some((o, i))) => {
                self.dock_hover_started = Some((o, i, Instant::now()));
            }
            (_, None) => {
                self.dock_hover_started = None;
            }
        }

        // Drag-motion bookkeeping (unchanged).
        let Some(d) = self.dock_drag.as_mut() else { return };
        d.current_x = px;
        d.current_y = py;
        if !d.started {
            let dx = d.current_x - d.start_x;
            let dy = d.current_y - d.start_y;
            if (dx * dx + dy * dy).sqrt() >= DOCK_DRAG_THRESHOLD {
                d.started = true;
            }
        }
    }

    /// Pair to [`dock_press`](Self::dock_press): commit a drag (reorder
    /// / unpin / pin) or fall through to a plain activate if the
    /// pointer never crossed the threshold. Returns whether the
    /// release was consumed.
    ///
    /// Drop semantics:
    /// - Pinned source dropped on the panel → reorder within pins.
    /// - Pinned source dropped *off* the panel → unpin.
    /// - Non-pinned source dropped on the panel → pin the app at the
    ///   snap target (no-op if its app is already pinned).
    /// - Non-pinned source dropped off the panel → abandon, no write.
    pub fn dock_release(&mut self, px: f32, py: f32) -> bool {
        let Some(mut d) = self.dock_drag.take() else { return false };
        // Use the release position as the freshest sample; some
        // devices skip a final motion and go straight to up.
        d.current_x = px;
        d.current_y = py;
        if !d.started {
            // It was a click, not a drag — activate the tile under
            // the original press.
            if let Some(entry) = self.dock_tiles_for(d.output).get(d.from_idx).cloned() {
                self.activate_entry(&entry);
            }
            return true;
        }

        // The sentinel buttons (apps / recents / settings) aren't
        // pinnable or reorderable — a drag off any of them commits nothing.
        if d.source_app == APPS_BUTTON_APP
            || d.source_app == SETTINGS_BUTTON_APP
            || d.source_app == RECENTS_BUTTON_APP
        {
            return true;
        }

        // Drag committed: where did it land?
        let panel = self.dock_panel_rect_for(d.output);
        let on_panel = panel.is_some_and(|p| {
            d.current_x >= p.x
                && d.current_x <= p.x + p.w
                && d.current_y >= p.y
                && d.current_y <= p.y + p.h
        });
        let target =
            self.dock_drag_target_idx_at(d.output, d.current_x, d.current_y);

        if d.source_was_pinned {
            if !on_panel {
                // Pinned tile dragged off the bar → unpin.
                if d.from_idx < self.config.dock_pinned.len() {
                    self.config.dock_pinned.remove(d.from_idx);
                    if let Err(e) = self.config.save() {
                        tracing::warn!(?e, "could not persist unpin");
                    }
                }
                return true;
            }
            // Pinned + on-panel → reorder.
            let Some(target_idx) = target else { return true };
            let pinned_len = self.config.dock_pinned.len();
            if d.from_idx >= pinned_len {
                return true; // pin vanished mid-drag; nothing to commit.
            }
            let insert_at = if target_idx > d.from_idx {
                target_idx - 1
            } else {
                target_idx
            };
            if insert_at == d.from_idx {
                return true; // no-op move.
            }
            let app = self.config.dock_pinned.remove(d.from_idx);
            let bounded_insert = insert_at.min(self.config.dock_pinned.len());
            self.config.dock_pinned.insert(bounded_insert, app);
            if let Err(e) = self.config.save() {
                tracing::warn!(?e, "could not persist reordered dock_pinned");
            }
            return true;
        }

        // Non-pinned source: only meaningful on-panel — pins the app.
        if !on_panel {
            return true;
        }
        if self.config.dock_pinned.iter().any(|p| p == &d.source_app) {
            return true; // already pinned (e.g. second window of a pinned app).
        }
        let insert_at = match target {
            Some(t) => t.min(self.config.dock_pinned.len()),
            // No pins exist yet → pinning starts the list.
            None => 0,
        };
        self.config.dock_pinned.insert(insert_at, d.source_app);
        if let Err(e) = self.config.save() {
            tracing::warn!(?e, "could not persist new pin");
        }
        true
    }

    /// Abandon any in-flight dock interaction without committing it —
    /// the touch backends call this on `TouchCancel`, where the
    /// gesture went away without a clean up event. Clears the drag
    /// preview but never writes the pin list.
    pub fn dock_touch_cancel(&mut self) {
        self.dock_drag = None;
        self.dock_touch_slot = None;
    }

    /// Insertion index the pointer is currently snapped to during a
    /// drag, in the *pre-removal* pinned list — range
    /// `0..=pinned_count` (0 = before first, `pinned_count` = after
    /// last). Counts pinned slots whose centre sits left of the
    /// pointer. Public so the renderer can show the insertion preview
    /// in agreement with what `dock_release` will commit.
    pub fn dock_drag_target_idx_at(
        &self,
        out: OutputId,
        px: f32,
        py: f32,
    ) -> Option<usize> {
        let pinned_len = self.config.dock_pinned.len();
        if pinned_len == 0 {
            return None;
        }
        let tiles = self.dock_tiles_for(out);
        // Pins are placed first by `dock_tiles_for`, so the first
        // `pinned_len` rects in `tiles` are pinned-slot rects. For
        // a horizontal dock we count pins whose centre.x is left of
        // the pointer; for a vertical dock, centre.y above it.
        let horizontal = self.config.dock_edge.is_horizontal();
        let count = tiles
            .iter()
            .take(pinned_len)
            .filter(|e| {
                if horizontal {
                    e.rect.x + e.rect.w / 2.0 < px
                } else {
                    e.rect.y + e.rect.h / 2.0 < py
                }
            })
            .count();
        Some(count)
    }

    /// Right-press entry point: if the pointer is on a dock tile,
    /// build its context-menu items and open the plaque. If a menu is
    /// already open, the right press just closes it (toggle). Returns
    /// whether the press was consumed so the backend swallows it.
    pub fn dock_right_press(&mut self, px: f32, py: f32) -> bool {
        if !self.config.dock {
            return false;
        }
        // A second right-press anywhere = close + consume (toggle).
        if self.dock_menu.is_some() {
            self.dock_menu = None;
            return true;
        }
        let Some(out) = self.wm.output_at(px, py) else { return false };
        if self.dock_fully_hidden_for(out) {
            return false;
        }
        let tiles = self.dock_tiles_for(out);
        let Some(entry) = tiles.iter().find(|e| {
            let r = e.rect;
            px >= r.x && px <= r.x + r.w && py >= r.y && py <= r.y + r.h
        }) else {
            return false;
        };
        if entry.app == APPS_BUTTON_APP
            || entry.app == SETTINGS_BUTTON_APP
            || entry.app == RECENTS_BUTTON_APP
        {
            return true; // sentinel buttons have no right-click context menu
        }
        let items = self.build_dock_menu_items(entry);
        let rect = self.compute_dock_menu_rect(out, entry.rect, items.len());
        self.dock_menu = Some(DockMenu { output: out, rect, items });
        true
    }

    /// Left-press handling while a context menu is open: if the press
    /// lands on a menu row, fire that action and close; otherwise
    /// close the menu. Either way the press is consumed so it never
    /// reaches a tile or a client. Returns whether a menu was
    /// involved (callers fall through to `dock_press` when `false`).
    pub fn dock_menu_left_press(&mut self, px: f32, py: f32) -> bool {
        let Some(menu) = self.dock_menu.clone() else { return false };
        // Off-menu click → just dismiss.
        let r = menu.rect;
        if px < r.x || px > r.x + r.w || py < r.y || py > r.y + r.h {
            self.dock_menu = None;
            return true;
        }
        // On-menu: figure out which row.
        let row_y = py - r.y - DOCK_MENU_PAD;
        if row_y >= 0.0 {
            let idx = (row_y / DOCK_MENU_ROW_H) as usize;
            if idx < menu.items.len() {
                let action = menu.items[idx].action.clone();
                self.run_dock_menu_action(action);
            }
        }
        self.dock_menu = None;
        true
    }

    /// Close any open dock context menu. Called from Escape and
    /// whenever the compositor needs to dismiss it (e.g. focus
    /// changes, dock vanishes via a config edit).
    pub fn dock_close_menu(&mut self) {
        self.dock_menu = None;
    }

    // --- Floating selection menu ------------------------------------------

    /// Open the Android-style action menu anchored at `(ax, ay)` (the
    /// long-press point, in WM-global logical px). Replaces any open menu.
    pub fn open_selection_menu(&mut self, ax: f32, ay: f32) {
        let Some(out) = self.wm.output_at(ax, ay).or_else(|| self.wm.primary_output()) else {
            return;
        };
        // Remember the window under the menu point so Copy/Paste can re-focus it
        // before synthesising the clipboard chord (the menu interaction must not
        // leave the keys with nowhere to land). Fall back to the focused window.
        self.selection_menu_target = self
            .wm
            .hit_test(ax, ay)
            .or_else(|| self.wm.all_windows().into_iter().find(|w| w.focused).map(|w| w.id));
        self.floating_menu = Some(self.build_selection_menu(out, ax, ay));
    }

    /// Dismiss the floating action menu (Escape, focus change, action fired).
    pub fn close_selection_menu(&mut self) {
        self.floating_menu = None;
        self.atspi_menu_ctx = None;
        self.selection_menu_target = None;
    }

    /// Handle a press at `(px, py)` while the floating menu is open. On a
    /// button → fire its action and close; off-menu → just dismiss. Either way
    /// the press is consumed so it never reaches the client. Returns whether a
    /// menu was open (callers forward the press normally when `false`).
    pub fn selection_menu_press(&mut self, px: f32, py: f32) -> bool {
        let Some(menu) = self.floating_menu.take() else { return false };
        if let Some(idx) = menu.item_at(px, py) {
            if crate::clip_debug() {
                tracing::info!(at = ?(px, py), item = ?menu.items[idx].action, "SELECTION menu press → item");
            }
            self.run_selection_action(menu.items[idx].action);
        } else {
            if crate::clip_debug() {
                tracing::info!(at = ?(px, py), "SELECTION menu press → off-menu (dismiss)");
            }
            // Off-menu tap dismisses — also drop the Tier C overlay it belonged
            // to (the native panel manages its own dismissal separately).
            self.atspi_selection = None;
        }
        // The right-click AT-SPI context is consumed once the menu closes.
        self.atspi_menu_ctx = None;
        self.selection_menu_target = None;
        true
    }

    /// Lay out the four-button action strip centred above `(ax, ay)`, clamped
    /// to the output's bounds (flips below the finger if there's no room
    /// above). Button widths are derived from label length so the pure
    /// hit-test needs no font; see [`SEL_MENU_CHAR_W`].
    fn build_selection_menu(&self, out: OutputId, ax: f32, ay: f32) -> FloatingMenu {
        let defs = [
            ("Kopyala", SelectionAction::Copy, "edit-copy"),
            ("Yapıştır", SelectionAction::Paste, "edit-paste"),
            ("Tümünü Seç", SelectionAction::SelectAll, "edit-select-all"),
            ("Ara", SelectionAction::Search, "edit-find"),
        ];
        let mut items = Vec::with_capacity(defs.len());
        let mut widths = Vec::with_capacity(defs.len());
        for (label, action, icon) in defs {
            let w = 2.0 * SEL_MENU_BTN_PAD + label.chars().count() as f32 * SEL_MENU_CHAR_W;
            widths.push(w);
            items.push(FloatingMenuItem { label: label.to_string(), action, icon });
        }
        let total_w: f32 = widths.iter().sum();

        let bounds = self
            .wm
            .output(out)
            .map(|o| o.bounds)
            .unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0));
        let x = (ax - total_w / 2.0).clamp(bounds.x, (bounds.x + bounds.w - total_w).max(bounds.x));
        // Float above the finger; drop below if it would clip the top edge.
        let mut y = ay - SEL_MENU_GAP - SEL_MENU_H;
        if y < bounds.y {
            y = ay + SEL_MENU_GAP;
        }
        y = y.clamp(bounds.y, (bounds.y + bounds.h - SEL_MENU_H).max(bounds.y));

        let rect = Rect::new(x, y, total_w, SEL_MENU_H);
        let mut buttons = Vec::with_capacity(widths.len());
        let mut bx = x;
        for w in &widths {
            buttons.push(Rect::new(bx, y, *w, SEL_MENU_H));
            bx += w;
        }
        FloatingMenu { output: out, rect, items, buttons }
    }

    /// Fire a [`SelectionAction`]. Over a **native text panel** (Tier A) we own
    /// the text, so Copy reads it straight out of the buffer onto the clipboard
    /// and Select-all extends the buffer selection — no key synthesis (there's
    /// no client to receive one). Otherwise (Tier B, a foreign client has
    /// focus) we synthesise the keystroke that toolkit already binds.
    pub fn run_selection_action(&mut self, action: SelectionAction) {
        // evdev key codes (linux/input-event-codes.h).
        const KEY_LEFTCTRL: u32 = 29;
        const KEY_LEFTSHIFT: u32 = 42;
        const KEY_A: u32 = 30;
        const KEY_C: u32 = 46;
        const KEY_V: u32 = 47;

        // Terminals bind copy/paste to Ctrl+Shift+C/V — plain Ctrl+C there is
        // SIGINT (it kills the running command instead of copying), Ctrl+V is a
        // literal paste of the control char. So the synthesised chord must carry
        // Shift when a terminal emulator has focus.
        let term = self.focused_is_terminal();
        let copy_mods: &[u32] = if term { &[KEY_LEFTCTRL, KEY_LEFTSHIFT] } else { &[KEY_LEFTCTRL] };

        // Selection actions are rare, user-initiated events (a menu click), not
        // a hot path — log unconditionally so a live copy/paste failure is
        // diagnosable from the journal without needing a debug env flag.
        let focused_app = self
            .wm
            .all_windows()
            .into_iter()
            .find(|w| w.focused)
            .map(|w| w.app);
        let has_kbd_focus = self
            .seat
            .get_keyboard()
            .map(|k| k.current_focus().is_some())
            .unwrap_or(false);
        // The surface to re-focus before synthesising — the window the menu was
        // opened over. Resolved up front (immutable borrow) so the synthesise
        // branches can `set_keyboard_focus` it without aliasing `self`.
        let menu_target_surface = self
            .selection_menu_target
            .and_then(|id| self.surface_for_window(id));

        // Resolve native-panel context up front so the immutable borrow is
        // released before any `&mut self` call below.
        let panel_open = self.text_panel.is_some();
        // Copy source, in priority order: native panel selection, then a Tier C
        // (AT-SPI) foreign-app selection. Either lets Copy read the text
        // directly instead of synthesising Ctrl+C.
        let native_copy = self
            .text_panel
            .as_ref()
            .filter(|p| p.native.has_selection())
            .map(|p| p.native.selected_text())
            .or_else(|| self.atspi_selection.as_ref().map(|a| a.text.clone()))
            .or_else(|| self.atspi_menu_selected_text());

        match action {
            // Search is Copy for now (the web-search hand-off lands later).
            SelectionAction::Copy | SelectionAction::Search => {
                if crate::clip_debug() {
                    let branch = if native_copy.is_some() { "clipboard-direct" } else { "synthesize" };
                    tracing::info!(
                        ?focused_app, is_terminal = term, has_kbd_focus,
                        refocus = menu_target_surface.is_some(),
                        chord = if term { "Ctrl+Shift+C" } else { "Ctrl+C" },
                        branch,
                        "SELECTION Copy"
                    );
                }
                match native_copy {
                    Some(text) => self.copy_text_to_clipboard(text),
                    None => {
                        // Re-assert focus on the menu's target window so the
                        // synthesised chord actually reaches it.
                        if let Some(s) = menu_target_surface.clone() {
                            self.set_keyboard_focus(Some(s));
                        }
                        self.synthesize_chord(copy_mods, KEY_C);
                    }
                }
                // Android dismisses the selection + handles after a copy.
                self.atspi_selection = None;
                if let Some(panel) = self.text_panel.as_mut() {
                    panel.native.clear();
                }
            }
            SelectionAction::Paste => {
                // Prefer a direct AT-SPI caret-insert (reliable); fall back to
                // synthesising Ctrl+V (e.g. another app owns the clipboard, or
                // the target isn't an AT-SPI editable).
                let pasted_via_atspi = self.atspi_paste();
                if crate::clip_debug() {
                    tracing::info!(
                        ?focused_app, is_terminal = term, has_kbd_focus,
                        refocus = menu_target_surface.is_some(),
                        via = if pasted_via_atspi { "atspi-insert" } else if term { "Ctrl+Shift+V" } else { "Ctrl+V" },
                        "SELECTION Paste"
                    );
                }
                if !pasted_via_atspi {
                    if let Some(s) = menu_target_surface.clone() {
                        self.set_keyboard_focus(Some(s));
                    }
                    self.synthesize_chord(copy_mods, KEY_V);
                }
            }
            SelectionAction::SelectAll => {
                if panel_open {
                    if let Some(panel) = self.text_panel.as_mut() {
                        panel.native.select_all();
                    }
                } else {
                    self.synthesize_chord(&[KEY_LEFTCTRL], KEY_A);
                }
            }
        }
    }

    /// Whether the currently focused window is a terminal emulator. Terminals
    /// bind copy/paste to Ctrl+Shift+C/V (plain Ctrl+C is SIGINT), so the
    /// synthesised clipboard chord must add Shift for them. Heuristic on the
    /// window's `app_id`/WM_CLASS — covers the common emulators; an unrecognised
    /// terminal just falls back to the plain chord (the prior behaviour).
    fn focused_is_terminal(&self) -> bool {
        self.wm
            .all_windows()
            .into_iter()
            .find(|w| w.focused)
            .map(|w| {
                let a = w.app.to_ascii_lowercase();
                [
                    "terminal", "konsole", "xterm", "alacritty", "kitty",
                    "wezterm", "rxvt", "tilix", "terminator", "kgx", "foot",
                ]
                .iter()
                .any(|t| a.contains(t))
            })
            .unwrap_or(false)
    }

    /// Take ownership of the clipboard (and primary) as a compositor-side text
    /// source. The bytes are served lazily to a requesting client via
    /// `SelectionHandler::send_selection` ([`SelectionOrigin::NativeText`]).
    pub fn copy_text_to_clipboard(&mut self, text: String) {
        use smithay::wayland::selection::data_device::set_data_device_selection;
        use smithay::wayland::selection::primary_selection::set_primary_selection;
        if text.is_empty() {
            return;
        }
        self.clipboard_text = Some(text);
        let mimes = vec![
            "text/plain;charset=utf-8".to_string(),
            "text/plain".to_string(),
            "UTF8_STRING".to_string(),
            "STRING".to_string(),
        ];
        set_data_device_selection(
            &self.display_handle,
            &self.seat,
            mimes.clone(),
            SelectionOrigin::NativeText,
        );
        set_primary_selection(&self.display_handle, &self.seat, mimes, SelectionOrigin::NativeText);
    }

    /// Take ownership of the CLIPBOARD with `png` as `image/png` (a screenshot).
    /// Served lazily to a requesting client via `SelectionHandler::send_selection`
    /// (the [`SelectionOrigin::NativeText`] arm branches on the requested mime).
    /// Clipboard only — primary selection stays text/middle-click. Replacing the
    /// selection supersedes any prior text copy, as a real clipboard does.
    pub fn copy_image_to_clipboard(&mut self, png: Vec<u8>) {
        use smithay::wayland::selection::data_device::set_data_device_selection;
        if png.is_empty() {
            return;
        }
        self.clipboard_image = Some(png);
        set_data_device_selection(
            &self.display_handle,
            &self.seat,
            vec!["image/png".to_string()],
            SelectionOrigin::NativeText,
        );
    }

    // --- Tier A text panel ------------------------------------------------

    /// Open the native text panel centred on `out` (or close it if open).
    pub fn toggle_text_panel(&mut self, out: OutputId) {
        if self.text_panel.take().is_some() {
            self.close_selection_menu();
            return;
        }
        let bounds = self
            .wm
            .output(out)
            .map(|o| o.bounds)
            .unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0));
        let pad = SEL_PANEL_PAD;
        let panel_w = (bounds.w * 0.6).clamp(360.0, 760.0);
        let text_w = (panel_w - 2.0 * pad).max(1.0);
        let mut native =
            NativeText::new(SEL_PANEL_DEMO, SEL_PANEL_FONT_PX, SEL_PANEL_LINE_H, text_w);
        let text_size = native.size();
        let panel_h = text_size.1 + 2.0 * pad;
        let x = bounds.x + (bounds.w - panel_w) / 2.0;
        let y = bounds.y + (bounds.h - panel_h) / 2.0;
        // Rasterise once, upload-ready: `Abgr8888` = [R,G,B,A] in memory at
        // scale 1, matching what `NativeText::rasterize` produces.
        let bitmap = native.rasterize(SEL_PANEL_TEXT_RGB).map(|(rgba, w, h)| {
            let buf = MemoryRenderBuffer::from_slice(
                &rgba,
                Fourcc::Abgr8888,
                (w as i32, h as i32),
                1,
                Transform::Normal,
                None,
            );
            (buf, w, h)
        });
        // Panels are mutually exclusive with the other full overlays.
        self.close_selection_menu();
        self.apps_menu = None;
        self.control_center = None;
        self.text_panel = Some(TextPanel {
            output: out,
            rect: Rect::new(x, y, panel_w, panel_h),
            text_origin: (x + pad, y + pad),
            text_size,
            native,
            bitmap,
            dragging: false,
        });
    }

    /// Dismiss the text panel.
    pub fn close_text_panel(&mut self) {
        self.text_panel = None;
    }

    // --- Tier C: AT-SPI selection over a foreign app ----------------------

    /// Long-press at screen `(sx, sy)` over a focused foreign app that exposes
    /// AT-SPI: select the word there (driving the app's own selection too),
    /// capture its text + screen geometry for our handles/highlight, arm a drag
    /// (so the same finger can extend it), and pop the action menu. Returns
    /// whether Tier C handled it.
    pub fn atspi_select_word_at(&mut self, sx: f32, sy: f32) -> bool {
        let Some(sel) = self.atspi_resolve_word(sx, sy) else {
            return false;
        };
        let anchor = sel.handles.map(|(_s, e)| e);
        self.atspi_selection = Some(sel);
        if let Some((gx, gy)) = anchor {
            self.open_selection_menu(gx, gy);
        }
        true
    }

    /// Clear the Tier C selection overlay.
    pub fn clear_atspi_selection(&mut self) {
        self.atspi_selection = None;
    }

    /// Open the action menu as a **right-click context menu** at `(sx, sy)`,
    /// capturing the AT-SPI text under the cursor so Copy/Paste can act on it
    /// directly (read selection / caret-insert) rather than synthesising keys.
    pub fn open_text_context_menu(&mut self, sx: f32, sy: f32) {
        self.atspi_menu_ctx = self.atspi_text_at(sx, sy);
        self.open_selection_menu(sx, sy);
    }

    /// The AT-SPI text accessible under a screen point + the focused window's
    /// origin (for window-relative coords). `None` without AT-SPI.
    fn atspi_text_at(&self, sx: f32, sy: f32) -> Option<(crate::atspi::AccRef, (f32, f32))> {
        let bridge = self.atspi.as_ref()?;
        let (wx, wy) = self
            .wm
            .all_windows()
            .into_iter()
            .find(|w| w.focused)
            .map(|w| (w.geom.x, w.geom.y))?;
        let acc = bridge
            .find_text_at((sx - wx) as i32, (sy - wy) as i32)
            .or_else(|| bridge.focused())?;
        Some((acc, (wx, wy)))
    }

    /// Read the current selection of the right-click context's AT-SPI text, if
    /// any — used so Copy grabs exactly what the app has selected (e.g. a
    /// mouse-drag selection) without synthesising Ctrl+C.
    fn atspi_menu_selected_text(&self) -> Option<String> {
        let (acc, _) = self.atspi_menu_ctx.as_ref()?;
        let text = self.atspi.as_ref()?.text(acc)?;
        if text.get_n_selections().unwrap_or(0) <= 0 {
            return None;
        }
        let (s, e) = text.get_selection(0).ok()?;
        if s >= e {
            return None;
        }
        text.get_text(s, e).ok().filter(|t| !t.is_empty())
    }

    /// Paste via AT-SPI: insert the compositor's clipboard text at the caret of
    /// the right-click context's editable text. Reliable (no focus / keybinding
    /// dependency). Returns whether it inserted; the caller synthesises Ctrl+V
    /// otherwise (e.g. when another app owns the clipboard, or it isn't AT-SPI).
    fn atspi_paste(&self) -> bool {
        let Some((acc, _)) = self.atspi_menu_ctx.as_ref() else { return false };
        let Some(text) = self.clipboard_text.clone().filter(|t| !t.is_empty()) else {
            return false;
        };
        let Some(bridge) = self.atspi.as_ref() else { return false };
        let Some(tproxy) = bridge.text(acc) else { return false };
        let caret = tproxy.caret_offset().unwrap_or(-1);
        if caret < 0 {
            return false;
        }
        let Some(ed) = bridge.editable(acc) else { return false };
        ed.insert_text(caret, &text, text.chars().count() as i32).unwrap_or(false)
    }

    /// Press near a Tier C handle → grab it (re-seat the anchor to the opposite
    /// end so the grabbed handle becomes the moving `focus`), start a drag, and
    /// hide the menu while dragging. Returns whether a handle was grabbed.
    /// Checked *before* the action menu so a handle on the selection beats the
    /// menu's off-tap dismiss.
    pub fn atspi_handle_press(&mut self, sx: f32, sy: f32) -> bool {
        let Some(sel) = self.atspi_selection.as_ref() else { return false };
        let Some((hs, he)) = sel.handles else { return false };
        let d = |p: (f32, f32)| (p.0 - sx).hypot(p.1 - sy);
        let (near_s, near_e) = (d(hs) <= SEL_HANDLE_HIT, d(he) <= SEL_HANDLE_HIT);
        if !near_s && !near_e {
            return false;
        }
        let (s, e) = sel.range();
        let grab_start = near_s && (!near_e || d(hs) <= d(he));
        let sel = self.atspi_selection.as_mut().unwrap();
        if grab_start {
            sel.anchor = e; // fix the end, drag the start
            sel.focus = s;
        } else {
            sel.anchor = s; // fix the start, drag the end
            sel.focus = e;
        }
        sel.dragging = true;
        sel.pending = None;
        sel.last_query_ms = 0; // let the first drag motion flush immediately
        self.floating_menu = None;
        true
    }

    /// Motion while a Tier C drag is live → record the latest point and flush
    /// it (a D-Bus `SetSelection` + extents) at most every
    /// [`ATSPI_DRAG_DEBOUNCE_MS`]. Coalescing keeps a fast drag from flooding
    /// the app; the final point is always applied on release.
    pub fn atspi_input_motion(&mut self, sx: f32, sy: f32) -> bool {
        if !self.atspi_selection.as_ref().is_some_and(|s| s.dragging) {
            return false;
        }
        let now = self.start_time.elapsed().as_millis() as u64;
        if let Some(sel) = self.atspi_selection.as_mut() {
            sel.pending = Some((sx, sy));
        }
        if self.atspi_drag_due(now) {
            self.atspi_flush_drag(now);
        }
        true
    }

    /// Debounce tick: flush a pending drag point once the interval elapses even
    /// if the finger has stopped moving (no new motion events arrive). Called
    /// from the main loop.
    pub fn atspi_drag_tick(&mut self) {
        let pending = self
            .atspi_selection
            .as_ref()
            .is_some_and(|s| s.dragging && s.pending.is_some());
        if !pending {
            return;
        }
        let now = self.start_time.elapsed().as_millis() as u64;
        if self.atspi_drag_due(now) {
            self.atspi_flush_drag(now);
        }
    }

    fn atspi_drag_due(&self, now_ms: u64) -> bool {
        self.atspi_selection
            .as_ref()
            .map_or(false, |s| now_ms.saturating_sub(s.last_query_ms) >= ATSPI_DRAG_DEBOUNCE_MS)
    }

    /// Apply the latest pending drag point: map it to a character offset, move
    /// the focus end, drive the app's selection, and refresh a fast single-box
    /// highlight (the accurate multi-line geometry is recomputed on release).
    fn atspi_flush_drag(&mut self, now_ms: u64) {
        let (acc, coord, origin, point) = match self.atspi_selection.as_ref() {
            Some(s) => match s.pending {
                Some(p) => (s.acc.clone(), s.coord, s.origin, p),
                None => return,
            },
            None => return,
        };
        let Some(bridge) = self.atspi.as_ref() else { return };
        let Some(text) = bridge.text(&acc) else { return };
        let (px, py) = if coord == crate::atspi::COORD_WINDOW {
            (point.0 - origin.0, point.1 - origin.1)
        } else {
            point
        };
        let off = text.get_offset_at_point(px as i32, py as i32, coord).unwrap_or(-1);
        let sel = self.atspi_selection.as_mut().unwrap();
        // Consume the sample regardless, so a point that misses the text doesn't
        // get retried forever.
        sel.pending = None;
        sel.last_query_ms = now_ms;
        if off < 0 || sel.focus == off {
            return;
        }
        sel.focus = off;
        let (s, e) = sel.range();
        let _ = text.set_selection(0, s, e);
        if let Ok((x, y, w, h)) = text.get_range_extents(s, e, coord) {
            let r = Rect::new(origin.0 + x as f32, origin.1 + y as f32, w as f32, h as f32);
            sel.handles = Some(((r.x, r.y + r.h), (r.x + r.w, r.y + r.h)));
            sel.highlight = vec![r];
        }
    }

    /// End a Tier C drag: apply any pending point, recompute accurate per-line
    /// geometry + the selected text, then re-pop the menu (or clear if the
    /// selection collapsed). Returns whether it was consumed.
    pub fn atspi_input_up(&mut self) -> bool {
        if !self.atspi_selection.as_ref().is_some_and(|s| s.dragging) {
            return false;
        }
        // Apply the final drag point immediately (ignore the debounce).
        if self.atspi_selection.as_ref().is_some_and(|s| s.pending.is_some()) {
            let now = self.start_time.elapsed().as_millis() as u64;
            self.atspi_flush_drag(now);
        }
        if let (Some(bridge), Some(sel)) = (self.atspi.as_ref(), self.atspi_selection.as_mut()) {
            sel.dragging = false;
            Self::atspi_recompute(bridge, sel);
        }
        let collapsed = self
            .atspi_selection
            .as_ref()
            .map(|s| {
                let (a, b) = s.range();
                a == b
            })
            .unwrap_or(true);
        if collapsed {
            self.atspi_selection = None;
            self.close_selection_menu();
        } else if let Some((gx, gy)) = self.atspi_selection.as_ref().and_then(|s| s.handles).map(|(_s, e)| e) {
            self.open_selection_menu(gx, gy);
        }
        true
    }

    /// Re-read the focused app's *actual* selection (it changed — the user
    /// extended it natively, or our `SetSelection` settled) and realign our
    /// overlay to match. No-op during our own drag (we own the selection then);
    /// clears the overlay if the app dropped its selection. Only syncs an
    /// existing overlay — it never pops UI over an unsolicited in-app selection.
    pub fn atspi_sync_selection(&mut self) {
        match self.atspi_selection.as_ref() {
            Some(s) if s.dragging => return, // our drag owns it
            Some(_) => {}
            None => return,
        }
        let acc = self.atspi_selection.as_ref().unwrap().acc.clone();
        // Query the app's current selection: outer None = query failed (leave
        // as-is); Some(None) = no selection; Some(Some(range)) = a live range.
        let queried: Option<Option<(i32, i32)>> =
            self.atspi.as_ref().and_then(|b| b.text(&acc)).and_then(|t| {
                if t.get_n_selections().unwrap_or(0) <= 0 {
                    return Some(None);
                }
                t.get_selection(0).ok().map(Some)
            });
        match queried {
            Some(Some((s, e))) if s < e => {
                if let Some(sel) = self.atspi_selection.as_mut() {
                    sel.anchor = s;
                    sel.focus = e;
                }
                if let (Some(bridge), Some(sel)) =
                    (self.atspi.as_ref(), self.atspi_selection.as_mut())
                {
                    Self::atspi_recompute(bridge, sel);
                }
                if self.atspi_selection.as_ref().is_some_and(|s| s.highlight.is_empty()) {
                    self.atspi_selection = None;
                    self.close_selection_menu();
                }
            }
            // Queried, but the app has no (or a collapsed) selection → clear.
            Some(_) => {
                self.atspi_selection = None;
                self.close_selection_menu();
            }
            // Query failed (app busy / gone) → leave the overlay untouched.
            None => {}
        }
    }

    /// Recompute `sel`'s highlight (per-line boxes), handles (first line's left,
    /// last line's right) and text from AT-SPI. Associated fn (not a method) so
    /// it can borrow `self.atspi` and `self.atspi_selection` disjointly.
    fn atspi_recompute(bridge: &crate::atspi::AtspiBridge, sel: &mut AtspiSelection) {
        let (s, e) = sel.range();
        let (ox, oy) = sel.origin;
        sel.highlight = bridge
            .range_line_boxes(&sel.acc, s, e, sel.coord)
            .into_iter()
            .map(|(x, y, w, h)| Rect::new(ox + x as f32, oy + y as f32, w as f32, h as f32))
            .collect();
        sel.handles = match (sel.highlight.first(), sel.highlight.last()) {
            (Some(f), Some(l)) => Some(((f.x, f.y + f.h), (l.x + l.w, l.y + l.h))),
            _ => None,
        };
        if let Some(text) = bridge.text(&sel.acc) {
            if let Ok(t) = text.get_text(s, e) {
                sel.text = t;
            }
        }
    }

    /// Resolve the word under a screen point via AT-SPI and build the initial
    /// (dragging) selection.
    ///
    /// **Always uses WINDOW coordinates** offset by the focused window's
    /// on-screen origin (which only *we* know). Live-verified rationale: a
    /// Wayland client reports its own window at screen `(0,0)` — `foot` returns
    /// identical SCREEN and WINDOW extents, both window-relative — so SCREEN
    /// coords are unusable. `Window.geom` is the client's content top-left
    /// (the SSD bar is drawn above it; maximise already insets geom by `BAR_H`),
    /// so it's the exact origin to add. The `find_first_text` fallback covers
    /// apps that expose a text tree but never emit a focus event (e.g. `foot`).
    fn atspi_resolve_word(&self, sx: f32, sy: f32) -> Option<AtspiSelection> {
        use crate::atspi::{COORD_WINDOW, GRANULARITY_WORD};
        let bridge = self.atspi.as_ref()?;

        // Focused window's content origin → maps screen ↔ window-relative coords.
        let (wx, wy) = self
            .wm
            .all_windows()
            .into_iter()
            .find(|w| w.focused)
            .map(|w| (w.geom.x, w.geom.y))?;
        let (ox, oy) = (wx, wy);
        let coord = COORD_WINDOW;
        let (rx, ry) = ((sx - wx) as i32, (sy - wy) as i32);

        // Find the text actually *under the finger* by hit-testing window-relative
        // extents — not the focused widget (apps often don't emit focus events)
        // and not the first text in the tree (that's usually the window title).
        let acc = bridge
            .find_text_at(rx, ry)
            .or_else(|| bridge.focused())?;
        let text = bridge.text(&acc)?;
        let off = text.get_offset_at_point(rx, ry, COORD_WINDOW).ok()?;
        if off < 0 {
            return None;
        }

        let (word, start, end) = text.get_string_at_offset(off, GRANULARITY_WORD).ok()?;
        if start >= end {
            return None;
        }
        tracing::debug!(sx, sy, wx, wy, off, start, end, ?word, "atspi: resolved word under touch");
        // Best-effort: drive the app's own selection so it agrees with our overlay.
        let _ = text.set_selection(0, start, end);
        let mut sel = AtspiSelection {
            acc,
            coord,
            origin: (ox, oy),
            anchor: start,
            focus: end,
            text: word,
            highlight: Vec::new(),
            handles: None,
            dragging: true,
            pending: None,
            last_query_ms: 0,
        };
        Self::atspi_recompute(bridge, &mut sel);
        if sel.highlight.is_empty() {
            return None;
        }
        Some(sel)
    }

    // --- unified panel input (shared by touch + pointer, both backends) ---

    /// A press at `(x, y)` while the panel is open: route it into the panel and
    /// arm the long-press recogniser (so a still hold escalates to a word
    /// select). Returns whether the panel consumed it.
    pub fn panel_input_down(&mut self, x: f32, y: f32) -> bool {
        if self.text_panel.is_none() {
            return false;
        }
        self.text_panel_press(x, y);
        // Off-panel presses dismiss the panel; only arm the recogniser if it's
        // still open after the press.
        if self.text_panel.is_some() {
            let now = self.start_time.elapsed().as_millis() as u64;
            self.selection_recognizer.down(x, y, now);
        } else {
            self.selection_recognizer.cancel();
        }
        true
    }

    /// Motion while a panel drag is live → extend the selection (and feed the
    /// recogniser so movement cancels a pending long-press). Returns consumed.
    pub fn panel_input_motion(&mut self, x: f32, y: f32) -> bool {
        if !self.text_panel.as_ref().is_some_and(|p| p.dragging) {
            return false;
        }
        self.selection_recognizer.motion(x, y);
        self.text_panel_motion(x, y);
        true
    }

    /// A release while the panel is open: settle the drag, then escalate a
    /// double-/triple-tap to word/line selection. Returns consumed.
    pub fn panel_input_up(&mut self) -> bool {
        if self.text_panel.is_none() {
            return false;
        }
        let now = self.start_time.elapsed().as_millis() as u64;
        let gesture = self.selection_recognizer.up(now);
        self.text_panel_release();
        self.route_tap_gesture(gesture);
        true
    }

    /// Escalate a multi-tap from the recogniser into a panel word/line select.
    /// Single taps (and drags/long-presses, which return `None`/other) do
    /// nothing here — the caret/long-press paths already handled those.
    fn route_tap_gesture(&mut self, gesture: Option<SelectionGesture>) {
        match gesture {
            Some(SelectionGesture::DoubleTap { x, y }) => {
                self.text_panel_tap_select(x, y, false);
            }
            Some(SelectionGesture::TripleTap { x, y }) => {
                self.text_panel_tap_select(x, y, true);
            }
            _ => {}
        }
    }

    /// Press at `(px, py)` (WM-global) while a panel is open. Inside the text:
    /// grab a handle if one is under the finger, else start a fresh
    /// drag-select. Inside the plaque but off the text: consume. Outside the
    /// plaque: dismiss the panel. Returns whether the press was consumed.
    pub fn text_panel_press(&mut self, px: f32, py: f32) -> bool {
        let Some(panel) = self.text_panel.as_mut() else { return false };
        if !panel.contains(px, py) {
            self.text_panel = None;
            self.floating_menu = None;
            return true;
        }
        // Dismiss any open action menu (disjoint field — keep the panel borrow).
        self.floating_menu = None;
        if panel.in_text(px, py) {
            let (lx, ly) = panel.local(px, py);
            if panel.native.grab_handle(lx, ly, SEL_HANDLE_HIT).is_none() {
                panel.native.begin_drag_at(lx, ly);
            }
            panel.dragging = true;
        }
        true
    }

    /// Motion at `(px, py)` while a panel drag is live → extend the selection.
    /// Returns whether it was consumed.
    pub fn text_panel_motion(&mut self, px: f32, py: f32) -> bool {
        let Some(panel) = self.text_panel.as_mut() else { return false };
        if !panel.dragging {
            return false;
        }
        let (lx, ly) = panel.local(px, py);
        panel.native.extend_to(lx, ly);
        true
    }

    /// End a panel drag. If a selection remains, pop the action menu at its end
    /// handle; otherwise just settle the caret.
    pub fn text_panel_release(&mut self) {
        let menu_at = {
            let Some(panel) = self.text_panel.as_mut() else { return };
            if !panel.dragging {
                return;
            }
            panel.dragging = false;
            panel
                .native
                .handle_points()
                .map(|(_s, e)| panel.to_global(e))
        };
        if let Some((gx, gy)) = menu_at {
            self.open_selection_menu(gx, gy);
        }
    }

    /// Long-press inside the panel text → select the word and open the menu.
    /// Returns whether it was handled (so the caller doesn't also treat it as a
    /// foreign-client long-press).
    pub fn text_panel_long_press(&mut self, px: f32, py: f32) -> bool {
        self.text_panel_select(px, py, false)
    }

    /// Double-tap inside the panel → select word; triple-tap (`line = true`) →
    /// select the line. Returns whether handled.
    pub fn text_panel_tap_select(&mut self, px: f32, py: f32, line: bool) -> bool {
        self.text_panel_select(px, py, line)
    }

    fn text_panel_select(&mut self, px: f32, py: f32, line: bool) -> bool {
        let anchor = {
            let Some(panel) = self.text_panel.as_mut() else { return false };
            if !panel.in_text(px, py) {
                return false;
            }
            let (lx, ly) = panel.local(px, py);
            if line {
                panel.native.select_line_at(lx, ly);
            } else {
                panel.native.select_word_at(lx, ly);
            }
            panel.native.handle_points().map(|(_s, e)| panel.to_global(e))
        };
        if let Some((gx, gy)) = anchor {
            self.open_selection_menu(gx, gy);
        }
        true
    }

    /// Synthesise a modifier+key chord to the focused client: press each
    /// modifier, press+release the key, release the modifiers in reverse. Codes
    /// are raw evdev; Smithay wants xkb keycodes (evdev + 8, matching the
    /// libinput backend's own `key_code()` offset). Sending a clean,
    /// self-contained press/release run avoids leaving a modifier stuck.
    pub fn synthesize_chord(&mut self, mods_evdev: &[u32], key_evdev: u32) {
        use smithay::backend::input::KeyState;
        use smithay::input::keyboard::FilterResult;
        use smithay::utils::SERIAL_COUNTER;

        let Some(kb) = self.seat.get_keyboard() else { return };
        let time = self.start_time.elapsed().as_millis() as u32;
        // `kb` is an owned Arc handle independent of `self`, so it can drive
        // `kb.input(self, …)` without aliasing.
        let tap = |st: &mut Self, code: u32, state: KeyState| {
            kb.input::<(), _>(
                st,
                (code + 8).into(),
                state,
                SERIAL_COUNTER.next_serial(),
                time,
                |_, _, _| FilterResult::<()>::Forward,
            );
        };
        for &m in mods_evdev {
            tap(self, m, KeyState::Pressed);
        }
        tap(self, key_evdev, KeyState::Pressed);
        tap(self, key_evdev, KeyState::Released);
        for &m in mods_evdev.iter().rev() {
            tap(self, m, KeyState::Released);
        }
    }

    // -- On-screen keyboard -------------------------------------------------
    //
    // Auto show / hide is event-driven from the text-input observer
    // (`crate::text_input::BacakState::ti_update_osk`), not polled.

    /// Output + field rectangle (global logical) for the surface owning the
    /// active text input, so the OSK can bind the right monitor and avoid
    /// covering the field.
    pub(crate) fn osk_field_geometry(&self, surface: &WlSurface) -> (Option<OutputId>, OskRect) {
        let id = self.window_for(surface);
        let output = id.and_then(|i| self.wm.output_for_window(i)).map(|o| o.id);
        let rect = id
            .and_then(|i| self.wm.get(i).ok())
            .map(|w| OskRect { x: w.geom.x, y: w.geom.y, w: w.geom.w, h: w.geom.h })
            .unwrap_or(OskRect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 });
        (output, rect)
    }

    /// True when the OSK is visible and the global-logical point falls inside
    /// its panel. The pointer is then "over" the overlay and must be captured
    /// — not forwarded to a window behind it and never allowed to change focus
    /// (else hovering the keyboard would steal focus from the text field and
    /// auto-hide the OSK).
    pub fn osk_hit_test(&self, gx: f64, gy: f64) -> bool {
        if !self.osk.is_visible() || self.osk_blocked() {
            return false;
        }
        let Some((px, py)) = self.osk_local_point(gx, gy) else { return false };
        let p = self.osk.panel_rect();
        px >= p.x && px < p.x + p.w && py >= p.y && py < p.y + p.h
    }

    /// Convert a global-logical point to the OSK's bound-output-local space,
    /// where [`OskController`] geometry lives.
    fn osk_local_point(&self, gx: f64, gy: f64) -> Option<(f32, f32)> {
        let out = self.osk.bound_output().or_else(|| self.wm.primary_output())?;
        let o = self.wm.output(out)?;
        Some((gx as f32 - o.bounds.x, gy as f32 - o.bounds.y))
    }

    /// True while a full-screen / modal overlay (Overview "recents", apps grid,
    /// Control Center, screenshot dialog) is open — the OSK must then stop
    /// rendering and stop swallowing input so that overlay is fully usable.
    pub fn osk_blocked(&self) -> bool {
        self.overview.is_some()
            || self.control_center.is_some()
            || self.shot_dialog.is_some()
    }

    /// Route a press (touch-down / pointer-press) at global-logical `(gx,gy)`
    /// to the keyboard. Returns `true` if the OSK consumed it — the caller must
    /// then *not* forward the event to a client. A press on the title strip
    /// begins a drag; a press on a key types immediately.
    pub fn osk_press_global(&mut self, gx: f64, gy: f64) -> bool {
        if !self.osk.is_visible() || self.osk_blocked() {
            return false;
        }
        let Some((px, py)) = self.osk_local_point(gx, gy) else { return false };
        let now = self.start_time.elapsed().as_millis() as u64;

        // Text entry for the Wi-Fi picker (password OR static IP): capture glyphs
        // into our own buffer instead of synthesizing keycodes to a client.
        if self.wifi_text_active() {
            match self.osk.press_text_at(px, py, now) {
                OskTextPress::Miss => return false,
                OskTextPress::Title => return true,
                OskTextPress::Char(s) => self.wifi_pw_input(&s),
                OskTextPress::Backspace => self.wifi_pw_backspace(),
                OskTextPress::Enter => self.wifi_text_submit(),
                OskTextPress::Action(action) => {
                    if self.osk_dispatch(action) {
                        // Hide pressed → cancel back to the list / details.
                        if let Some(out) = self.wifi_panel.as_ref().map(|p| p.output) {
                            if self.wifi_panel.as_ref().is_some_and(|p| p.static_entry.is_some()) {
                                self.wifi_static_cancel(out);
                            } else {
                                self.wifi_pw_cancel(out);
                            }
                        }
                    }
                }
            }
            self.apply_osk_xkb();
            self.osk_dirty = true;
            return true;
        }

        // Apps-menu search bar: route chars/Backspace/Enter to the query,
        // so the on-screen keyboard can type into the search field.
        if self.apps_menu.is_some() {
            match self.osk.press_text_at(px, py, now) {
                OskTextPress::Miss => return false,
                OskTextPress::Title => return true,
                OskTextPress::Char(s) => {
                    for c in s.chars() {
                        if !c.is_control() {
                            self.apps_menu_type(c);
                        }
                    }
                }
                OskTextPress::Backspace => { self.apps_menu_backspace(); }
                OskTextPress::Enter => { self.apps_menu_enter(); }
                OskTextPress::Action(action) => { let _ = self.osk_dispatch(action); }
            }
            self.apply_osk_xkb();
            self.osk_dirty = true;
            return true;
        }

        // Desktop Settings auth / text entry: route keys to ds_type_char.
        if self.desktop_settings.as_ref().is_some_and(|d| !matches!(d.mode, DsMode::Main)) {
            match self.osk.press_text_at(px, py, now) {
                OskTextPress::Miss => return false,
                OskTextPress::Title => return true,
                OskTextPress::Char(s) => {
                    for c in s.chars() { self.ds_type_char(c); }
                }
                OskTextPress::Backspace => { self.ds_backspace(); }
                OskTextPress::Enter => {
                    // Enter in auth/entry mode submits (same as tapping Onayla).
                    let mode_key = self.desktop_settings.as_ref().map(|d| match &d.mode {
                        DsMode::Auth { .. } => "auth",
                        _ => "entry",
                    });
                    match mode_key.as_deref() {
                        Some("auth") => {
                            let pw = self.desktop_settings.as_ref().map(|d| d.auth_buf.clone()).unwrap_or_default();
                            let action = match self.desktop_settings.as_ref().map(|d| &d.mode) {
                                Some(DsMode::Auth { pending }) => pending.clone(),
                                _ => return true,
                            };
                            self.ds_verify_auth(pw, action);
                        }
                        Some("entry") => {
                            let ok_rect = self.desktop_settings.as_ref().map(|d| d.entry_ok_rect);
                            if let Some(r) = ok_rect {
                                self.ds_entry_press(r.x + 1.0, r.y + 1.0);
                            }
                        }
                        _ => {}
                    }
                }
                OskTextPress::Action(action) => { let _ = self.osk_dispatch(action); }
            }
            self.apply_osk_xkb();
            self.osk_dirty = true;
            return true;
        }

        // Bluetooth PIN / passkey entry: same capture-into-our-buffer path.
        if self.bt_pin_active() {
            match self.osk.press_text_at(px, py, now) {
                OskTextPress::Miss => return false,
                OskTextPress::Title => return true,
                OskTextPress::Char(s) => {
                    // raw=0 isn't a special key → treated as text input.
                    self.bt_pin_key(0, &s);
                }
                OskTextPress::Backspace => {
                    self.bt_pin_key(0xff08, "");
                }
                OskTextPress::Enter => {
                    self.bt_pin_key(0xff0d, "");
                }
                OskTextPress::Action(action) => {
                    let _ = self.osk_dispatch(action);
                }
            }
            self.apply_osk_xkb();
            self.osk_dirty = true;
            return true;
        }

        match self.osk.press_at(px, py, now) {
            OskPress::Miss => false,
            OskPress::TitleGrab => true,
            OskPress::Key(action) => {
                if self.osk_dispatch(action) {
                    self.osk_hide();
                }
                // A layout key may have queued a seat xkb change.
                self.apply_osk_xkb();
                true
            }
        }
    }

    /// Route motion while a press is held: continues an active drag. Returns
    /// `true` while the OSK owns the gesture (dragging).
    pub fn osk_motion_global(&mut self, gx: f64, gy: f64) -> bool {
        if !self.osk.is_dragging() {
            return false;
        }
        if let Some((px, py)) = self.osk_local_point(gx, gy) {
            self.osk.drag_to(px, py);
        }
        true
    }

    /// Pointer hovering over the OSK overlay → capture it: clear the *pointer*
    /// focus (so no window behind reacts) but leave keyboard focus untouched,
    /// so the text field keeps focus and the keyboard stays open. Drives the
    /// seat's own pointer handle (not the backend's), so it works from the
    /// plugin input layer.
    pub fn osk_hover_capture(&mut self, gx: f64, gy: f64) {
        let Some(ptr) = self.seat.get_pointer() else { return };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.start_time.elapsed().as_millis() as u32;
        let location = smithay::utils::Point::<f64, smithay::utils::Logical>::from((gx, gy));
        ptr.motion(
            self,
            None,
            &smithay::input::pointer::MotionEvent { location, serial, time },
        );
        ptr.frame(self);
    }

    /// Route a release: end any drag, clear the press highlight. Returns
    /// whether the OSK was the gesture owner.
    pub fn osk_release_global(&mut self) -> bool {
        let owned = self.osk.is_dragging();
        self.osk.drag_end();
        self.osk.release();
        owned
    }

    /// Long-press alternates for the key under a held global point, e.g.
    /// `a → à á â ä`. Empty when the key has none. The input layer shows these
    /// in a popup; selecting one commits it via [`Self::osk_dispatch`].
    pub fn osk_alternates_global(&self, gx: f64, gy: f64) -> Vec<String> {
        match self.osk_local_point(gx, gy) {
            Some((px, py)) => self.osk.alternates_at(px, py),
            None => Vec::new(),
        }
    }

    /// Hide the keyboard (the Hide key, or focus loss).
    pub fn osk_hide(&mut self) {
        self.osk.close();
        let _ = self.osk.confirm_close();
    }

    /// Apply any pending seat xkb layout queued by the OSK (a layout switch, or
    /// the initial layout when the keyboard first opens), so the synthesized
    /// evdev codes actually emit the active page's glyphs (e.g. Turkish-F).
    pub fn apply_osk_xkb(&mut self) {
        let Some((layout, variant)) = self.osk.take_pending_xkb() else { return };
        let Some(kb) = self.seat.get_keyboard() else { return };
        let cfg = smithay::input::keyboard::XkbConfig {
            layout,
            variant,
            ..Default::default()
        };
        if let Err(e) = kb.set_xkb_config(self, cfg) {
            tracing::warn!(?layout, ?variant, ?e, "OSK: set_xkb_config failed");
        } else {
            tracing::info!(layout, variant, "OSK: seat xkb layout applied");
        }
    }

    /// Deliver a resolved key to the focused client. Returns `true` when the
    /// keyboard should hide afterwards (the Hide key).
    pub fn osk_dispatch(&mut self, action: crate::keyboard::KeyAction) -> bool {
        use crate::keyboard::{Action as KbAction, KeyAction};
        match action {
            KeyAction::Chord { mods, code } => {
                self.synthesize_chord(&mods, code);
                false
            }
            KeyAction::Commit(text) => {
                self.osk_commit_text(text);
                false
            }
            KeyAction::Redraw => false,
            KeyAction::Special(a) => {
                match a {
                    KbAction::Copy => self.run_selection_action(SelectionAction::Copy),
                    KbAction::Paste => self.run_selection_action(SelectionAction::Paste),
                    KbAction::SelectAll => self.run_selection_action(SelectionAction::SelectAll),
                    KbAction::Cut => self.osk_cut(),
                    KbAction::Recents => {
                        // Open the Recent-Apps overview on the OSK's output. The
                        // OSK then auto-suppresses (see `osk_blocked`).
                        if let Some(out) =
                            self.osk.bound_output().or_else(|| self.wm.primary_output())
                        {
                            self.toggle_overview(out);
                        }
                    }
                    KbAction::Hide => return true,
                    // DockDefault / ToggleSplit / layout switches are handled
                    // inside the keyboard model; nothing to deliver.
                    _ => {}
                }
                false
            }
        }
    }

    /// Commit an emoji / accented glyph that has no keycode.
    ///
    /// Primary path: deliver the literal string straight to the focused
    /// `text-input-v3` field via `commit_string` + `done` — exactly what an
    /// IME (ibus/fcitx) would do through input-method-v2, except the compositor
    /// drives the text-input resource itself. This is the protocol-correct
    /// insertion: the client splices the glyph at its cursor and the user's
    /// clipboard is left untouched.
    ///
    /// Fallback: when no field has an active text-input (a terminal or legacy
    /// XWayland app the OSK was opened over manually), there is nothing to
    /// `commit_string` to, so we fall back to the clipboard-paste path so the
    /// glyph still lands.
    fn osk_commit_text(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        // Primary: direct `commit_string` to the focused field (clipboard
        // untouched). Fallback: clipboard-paste for clients with no active
        // text-input (terminals / legacy X11 the OSK was opened over manually).
        if !self.ti_commit_string(&text) {
            self.osk_commit_via_clipboard(text);
        }
    }

    /// Last-resort commit for clients without text-input-v3: own the clipboard
    /// with the glyph and synthesise paste (Ctrl+V, or Ctrl+Shift+V in a
    /// terminal). Replaces the user's clipboard — only used when a direct
    /// `commit_string` is impossible.
    fn osk_commit_via_clipboard(&mut self, text: String) {
        const KEY_LEFTCTRL: u32 = 29;
        const KEY_LEFTSHIFT: u32 = 42;
        const KEY_V: u32 = 47;
        self.copy_text_to_clipboard(text);
        let mods: &[u32] = if self.focused_is_terminal() {
            &[KEY_LEFTCTRL, KEY_LEFTSHIFT]
        } else {
            &[KEY_LEFTCTRL]
        };
        self.synthesize_chord(mods, KEY_V);
    }

    fn osk_cut(&mut self) {
        const KEY_LEFTCTRL: u32 = 29;
        const KEY_LEFTSHIFT: u32 = 42;
        const KEY_X: u32 = 45;
        let mods: &[u32] = if self.focused_is_terminal() {
            &[KEY_LEFTCTRL, KEY_LEFTSHIFT]
        } else {
            &[KEY_LEFTCTRL]
        };
        self.synthesize_chord(mods, KEY_X);
    }

    /// Toggle the applications grid menu on `out`. Closes it if already
    /// open; otherwise scans installed apps, lays them out in a centred
    /// grid and rasterises each label once (so the render path is cheap).
    pub fn toggle_apps_menu(&mut self, out: OutputId) {
        if self.apps_menu.take().is_some() {
            return; // was open → now closed
        }
        // The dock overlays are mutually exclusive.
        self.control_center = None;
        self.wifi_panel = None;
        self.bt_panel = None;
        self.close_overview();
        let all = crate::icons::list_desktop_apps();
        if all.is_empty() {
            return;
        }
        self.apps_menu = Some(self.build_apps_menu(out, all, String::new(), 0, 0.0, 0.0));
    }

    /// Build the apps menu for `out`: filter `all` by `query`, lay the matches
    /// into a fixed panel (search bar + a scroll viewport of `cell` grid cells),
    /// rasterise labels + the query, and seat the scroll at `scroll` (clamped).
    /// Shared by open + every query/scroll change.
    fn build_apps_menu(
        &self,
        out: OutputId,
        all: Vec<(String, String, u8)>,
        query: String,
        selected: usize,
        scroll_y: f32,
        osk_h: f32,
    ) -> AppsMenu {
        let bounds = self
            .wm
            .output(out)
            .map(|o| o.bounds)
            .unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0));
        let cell = APPS_MENU_CELL;
        let pad = APPS_MENU_PAD;
        let sidebar_w = APPS_MENU_SIDEBAR_W;
        let q = query.to_lowercase();

        // All possible category tabs. "Tümü" (mask 0) is always shown.
        // The rest are shown only when at least one installed app matches.
        let all_cat_defs: &[(&str, u8)] = &[
            ("Tümü",       0),
            ("Sistem",     crate::icons::CAT_SYSTEM),
            ("İnternet",   crate::icons::CAT_INTERNET),
            ("Medya",      crate::icons::CAT_MEDIA),
            ("Ofis",       crate::icons::CAT_OFFICE),
            ("Geliştirme", crate::icons::CAT_DEVELOPMENT),
            ("Araçlar",    crate::icons::CAT_UTILITY),
            ("Eğitim",     crate::icons::CAT_EDUCATION),
            ("Oyunlar",    crate::icons::CAT_GAME),
        ];
        let cat_defs: Vec<(&str, u8)> = all_cat_defs
            .iter()
            .filter(|(_, mask)| {
                *mask == 0 || all.iter().any(|(_, _, m)| m & mask != 0)
            })
            .copied()
            .collect();
        let selected = selected.min(cat_defs.len().saturating_sub(1));
        let sel_mask = cat_defs[selected].1;

        let label_color = [230u8, 232, 240];
        let label_max_w = (cell - 8.0).max(1.0) as usize;
        let items: Vec<AppMenuItem> = all
            .iter()
            .filter(|(_, name, mask)| {
                (q.is_empty() || name.to_lowercase().contains(&q))
                    && (sel_mask == 0 || mask & sel_mask != 0)
            })
            .map(|(app, name, _)| AppMenuItem {
                app: app.clone(),
                label: cc_rasterize(self.text.as_ref(), name, APPS_MENU_LABEL_PX, label_color, label_max_w),
            })
            .collect();
        let n = items.len();

        // Columns and visible rows are derived from the OUTPUT size only —
        // never from the (filtered) item count — so the panel is exactly the
        // same size in every category. A sparse category just leaves empty
        // cells / scroll room instead of shrinking the panel.
        let avail_w = bounds.w * 0.85 - sidebar_w - 2.0 * pad;
        let cols = ((avail_w / cell).floor() as usize).clamp(1, 7);
        let total_rows = n.div_ceil(cols);
        let search_h = APPS_MENU_SEARCH_H;
        // Occupy at most the bottom half of the output (minus the OSK when
        // it is open) so the panel never covers the keyboard.
        let avail_h = (bounds.h - osk_h) * 0.5;
        let visible_rows = (((avail_h - search_h - 3.0 * pad) / cell).floor() as usize).max(1);

        let grid_h = visible_rows as f32 * cell;
        // Clamp the requested pixel scroll to the content that overflows;
        // seat the spring there at rest.
        let max_scroll_y = (total_rows as f32 * cell - grid_h).max(0.0);
        let scroll_y = scroll_y.clamp(0.0, max_scroll_y) as f64;
        let scroll = Spring::settle_to(scroll_y, scroll_y);
        let content_h = search_h + grid_h + 3.0 * pad;
        // The panel must also be tall enough for the sidebar (heading +
        // every category row).
        let sidebar_h = 2.0 * pad + 22.0 + pad + cat_defs.len() as f32 * APPS_MENU_CAT_ROW_H;
        let panel_h = content_h.max(sidebar_h);
        let panel_w = sidebar_w + cols as f32 * cell + 2.0 * pad;
        let panel_x = bounds.x + (bounds.w - panel_w) / 2.0;
        // Anchor above the OSK (or bottom of output when OSK is hidden).
        let gap = pad;
        let bottom = bounds.y + bounds.h - osk_h;
        let panel_y = (bottom - panel_h - gap)
            .max(bounds.y + (bounds.h - osk_h) / 2.0)
            .max(bounds.y);
        let panel = Rect::new(panel_x, panel_y, panel_w, panel_h);

        let sidebar = Rect::new(panel_x, panel_y, sidebar_w, panel_h);
        let content_x = panel_x + sidebar_w + pad;
        let content_w = cols as f32 * cell;
        let search = Rect::new(content_x, panel_y + pad, content_w, search_h);
        let grid_x = content_x;
        let grid_y = panel_y + pad + search_h + pad;

        // Sidebar contents: heading + a row per category.
        let header = cc_rasterize(
            self.text.as_ref(),
            "Kategoriler",
            APPS_MENU_LABEL_PX - 2.0,
            [120, 128, 140],
            (sidebar_w - 2.0 * pad).max(1.0) as usize,
        );
        let cats_top = panel_y + 2.0 * pad + 22.0 + pad;
        let cats: Vec<AppCategoryTab> = cat_defs
            .iter()
            .enumerate()
            .map(|(i, (label, mask))| {
                let rect = Rect::new(
                    panel_x + 10.0,
                    cats_top + i as f32 * APPS_MENU_CAT_ROW_H,
                    sidebar_w - 20.0,
                    APPS_MENU_CAT_ROW_H - 4.0,
                );
                let col = if i == selected { [236u8, 239, 245] } else { [188, 194, 204] };
                AppCategoryTab {
                    mask: *mask,
                    rect,
                    label: cc_rasterize(self.text.as_ref(), label, APPS_MENU_LABEL_PX, col, (rect.w - 24.0).max(1.0) as usize),
                }
            })
            .collect();

        let query_label = if query.is_empty() {
            cc_rasterize(self.text.as_ref(), "Uygulamalar içinde ara…", APPS_MENU_LABEL_PX + 1.0, [120, 128, 140], (search.w - 24.0).max(1.0) as usize)
        } else {
            cc_rasterize(self.text.as_ref(), &query, APPS_MENU_LABEL_PX + 1.0, [236, 239, 245], (search.w - 24.0).max(1.0) as usize)
        };

        AppsMenu {
            output: out,
            panel,
            sidebar,
            header,
            cats,
            selected,
            search,
            grid_x,
            grid_y,
            grid_h,
            cols,
            cell,
            visible_rows,
            scroll,
            query,
            query_label,
            all,
            items,
        }
    }

    /// Append a typed character to the search query and re-filter.
    /// Height occupied by the OSK when it is docked and visible (0 otherwise).
    fn current_osk_h(&self) -> f32 {
        if self.osk.is_visible() {
            self.osk.panel_rect().h
        } else {
            0.0
        }
    }

    pub fn apps_menu_type(&mut self, c: char) {
        let Some(menu) = self.apps_menu.take() else { return };
        let mut query = menu.query;
        query.push(c);
        let osk_h = self.current_osk_h();
        self.apps_menu = Some(self.build_apps_menu(menu.output, menu.all, query, menu.selected, 0.0, osk_h));
    }

    /// Delete the last query character and re-filter.
    pub fn apps_menu_backspace(&mut self) {
        let Some(menu) = self.apps_menu.take() else { return };
        let mut query = menu.query;
        query.pop();
        let osk_h = self.current_osk_h();
        self.apps_menu = Some(self.build_apps_menu(menu.output, menu.all, query, menu.selected, 0.0, osk_h));
    }

    /// Select category tab `idx` and rebuild the filtered grid (keeping
    /// the current search query, resetting the scroll to the top).
    pub fn apps_menu_select_cat(&mut self, idx: usize) {
        let Some(menu) = self.apps_menu.take() else { return };
        let osk_h = self.current_osk_h();
        self.apps_menu = Some(self.build_apps_menu(menu.output, menu.all, menu.query, idx, 0.0, osk_h));
    }

    /// Launch the first (top-left) match and close — the Enter shortcut.
    pub fn apps_menu_enter(&mut self) {
        let Some(menu) = self.apps_menu.take() else { return };
        if let Some(app) = menu.items.first().map(|it| it.app.clone()) {
            self.run_dock_menu_action(DockMenuAction::Launch(app));
        }
    }

    /// Scroll the grid by `delta` wheel notches (one row per notch).
    /// Retargets the scroll spring so the wheel eases too (and chains
    /// smoothly when spun quickly), clamped to the content.
    pub fn apps_menu_scroll(&mut self, delta: i32) {
        let Some(menu) = self.apps_menu.as_mut() else { return };
        let max = menu.max_scroll_y() as f64;
        let target = (menu.scroll.target + delta as f64 * menu.cell as f64).clamp(0.0, max);
        menu.scroll.retarget(target);
    }

    /// Left press while the apps menu is open: a visible cell launches + closes;
    /// a press elsewhere in the panel (search bar / gaps) keeps it open; a press
    /// outside the panel dismisses. Returns `false` only when no menu is open.
    pub fn apps_menu_left_press(&mut self, px: f32, py: f32) -> bool {
        let Some(menu) = self.apps_menu.as_ref() else { return false };
        if !menu.panel.contains(px, py) {
            self.apps_menu = None; // click outside → dismiss
            return true;
        }
        // A category row → switch tabs (re-filters, keeps the menu open).
        let mut select_cat = None;
        for (i, tab) in menu.cats.iter().enumerate() {
            if tab.rect.contains(px, py) {
                select_cat = Some(i);
                break;
            }
        }
        if let Some(i) = select_cat {
            if i != menu.selected {
                self.apps_menu_select_cat(i);
            }
            return true;
        }
        // Tap on the search bar → open the on-screen keyboard and
        // reposition the menu to sit above it.
        if menu.search.contains(px, py) {
            let out = menu.output;
            let panel = menu.panel;
            if !self.osk.is_visible() {
                self.osk.bind(self.wm.clone(), out);
                let field = FocusedField {
                    rect: OskRect { x: panel.x, y: panel.y, w: panel.w, h: panel.h },
                    mode: InputMode::Text,
                    has_hw_keyboard: false,
                };
                self.osk.open_for(field);
                let _ = self.osk.confirm_open();
                self.apply_osk_xkb();
                self.osk_dirty = true;
            }
            // Rebuild the menu layout accounting for the now-visible keyboard.
            let osk_h = self.current_osk_h();
            if osk_h > 0.0 {
                if let Some(menu) = self.apps_menu.take() {
                    let scroll = menu.scroll.target;
                    self.apps_menu = Some(self.build_apps_menu(
                        menu.output, menu.all, menu.query, menu.selected, scroll as f32, osk_h,
                    ));
                }
            }
            return true;
        }

        let mut launch = None;
        for (i, it) in menu.items.iter().enumerate() {
            if menu.cell_visible(i) && menu.cell_hit(i, px, py) {
                launch = Some(it.app.clone());
                break;
            }
        }
        if let Some(app) = launch {
            self.apps_menu = None;
            self.run_dock_menu_action(DockMenuAction::Launch(app));
        }
        // Otherwise keep the menu open (search bar / sidebar / gap click).
        true
    }

    /// Touch-down while the apps menu is open. Mirrors
    /// [`apps_menu_left_press`] for the immediate actions (dismiss on an
    /// outside press, switch tab on a category row) but, for a press in
    /// the icon grid, starts a single-finger drag-scroll instead of
    /// launching — the launch is deferred to [`apps_menu_touch_up`] and
    /// only fires if the finger never moved past the tap slop.
    pub fn apps_menu_touch_down(&mut self, px: f32, py: f32) -> AppsTouch {
        let Some(menu) = self.apps_menu.as_ref() else { return AppsTouch::None };
        if !menu.panel.contains(px, py) {
            self.apps_menu = None; // outside → dismiss
            return AppsTouch::Consumed;
        }
        for (i, tab) in menu.cats.iter().enumerate() {
            if tab.rect.contains(px, py) {
                if i != menu.selected {
                    self.apps_menu_select_cat(i);
                }
                return AppsTouch::Consumed;
            }
        }
        // A press in the scrolling grid viewport begins a drag-scroll.
        if menu.grid_rect().contains(px, py) {
            self.apps_menu_drag = Some(AppsMenuDrag {
                start_y: py,
                start_scroll_y: menu.scroll.pos as f32,
                press_x: px,
                press_y: py,
                moved: false,
                last_y: py,
                last_t: Instant::now(),
                velocity: 0.0,
            });
            return AppsTouch::Drag;
        }
        // Search bar tap: open the OSK and rebuild the menu above the keyboard.
        if menu.search.contains(px, py) {
            let out = menu.output;
            let panel = menu.panel;
            if !self.osk.is_visible() {
                self.osk.bind(self.wm.clone(), out);
                let field = FocusedField {
                    rect: OskRect { x: panel.x, y: panel.y, w: panel.w, h: panel.h },
                    mode: InputMode::Text,
                    has_hw_keyboard: false,
                };
                self.osk.open_for(field);
                let _ = self.osk.confirm_open();
                self.apply_osk_xkb();
                self.osk_dirty = true;
            }
            let osk_h = self.current_osk_h();
            if osk_h > 0.0 {
                if let Some(m) = self.apps_menu.take() {
                    let scroll = m.scroll.target;
                    self.apps_menu = Some(self.build_apps_menu(
                        m.output, m.all, m.query, m.selected, scroll as f32, osk_h,
                    ));
                }
            }
            return AppsTouch::Consumed;
        }
        // Sidebar / other gap: keep the menu open, no drag.
        AppsTouch::Consumed
    }

    /// Update the in-flight grid drag-scroll from the finger's current y.
    /// Pixel-smooth and finger-following; tracks an EMA velocity for the
    /// release fling. Any travel past the tap slop marks the gesture as a
    /// scroll so release won't launch.
    pub fn apps_menu_touch_motion(&mut self, py: f32) {
        let Some(mut d) = self.apps_menu_drag.take() else { return };
        if (py - d.press_y).abs() > OVERVIEW_TAP_SLOP {
            d.moved = true;
        }
        // EMA velocity (px/s, positive = scrolling down the list, i.e. the
        // finger moving up). Mirrors the overview carousel's fling sampler.
        let now = Instant::now();
        let dt = now.saturating_duration_since(d.last_t).as_secs_f32().max(1e-3);
        let inst = (d.last_y - py) / dt;
        d.velocity = 0.3 * inst + 0.7 * d.velocity;
        d.last_y = py;
        d.last_t = now;
        if let Some(menu) = self.apps_menu.as_mut() {
            let max = menu.max_scroll_y() as f64;
            let pos = ((d.start_scroll_y + (d.start_y - py)) as f64).clamp(0.0, max);
            // 1:1 tracking: drive pos directly and keep the spring at rest
            // there (target == pos, no velocity) so it doesn't fight the
            // finger; the fling is seeded on release.
            menu.scroll.pos = pos;
            menu.scroll.target = pos;
            menu.scroll.vel = 0.0;
        }
        self.apps_menu_drag = Some(d);
    }

    /// End the grid drag-scroll. A drag that never moved is a tap: launch
    /// the icon under the original press point (and close). A drag that
    /// scrolled just settles. Returns whether a launch fired.
    pub fn apps_menu_touch_up(&mut self) -> bool {
        let Some(d) = self.apps_menu_drag.take() else { return false };
        if d.moved {
            // Was a scroll → seed the fling: project a target ahead in the
            // throw direction and hand the spring the release velocity, so
            // it coasts to rest (clamped) instead of stopping dead.
            if let Some(menu) = self.apps_menu.as_mut() {
                let max = menu.max_scroll_y() as f64;
                let target =
                    (menu.scroll.pos + d.velocity as f64 * APPS_MENU_FLING_SECS).clamp(0.0, max);
                menu.scroll.target = target;
                menu.scroll.vel = d.velocity as f64;
            }
            return false;
        }
        let Some(menu) = self.apps_menu.as_ref() else { return false };
        let mut launch = None;
        for (i, it) in menu.items.iter().enumerate() {
            if menu.cell_visible(i) && menu.cell_hit(i, d.press_x, d.press_y) {
                launch = Some(it.app.clone());
                break;
            }
        }
        if let Some(app) = launch {
            self.apps_menu = None;
            self.run_dock_menu_action(DockMenuAction::Launch(app));
            return true;
        }
        false
    }

    /// Drop any in-flight grid drag (touch cancel / menu torn down).
    pub fn apps_menu_touch_cancel(&mut self) {
        self.apps_menu_drag = None;
    }

    /// Close the applications menu if open (Escape, focus change, etc.).
    pub fn close_apps_menu(&mut self) {
        self.apps_menu = None;
        self.apps_menu_drag = None;
    }

    /// Toggle the Control Center on `out`: close it if open, else build a
    /// fresh one (snapshotting live Wi-Fi/Bluetooth/volume state).
    pub fn toggle_control_center(&mut self, out: OutputId) {
        if self.control_center.take().is_some() {
            return; // was open → now closed
        }
        self.open_control_center(out);
    }

    /// Close the Control Center if open (Escape, focus change, etc.).
    pub fn close_control_center(&mut self) {
        self.control_center = None;
    }

    /// Build the Control-Center panel anchored to the top-right of `out`.
    /// Layout is a vertical stack: clock header, a Wi-Fi/Bluetooth toggle
    /// row, a dark-mode toggle, brightness + volume sliders, and a power
    /// row (shut down / restart / log out).
    fn open_control_center(&mut self, out: OutputId) {
        // The dock overlays are mutually exclusive.
        self.apps_menu = None;
        self.wifi_panel = None;
        self.bt_panel = None;
        self.close_overview();
        let bounds = self
            .wm
            .output(out)
            .map(|o| o.bounds)
            .unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0));

        // Placeholders so the panel opens instantly; the real values (each a
        // blocking nmcli/bluetoothctl/wpctl/date subprocess) are fetched on a
        // background thread and applied in `tick_animations` via `cc_rx`. Doing
        // them synchronously here froze the render loop on open.
        let wifi_on = false;
        let bt_on = false;
        let (volume, muted) = (0.5_f32, false);
        let ssid: Option<String> = None;
        let clock_text = String::new();

        // Geometry (logical px).
        const PAD: f32 = 16.0;
        const GAP: f32 = 12.0;
        const W: f32 = 380.0;
        const CH: f32 = 30.0; // clock header
        const TR: f32 = 96.0; // toggle row (wifi/bt)
        const EH: f32 = 48.0; // ethernet button row
        const DK: f32 = 56.0; // dark-mode tile
        const SL: f32 = 54.0; // each slider (brightness, output vol, mic vol)
        const AD: f32 = 48.0; // audio device / mic device button rows
        const SH: f32 = 48.0; // screenshot button row
        const PR: f32 = 60.0; // power row
        let inner = W - 2.0 * PAD;
        let col_w = (inner - GAP) / 2.0;
        let btn_w = (inner - 2.0 * GAP) / 3.0;

        // Which optional plugin sections are installed?
        let net_on = std::path::Path::new("/usr/share/bacak/plugins/network.plugin").exists();
        let aud_on = std::path::Path::new("/usr/share/bacak/plugins/audio.plugin").exists();
        let ds_on = std::path::Path::new("/usr/share/bacak/plugins/desktop-settings.plugin").exists();

        // Dynamic panel height: core rows + optional network + optional audio + optional DS.
        let net_h = if net_on { TR + GAP + EH + GAP } else { 0.0 };
        let aud_h = if aud_on { SL + GAP + AD + GAP + SL + GAP + AD + GAP } else { 0.0 };
        let ds_h = if ds_on { SH + GAP } else { 0.0 };
        let panel_h = PAD + CH + GAP + net_h + DK + GAP + SL + GAP + aud_h + ds_h + SH + GAP + PR + PAD;

        let m = 14.0;
        let bx = (bounds.x + bounds.w - W - m).max(bounds.x + m);
        let by = (bounds.y + m).max(bounds.y);
        let panel = Rect::new(bx, by, W, panel_h);

        let cx = bx + PAD;
        let mut y = by + PAD + CH + GAP;

        let text = self.text.as_ref();
        const LABEL: [u8; 3] = [232, 236, 244];
        const SUB: [u8; 3] = [170, 178, 196];
        let lw = (col_w - 18.0).max(1.0) as usize;
        let bw = (btn_w - 8.0).max(1.0) as usize;
        let iw = (inner - 18.0) as usize;

        let wifi_sub = if wifi_on {
            ssid.clone().unwrap_or_else(|| "Açık".to_string())
        } else {
            "Kapalı".to_string()
        };

        let mut tiles: Vec<CcTile> = Vec::new();

        // --- Network section (optional) ---
        if net_on {
            tiles.push(CcTile {
                rect: Rect::new(cx, y, col_w, TR),
                action: CcAction::WifiToggle,
                kind: CcKind::Toggle,
                label: cc_rasterize(text, "Wi-Fi", 15.0, LABEL, lw),
                sub: cc_rasterize(text, &wifi_sub, 12.0, SUB, lw),
            });
            tiles.push(CcTile {
                rect: Rect::new(cx + col_w + GAP, y, col_w, TR),
                action: CcAction::BtToggle,
                kind: CcKind::Toggle,
                label: cc_rasterize(text, "Bluetooth", 15.0, LABEL, lw),
                sub: cc_rasterize(text, if bt_on { "Açık" } else { "Kapalı" }, 12.0, SUB, lw),
            });
            y += TR + GAP;
            tiles.push(CcTile {
                rect: Rect::new(cx, y, inner, EH),
                action: CcAction::EthernetSettings,
                kind: CcKind::Button { danger: false },
                label: cc_rasterize(text, "Ethernet Ayarları", 15.0, LABEL, iw),
                sub: None,
            });
            y += EH + GAP;
        }

        // --- Core: dark mode + brightness ---
        tiles.push(CcTile {
            rect: Rect::new(cx, y, inner, DK),
            action: CcAction::DarkToggle,
            kind: CcKind::Toggle,
            label: cc_rasterize(text, "Karanlık Mod", 15.0, LABEL, iw),
            sub: None,
        });
        y += DK + GAP;
        tiles.push(CcTile {
            rect: Rect::new(cx, y, inner, SL),
            action: CcAction::Brightness,
            kind: CcKind::Slider,
            label: cc_rasterize(text, "Parlaklık", 13.0, SUB, iw),
            sub: None,
        });
        y += SL + GAP;

        // --- Audio section (optional) ---
        if aud_on {
            tiles.push(CcTile {
                rect: Rect::new(cx, y, inner, SL),
                action: CcAction::Volume,
                kind: CcKind::Slider,
                label: cc_rasterize(text, "Ses Çıkışı", 13.0, SUB, iw),
                sub: None,
            });
            y += SL + GAP;
            tiles.push(CcTile {
                rect: Rect::new(cx, y, inner, AD),
                action: CcAction::AudioSettings,
                kind: CcKind::Button { danger: false },
                label: cc_rasterize(text, "Ses Çıkış Cihazı", 15.0, LABEL, iw),
                sub: None,
            });
            y += AD + GAP;
            tiles.push(CcTile {
                rect: Rect::new(cx, y, inner, SL),
                action: CcAction::MicVolume,
                kind: CcKind::Slider,
                label: cc_rasterize(text, "Mikrofon", 13.0, SUB, iw),
                sub: None,
            });
            y += SL + GAP;
            tiles.push(CcTile {
                rect: Rect::new(cx, y, inner, AD),
                action: CcAction::MicSettings,
                kind: CcKind::Button { danger: false },
                label: cc_rasterize(text, "Mikrofon Cihazı", 15.0, LABEL, iw),
                sub: None,
            });
            y += AD + GAP;
        }

        // --- Core: desktop settings + screenshot + power ---
        let ds_on = std::path::Path::new("/usr/share/bacak/plugins/desktop-settings.plugin").exists();
        if ds_on {
            tiles.push(CcTile {
                rect: Rect::new(cx, y, inner, SH),
                action: CcAction::DesktopSettings,
                kind: CcKind::Button { danger: false },
                label: cc_rasterize(text, "⚙  Masaüstü Ayarları", 15.0, LABEL, iw),
                sub: None,
            });
            y += SH + GAP;
        }
        tiles.push(CcTile {
            rect: Rect::new(cx, y, inner, SH),
            action: CcAction::Screenshot,
            kind: CcKind::Button { danger: false },
            label: cc_rasterize(text, "❏  Ekran Görüntüsü", 15.0, LABEL, iw),
            sub: None,
        });
        y += SH + GAP;
        tiles.push(CcTile {
            rect: Rect::new(cx, y, btn_w, PR),
            action: CcAction::PowerOff,
            kind: CcKind::Button { danger: true },
            label: cc_rasterize(text, "Kapat", 13.0, LABEL, bw),
            sub: None,
        });
        tiles.push(CcTile {
            rect: Rect::new(cx + btn_w + GAP, y, btn_w, PR),
            action: CcAction::Reboot,
            kind: CcKind::Button { danger: false },
            label: cc_rasterize(text, "Yeniden", 13.0, LABEL, bw),
            sub: None,
        });
        tiles.push(CcTile {
            rect: Rect::new(cx + 2.0 * (btn_w + GAP), y, btn_w, PR),
            action: CcAction::Logout,
            kind: CcKind::Button { danger: false },
            label: cc_rasterize(text, "Çıkış", 13.0, LABEL, bw),
            sub: None,
        });

        let clock = cc_rasterize(text, &clock_text, 14.0, LABEL, (inner) as usize);

        self.control_center = Some(ControlCenter {
            output: out,
            panel,
            tiles,
            clock,
            wifi_on,
            bt_on,
            volume,
            muted,
            mic_volume: 0.75,
        });

        // Fetch the real tile state off-thread; applied in `tick_animations`.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (volume, muted) = crate::controls::volume();
            let (mic_volume, _) = crate::controls::mic_volume();
            let _ = tx.send(CcSnapshot {
                wifi_on: crate::controls::wifi_enabled(),
                bt_on: crate::controls::bt_enabled(),
                volume,
                muted,
                mic_volume,
                ssid: crate::controls::wifi_ssid(),
                clock: crate::controls::datetime(),
            });
        });
        self.cc_rx = Some(rx);
    }

    /// Apply a background-fetched Control-Center snapshot: update the live state
    /// and re-rasterise the labels that depend on it (clock, Wi-Fi/Bluetooth sub).
    fn cc_apply_snapshot(&mut self, snap: CcSnapshot) {
        const SUB: [u8; 3] = [170, 178, 196];
        const LABEL: [u8; 3] = [232, 236, 244];
        let wifi_sub_text = if snap.wifi_on {
            snap.ssid.clone().unwrap_or_else(|| "Açık".to_string())
        } else {
            "Kapalı".to_string()
        };
        let bt_sub_text = if snap.bt_on { "Açık" } else { "Kapalı" };
        let text = self.text.as_ref();
        let wifi_sub = cc_rasterize(text, &wifi_sub_text, 12.0, SUB, 150);
        let bt_sub = cc_rasterize(text, bt_sub_text, 12.0, SUB, 150);
        let clock = cc_rasterize(text, &snap.clock, 14.0, LABEL, 348);
        if let Some(cc) = self.control_center.as_mut() {
            cc.wifi_on = snap.wifi_on;
            cc.bt_on = snap.bt_on;
            cc.volume = snap.volume;
            cc.muted = snap.muted;
            cc.mic_volume = snap.mic_volume;
            cc.clock = clock;
            for t in cc.tiles.iter_mut() {
                if matches!(t.action, CcAction::WifiToggle) {
                    t.sub = wifi_sub.clone();
                } else if matches!(t.action, CcAction::BtToggle) {
                    t.sub = bt_sub.clone();
                }
            }
        }
    }

    /// Open the Android-style Wi-Fi panel: a header on/off switch and (when on)
    /// the scanned network list. Reflects the current radio state — it does NOT
    /// force the radio on; the header switch does that.
    fn open_wifi_panel(&mut self, out: OutputId) {
        self.apps_menu = None;
        self.bt_panel = None;
        if crate::controls::wifi_enabled() {
            self.wifi_start_scan(out);
        } else {
            self.build_wifi_panel(out, Vec::new(), Some("Kapalı".to_string()), false, false);
        }
    }

    /// Show the scanning spinner and kick off a background scan (the slow NM
    /// rescan never blocks the compositor; an empty first pass is retried once
    /// in case the radio only just came up).
    fn wifi_start_scan(&mut self, out: OutputId) {
        self.build_wifi_panel(out, Vec::new(), Some("Aranıyor…".to_string()), true, true);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // Stage 1: paint the cached list instantly.
            let _ = tx.send(WifiMsg::ScannedPartial(crate::controls::wifi_scan_cached()));
            // Stage 2: kick a real rescan, then re-read a few times as the
            // (async) results trickle in — while connected the driver refreshes
            // other APs' signals slowly, so poll rather than wait a fixed time.
            crate::controls::wifi_rescan_trigger();
            for i in 0..3 {
                std::thread::sleep(std::time::Duration::from_millis(1800));
                let nets = crate::controls::wifi_scan_cached();
                let msg = if i == 2 {
                    WifiMsg::Scanned(nets) // final → ends scanning
                } else {
                    WifiMsg::ScannedPartial(nets) // intermediate refresh
                };
                if tx.send(msg).is_err() {
                    break; // panel closed / superseded
                }
            }
        });
        self.wifi_rx = Some(rx);
    }

    /// Header on/off switch: flip the radio. On→off rebuilds the off panel (list
    /// hidden); off→on enables the radio and scans.
    fn wifi_toggle_radio(&mut self) {
        let Some(out) = self.wifi_panel.as_ref().map(|p| p.output) else {
            return;
        };
        let on = self.wifi_panel.as_ref().map(|p| p.wifi_on).unwrap_or(false);
        if on {
            crate::controls::set_wifi(false);
            self.wifi_rx = None;
            self.build_wifi_panel(out, Vec::new(), Some("Kapalı".to_string()), false, false);
        } else {
            crate::controls::set_wifi(true);
            self.wifi_start_scan(out);
        }
    }

    /// Open the per-network details screen for the connected network: show a
    /// "Yükleniyor…" panel, then fetch the details (autoconnect / IP method /
    /// MAC / IPv4 / IPv6 — several blocking nmcli calls) on a background thread.
    /// Open the per-device details screen (`eth` = Ethernet, else Wi-Fi). Shows
    /// a "Yükleniyor…" panel, then fetches the details on a background thread
    /// (for Ethernet it first takes the device under NetworkManager).
    fn enter_net_details(&mut self, out: OutputId, eth: bool) {
        self.build_wifi_details_panel(out, crate::controls::WifiDetails::default(), true, eth);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // Read whatever NM exposes (MAC/IP/state work even when the device is
            // unmanaged — e.g. a static/ifupdown Ethernet); no forced managing.
            let d = crate::controls::net_details(eth).unwrap_or_default();
            let _ = tx.send(WifiMsg::Details(d));
        });
        self.wifi_rx = Some(rx);
    }

    /// Build the details screen. `loading` shows a spinner status while the real
    /// values are fetched; `eth` switches the title + Back behaviour. Rows:
    /// auto-reconnect (switch), DHCP / Static, MAC / IPv4 / IPv6 (read-only), Back.
    fn build_wifi_details_panel(
        &mut self,
        out: OutputId,
        d: crate::controls::WifiDetails,
        loading: bool,
        eth: bool,
    ) {
        let nets = self.wifi_panel.as_ref().map(|p| p.nets.clone()).unwrap_or_default();
        let bounds = self
            .wm
            .output(out)
            .map(|o| o.bounds)
            .unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0));

        const PAD: f32 = 16.0;
        const W: f32 = 380.0;
        const CH: f32 = 30.0;
        const STH: f32 = 22.0;
        const ROW_H: f32 = 52.0;
        const GAP: f32 = 8.0;
        const SWITCH_W: f32 = 50.0;
        const SWITCH_H: f32 = 28.0;
        const LABEL: [u8; 3] = [232, 236, 244];
        const SUB: [u8; 3] = [170, 178, 196];

        let inner = W - 2.0 * PAD;
        let lw = (inner - 24.0).max(1.0) as usize;
        let text = self.text.as_ref();

        // An Ethernet device managed outside NetworkManager (Debian's default
        // ifupdown/static setup) can't be toggled or switched DHCP/static through
        // nmcli, so show a read-only screen instead of dead controls.
        let read_only = eth && !loading && !d.managed;
        let n_rows: f32 = if read_only { 4.0 } else { 7.0 };
        let panel_h = PAD + CH + GAP + STH + GAP + n_rows * ROW_H + (n_rows - 1.0) * GAP + PAD;
        let m = 14.0;
        let bx = (bounds.x + bounds.w - W - m).max(bounds.x + m);
        let by = (bounds.y + m).max(bounds.y);
        let panel = Rect::new(bx, by, W, panel_h);
        let cx = bx + PAD;

        let title_text = if eth { "Ethernet" } else { "Ağ Ayarları" };
        let title = cc_rasterize(text, title_text, 17.0, LABEL, inner as usize);
        let status_text = if loading {
            "Yükleniyor…".to_string()
        } else if read_only {
            "Sistem tarafından yönetiliyor (NM dışı)".to_string()
        } else if eth {
            let st = if d.connected { "Bağlı" } else { "Bağlı değil" };
            if d.conn.is_empty() { st.to_string() } else { format!("{} · {}", d.conn, st) }
        } else {
            d.conn.clone()
        };
        let status = cc_rasterize(text, &status_text, 12.0, SUB, inner as usize);

        let mut rows = Vec::with_capacity(n_rows as usize);
        let mut y = by + PAD + CH + GAP + STH + GAP;

        // The on/off + DHCP/Static controls only when NM actually manages the
        // device (always for Wi-Fi; for Ethernet only if not externally managed).
        let ac_switch_rect = Rect::new(
            cx + inner - SWITCH_W - 14.0,
            y + (ROW_H - SWITCH_H) / 2.0,
            SWITCH_W,
            SWITCH_H,
        );
        if !read_only {
            // 1. Ethernet: link on/off; Wi-Fi: auto-reconnect.
            let (row1_label, row1_action) = if eth {
                ("Bağlantıyı aç/kapat", WifiAction::ToggleLink)
            } else {
                ("Otomatik yeniden bağlan", WifiAction::ToggleAutoconnect)
            };
            rows.push(WifiRow {
                rect: Rect::new(cx, y, inner, ROW_H),
                action: row1_action,
                label: cc_rasterize(text, row1_label, 14.0, LABEL, (lw - 70).max(1)),
                meta: None,
                active: false,
            });
            y += ROW_H + GAP;
            // 2. DHCP / 3. Static (the current one is accent-tinted).
            rows.push(WifiRow {
                rect: Rect::new(cx, y, inner, ROW_H),
                action: WifiAction::SetDhcp,
                label: cc_rasterize(text, "DHCP (otomatik)", 15.0, LABEL, lw),
                meta: None,
                active: d.dhcp,
            });
            y += ROW_H + GAP;
            rows.push(WifiRow {
                rect: Rect::new(cx, y, inner, ROW_H),
                action: WifiAction::SetStatic,
                label: cc_rasterize(text, "Statik IP", 15.0, LABEL, lw),
                meta: None,
                active: !d.dhcp,
            });
            y += ROW_H + GAP;
        }
        // read-only info (always).
        for (lab, val) in [("MAC", &d.mac), ("IPv4", &d.ipv4), ("IPv6", &d.ipv6)] {
            let t = if val.is_empty() { format!("{lab}: —") } else { format!("{lab}: {val}") };
            rows.push(WifiRow {
                rect: Rect::new(cx, y, inner, ROW_H),
                action: WifiAction::Info,
                label: cc_rasterize(text, &t, 13.0, SUB, lw),
                meta: None,
                active: false,
            });
            y += ROW_H + GAP;
        }
        // back.
        rows.push(WifiRow {
            rect: Rect::new(cx, y, inner, ROW_H),
            action: WifiAction::Back,
            label: cc_rasterize(text, "‹ Geri", 15.0, LABEL, lw),
            meta: None,
            active: false,
        });

        self.wifi_panel = Some(WifiPanel {
            output: out,
            panel,
            title,
            status,
            rows,
            wifi_on: true,
            switch_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            nets,
            connecting: None,
            scanning: loading,
            pw_for: None,
            pw_buf: String::new(),
            pw_show: false,
            pw_field: None,
            pw_text: None,
            details: Some(d),
            ac_switch_rect,
            static_entry: None,
            eth,
        });
    }

    /// (Re)build the Wi-Fi panel from a scan result. `status` overrides the auto
    /// "N ağ bulundu" line; `scanning` flags a background scan; `wifi_on` drives
    /// the header switch and whether the network list is shown.
    fn build_wifi_panel(
        &mut self,
        out: OutputId,
        nets: Vec<crate::controls::WifiNet>,
        status: Option<String>,
        scanning: bool,
        wifi_on: bool,
    ) {
        let bounds = self
            .wm
            .output(out)
            .map(|o| o.bounds)
            .unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0));

        const PAD: f32 = 16.0;
        const W: f32 = 380.0;
        const CH: f32 = 30.0; // title
        const STH: f32 = 20.0; // status line
        const ROW_H: f32 = 52.0;
        const GAP: f32 = 8.0;
        const MAX_ROWS: usize = 9; // no scroll yet — cap the visible list

        const LABEL: [u8; 3] = [232, 236, 244];
        const SUB: [u8; 3] = [170, 178, 196];

        let inner = W - 2.0 * PAD;
        let lw = (inner - 24.0).max(1.0) as usize;
        let text = self.text.as_ref();

        let shown = nets.len().min(MAX_ROWS);
        let n_rows = shown + 1; // + the "Kapat" (dismiss) button
        let list_h = n_rows as f32 * ROW_H + n_rows.saturating_sub(1) as f32 * GAP;
        let panel_h = PAD + CH + GAP + STH + GAP + list_h + PAD;

        let m = 14.0;
        let bx = (bounds.x + bounds.w - W - m).max(bounds.x + m);
        let by = (bounds.y + m).max(bounds.y);
        let panel = Rect::new(bx, by, W, panel_h);
        let cx = bx + PAD;

        // Header switch (Android-style), right-aligned in the title row.
        const SWITCH_W: f32 = 50.0;
        const SWITCH_H: f32 = 28.0;
        let switch_rect = Rect::new(
            bx + W - PAD - SWITCH_W,
            by + PAD + (CH - SWITCH_H) / 2.0,
            SWITCH_W,
            SWITCH_H,
        );

        let title = cc_rasterize(text, "Wi-Fi", 17.0, LABEL, (inner - SWITCH_W - 12.0).max(1.0) as usize);
        let status_text = status.unwrap_or_else(|| {
            if nets.is_empty() {
                "Ağ bulunamadı".to_string()
            } else {
                format!("{} ağ bulundu", nets.len())
            }
        });
        let status = cc_rasterize(text, &status_text, 12.0, SUB, inner as usize);

        let mut rows = Vec::with_capacity(n_rows);
        let mut y = by + PAD + CH + GAP + STH + GAP;
        // Dark text for the connected row (its card is a bright turquoise — see
        // render); white/grey for the rest (dark cards).
        const DARK: [u8; 3] = [18, 22, 30];
        const DARK_SUB: [u8; 3] = [40, 48, 60];
        for net in nets.iter().take(MAX_ROWS) {
            let rect = Rect::new(cx, y, inner, ROW_H);
            let (lbl_col, meta_col) = if net.active { (DARK, DARK_SUB) } else { (LABEL, SUB) };
            let label = cc_rasterize(text, &net.ssid, 15.0, lbl_col, lw);
            let lock = if net.secured { "Kilitli" } else { "Açık" };
            let meta_text = if net.active {
                format!("Bağlı  ·  %{}", net.signal)
            } else {
                format!("%{}  ·  {}", net.signal, lock)
            };
            let meta = cc_rasterize(text, &meta_text, 12.0, meta_col, lw);
            // The connected network opens its details screen; others connect.
            let action = if net.active {
                WifiAction::OpenDetails
            } else {
                WifiAction::Connect { ssid: net.ssid.clone(), secured: net.secured }
            };
            rows.push(WifiRow { rect, action, label, meta, active: net.active });
            y += ROW_H + GAP;
        }
        // Trailing dismiss button (the Wi-Fi radio on/off now lives on the
        // Control Center tile, so the picker only needs a plain close).
        rows.push(WifiRow {
            rect: Rect::new(cx, y, inner, ROW_H),
            action: WifiAction::ClosePanel,
            label: cc_rasterize(text, "Kapat", 15.0, LABEL, lw),
            meta: None,
            active: false,
        });

        self.wifi_panel = Some(WifiPanel {
            output: out,
            panel,
            title,
            status,
            rows,
            wifi_on,
            switch_rect,
            nets,
            connecting: None,
            scanning,
            pw_for: None,
            pw_buf: String::new(),
            pw_show: false,
            pw_field: None,
            pw_text: None,
            details: None,
            ac_switch_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            static_entry: None,
            eth: false,
        });
    }

    /// Rebuild the panel as a password prompt for `ssid`: the network name, a
    /// masked field (shown via the status line), and Connect / Cancel buttons.
    fn build_wifi_pw_panel(&mut self, out: OutputId, ssid: String) {
        // Carry the last scan forward so Cancel / a finished connect can rebuild
        // the list without re-scanning.
        let nets = self.wifi_panel.as_ref().map(|p| p.nets.clone()).unwrap_or_default();
        let bounds = self
            .wm
            .output(out)
            .map(|o| o.bounds)
            .unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0));

        const PAD: f32 = 16.0;
        const W: f32 = 380.0;
        const CH: f32 = 30.0;
        const STH: f32 = 24.0;
        const ROW_H: f32 = 52.0;
        const GAP: f32 = 8.0;
        const LABEL: [u8; 3] = [232, 236, 244];
        const SUB: [u8; 3] = [170, 178, 196];

        let inner = W - 2.0 * PAD;
        let lw = (inner - 24.0).max(1.0) as usize;
        // Password text is clipped to leave room for the reveal (eye) button.
        let tw = (inner - 14.0 - WIFI_EYE_W).max(1.0) as usize;
        let text = self.text.as_ref();

        let panel_h = PAD + CH + GAP + STH + GAP + ROW_H + GAP + 2.0 * ROW_H + GAP + PAD;
        let m = 14.0;
        let bx = (bounds.x + bounds.w - W - m).max(bounds.x + m);
        let by = (bounds.y + m).max(bounds.y);
        let panel = Rect::new(bx, by, W, panel_h);
        let cx = bx + PAD;

        let title = cc_rasterize(text, &ssid, 17.0, LABEL, inner as usize);
        let status = cc_rasterize(text, "Parola gerekli", 13.0, SUB, inner as usize);
        // Empty field → grey placeholder; the eye reveals it once typed.
        let pw_text = cc_rasterize(text, "Parola", 15.0, SUB, tw);

        let mut y = by + PAD + CH + GAP + STH + GAP;
        let pw_field = Rect::new(cx, y, inner, ROW_H);
        y += ROW_H + GAP;
        let connect = WifiRow {
            rect: Rect::new(cx, y, inner, ROW_H),
            action: WifiAction::PwConnect,
            label: cc_rasterize(text, "Bağlan", 15.0, LABEL, lw),
            meta: None,
            active: true, // accent tint
        };
        y += ROW_H + GAP;
        let cancel = WifiRow {
            rect: Rect::new(cx, y, inner, ROW_H),
            action: WifiAction::PwCancel,
            label: cc_rasterize(text, "İptal", 15.0, LABEL, lw),
            meta: None,
            active: false,
        };

        self.wifi_panel = Some(WifiPanel {
            output: out,
            panel,
            title,
            status,
            rows: vec![connect, cancel],
            wifi_on: true,
            switch_rect: Rect::new(0.0, 0.0, 0.0, 0.0), // no switch in password mode
            nets,
            connecting: None,
            scanning: false,
            pw_for: Some(ssid),
            pw_buf: String::new(),
            pw_show: false,
            pw_field: Some(pw_field),
            pw_text,
            details: None,
            ac_switch_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            static_entry: None,
            eth: false,
        });
    }

    /// The keyboard is feeding the panel (password OR static-IP entry).
    pub fn wifi_text_active(&self) -> bool {
        self.wifi_panel
            .as_ref()
            .map(|p| p.pw_for.is_some() || p.static_entry.is_some())
            .unwrap_or(false)
    }

    /// Enter password mode for a secured network and pop the on-screen keyboard.
    fn enter_wifi_password(&mut self, out: OutputId, ssid: String) {
        self.build_wifi_pw_panel(out, ssid);
        self.osk.bind(self.wm.clone(), out);
        let rect = self
            .wifi_panel
            .as_ref()
            .map(|p| OskRect { x: p.panel.x, y: p.panel.y, w: p.panel.w, h: p.panel.h })
            .unwrap_or(OskRect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 });
        let field = FocusedField { rect, mode: InputMode::Password, has_hw_keyboard: false };
        self.osk.open_for(field);
        let _ = self.osk.confirm_open();
        self.apply_osk_xkb();
        self.osk_dirty = true;
    }

    /// Refresh the input box text. Password mode: grey "Parola" placeholder,
    /// masked dots, or clear text when revealed. Static mode: plain text with a
    /// per-step placeholder (no masking, no eye).
    fn wifi_pw_refresh(&mut self) {
        let Some((n, show, buf, static_step)) = self.wifi_panel.as_ref().map(|p| {
            (p.pw_buf.chars().count(), p.pw_show, p.pw_buf.clone(), p.static_entry.as_ref().map(|s| s.step))
        }) else {
            return;
        };
        const SUB: [u8; 3] = [170, 178, 196];
        const LABEL: [u8; 3] = [232, 236, 244];
        let tw = (380.0_f32 - 2.0 * 16.0 - 14.0 - WIFI_EYE_W).max(1.0) as usize;
        let (s, col) = if let Some(step) = static_step {
            if n == 0 {
                (static_field_hint(step).to_string(), SUB)
            } else {
                (buf, LABEL)
            }
        } else if n == 0 {
            ("Parola".to_string(), SUB)
        } else if show {
            (buf, LABEL)
        } else {
            ("•".repeat(n), LABEL)
        };
        let lbl = cc_rasterize(self.text.as_ref(), &s, 15.0, col, tw);
        if let Some(p) = self.wifi_panel.as_mut() {
            p.pw_text = lbl;
        }
    }

    /// Submit the active text field: password connect, or advance/apply static IP.
    fn wifi_text_submit(&mut self) {
        if self.wifi_panel.as_ref().is_some_and(|p| p.static_entry.is_some()) {
            self.wifi_static_next();
        } else {
            self.wifi_pw_submit();
        }
    }

    /// Toggle clear-text reveal of the typed password (eye button). The eye icon
    /// itself is chosen from `pw_show` at render time.
    fn wifi_pw_toggle_show(&mut self) {
        if let Some(p) = self.wifi_panel.as_mut() {
            p.pw_show = !p.pw_show;
        }
        self.wifi_pw_refresh();
    }

    fn wifi_pw_input(&mut self, s: &str) {
        if let Some(p) = self.wifi_panel.as_mut() {
            p.pw_buf.push_str(s);
        }
        self.wifi_pw_refresh();
    }

    fn wifi_pw_backspace(&mut self) {
        if let Some(p) = self.wifi_panel.as_mut() {
            p.pw_buf.pop();
        }
        self.wifi_pw_refresh();
    }

    /// Connect with the typed password, hide the keyboard, leave password mode.
    fn wifi_pw_submit(&mut self) {
        let Some((ssid, pw)) = self
            .wifi_panel
            .as_ref()
            .and_then(|p| p.pw_for.clone().map(|s| (s, p.pw_buf.clone())))
        else {
            return;
        };
        if pw.is_empty() {
            // An empty password on a secured network yields a confusing
            // "key-mgmt is missing" from nmcli — just prompt instead.
            self.wifi_set_status("Parola girin");
            return; // keep the keyboard up
        }
        self.osk_hide();
        self.osk_dirty = true;
        // Keep `pw_for` set so the result handler can tell this was a password
        // attempt (success → rebuild list; failure → re-prompt).
        self.wifi_start_connect(ssid, Some(pw));
    }

    /// Abandon password entry: hide the keyboard and return to the list.
    fn wifi_pw_cancel(&mut self, out: OutputId) {
        self.osk_hide();
        self.osk_dirty = true;
        self.open_wifi_panel(out);
    }

    /// Open the OSK in plain-text mode for the picker's own text entry.
    fn open_wifi_keyboard(&mut self, out: OutputId) {
        self.osk.bind(self.wm.clone(), out);
        let rect = self
            .wifi_panel
            .as_ref()
            .map(|p| OskRect { x: p.panel.x, y: p.panel.y, w: p.panel.w, h: p.panel.h })
            .unwrap_or(OskRect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 });
        let field = FocusedField { rect, mode: InputMode::Text, has_hw_keyboard: false };
        self.osk.open_for(field);
        let _ = self.osk.confirm_open();
        self.apply_osk_xkb();
        self.osk_dirty = true;
    }

    /// Begin static-IP entry for the connected network (IP/prefix → gateway →
    /// DNS), prefilling step 0 with the current IP.
    fn enter_wifi_static(&mut self, out: OutputId) {
        let Some(d) = self.wifi_panel.as_ref().and_then(|p| p.details.clone()) else {
            return;
        };
        let entry = StaticEntry {
            conn: d.conn.clone(),
            step: 0,
            ip: String::new(),
            gateway: String::new(),
            gw_prefill: d.gateway.clone(),
            dns_prefill: d.dns.clone(),
        };
        self.build_wifi_static_panel(out, entry, d.ipv4.clone());
        self.open_wifi_keyboard(out);
        // IP / gateway / DNS are digits + dots, so open on the numeric page
        // (the OSK otherwise starts on Turkish-F letters and the user can't
        // reach the digits easily).
        let _ = self.osk.goto_layout(crate::keyboard::LayoutId::Numeric);
        self.apply_osk_xkb();
        self.osk_dirty = true;
    }

    /// Build the static-IP entry panel for the current step: a title, a progress
    /// status, the editable box, and Next/Apply + Cancel buttons.
    fn build_wifi_static_panel(&mut self, out: OutputId, entry: StaticEntry, buf: String) {
        let nets = self.wifi_panel.as_ref().map(|p| p.nets.clone()).unwrap_or_default();
        let eth = self.wifi_panel.as_ref().map(|p| p.eth).unwrap_or(false);
        let details = self.wifi_panel.as_ref().and_then(|p| p.details.clone());
        let bounds = self
            .wm
            .output(out)
            .map(|o| o.bounds)
            .unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0));

        const PAD: f32 = 16.0;
        const W: f32 = 380.0;
        const CH: f32 = 30.0;
        const STH: f32 = 22.0;
        const ROW_H: f32 = 52.0;
        const GAP: f32 = 8.0;
        const LABEL: [u8; 3] = [232, 236, 244];
        const SUB: [u8; 3] = [170, 178, 196];

        let inner = W - 2.0 * PAD;
        let lw = (inner - 24.0).max(1.0) as usize;
        let text = self.text.as_ref();

        let panel_h = PAD + CH + GAP + STH + GAP + ROW_H + GAP + 2.0 * ROW_H + GAP + PAD;
        let m = 14.0;
        let bx = (bounds.x + bounds.w - W - m).max(bounds.x + m);
        let by = (bounds.y + m).max(bounds.y);
        let panel = Rect::new(bx, by, W, panel_h);
        let cx = bx + PAD;

        let title = cc_rasterize(text, static_field_title(entry.step), 16.0, LABEL, inner as usize);
        let prog = |v: &str| if v.is_empty() { "…".to_string() } else { v.to_string() };
        let status_text = format!("IP: {}   GW: {}", prog(&entry.ip), prog(&entry.gateway));
        let status = cc_rasterize(text, &status_text, 12.0, SUB, inner as usize);

        let mut y = by + PAD + CH + GAP + STH + GAP;
        let pw_field = Rect::new(cx, y, inner, ROW_H);
        y += ROW_H + GAP;
        let next_label = if entry.step >= 2 { "Uygula" } else { "İleri" };
        let next = WifiRow {
            rect: Rect::new(cx, y, inner, ROW_H),
            action: WifiAction::StaticNext,
            label: cc_rasterize(text, next_label, 15.0, LABEL, lw),
            meta: None,
            active: true,
        };
        y += ROW_H + GAP;
        let cancel = WifiRow {
            rect: Rect::new(cx, y, inner, ROW_H),
            action: WifiAction::StaticCancel,
            label: cc_rasterize(text, "İptal", 15.0, LABEL, lw),
            meta: None,
            active: false,
        };

        self.wifi_panel = Some(WifiPanel {
            output: out,
            panel,
            title,
            status,
            rows: vec![next, cancel],
            wifi_on: true,
            switch_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            nets,
            connecting: None,
            scanning: false,
            pw_for: None,
            pw_buf: buf,
            pw_show: false,
            pw_field: Some(pw_field),
            pw_text: None,
            details,
            ac_switch_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            static_entry: Some(entry),
            eth,
        });
        self.wifi_pw_refresh(); // render the box text from `pw_buf`
    }

    /// Accept the current static-IP field → next field, or apply on the last.
    fn wifi_static_next(&mut self) {
        let Some((out, mut entry, buf)) = self
            .wifi_panel
            .as_ref()
            .and_then(|p| p.static_entry.clone().map(|e| (p.output, e, p.pw_buf.clone())))
        else {
            return;
        };
        let val = buf.trim().to_string();
        match entry.step {
            0 => {
                if val.is_empty() {
                    self.wifi_set_status("IP adresi girin");
                    return;
                }
                entry.ip = val;
                entry.step = 1;
                let prefill = entry.gw_prefill.clone();
                self.build_wifi_static_panel(out, entry, prefill);
            }
            1 => {
                entry.gateway = val; // may be left blank
                entry.step = 2;
                let prefill = entry.dns_prefill.clone();
                self.build_wifi_static_panel(out, entry, prefill);
            }
            _ => {
                // Step 2 → apply on a background thread (modify + reactivate
                // blocks for seconds), then re-fetch and show details.
                self.osk_hide();
                self.osk_dirty = true;
                let eth = self.wifi_panel.as_ref().map(|p| p.eth).unwrap_or(false);
                let (conn, ip, gw, dns) = (entry.conn.clone(), entry.ip.clone(), entry.gateway.clone(), val);
                let cur = self.wifi_panel.as_ref().and_then(|p| p.details.clone()).unwrap_or_default();
                self.build_wifi_details_panel(out, cur, true, eth); // "Yükleniyor…"
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    crate::controls::wifi_set_static(&conn, &ip, &gw, &dns);
                    let _ = tx.send(WifiMsg::Details(
                        crate::controls::net_details(eth).unwrap_or_default(),
                    ));
                });
                self.wifi_rx = Some(rx);
            }
        }
    }

    /// Abandon static-IP entry: hide the keyboard and return to the details screen.
    fn wifi_static_cancel(&mut self, out: OutputId) {
        self.osk_hide();
        self.osk_dirty = true;
        let eth = self.wifi_panel.as_ref().map(|p| p.eth).unwrap_or(false);
        self.enter_net_details(out, eth);
    }

    /// Route a *physical*-keyboard key into the picker's text entry (password /
    /// static IP) — `raw` is the xkb keysym, `utf8` its text. Returns whether it
    /// was consumed (the caller then intercepts it instead of forwarding to a
    /// client). Mirrors the on-screen-keyboard capture path.
    pub fn wifi_physical_key(&mut self, raw: u32, utf8: &str) -> bool {
        if !self.wifi_text_active() {
            return false;
        }
        const KEY_ESC: u32 = 0xff1b;
        const KEY_BACKSPACE: u32 = 0xff08;
        const KEY_RETURN: u32 = 0xff0d;
        const KEY_KP_ENTER: u32 = 0xff8d;
        match raw {
            KEY_ESC => {
                if let Some(out) = self.wifi_panel.as_ref().map(|p| p.output) {
                    if self.wifi_panel.as_ref().is_some_and(|p| p.static_entry.is_some()) {
                        self.wifi_static_cancel(out);
                    } else {
                        self.wifi_pw_cancel(out);
                    }
                }
                true
            }
            KEY_BACKSPACE => {
                self.wifi_pw_backspace();
                true
            }
            KEY_RETURN | KEY_KP_ENTER => {
                self.wifi_text_submit();
                true
            }
            _ => {
                if let Some(c) = utf8.chars().next() {
                    if !c.is_control() {
                        self.wifi_pw_input(&c.to_string());
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Re-rasterise just the panel's status line in place (no relayout).
    fn wifi_set_status(&mut self, s: &str) {
        const SUB: [u8; 3] = [170, 178, 196];
        let inner = (380.0 - 2.0 * 16.0) as usize;
        let lbl = cc_rasterize(self.text.as_ref(), s, 12.0, SUB, inner);
        if let Some(p) = self.wifi_panel.as_mut() {
            p.status = lbl;
        }
    }

    // ----- Bluetooth panel ---------------------------------------------------

    /// Open the Bluetooth panel: start (or reuse) the `bluetoothctl` coprocess,
    /// power the adapter on, scan, and list known devices.
    fn open_bt_panel(&mut self, out: OutputId) {
        self.apps_menu = None;
        self.wifi_panel = None;
        if self.btctl.is_none() {
            self.btctl = crate::bluetooth::BtCtl::start();
        }
        let powered = crate::controls::bt_enabled();
        // Seed the list with already-bonded devices (so they show in the paired
        // section immediately, not offered for a failing re-pair).
        let devices = self.bt_known_devices();
        if let Some(bt) = self.btctl.as_ref() {
            if powered {
                // Be discoverable + pairable so a phone can initiate pairing with
                // us (incoming requests auto-confirm via the agent), and scan.
                bt.send("pairable on");
                bt.send("discoverable on");
                bt.scan(true);
            }
        }
        self.build_bt_panel(out, devices, powered, powered);
    }

    /// Query BlueZ for paired + connected devices and return them as
    /// `BtDevice`s (the panel seed / refresh source). The query is **unioned**
    /// into `bt_paired`, never overwritten — `bluetoothctl devices Paired` can
    /// lag a just-completed bond by a moment, and overwriting would drop the
    /// freshly-paired device (it would then only appear after a panel reopen).
    fn bt_known_devices(&mut self) -> Vec<BtDevice> {
        let paired = crate::bluetooth::paired_devices();
        let connected: std::collections::HashSet<String> =
            crate::bluetooth::connected_macs().into_iter().collect();
        // Names from the query, plus any we already know from the live list.
        let mut names: std::collections::HashMap<String, String> = paired.into_iter().collect();
        for m in names.keys() {
            self.bt_paired.insert(m.clone());
        }
        if let Some(p) = self.bt_panel.as_ref() {
            for d in &p.devices {
                if !d.name.is_empty() {
                    names.entry(d.mac.clone()).or_insert_with(|| d.name.clone());
                }
            }
        }
        self.bt_paired
            .iter()
            .map(|mac| BtDevice {
                connected: connected.contains(mac),
                paired: true,
                name: names.get(mac).cloned().unwrap_or_default(),
                mac: mac.clone(),
            })
            .collect()
    }

    /// Re-query known devices (after pair/forget) and rebuild, keeping any
    /// freshly-discovered (unpaired) devices from the live list.
    fn bt_refresh_devices(&mut self) {
        let out = self.bt_panel.as_ref().map(|p| p.output);
        let powered = self.bt_panel.as_ref().map(|p| p.powered).unwrap_or(false);
        // Discovered (unpaired) devices we want to keep.
        let discovered: Vec<BtDevice> = self
            .bt_panel
            .as_ref()
            .map(|p| p.devices.iter().filter(|d| !d.paired).cloned().collect())
            .unwrap_or_default();
        let mut devices = self.bt_known_devices();
        for d in discovered {
            if !devices.iter().any(|x| x.mac == d.mac) {
                devices.push(d);
            }
        }
        if let Some(out) = out {
            self.build_bt_panel(out, devices, powered, false);
        }
    }

    /// (Re)build the Bluetooth panel from the current device list.
    fn build_bt_panel(&mut self, out: OutputId, mut devices: Vec<BtDevice>, powered: bool, scanning: bool) {
        // A scanned device that BlueZ already bonded should count as paired (the
        // `paired` flag otherwise only arrives via a live pairing event).
        for d in devices.iter_mut() {
            if self.bt_paired.contains(&d.mac) {
                d.paired = true;
            }
        }
        let bounds = self
            .wm
            .output(out)
            .map(|o| o.bounds)
            .unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0));

        const PAD: f32 = 16.0;
        const W: f32 = 380.0;
        const CH: f32 = 30.0;
        const STH: f32 = 20.0;
        const ROW_H: f32 = 52.0;
        const GAP: f32 = 8.0;
        const SWITCH_W: f32 = 50.0;
        const SWITCH_H: f32 = 28.0;
        const MAX_ROWS: usize = 8;
        const LABEL: [u8; 3] = [232, 236, 244];
        const SUB: [u8; 3] = [170, 178, 196];
        const DARK: [u8; 3] = [18, 22, 30];
        const DARK_SUB: [u8; 3] = [40, 48, 60];

        let inner = W - 2.0 * PAD;
        let lw = (inner - 24.0).max(1.0) as usize;
        let text = self.text.as_ref();

        const FORGET_W: f32 = 78.0;
        const HDR_H: f32 = 24.0;

        // Split into paired (top section) and available (discovered, unpaired).
        // Stable order within each (no name tiebreak — names stream in async).
        let mut paired: Vec<&BtDevice> = devices.iter().filter(|d| d.paired).collect();
        paired.sort_by_key(|d| std::cmp::Reverse(d.connected));
        let avail: Vec<&BtDevice> =
            devices.iter().filter(|d| !d.paired).take(MAX_ROWS).collect();

        // Panel height from the actual row + header count.
        let mut rows_h = 0.0_f32;
        if powered {
            rows_h += ROW_H + GAP; // "Cihazları Tara" button
        }
        if powered && !paired.is_empty() {
            rows_h += HDR_H + GAP + paired.len() as f32 * (ROW_H + GAP);
        }
        if powered {
            rows_h += HDR_H + GAP + avail.len() as f32 * (ROW_H + GAP);
        }
        rows_h += ROW_H; // Kapat
        let panel_h = PAD + CH + GAP + STH + GAP + rows_h + PAD;

        let m = 14.0;
        let bx = (bounds.x + bounds.w - W - m).max(bounds.x + m);
        let by = (bounds.y + m).max(bounds.y);
        let panel = Rect::new(bx, by, W, panel_h);
        let cx = bx + PAD;

        let switch_rect = Rect::new(
            bx + W - PAD - SWITCH_W,
            by + PAD + (CH - SWITCH_H) / 2.0,
            SWITCH_W,
            SWITCH_H,
        );
        let title = cc_rasterize(text, "Bluetooth", 17.0, LABEL, (inner - SWITCH_W - 12.0).max(1.0) as usize);
        let status_text = if !powered {
            "Kapalı".to_string()
        } else if scanning && avail.is_empty() {
            "Aranıyor…".to_string()
        } else {
            format!("{} eşleşmiş · {} bulundu", paired.len(), avail.len())
        };
        let status = cc_rasterize(text, &status_text, 12.0, SUB, inner as usize);

        let mut rows = Vec::new();
        let mut y = by + PAD + CH + GAP + STH + GAP;

        let header = |rows: &mut Vec<BtRow>, y: &mut f32, txt: &str| {
            rows.push(BtRow {
                rect: Rect::new(cx, *y, inner, HDR_H),
                action: BtAction::Header,
                label: cc_rasterize(self.text.as_ref(), txt, 11.0, SUB, lw),
                meta: None,
                connected: false,
                forget: None,
            });
            *y += HDR_H + GAP;
        };

        // --- "Cihazları Tara" button ---
        if powered {
            let lbl = if scanning { "Aranıyor…" } else { "Cihazları Tara" };
            rows.push(BtRow {
                rect: Rect::new(cx, y, inner, ROW_H),
                action: BtAction::Scan,
                label: cc_rasterize(text, lbl, 15.0, LABEL, lw),
                meta: None,
                connected: false,
                forget: None,
            });
            y += ROW_H + GAP;
        }

        // --- Paired section ---
        if powered && !paired.is_empty() {
            header(&mut rows, &mut y, "EŞLEŞMİŞ CİHAZLAR");
            for dev in &paired {
                let rect = Rect::new(cx, y, inner, ROW_H);
                let (lbl_col, meta_col) = if dev.connected { (DARK, DARK_SUB) } else { (LABEL, SUB) };
                let name = if dev.name.is_empty() { dev.mac.clone() } else { dev.name.clone() };
                let label = cc_rasterize(text, &name, 15.0, lbl_col, (lw as f32 - FORGET_W) as usize);
                let meta = cc_rasterize(
                    text,
                    if dev.connected { "Bağlı" } else { "Eşleşmiş" },
                    12.0,
                    meta_col,
                    (lw as f32 - FORGET_W) as usize,
                );
                let action = if dev.connected {
                    BtAction::Disconnect(dev.mac.clone())
                } else {
                    BtAction::Connect(dev.mac.clone())
                };
                let fcol = if dev.connected { DARK } else { LABEL };
                let frect = Rect::new(cx + inner - FORGET_W, y, FORGET_W, ROW_H);
                let flabel = cc_rasterize(text, "Unut", 13.0, fcol, (FORGET_W - 8.0) as usize);
                rows.push(BtRow {
                    rect,
                    action,
                    label,
                    meta,
                    connected: dev.connected,
                    forget: Some((frect, flabel)),
                });
                y += ROW_H + GAP;
            }
        }

        // --- Available section ---
        if powered {
            header(&mut rows, &mut y, "KULLANILABİLİR");
            for dev in &avail {
                let rect = Rect::new(cx, y, inner, ROW_H);
                let name = if dev.name.is_empty() { dev.mac.clone() } else { dev.name.clone() };
                let label = cc_rasterize(text, &name, 15.0, LABEL, lw);
                let meta = cc_rasterize(text, "Eşleştirmek için dokun", 12.0, SUB, lw);
                rows.push(BtRow {
                    rect,
                    action: BtAction::Pair(dev.mac.clone()),
                    label,
                    meta,
                    connected: false,
                    forget: None,
                });
                y += ROW_H + GAP;
            }
        }

        rows.push(BtRow {
            rect: Rect::new(cx, y, inner, ROW_H),
            action: BtAction::Close,
            label: cc_rasterize(text, "Kapat", 15.0, LABEL, lw),
            meta: None,
            connected: false,
            forget: None,
        });
        let devs = devices.clone();

        // Carry the scan deadline across relayouts; start a fresh ~12s window
        // when scanning begins.
        let scan_deadline = if scanning {
            self.bt_panel
                .as_ref()
                .and_then(|p| p.scan_deadline)
                .or_else(|| Some(Instant::now() + std::time::Duration::from_secs(12)))
        } else {
            None
        };
        self.bt_panel = Some(BtPanel {
            output: out,
            panel,
            title,
            status,
            rows,
            powered,
            switch_rect,
            devices: devs,
            scanning,
            scan_deadline,
            busy: None,
            dialog: None,
            dialog_title: None,
            dialog_body: None,
            pin_buf: String::new(),
            dlg_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            dlg_ok_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            dlg_cancel_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            dlg_ok_label: None,
            dlg_cancel_label: None,
        });
    }

    fn bt_set_status(&mut self, s: &str) {
        const SUB: [u8; 3] = [170, 178, 196];
        let inner = (380.0 - 2.0 * 16.0) as usize;
        let lbl = cc_rasterize(self.text.as_ref(), s, 12.0, SUB, inner);
        if let Some(p) = self.bt_panel.as_mut() {
            p.status = lbl;
        }
    }

    /// Handle a press while the Bluetooth panel is open.
    pub fn bt_panel_press(&mut self, px: f32, py: f32) -> bool {
        let Some(p) = self.bt_panel.as_ref() else {
            return false;
        };
        let has_dialog = p.dialog.is_some();
        let inside = px >= p.panel.x
            && px <= p.panel.x + p.panel.w
            && py >= p.panel.y
            && py <= p.panel.y + p.panel.h;
        // Safety against lock-ups: a tap OUTSIDE always dismisses the panel, even
        // with a dialog up (a stuck modal dialog must never swallow all input).
        // Exception: PIN/passkey entry, where an outside tap would lose typing.
        if !inside {
            let entry = matches!(
                p.dialog,
                Some(BtDialogKind::EnterPin | BtDialogKind::EnterPasskey)
            );
            if entry {
                return true; // keep the entry dialog, consume the tap
            }
            self.osk_hide();
            self.close_bt_panel();
            return true;
        }
        // Inside + a dialog → route to the dialog buttons.
        if has_dialog {
            return self.bt_dialog_press(px, py);
        }
        let out = p.output;
        let sw = p.switch_rect;
        if px >= sw.x && px <= sw.x + sw.w && py >= sw.y && py <= sw.y + sw.h {
            self.bt_toggle_power();
            return true;
        }
        // A tap on a paired row's right-edge "Unut" sub-rect forgets it.
        let forget_mac = p.rows.iter().find_map(|r| {
            let (fr, _) = r.forget.as_ref()?;
            let hit = px >= fr.x && px <= fr.x + fr.w && py >= fr.y && py <= fr.y + fr.h;
            match (&r.action, hit) {
                (BtAction::Connect(m) | BtAction::Disconnect(m), true) => Some(m.clone()),
                _ => None,
            }
        });
        if let Some(mac) = forget_mac {
            if let Some(bt) = self.btctl.as_ref() {
                bt.remove(&mac);
            }
            self.bt_paired.remove(&mac); // union won't re-add a forgotten device
            self.bt_set_status("Unutuldu");
            self.bt_refresh_devices();
            return true;
        }
        let action = p
            .rows
            .iter()
            .find(|r| {
                let g = r.rect;
                px >= g.x && px <= g.x + g.w && py >= g.y && py <= g.y + g.h
            })
            .map(|r| r.action.clone());
        match action {
            Some(BtAction::Close) => self.close_bt_panel(),
            Some(BtAction::Scan) => self.bt_start_scan(),
            Some(BtAction::Header) | Some(BtAction::Forget(_)) | None => {}
            Some(BtAction::Pair(mac)) => {
                // Freeze discovery so the list stops moving during pairing.
                self.bt_stop_scan();
                self.bt_last_pk = None; // fresh pairing → allow a new auto-confirm
                if let Some(bt) = self.btctl.as_ref() {
                    bt.pair(&mac);
                }
                if let Some(p) = self.bt_panel.as_mut() {
                    p.busy = Some(mac);
                }
                self.bt_set_status("Eşleştiriliyor…");
            }
            Some(BtAction::Connect(mac)) => {
                self.bt_stop_scan();
                if let Some(bt) = self.btctl.as_ref() {
                    bt.connect(&mac);
                }
                if let Some(p) = self.bt_panel.as_mut() {
                    p.busy = Some(mac);
                }
                self.bt_set_status("Bağlanıyor…");
            }
            Some(BtAction::Disconnect(mac)) => {
                if let Some(bt) = self.btctl.as_ref() {
                    bt.disconnect(&mac);
                }
                self.bt_set_status("Bağlantı kesiliyor…");
            }
            Some(BtAction::TogglePower) => self.bt_toggle_power(),
        }
        let _ = out;
        true
    }

    // ----- Audio device panel -------------------------------------------

    /// Build and open the audio output device picker.
    fn open_audio_panel(&mut self, out: OutputId) {
        self.wifi_panel = None;
        self.bt_panel = None;
        self.control_center = None;

        let bounds = self
            .wm
            .output(out)
            .map(|o| o.bounds)
            .unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0));

        const PAD: f32 = 16.0;
        const GAP: f32 = 10.0;
        const W: f32 = 380.0;
        const ROW_H: f32 = 52.0;
        const TITLE_H: f32 = 36.0;

        let text = self.text.as_ref();
        const LABEL: [u8; 3] = [232, 236, 244];
        const SUB: [u8; 3] = [170, 178, 196];

        // Fetch sinks; show status row if none found.
        let sinks = crate::controls::list_sinks();
        let status_text = if sinks.is_empty() { Some("Cihaz bulunamadı") } else { None };

        const CLOSE_H: f32 = 44.0;
        let row_count = sinks.len() as f32;
        let sink_block = if sinks.is_empty() {
            ROW_H  // "Cihaz bulunamadı" placeholder
        } else {
            row_count * ROW_H + (row_count - 1.0) * GAP
        };
        let panel_h = PAD + TITLE_H + GAP + sink_block + GAP + CLOSE_H + PAD;

        let m = 14.0;
        let bx = (bounds.x + bounds.w - W - m).max(bounds.x + m);
        let by = (bounds.y + m).max(bounds.y);
        let panel = Rect::new(bx, by, W, panel_h);

        let cx = bx + PAD;
        let inner = W - 2.0 * PAD;
        let title = cc_rasterize(text, "Ses Çıkış Cihazı", 15.0, LABEL, inner as usize);
        let status = status_text.and_then(|t| cc_rasterize(text, t, 14.0, SUB, inner as usize));

        let mut rows = Vec::new();
        let mut ry = by + PAD + TITLE_H + GAP;
        if sinks.is_empty() {
            // Placeholder row when PipeWire has no sinks.
            rows.push(AudioRow {
                rect: Rect::new(cx, ry, inner, ROW_H),
                action: AudioAction::Close,
                label: None,
                is_default: false,
            });
            ry += ROW_H + GAP;
        } else {
            for sink in &sinks {
                let rect = Rect::new(cx, ry, inner, ROW_H);
                let label = cc_rasterize(text, &sink.name, 15.0, LABEL, (inner - 40.0) as usize);
                rows.push(AudioRow {
                    rect,
                    action: AudioAction::SelectSink(sink.id.clone()),
                    label,
                    is_default: sink.is_default,
                });
                ry += ROW_H + GAP;
            }
        }
        // Close button always present at the bottom.
        rows.push(AudioRow {
            rect: Rect::new(cx, ry, inner, CLOSE_H),
            action: AudioAction::Close,
            label: cc_rasterize(text, "Kapat", 15.0, LABEL, inner as usize),
            is_default: false,
        });

        self.audio_panel = Some(AudioPanel { output: out, panel, title, rows, status });
    }

    /// Handle a pointer/touch press while the audio panel is open.
    pub fn audio_panel_press(&mut self, px: f32, py: f32) -> bool {
        let Some(p) = self.audio_panel.as_ref() else {
            return false;
        };
        let inside = px >= p.panel.x
            && px <= p.panel.x + p.panel.w
            && py >= p.panel.y
            && py <= p.panel.y + p.panel.h;
        if !inside {
            self.audio_panel = None;
            return true;
        }
        let action = p
            .rows
            .iter()
            .find(|r| {
                let r = r.rect;
                px >= r.x && px <= r.x + r.w && py >= r.y && py <= r.y + r.h
            })
            .map(|r| r.action.clone());
        match action {
            Some(AudioAction::SelectSink(id)) => {
                crate::controls::set_default_sink(&id);
                let out = self.audio_panel.as_ref().map(|p| p.output);
                self.audio_panel = None;
                // Reopen to show the updated default.
                if let Some(o) = out {
                    self.open_audio_panel(o);
                }
            }
            Some(AudioAction::SelectSource(_)) | Some(AudioAction::Close) | None => {
                self.audio_panel = None;
            }
        }
        true
    }

    fn open_mic_panel(&mut self, out: OutputId) {
        self.wifi_panel = None;
        self.bt_panel = None;
        self.control_center = None;
        self.audio_panel = None;

        let bounds = self
            .wm
            .output(out)
            .map(|o| o.bounds)
            .unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0));

        const PAD: f32 = 16.0;
        const GAP: f32 = 10.0;
        const W: f32 = 380.0;
        const ROW_H: f32 = 52.0;
        const TITLE_H: f32 = 36.0;
        const CLOSE_H: f32 = 44.0;

        let text = self.text.as_ref();
        const LABEL: [u8; 3] = [232, 236, 244];
        const SUB: [u8; 3] = [170, 178, 196];

        let sources = crate::controls::list_sources();
        let status_text = if sources.is_empty() { Some("Cihaz bulunamadı") } else { None };
        let status = status_text.and_then(|t| cc_rasterize(text, t, 14.0, SUB, (W - 2.0 * PAD) as usize));

        let sink_block = if sources.is_empty() {
            ROW_H
        } else {
            sources.len() as f32 * ROW_H + (sources.len() as f32 - 1.0) * GAP
        };
        let panel_h = PAD + TITLE_H + GAP + sink_block + GAP + CLOSE_H + PAD;

        let m = 14.0;
        let bx = (bounds.x + bounds.w - W - m).max(bounds.x + m);
        let by = (bounds.y + m).max(bounds.y);
        let panel = Rect::new(bx, by, W, panel_h);

        let cx = bx + PAD;
        let inner = W - 2.0 * PAD;
        let title = cc_rasterize(text, "Mikrofon Cihazı", 15.0, LABEL, inner as usize);

        let mut rows = Vec::new();
        let mut ry = by + PAD + TITLE_H + GAP;
        if sources.is_empty() {
            rows.push(AudioRow {
                rect: Rect::new(cx, ry, inner, ROW_H),
                action: AudioAction::Close,
                label: None,
                is_default: false,
            });
            ry += ROW_H + GAP;
        } else {
            for src in &sources {
                let label = cc_rasterize(text, &src.name, 15.0, LABEL, (inner - 40.0) as usize);
                rows.push(AudioRow {
                    rect: Rect::new(cx, ry, inner, ROW_H),
                    action: AudioAction::SelectSource(src.id.clone()),
                    label,
                    is_default: src.is_default,
                });
                ry += ROW_H + GAP;
            }
        }
        rows.push(AudioRow {
            rect: Rect::new(cx, ry, inner, CLOSE_H),
            action: AudioAction::Close,
            label: cc_rasterize(text, "Kapat", 15.0, LABEL, inner as usize),
            is_default: false,
        });

        self.mic_panel = Some(AudioPanel { output: out, panel, title, rows, status });
    }

    pub fn mic_panel_press(&mut self, px: f32, py: f32) -> bool {
        let Some(p) = self.mic_panel.as_ref() else { return false };
        let inside = px >= p.panel.x
            && px <= p.panel.x + p.panel.w
            && py >= p.panel.y
            && py <= p.panel.y + p.panel.h;
        if !inside {
            self.mic_panel = None;
            return true;
        }
        let action = p
            .rows
            .iter()
            .find(|r| {
                let r = r.rect;
                px >= r.x && px <= r.x + r.w && py >= r.y && py <= r.y + r.h
            })
            .map(|r| r.action.clone());
        match action {
            Some(AudioAction::SelectSource(id)) => {
                crate::controls::set_default_source(&id);
                let out = self.mic_panel.as_ref().map(|p| p.output);
                self.mic_panel = None;
                if let Some(o) = out {
                    self.open_mic_panel(o);
                }
            }
            Some(AudioAction::SelectSink(_)) | Some(AudioAction::Close) | None => {
                self.mic_panel = None;
            }
        }
        true
    }

    fn bt_toggle_power(&mut self) {
        let Some(out) = self.bt_panel.as_ref().map(|p| p.output) else { return };
        let on = self.bt_panel.as_ref().map(|p| p.powered).unwrap_or(false);
        let new_on = !on;
        crate::controls::set_bt(new_on);
        if let Some(bt) = self.btctl.as_ref() {
            bt.power(new_on);
            if new_on {
                bt.send("pairable on");
                bt.send("discoverable on");
            } else {
                bt.send("discoverable off");
                bt.scan(false);
            }
        }
        // Do NOT auto-scan on power-on: the adapter needs ~100 ms to come up, so
        // an immediate `scan on` fails with org.bluez.Error.NotReady. Show the
        // known (paired) devices; the user taps "Cihazları Tara" to discover new.
        if new_on {
            let devices = self.bt_known_devices();
            self.build_bt_panel(out, devices, true, false);
        } else {
            self.build_bt_panel(out, Vec::new(), false, false);
        }
    }

    /// Manual "Cihazları Tara" button: start a fresh discovery (the adapter is
    /// already powered+ready by the time the user taps this).
    fn bt_start_scan(&mut self) {
        let Some(out) = self.bt_panel.as_ref().map(|p| p.output) else { return };
        let devices = self.bt_panel.as_ref().map(|p| p.devices.clone()).unwrap_or_default();
        if let Some(bt) = self.btctl.as_ref() {
            bt.scan(true);
        }
        self.build_bt_panel(out, devices, true, true);
    }

    fn close_bt_panel(&mut self) {
        self.bt_panel = None;
        self.bt_last_pk = None;
        if let Some(bt) = self.btctl.as_ref() {
            bt.scan(false);
            bt.send("discoverable off"); // stop advertising once the panel closes
        }
    }

    /// Stop discovery and freeze the list (so rows don't move under the finger).
    fn bt_stop_scan(&mut self) {
        if let Some(bt) = self.btctl.as_ref() {
            bt.scan(false);
        }
        let Some(out) = self.bt_panel.as_ref().map(|p| p.output) else { return };
        let devices = self.bt_panel.as_ref().map(|p| p.devices.clone()).unwrap_or_default();
        let powered = self.bt_panel.as_ref().map(|p| p.powered).unwrap_or(false);
        self.build_bt_panel(out, devices, powered, false);
    }

    /// Show a pairing-agent dialog (passkey confirm / display / PIN entry).
    fn bt_open_dialog(&mut self, kind: BtDialogKind) {
        const LABEL: [u8; 3] = [232, 236, 244];
        const SUB: [u8; 3] = [170, 178, 196];
        // Single-line body (cc_rasterize is one line — no '\n').
        let (title, body) = match &kind {
            BtDialogKind::ConfirmPasskey(code) => (
                "Eşleşme isteği".to_string(),
                if code.is_empty() {
                    "Bu cihazla eşleşilsin mi?".to_string()
                } else {
                    format!("Kod: {code}  — kabul edilsin mi?")
                },
            ),
            BtDialogKind::DisplayPasskey(code) => {
                ("Eşleştirme".to_string(), format!("Diğer cihaza girin: {code}"))
            }
            BtDialogKind::EnterPin => ("PIN gerekli".to_string(), "Cihazın PIN kodunu girin".to_string()),
            BtDialogKind::EnterPasskey => {
                ("Passkey gerekli".to_string(), "Cihazın passkey'ini girin".to_string())
            }
        };
        let ok_text = match &kind {
            BtDialogKind::ConfirmPasskey(_) => "Kabul Et",
            BtDialogKind::DisplayPasskey(_) => "Tamam",
            _ => "Gönder",
        };
        let text = self.text.as_ref();
        let dt = cc_rasterize(text, &title, 16.0, LABEL, 320);
        let db = cc_rasterize(text, &body, 14.0, SUB, 320);
        let ok_l = cc_rasterize(text, ok_text, 15.0, LABEL, 150);
        let cancel_text = match &kind {
            BtDialogKind::ConfirmPasskey(_) => "Reddet",
            _ => "İptal",
        };
        let cancel_l = cc_rasterize(text, cancel_text, 15.0, LABEL, 150);

        let out = self.bt_panel.as_ref().map(|p| p.panel);
        let entry = matches!(kind, BtDialogKind::EnterPin | BtDialogKind::EnterPasskey);
        if let (Some(panel), Some(p)) = (out, self.bt_panel.as_mut()) {
            // Dialog box anchored just under the header (always on-screen) — the
            // buttons live INSIDE it, so render and hit-test agree.
            const DW_PAD: f32 = 12.0;
            const DH: f32 = 196.0;
            let dx = panel.x + DW_PAD;
            let dy = panel.y + 56.0;
            let dw = panel.w - 2.0 * DW_PAD;
            let bw = (dw - 3.0 * 16.0) / 2.0;
            let bh = 52.0;
            let by = dy + DH - 16.0 - bh;
            p.dlg_rect = Rect::new(dx, dy, dw, DH);
            p.dlg_ok_rect = Rect::new(dx + 16.0, by, bw, bh);
            p.dlg_cancel_rect = Rect::new(dx + 16.0 + bw + 16.0, by, bw, bh);
            p.dialog = Some(kind);
            p.dialog_title = dt;
            p.dialog_body = db;
            p.dlg_ok_label = ok_l;
            p.dlg_cancel_label = cancel_l;
            p.pin_buf.clear();
        }
        // PIN/passkey entry pops the on-screen keyboard.
        if entry {
            if let Some(o) = self.bt_panel.as_ref().map(|p| p.output) {
                self.open_wifi_keyboard(o); // generic text-mode OSK
            }
        }
        self.osk_dirty = true;
    }

    /// Press handler while a pairing dialog is shown.
    fn bt_dialog_press(&mut self, px: f32, py: f32) -> bool {
        let Some(p) = self.bt_panel.as_ref() else { return false };
        let (ok, cancel) = (p.dlg_ok_rect, p.dlg_cancel_rect);
        let entry = matches!(p.dialog, Some(BtDialogKind::EnterPin | BtDialogKind::EnterPasskey));
        let hit_ok = px >= ok.x && px <= ok.x + ok.w && py >= ok.y && py <= ok.y + ok.h;
        let hit_cancel = px >= cancel.x && px <= cancel.x + cancel.w && py >= cancel.y && py <= cancel.y + cancel.h;
        if hit_ok {
            if entry {
                let pin = self.bt_panel.as_ref().map(|p| p.pin_buf.clone()).unwrap_or_default();
                if let Some(bt) = self.btctl.as_ref() {
                    bt.agent_value(&pin);
                }
                self.osk_hide();
            } else {
                // Accept → answer yes; stay frozen (busy) until BlueZ reports
                // Paired (the busy poll then refreshes + shows the device).
                if let Some(bt) = self.btctl.as_ref() {
                    bt.agent_yes(true);
                }
                self.bt_set_status("Eşleşiyor…");
            }
            self.bt_close_dialog();
        } else if hit_cancel {
            if entry {
                self.osk_hide();
            }
            if let Some(bt) = self.btctl.as_ref() {
                bt.agent_yes(false);
            }
            self.bt_close_dialog();
            // Reject → unfreeze and return to the list.
            let out = self.bt_panel.as_ref().map(|p| p.output);
            let devices = self.bt_panel.as_ref().map(|p| p.devices.clone()).unwrap_or_default();
            if let Some(p) = self.bt_panel.as_mut() {
                p.busy = None;
            }
            if let Some(out) = out {
                self.build_bt_panel(out, devices, true, false);
                self.bt_set_status("Reddedildi");
            }
        }
        true
    }

    fn bt_close_dialog(&mut self) {
        self.osk_dirty = true;
        if let Some(p) = self.bt_panel.as_mut() {
            p.dialog = None;
            p.dialog_title = None;
            p.dialog_body = None;
            p.pin_buf.clear();
        }
    }

    /// `true` while a BT PIN/passkey dialog is capturing keyboard text.
    pub fn bt_pin_active(&self) -> bool {
        self.bt_panel
            .as_ref()
            .map(|p| matches!(p.dialog, Some(BtDialogKind::EnterPin | BtDialogKind::EnterPasskey)))
            .unwrap_or(false)
    }

    fn bt_pin_refresh(&mut self) {
        let n = self.bt_panel.as_ref().map(|p| p.pin_buf.chars().count()).unwrap_or(0);
        self.bt_set_status(&"•".repeat(n));
    }

    /// Route an OSK / physical key into the BT PIN entry. Returns consumed.
    pub fn bt_pin_key(&mut self, raw: u32, utf8: &str) -> bool {
        if !self.bt_pin_active() {
            return false;
        }
        const KEY_BACKSPACE: u32 = 0xff08;
        const KEY_RETURN: u32 = 0xff0d;
        const KEY_KP_ENTER: u32 = 0xff8d;
        const KEY_ESC: u32 = 0xff1b;
        match raw {
            KEY_BACKSPACE => {
                if let Some(p) = self.bt_panel.as_mut() {
                    p.pin_buf.pop();
                }
                self.bt_pin_refresh();
                true
            }
            KEY_RETURN | KEY_KP_ENTER => {
                let pin = self.bt_panel.as_ref().map(|p| p.pin_buf.clone()).unwrap_or_default();
                if let Some(bt) = self.btctl.as_ref() {
                    bt.agent_value(&pin);
                }
                self.osk_hide();
                self.bt_close_dialog();
                true
            }
            KEY_ESC => {
                if let Some(bt) = self.btctl.as_ref() {
                    bt.agent_yes(false);
                }
                self.osk_hide();
                self.bt_close_dialog();
                true
            }
            _ => {
                if let Some(c) = utf8.chars().next() {
                    if !c.is_control() {
                        if let Some(p) = self.bt_panel.as_mut() {
                            p.pin_buf.push(c);
                        }
                        self.bt_pin_refresh();
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Drain pending `bluetoothctl` events into the panel. Returns whether
    /// anything changed (caller redraws). Called each tick while the panel is open.
    pub fn bt_poll(&mut self) -> bool {
        if self.bt_panel.is_none() {
            return false;
        }
        use crate::bluetooth::BtEvent;

        // While a pairing is in flight (busy) the list is FROZEN — no relayout.
        // The passkey numeric-comparison is **auto-confirmed**: both devices
        // computed the same code and the user already initiated from the trusted
        // Control Center, so we answer "yes" automatically (the interactive
        // confirm tap proved unreliable to deliver during the BlueZ exchange) and
        // surface the code in the status line for transparency.
        let busy = self.bt_panel.as_ref().and_then(|p| p.busy.clone());
        if busy.is_some() {
            let mut done: Option<String> = None;
            let mut status: Option<String> = None;
            let mut confirm: Option<String> = None; // distinct passkey to auto-yes
            let mut newly_paired: Option<String> = None;
            let mut devices = self.bt_panel.as_ref().map(|p| p.devices.clone()).unwrap_or_default();
            let last_pk = self.bt_last_pk.clone();
            if let Some(bt) = self.btctl.as_ref() {
                while let Ok(ev) = bt.rx.try_recv() {
                    match ev {
                        BtEvent::ConfirmPasskey { passkey, .. }
                            if last_pk.as_deref() != Some(passkey.as_str()) =>
                        {
                            confirm = Some(passkey);
                        }
                        BtEvent::DisplayPasskey { passkey, .. } => {
                            status = Some(format!("Diğer cihaza girin: {passkey}"));
                        }
                        BtEvent::Paired { mac, paired: true } => {
                            if let Some(d) = devices.iter_mut().find(|d| d.mac == mac) {
                                d.paired = true;
                            }
                            bt.trust(&mac);
                            bt.connect(&mac);
                            newly_paired = Some(mac);
                            // Pairing itself is the success milestone — finish here
                            // (a phone connect can re-trigger a handshake; we don't
                            // want to stay frozen waiting on it).
                            done = Some("Eşleşti".into());
                        }
                        BtEvent::Connected { mac, connected: true } => {
                            let paired =
                                devices.iter().find(|d| d.mac == mac).map(|d| d.paired).unwrap_or(false);
                            if let Some(d) = devices.iter_mut().find(|d| d.mac == mac) {
                                d.connected = true;
                            }
                            if paired {
                                done = Some("Bağlandı".into());
                            }
                        }
                        BtEvent::Failed(msg) => {
                            done = Some(if msg.contains("AlreadyExists") {
                                "Zaten eşleşmiş".into()
                            } else {
                                format!("Başarısız: {msg}")
                            });
                        }
                        _ => {} // ignore device churn
                    }
                }
            }
            if let Some(passkey) = confirm {
                if let Some(bt) = self.btctl.as_ref() {
                    bt.agent_yes(true);
                }
                self.bt_last_pk = Some(passkey.clone());
                self.bt_set_status(&format!("Eşleşiyor… (kod {passkey})"));
            } else if let Some(status) = status {
                self.bt_set_status(&status);
            }
            if let Some(mac) = newly_paired {
                self.bt_paired.insert(mac);
            }
            let _ = devices;
            if let Some(status) = done {
                if let Some(p) = self.bt_panel.as_mut() {
                    p.busy = None;
                }
                // Re-query BlueZ: a phone/earbuds advertises a random MAC but
                // bonds under its *identity* address, so the just-paired device
                // would be invisible if we trusted the stale scan list.
                self.bt_refresh_devices();
                self.bt_set_status(&status);
            }
            return true; // keep polling for the pairing result
        }

        // Auto-stop scanning once the window elapses so the list settles.
        let expired = self
            .bt_panel
            .as_ref()
            .map(|p| p.scanning && p.scan_deadline.is_some_and(|d| Instant::now() >= d))
            .unwrap_or(false);
        if expired {
            self.bt_stop_scan();
            return true;
        }
        let mut events = Vec::new();
        if let Some(bt) = self.btctl.as_ref() {
            while let Ok(ev) = bt.rx.try_recv() {
                events.push(ev);
            }
        }
        if events.is_empty() {
            return false;
        }
        let mut devices = self.bt_panel.as_ref().map(|p| p.devices.clone()).unwrap_or_default();
        let mut powered = self.bt_panel.as_ref().map(|p| p.powered).unwrap_or(false);
        let busy = self.bt_panel.as_ref().and_then(|p| p.busy.clone());
        let mut dialog: Option<BtDialogKind> = None;
        let mut relayout = false;
        let mut status: Option<String> = None;
        let mut confirm: Option<String> = None; // distinct passkey to auto-yes
        let mut paired_now: Option<String> = None; // a pairing completed (e.g. incoming)
        let last_pk = self.bt_last_pk.clone();
        let paired_set = self.bt_paired.clone();
        for ev in events {
            match ev {
                BtEvent::Powered(on) => {
                    powered = on;
                    relayout = true;
                }
                BtEvent::Discovering(_) => {}
                BtEvent::Device { mac, name } => {
                    if let Some(d) = devices.iter_mut().find(|d| d.mac == mac) {
                        // Don't overwrite a real name with the dashed-MAC fallback.
                        if !name.is_empty() && name != mac.replace(':', "-") {
                            d.name = name;
                        }
                    } else {
                        devices.push(BtDevice { mac, name, paired: false, connected: false });
                    }
                    relayout = true;
                }
                BtEvent::Connected { mac, connected } => {
                    if let Some(d) = devices.iter_mut().find(|d| d.mac == mac) {
                        d.connected = connected;
                    }
                    status = Some(if connected { "Bağlandı".into() } else { "Bağlantı kesildi".into() });
                    relayout = true;
                }
                BtEvent::Paired { mac, paired } => {
                    if let Some(d) = devices.iter_mut().find(|d| d.mac == mac) {
                        d.paired = paired;
                    }
                    if paired {
                        // Incoming (phone-initiated) pairing or a late event.
                        if let Some(bt) = self.btctl.as_ref() {
                            bt.trust(&mac);
                        }
                        paired_now = Some(mac);
                        status = Some("Eşleşti".into());
                    }
                    relayout = true;
                }
                BtEvent::Removed { mac } => {
                    // Keep bonded devices in the list — a phone/earbuds emits a
                    // [DEL] when it stops advertising after pairing, but it's
                    // still paired and must stay in the "Eşleşmiş" section.
                    if !paired_set.contains(&mac) {
                        devices.retain(|d| d.mac != mac);
                    }
                    relayout = true;
                }
                BtEvent::ConfirmPasskey { passkey, .. } => {
                    // Auto-confirm (the interactive tap can't be delivered during
                    // the BlueZ exchange — see the busy path); dedup by value.
                    if last_pk.as_deref() != Some(passkey.as_str()) {
                        confirm = Some(passkey);
                    }
                }
                BtEvent::DisplayPasskey { passkey, .. } => {
                    status = Some(format!("Diğer cihaza girin: {passkey}"));
                }
                // PIN/passkey *entry* devices still use a typed dialog.
                BtEvent::RequestPin { .. } => dialog = Some(BtDialogKind::EnterPin),
                BtEvent::RequestPasskey { .. } => dialog = Some(BtDialogKind::EnterPasskey),
                BtEvent::Failed(msg) => {
                    status = Some(format!("Başarısız: {msg}"));
                }
            }
        }
        let _ = busy;
        // A pairing completed (often phone-initiated, under a different identity
        // MAC) — re-query BlueZ so the bonded device appears in the paired list.
        if let Some(mac) = paired_now {
            self.bt_paired.insert(mac);
            self.bt_refresh_devices();
            if let Some(s) = status {
                self.bt_set_status(&s);
            }
            return true;
        }
        if let Some(out) = self.bt_panel.as_ref().map(|p| p.output) {
            if relayout {
                let scanning = self.bt_panel.as_ref().map(|p| p.scanning).unwrap_or(powered);
                let had_dialog = self.bt_panel.as_ref().and_then(|p| p.dialog.clone());
                self.build_bt_panel(out, devices, powered, scanning);
                // Preserve an open dialog across a relayout.
                if dialog.is_none() {
                    if let Some(k) = had_dialog {
                        if let Some(p) = self.bt_panel.as_mut() {
                            p.dialog = Some(k);
                        }
                    }
                }
            }
        }
        if let Some(passkey) = confirm {
            // Incoming pairing request → auto-confirm. (A modal accept dialog
            // can't reliably receive its tap during the BlueZ exchange and would
            // lock the whole UI; we're only discoverable while the panel is open,
            // which the user deliberately opened to pair, so this is consented.)
            if let Some(bt) = self.btctl.as_ref() {
                bt.agent_yes(true);
            }
            self.bt_last_pk = Some(passkey.clone());
            self.bt_set_status(&format!("Eşleşme isteği kabul edildi (kod {passkey})"));
        } else if let Some(s) = status {
            self.bt_set_status(&s);
        }
        if let Some(k) = dialog {
            // bluetoothctl re-prints the same prompt after every event line, so
            // only (re)open the dialog when it isn't already showing the same one
            // (avoids flicker + clearing a half-typed PIN).
            let already = self.bt_panel.as_ref().and_then(|p| p.dialog.clone());
            let same = matches!(
                (&already, &k),
                (Some(BtDialogKind::ConfirmPasskey(a)), BtDialogKind::ConfirmPasskey(b)) if a == b
            ) || matches!(
                (&already, &k),
                (Some(BtDialogKind::DisplayPasskey(a)), BtDialogKind::DisplayPasskey(b)) if a == b
            ) || matches!(
                (&already, &k),
                (Some(BtDialogKind::EnterPin), BtDialogKind::EnterPin)
                    | (Some(BtDialogKind::EnterPasskey), BtDialogKind::EnterPasskey)
            );
            if !same {
                self.bt_open_dialog(k);
            }
        }
        true
    }

    /// Kick off a background `nmcli` connect (it blocks for seconds) and flag
    /// the panel as connecting so [`Self::tick_animations`] keeps redrawing
    /// until the worker reports back.
    fn wifi_start_connect(&mut self, ssid: String, password: Option<String>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let ssid_c = ssid.clone();
        std::thread::spawn(move || {
            let (ok, msg) = crate::controls::wifi_connect(&ssid_c, password.as_deref());
            let _ = tx.send(WifiMsg::Connected { ssid: ssid_c, ok, msg });
        });
        self.wifi_rx = Some(rx);
        if let Some(p) = self.wifi_panel.as_mut() {
            p.connecting = Some(ssid.clone());
        }
        self.wifi_set_status(&format!("Bağlanıyor: {ssid}…"));
    }

    /// Handle a press while the Wi-Fi picker is open. Tap a network → connect;
    /// tap the off button → radio off + close; tap outside → close. Returns
    /// `false` only when no picker is open (so the Control Center can try next).
    pub fn wifi_panel_press(&mut self, px: f32, py: f32) -> bool {
        let Some(p) = self.wifi_panel.as_ref() else {
            return false;
        };
        let inside = px >= p.panel.x
            && px <= p.panel.x + p.panel.w
            && py >= p.panel.y
            && py <= p.panel.y + p.panel.h;
        // A tap outside dismisses the panel — EXCEPT while typing (password /
        // static IP), where a stray tap would discard the entry; there the user
        // uses Cancel. (Dismissing on outside-tap is essential: the panel is
        // modal, so leaving it stuck would swallow every dock / app click.)
        let text_entry = p.pw_for.is_some() || p.static_entry.is_some();
        if !inside {
            if !text_entry {
                self.wifi_panel = None;
                self.wifi_rx = None;
            }
            return true;
        }
        // Ignore taps while a connect is in flight.
        if p.connecting.is_some() {
            return true;
        }
        let out = p.output;
        let pw_field = p.pw_field;
        let sw = p.switch_rect;
        let in_pw = p.pw_for.is_some();
        let hit = p
            .rows
            .iter()
            .find(|r| {
                let g = r.rect;
                px >= g.x && px <= g.x + g.w && py >= g.y && py <= g.y + g.h
            })
            .map(|r| r.action.clone());
        // Header on/off switch (list mode only).
        if !in_pw && px >= sw.x && px <= sw.x + sw.w && py >= sw.y && py <= sw.y + sw.h {
            self.wifi_toggle_radio();
            return true;
        }
        // Reveal (eye) button: the right end of the password box (password mode).
        if in_pw {
            if let Some(f) = pw_field {
                let ex = f.x + f.w - WIFI_EYE_W;
                if px >= ex && px <= f.x + f.w && py >= f.y && py <= f.y + f.h {
                    self.wifi_pw_toggle_show();
                    return true;
                }
            }
        }
        match hit {
            Some(WifiAction::ClosePanel) => {
                self.wifi_panel = None;
                self.wifi_rx = None;
            }
            Some(WifiAction::Connect { ssid, secured }) => {
                if secured {
                    self.enter_wifi_password(out, ssid);
                } else {
                    self.wifi_start_connect(ssid, None);
                }
            }
            Some(WifiAction::PwConnect) => self.wifi_pw_submit(),
            Some(WifiAction::PwCancel) => self.wifi_pw_cancel(out),
            Some(WifiAction::ToggleRadio) => {} // handled by the switch hit-test above
            Some(WifiAction::OpenDetails) => self.enter_net_details(out, false),
            Some(WifiAction::ToggleAutoconnect) => {
                let eth = self.wifi_panel.as_ref().map(|p| p.eth).unwrap_or(false);
                if let Some(mut d) = self.wifi_panel.as_ref().and_then(|p| p.details.clone()) {
                    let conn = d.conn.clone();
                    let new_ac = !d.autoconnect;
                    std::thread::spawn(move || crate::controls::wifi_set_autoconnect(&conn, new_ac));
                    d.autoconnect = new_ac; // optimistic
                    self.build_wifi_details_panel(out, d, false, eth);
                }
            }
            Some(WifiAction::ToggleLink) => {
                // Ethernet link on/off via nmcli connect/disconnect (blocks for
                // seconds), then re-read and refresh the details.
                if let Some(d) = self.wifi_panel.as_ref().and_then(|p| p.details.clone()) {
                    let on = !d.connected;
                    self.build_wifi_details_panel(out, d, true, true); // "Yükleniyor…"
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        crate::controls::ethernet_set_link(on);
                        let _ = tx.send(WifiMsg::Details(
                            crate::controls::net_details(true).unwrap_or_default(),
                        ));
                    });
                    self.wifi_rx = Some(rx);
                }
            }
            Some(WifiAction::SetDhcp) => {
                let eth = self.wifi_panel.as_ref().map(|p| p.eth).unwrap_or(false);
                if let Some(d) = self.wifi_panel.as_ref().and_then(|p| p.details.clone()) {
                    let conn = d.conn.clone();
                    self.build_wifi_details_panel(out, d, true, eth); // "Yükleniyor…"
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        crate::controls::wifi_set_dhcp(&conn);
                        let _ = tx.send(WifiMsg::Details(
                            crate::controls::net_details(eth).unwrap_or_default(),
                        ));
                    });
                    self.wifi_rx = Some(rx);
                }
            }
            Some(WifiAction::SetStatic) => self.enter_wifi_static(out),
            Some(WifiAction::StaticNext) => self.wifi_static_next(),
            Some(WifiAction::StaticCancel) => self.wifi_static_cancel(out),
            Some(WifiAction::Back) => {
                // Ethernet has no network list, so Back just closes the panel.
                if self.wifi_panel.as_ref().is_some_and(|p| p.eth) {
                    self.wifi_panel = None;
                    self.wifi_rx = None;
                } else {
                    let nets = self.wifi_panel.as_ref().map(|p| p.nets.clone()).unwrap_or_default();
                    self.build_wifi_panel(out, nets, None, false, true);
                }
            }
            Some(WifiAction::Info) => {} // read-only
            None => {} // tapped padding — stay open
        }
        true
    }

    /// Handle a left press while the Control Center is open. A click on a
    /// tile performs its action (toggle / set slider level / power); a
    /// click anywhere outside the panel closes it. Always consumes the
    /// press; returns `false` only when no Control Center is open.
    pub fn control_center_left_press(&mut self, px: f32, py: f32) -> bool {
        let Some(cc) = self.control_center.as_ref() else {
            return false;
        };
        let inside = px >= cc.panel.x
            && px <= cc.panel.x + cc.panel.w
            && py >= cc.panel.y
            && py <= cc.panel.y + cc.panel.h;
        if !inside {
            self.control_center = None;
            return true;
        }
        let Some((action, rect)) = cc
            .tiles
            .iter()
            .find(|t| {
                let r = t.rect;
                px >= r.x && px <= r.x + r.w && py >= r.y && py <= r.y + r.h
            })
            .map(|t| (t.action, t.rect))
        else {
            return true; // clicked panel padding → stay open, consume
        };

        // Slider level from the click's x within the inset track.
        let slider_level = || {
            let track = (rect.w - 2.0 * CC_SLIDER_INSET).max(1.0);
            ((px - rect.x - CC_SLIDER_INSET) / track).clamp(0.0, 1.0)
        };

        match action {
            CcAction::WifiToggle => {
                // Tap the tile → open the Android-style panel (on/off switch +
                // list). The radio toggle lives on the panel's header switch.
                if let Some(out) = self.control_center.as_ref().map(|c| c.output) {
                    self.control_center = None;
                    self.open_wifi_panel(out);
                }
            }
            CcAction::BtToggle => {
                // Tap the tile → open the Bluetooth panel (list + on/off switch),
                // like Wi-Fi. The radio toggle lives on the panel header switch.
                if let Some(out) = self.control_center.as_ref().map(|c| c.output) {
                    self.control_center = None;
                    self.open_bt_panel(out);
                }
            }
            CcAction::EthernetSettings => {
                // Open the Ethernet settings panel (manage + details, like Wi-Fi).
                if let Some(out) = self.control_center.as_ref().map(|c| c.output) {
                    self.control_center = None;
                    self.enter_net_details(out, true);
                }
            }
            CcAction::DarkToggle => {
                self.dark_mode = !self.dark_mode;
            }
            CcAction::Volume => {
                let level = slider_level();
                if let Some(cc) = self.control_center.as_mut() {
                    cc.volume = level;
                    cc.muted = level <= 0.001;
                }
                crate::controls::set_volume(level);
            }
            CcAction::MicVolume => {
                let level = slider_level();
                if let Some(cc) = self.control_center.as_mut() {
                    cc.mic_volume = level;
                }
                crate::controls::set_mic_volume(level);
            }
            CcAction::Brightness => {
                let level = slider_level();
                self.brightness = MIN_BRIGHTNESS + (1.0 - MIN_BRIGHTNESS) * level;
            }
            CcAction::AudioSettings => {
                if let Some(out) = self.control_center.as_ref().map(|c| c.output) {
                    self.control_center = None;
                    self.open_audio_panel(out);
                }
            }
            CcAction::MicSettings => {
                if let Some(out) = self.control_center.as_ref().map(|c| c.output) {
                    self.control_center = None;
                    self.open_mic_panel(out);
                }
            }
            CcAction::Screenshot => {
                // Open the options dialog (scope + delay) instead of grabbing
                // immediately. Close the Control Center behind it.
                let out = self.control_center.as_ref().map(|c| c.output);
                self.control_center = None;
                if let Some(output) = out {
                    self.open_shot_dialog(output);
                }
            }
            CcAction::DesktopSettings => {
                if let Some(out) = self.control_center.as_ref().map(|c| c.output) {
                    self.control_center = None;
                    self.open_desktop_settings(out);
                }
            }
            CcAction::PowerOff => crate::controls::power_off(),
            CcAction::Reboot => crate::controls::reboot(),
            CcAction::Logout => crate::controls::logout(),
        }
        true
    }

    /// The most-recently-focused live window — the "active window" the
    /// screenshot dialog captures.
    pub(crate) fn active_window(&self) -> Option<WindowId> {
        let wm = &self.wm;
        self.focus_history.most_recent_matching(|id| wm.get(id).is_ok())
    }

    /// Build the screenshot options dialog (scope + delay), centred on `out`.
    fn open_shot_dialog(&mut self, out: OutputId) {
        self.apps_menu = None;
        self.control_center = None;
        self.close_overview();
        let bounds = self
            .wm
            .output(out)
            .map(|o| o.bounds)
            .unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0));

        const PAD: f32 = 18.0;
        const GAP: f32 = 10.0;
        const W: f32 = 360.0;
        const TH: f32 = 28.0; // title
        const MH: f32 = 50.0; // mode pill
        const DLH: f32 = 18.0; // "Gecikme" label
        const DH: f32 = 42.0; // delay pill
        const AH: f32 = 50.0; // action button
        let inner = W - 2.0 * PAD;
        let panel_h = PAD + TH + GAP + MH + GAP + DLH + 6.0 + DH + GAP + AH + PAD;
        let bx = bounds.x + (bounds.w - W) / 2.0;
        let by = bounds.y + (bounds.h - panel_h) / 2.0;
        let panel = Rect::new(bx, by, W, panel_h);

        let cx = bx + PAD;
        let mut y = by + PAD + TH + GAP;
        let mw = (inner - GAP) / 2.0;
        let mode_whole = Rect::new(cx, y, mw, MH);
        let mode_window = Rect::new(cx + mw + GAP, y, mw, MH);
        y += MH + GAP + DLH + 6.0;
        let dw = (inner - 3.0 * GAP) / 4.0;
        let secs = [0u64, 3, 5, 10];
        let delays: Vec<(Rect, u64)> = secs
            .iter()
            .enumerate()
            .map(|(i, &s)| (Rect::new(cx + (dw + GAP) * i as f32, y, dw, DH), s))
            .collect();
        y += DH + GAP;
        let aw = (inner - GAP) / 2.0;
        let cancel = Rect::new(cx, y, aw, AH);
        let capture = Rect::new(cx + aw + GAP, y, aw, AH);

        let text = self.text.as_ref();
        const LABEL: [u8; 3] = [232, 236, 244];
        const SUB: [u8; 3] = [170, 178, 196];
        let delay_lbls = ["Hemen", "3 sn", "5 sn", "10 sn"]
            .iter()
            .map(|s| cc_rasterize(text, s, 13.0, LABEL, (dw - 6.0) as usize))
            .collect();

        self.shot_dialog = Some(ShotDialog {
            output: out,
            panel,
            mode_whole,
            mode_window,
            delays,
            capture,
            cancel,
            sel_window: false,
            sel_delay: 0,
            title: cc_rasterize(text, "Ekran Görüntüsü", 17.0, LABEL, inner as usize),
            l_whole: cc_rasterize(text, "Tüm Ekran", 14.0, LABEL, (mw - 12.0) as usize),
            l_window: cc_rasterize(text, "Aktif Pencere", 14.0, LABEL, (mw - 12.0) as usize),
            l_delay: cc_rasterize(text, "Gecikme", 12.0, SUB, inner as usize),
            delay_lbls,
            l_capture: cc_rasterize(text, "❏  Çek", 15.0, LABEL, (aw - 12.0) as usize),
            l_cancel: cc_rasterize(text, "İptal", 14.0, LABEL, (aw - 12.0) as usize),
        });
    }

    /// Handle a left press while the screenshot dialog is open. Selecting a
    /// scope / delay highlights it; "Çek" schedules the capture (after the
    /// chosen delay) and closes; "İptal" or a click outside closes. Always
    /// consumes; returns `false` only when no dialog is open.
    pub fn shot_dialog_press(&mut self, px: f32, py: f32) -> bool {
        let Some(d) = self.shot_dialog.as_ref() else { return false };
        let panel = d.panel;
        let (mode_whole, mode_window, cancel, capture) =
            (d.mode_whole, d.mode_window, d.cancel, d.capture);
        let delays = d.delays.clone();
        let (output, sel_window, sel_delay) = (d.output, d.sel_window, d.sel_delay);
        // `d` borrow ends here.

        let hit = |r: Rect| px >= r.x && px <= r.x + r.w && py >= r.y && py <= r.y + r.h;
        if !hit(panel) {
            self.shot_dialog = None;
            return true;
        }
        if hit(mode_whole) {
            if let Some(m) = self.shot_dialog.as_mut() { m.sel_window = false; }
            return true;
        }
        if hit(mode_window) {
            if let Some(m) = self.shot_dialog.as_mut() { m.sel_window = true; }
            return true;
        }
        for (r, s) in delays {
            if hit(r) {
                if let Some(m) = self.shot_dialog.as_mut() { m.sel_delay = s; }
                return true;
            }
        }
        if hit(cancel) {
            self.shot_dialog = None;
            return true;
        }
        if hit(capture) {
            let window = if sel_window { self.active_window() } else { None };
            self.shot_dialog = None;
            let req = ScreenshotReq { output, region: None, window };
            if sel_delay == 0 {
                self.pending_screenshot = Some(req);
            } else {
                self.pending_shot = Some((Instant::now() + Duration::from_secs(sel_delay), req));
            }
            return true;
        }
        true // padding inside the panel
    }

    /// Show a transient two-line notification banner (top-centre). The panel
    /// auto-sizes to the wider of the two pre-rasterised lines.
    pub fn show_toast(&mut self, output: OutputId, title: &str, sub: &str) {
        let text = self.text.as_ref();
        const T: [u8; 3] = [236, 240, 248];
        const S: [u8; 3] = [178, 188, 206];
        let maxw = 600usize;
        let title_l = cc_rasterize(text, title, 15.0, T, maxw);
        let sub_l = cc_rasterize(text, sub, 13.0, S, maxw);
        let lw = |l: &Label| l.as_ref().map(|(_, w, _)| *w as f32).unwrap_or(0.0);
        let content = lw(&title_l).max(lw(&sub_l));
        let w = (content + 40.0).clamp(220.0, 660.0);
        self.toast = Some(Toast {
            output,
            start: Instant::now(),
            title: title_l,
            sub: sub_l,
            w,
            h: 60.0,
        });
    }

    /// Per-frame: promote a scheduled (delayed) screenshot to the real pending
    /// request once its instant passes. Returns `true` if it fired.
    pub fn shot_tick(&mut self) -> bool {
        match self.pending_shot {
            Some((at, _)) if Instant::now() >= at => {
                if let Some((_, req)) = self.pending_shot.take() {
                    self.pending_screenshot = Some(req);
                }
                true
            }
            _ => false,
        }
    }

    /// Toggle the Android-style Overview on `out`.
    pub fn toggle_overview(&mut self, out: OutputId) {
        if self.overview.take().is_some() {
            self.overview_drag = None;
            self.overview_touch_slot = None;
            return; // was open → now closed
        }
        self.open_overview(out);
    }

    /// Close the Overview if open.
    pub fn close_overview(&mut self) {
        self.overview = None;
        self.overview_drag = None;
        self.overview_touch_slot = None;
    }

    /// Build a fresh Overview from the live, visible (non-minimised)
    /// window set in most-recently-used order. No-op (stays closed) when
    /// nothing is showing.
    fn open_overview(&mut self, out: OutputId) {
        // Mutually exclusive with the other dock overlays.
        self.apps_menu = None;
        self.control_center = None;
        self.wifi_panel = None;
        self.bt_panel = None;
        // Every window across all workspaces — the Overview is a global
        // "all open apps" view, not just the current workspace. Ordered
        // most-recently-used so the last-used app leads (and is centred).
        let windows = self.wm.all_windows();
        let all: Vec<WindowId> = windows.iter().map(|w| w.id).collect();
        let ids = self.focus_history.cycle_order(&all);
        if ids.is_empty() {
            return;
        }
        // Open centred on the most-recent card (index 0 → scroll 0).
        self.rebuild_overview(out, ids, 0.0);
    }

    /// Build the carousel's cards (in the given MRU order) plus the "Close all"
    /// pill, and seat the scroll spring at `scroll_pos` retargeted to the
    /// nearest valid card. Shared by [`open_overview`](Self::open_overview) and
    /// the dismiss reflow.
    fn rebuild_overview(&mut self, out: OutputId, ids: Vec<WindowId>, scroll_pos: f64) {
        if ids.is_empty() {
            self.close_overview();
            return;
        }
        let bounds = self
            .wm
            .output(out)
            .map(|o| o.bounds)
            .unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0));

        let n = ids.len();
        let label_color = [232u8, 236, 244];
        let label_max = (bounds.w * 0.46 - 40.0).max(1.0) as usize;
        let text = self.text.as_ref();

        let mut cards = Vec::with_capacity(n);
        for id in ids {
            let (app, title) = self
                .wm
                .get(id)
                .map(|w| {
                    let title = if w.title.trim().is_empty() {
                        w.app.clone()
                    } else {
                        w.title.clone()
                    };
                    (w.app, title)
                })
                .unwrap_or_default();
            let label = cc_rasterize(text, &title, 15.0, label_color, label_max);
            cards.push(OverviewCard { id, app, label });
        }

        // Scroll spring: start where we were, snap to the nearest live card.
        let sp = carousel::spacing(bounds);
        let mut scroll = Spring::settle_to(scroll_pos, scroll_pos);
        let target = carousel::index_scroll(carousel::selected(scroll_pos, n, sp), sp);
        scroll.retarget(target);

        // "Close all" pill, bottom-centre (Android-style).
        let pill_w = 132.0_f32;
        let pill_h = 40.0_f32;
        let close_all = Rect::new(
            bounds.x + (bounds.w - pill_w) / 2.0,
            bounds.y + bounds.h - pill_h - 40.0,
            pill_w,
            pill_h,
        );
        let close_all_label =
            cc_rasterize(text, "Close all", 15.0, label_color, pill_w as usize);
        self.overview = Some(Overview {
            output: out,
            cards,
            scroll,
            close_all,
            close_all_label,
            dismiss: None,
            error: None,
        });
    }

    /// Output bounds the open Overview is pinned to (fallback 1080p).
    fn overview_bounds(&self) -> Option<Rect> {
        let ov = self.overview.as_ref()?;
        Some(
            self.wm
                .output(ov.output)
                .map(|o| o.bounds)
                .unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0)),
        )
    }

    /// Carousel step (px between card centres) for the open Overview's output.
    /// Bounds-relative now that cards are uniform full size — see
    /// [`crate::carousel::spacing`]. Borrow-safe to call before taking a
    /// `&mut self.overview` (returns a plain `f64`). Falls back to a 1080p step.
    fn overview_spacing(&self) -> f64 {
        carousel::spacing(
            self.overview_bounds()
                .unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0)),
        )
    }

    /// A press while the Overview is open. On a card it begins a drag
    /// (which a release resolves to tap-switch / flick-close / snap-back);
    /// on empty space it dismisses. Consumes the press whenever the
    /// Overview is open.
    pub fn overview_press(&mut self, px: f32, py: f32) -> bool {
        let Some(ov) = self.overview.as_ref() else {
            return false;
        };
        // "Close all" pill: send close to every card's window, then dismiss.
        if ov.close_all.contains(px, py) {
            let ids: Vec<WindowId> = ov.cards.iter().map(|c| c.id).collect();
            for id in ids {
                self.close_window(id);
            }
            self.close_overview();
            return true;
        }
        // Begin a drag from anywhere in the carousel (axis decided on motion).
        // The card under the press is the vertical-dismiss target; if the press
        // missed every card we still scroll, dismissing whatever is centred.
        let bounds = self.overview_bounds().unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0));
        let sp = carousel::spacing(bounds);
        let n = ov.cards.len();
        let idx = carousel::hit_card(px, py, ov.scroll.pos, n, bounds)
            .unwrap_or_else(|| carousel::selected(ov.scroll.pos, n, sp));
        let card = ov.cards.get(idx).map(|c| c.id).unwrap_or(0);
        self.overview_drag = Some(OverviewDrag {
            card,
            start_x: px,
            start_y: py,
            current_x: px,
            current_y: py,
            start_scroll: ov.scroll.pos,
            last_x: px,
            last_y: py,
            last_t: Instant::now(),
            velocity: 0.0,
            axis: DragAxis::Undecided,
            dismiss_dy: 0.0,
        });
        true
    }

    /// Track an in-flight carousel drag — decide the axis past a slop, then
    /// either scroll (horizontal, 1:1 with the finger, rubber-banded at the
    /// ends) or dismiss the pressed card (vertical). Velocity is an EMA for the
    /// release fling. Cheap no-op when no drag is active.
    pub fn overview_pointer_motion(&mut self, px: f32, py: f32) {
        let Some(mut d) = self.overview_drag.take() else { return };
        let sp = self.overview_spacing(); // before the &mut borrow below
        let Some(ov) = self.overview.as_mut() else { return };
        d.current_x = px;
        d.current_y = py;

        if d.axis == DragAxis::Undecided {
            let (adx, ady) = ((px - d.start_x).abs(), (py - d.start_y).abs());
            if adx.max(ady) > OVERVIEW_TAP_SLOP {
                d.axis = if adx >= ady { DragAxis::Horizontal } else { DragAxis::Vertical };
            }
        }

        match d.axis {
            DragAxis::Horizontal => {
                // EMA velocity (px/s) for the fling.
                let now = Instant::now();
                let dt = now.saturating_duration_since(d.last_t).as_secs_f32().max(1e-3);
                let inst = (px - d.last_x) / dt;
                d.velocity = 0.3 * inst + 0.7 * d.velocity;
                d.last_x = px;
                d.last_t = now;
                // 1:1 scroll, bypassing the spring for zero-latency tracking;
                // the spring takes over on release. Finger right → lower index.
                let raw = d.start_scroll - (px - d.start_x) as f64;
                ov.scroll.pos = carousel::clamp_overscroll(raw, ov.cards.len(), sp);
                ov.scroll.vel = 0.0;
            }
            DragAxis::Vertical => {
                // EMA upward velocity (px/s, positive = moving up) for fling.
                let now = Instant::now();
                let dt = now.saturating_duration_since(d.last_t).as_secs_f32().max(1e-3);
                let inst = (d.last_y - py) / dt; // up → positive
                d.velocity = 0.3 * inst + 0.7 * d.velocity;
                d.last_y = py;
                d.last_t = now;
                d.dismiss_dy = (d.start_y - py).max(0.0);
            }
            DragAxis::Undecided => {}
        }
        self.overview_drag = Some(d);
    }

    /// Commit a carousel drag: horizontal → fling + snap to the nearest card;
    /// vertical past the threshold → dismiss that card and reflow; a near-
    /// stationary press → tap (centre card activates, side card centres, empty
    /// space closes). Returns whether the release was consumed.
    pub fn overview_release(&mut self, px: f32, py: f32) -> bool {
        let Some(mut d) = self.overview_drag.take() else {
            return self.overview.is_some();
        };
        d.current_x = px;
        d.current_y = py;
        let sp = self.overview_spacing(); // before any &mut borrow below

        match d.axis {
            DragAxis::Horizontal => {
                if let Some(ov) = self.overview.as_mut() {
                    let n = ov.cards.len();
                    let target = carousel::index_scroll(
                        carousel::fling_target(ov.scroll.pos, d.velocity as f64, n, sp),
                        sp,
                    );
                    ov.scroll.target = target;
                    ov.scroll.vel = -(d.velocity as f64); // inertia into the spring
                }
                true
            }
            DragAxis::Vertical => {
                // Dismiss on distance past the threshold OR a fast upward fling;
                // otherwise spring the card back to its slot.
                let fling = d.velocity >= DISMISS_VEL;
                if d.dismiss_dy >= OVERVIEW_CLOSE_DIST || fling {
                    self.start_card_dismiss(d.card, d.dismiss_dy, d.velocity.max(0.0));
                } else {
                    self.start_card_snapback(d.card, d.dismiss_dy);
                }
                true
            }
            DragAxis::Undecided => self.overview_tap(px, py), // a tap
        }
    }

    /// Begin a card's exit animation: it flies off the top edge, then the close
    /// is requested and the card is *held* until the window actually goes (or a
    /// timeout marks it refused — see [`tick_animations`]). `up_vel` is the
    /// initial upward velocity (px/s, ≥ 0) — the finger's fling or a brisk
    /// constant for a middle-click.
    fn start_card_dismiss(&mut self, id: WindowId, from_dy: f32, up_vel: f32) {
        // A rapid second dismiss: make sure the prior card's close was requested
        // so it isn't left half-dismissed (its card is dropped on actual close).
        if let Some(prev) = self
            .overview
            .as_ref()
            .and_then(|o| o.dismiss.as_ref())
            .filter(|d| d.id != id && matches!(d.kind, DismissKind::Closing | DismissKind::Pending(_)))
            .map(|d| d.id)
        {
            self.close_window(prev);
        }
        let bounds_h = self.overview_bounds().map(|b| b.h).unwrap_or(1080.0);
        let Some(ov) = self.overview.as_mut() else { return };
        let mut dy = Spring::settle_to(from_dy as f64, (bounds_h * 1.3) as f64);
        dy.vel = up_vel.max(0.0) as f64;
        ov.dismiss = Some(DismissAnim { id, dy, kind: DismissKind::Closing, off_at: bounds_h });
    }

    /// Begin a below-threshold snap-back: the card springs from `from_dy` back
    /// to its slot (no close).
    fn start_card_snapback(&mut self, id: WindowId, from_dy: f32) {
        if from_dy <= 0.5 {
            return; // wasn't lifted — nothing to animate
        }
        if let Some(ov) = self.overview.as_mut() {
            ov.dismiss = Some(DismissAnim {
                id,
                dy: Spring::settle_to(from_dy as f64, 0.0),
                kind: DismissKind::SnapBack,
                off_at: 0.0,
            });
        }
    }

    /// A window actually closed (driven from `toplevel_destroyed` / X11 destroy):
    /// drop its card and reflow the carousel — the confirmation that completes a
    /// dismiss, and also keeps the Overview honest if a window dies externally
    /// while it's open. No-op if the window has no card.
    pub fn overview_window_closed(&mut self, id: WindowId) {
        let Some(ov) = self.overview.as_ref() else { return };
        if !ov.cards.iter().any(|c| c.id == id) {
            return;
        }
        let out = ov.output;
        let scroll_pos = ov.scroll.pos;
        let remaining: Vec<WindowId> =
            ov.cards.iter().map(|c| c.id).filter(|cid| *cid != id).collect();
        if remaining.is_empty() {
            self.close_overview();
        } else {
            self.rebuild_overview(out, remaining, scroll_pos);
        }
    }

    /// Middle-click on a card → close it with the exit animation. Returns
    /// whether a card was hit (so the backend can swallow the click).
    pub fn overview_middle_click(&mut self, px: f32, py: f32) -> bool {
        let Some(ov) = self.overview.as_ref() else { return false };
        let bounds = self.overview_bounds().unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0));
        let n = ov.cards.len();
        let Some(i) = carousel::hit_card(px, py, ov.scroll.pos, n, bounds) else {
            return false;
        };
        let id = ov.cards[i].id;
        self.start_card_dismiss(id, 0.0, 1200.0); // brisk upward exit
        true
    }

    /// Resolve a tap inside the open Overview: the centred card activates and
    /// exits; a side card scrolls to centre (a second tap then activates);
    /// empty space dismisses.
    fn overview_tap(&mut self, px: f32, py: f32) -> bool {
        let Some(ov) = self.overview.as_ref() else { return false };
        let bounds = self.overview_bounds().unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0));
        let sp = carousel::spacing(bounds);
        let n = ov.cards.len();
        match carousel::hit_card(px, py, ov.scroll.pos, n, bounds) {
            Some(i) if i == carousel::selected(ov.scroll.pos, n, sp) => {
                let id = ov.cards[i].id;
                self.close_overview();
                self.restore_window_animated(id);
            }
            Some(i) => {
                if let Some(ov) = self.overview.as_mut() {
                    ov.scroll.retarget(carousel::index_scroll(i, sp));
                }
            }
            None => self.close_overview(),
        }
        true
    }

    /// Move the carousel selection by `delta` cards (mouse wheel / keyboard
    /// arrows / Alt+Tab while the Overview is open). No-op when closed.
    pub fn overview_wheel(&mut self, delta: i32) {
        let sp = self.overview_spacing(); // before the &mut borrow below
        let Some(ov) = self.overview.as_mut() else { return };
        let n = ov.cards.len() as i32;
        if n == 0 {
            return;
        }
        let cur = carousel::selected(ov.scroll.pos, ov.cards.len(), sp) as i32;
        let next = (cur + delta).clamp(0, n - 1) as usize;
        ov.scroll.retarget(carousel::index_scroll(next, sp));
    }

    /// Activate the currently-centred card and exit (Enter / centre tap).
    pub fn overview_activate_selected(&mut self) {
        let sp = self.overview_spacing();
        let Some(ov) = self.overview.as_ref() else { return };
        let Some(card) = ov.cards.get(carousel::selected(ov.scroll.pos, ov.cards.len(), sp)) else {
            return;
        };
        let id = card.id;
        self.close_overview();
        self.restore_window_animated(id);
    }

    fn build_dock_menu_items(&self, entry: &DockEntry) -> Vec<DockMenuItem> {
        let pinned = self.config.dock_pinned.iter().any(|p| p == &entry.app);
        let mut items = Vec::new();
        match entry.window {
            Some(id) => {
                items.push(DockMenuItem {
                    label: "Öne Getir".into(),
                    action: DockMenuAction::Activate(id),
                });
                items.push(DockMenuItem {
                    label: "Kapat".into(),
                    action: DockMenuAction::CloseWindow(id),
                });
                items.push(DockMenuItem {
                    label: if pinned {
                        "Sabitlemeyi Kaldır".into()
                    } else {
                        "Sabitle".into()
                    },
                    action: if pinned {
                        DockMenuAction::Unpin(entry.app.clone())
                    } else {
                        DockMenuAction::Pin(entry.app.clone())
                    },
                });
            }
            None => {
                items.push(DockMenuItem {
                    label: "Başlat".into(),
                    action: DockMenuAction::Launch(entry.app.clone()),
                });
                items.push(DockMenuItem {
                    label: "Sabitlemeyi Kaldır".into(),
                    action: DockMenuAction::Unpin(entry.app.clone()),
                });
            }
        }
        items
    }

    /// Place the menu plaque adjacent to the tile, on the panel's
    /// inner side (same direction the tooltip / thumbnail use), and
    /// clamped to stay within the output's bounds.
    fn compute_dock_menu_rect(
        &self,
        out: OutputId,
        tile: Rect,
        item_count: usize,
    ) -> Rect {
        let bounds = self
            .wm
            .output(out)
            .map(|o| o.bounds)
            .unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0));
        let bg_w = DOCK_MENU_W;
        let bg_h =
            item_count as f32 * DOCK_MENU_ROW_H + 2.0 * DOCK_MENU_PAD;
        let pad = (self.config.dock_height * 0.18).max(2.0);
        match self.config.dock_edge {
            DockEdge::Bottom => {
                let x =
                    (tile.x + tile.w / 2.0 - bg_w / 2.0).clamp(bounds.x, bounds.x + bounds.w - bg_w);
                let y = (tile.y - pad - bg_h).max(bounds.y);
                Rect::new(x, y, bg_w, bg_h)
            }
            DockEdge::Top => {
                let x =
                    (tile.x + tile.w / 2.0 - bg_w / 2.0).clamp(bounds.x, bounds.x + bounds.w - bg_w);
                let y = (tile.y + tile.h + pad).min(bounds.y + bounds.h - bg_h);
                Rect::new(x, y, bg_w, bg_h)
            }
            DockEdge::Left => {
                let x = (tile.x + tile.w + pad).min(bounds.x + bounds.w - bg_w);
                let y =
                    (tile.y + tile.h / 2.0 - bg_h / 2.0).clamp(bounds.y, bounds.y + bounds.h - bg_h);
                Rect::new(x, y, bg_w, bg_h)
            }
            DockEdge::Right => {
                let x = (tile.x - pad - bg_w).max(bounds.x);
                let y =
                    (tile.y + tile.h / 2.0 - bg_h / 2.0).clamp(bounds.y, bounds.y + bounds.h - bg_h);
                Rect::new(x, y, bg_w, bg_h)
            }
        }
    }

    fn run_dock_menu_action(&mut self, action: DockMenuAction) {
        match action {
            DockMenuAction::Activate(id) => self.restore_window_animated(id),
            DockMenuAction::CloseWindow(id) => self.close_window(id),
            DockMenuAction::Launch(app) => {
                self.prune_pending_launches();
                if !self.pending_launches.contains_key(&app)
                    && self.launch_app(&app)
                {
                    self.pending_launches.insert(app, Instant::now());
                }
            }
            DockMenuAction::Pin(app) => {
                if !self.config.dock_pinned.iter().any(|p| p == &app) {
                    self.config.dock_pinned.push(app);
                    if let Err(e) = self.config.save() {
                        tracing::warn!(?e, "could not persist pin from menu");
                    }
                }
            }
            DockMenuAction::Unpin(app) => {
                let before = self.config.dock_pinned.len();
                self.config.dock_pinned.retain(|p| p != &app);
                if self.config.dock_pinned.len() != before {
                    if let Err(e) = self.config.save() {
                        tracing::warn!(?e, "could not persist unpin from menu");
                    }
                }
            }
        }
    }

    /// Send `xdg_toplevel.close` to the window's surface — the
    /// graceful "please quit" signal. Clients that don't respond stay
    /// up; that's their choice, not ours to force.
    /// A pointer press on a server-side title bar: the close button closes the
    /// window, the rest of the bar starts a compositor-driven move drag (like
    /// the dock/overview drags — not a Smithay grab). Returns whether it was
    /// consumed, so the backends can swallow it before client routing. The
    /// title bar sits *above* the window content, so it never overlaps the
    /// client's own input region.
    pub fn title_press(&mut self, px: f32, py: f32) -> bool {
        use crate::decoration::{hit, DecoHit};
        let mut wins = self.wm.list_visible();
        wins.sort_by_key(|w| std::cmp::Reverse(w.z)); // topmost first
        let mut found: Option<(WindowId, Rect, DecoHit)> = None;
        for w in &wins {
            if !self.decorated.contains(&w.id) {
                continue;
            }
            let h = hit(w.geom, px, py);
            if h != DecoHit::None {
                found = Some((w.id, w.geom, h));
                break;
            }
        }
        let Some((id, geom, h)) = found else { return false };
        match h {
            DecoHit::Close => self.close_window(id),
            DecoHit::Drag => {
                // Double-click the bar within the slop → toggle maximise.
                let now = Instant::now();
                let dbl = self.last_title_click.is_some_and(|(lid, t, lx, ly)| {
                    lid == id
                        && now.duration_since(t).as_millis() < 400
                        && (px - lx).abs() < 8.0
                        && (py - ly).abs() < 8.0
                });
                if dbl {
                    self.last_title_click = None;
                    self.toggle_maximize(id);
                } else {
                    self.last_title_click = Some((id, now, px, py));
                    self.title_drag = Some(TitleDrag {
                        id,
                        grab_dx: px - geom.x,
                        grab_dy: py - geom.y,
                    });
                    let _ = self.wm.focus(id);
                    let s = self.surface_for_window(id);
                    self.set_keyboard_focus(s);
                }
            }
            DecoHit::None => {}
        }
        true
    }

    /// Track an in-flight title-bar drag — the window follows the pointer at
    /// the captured grab offset. Cheap no-op when no drag is active.
    pub fn title_pointer_motion(&mut self, px: f32, py: f32) {
        let Some(d) = self.title_drag else { return };
        if let Ok(w) = self.wm.get(d.id) {
            let _ = self.wm.r#move(
                d.id,
                Rect::new(px - d.grab_dx, py - d.grab_dy, w.geom.w, w.geom.h),
            );
        }
    }

    /// End a title-bar drag. Returns whether one was active (so the backend
    /// swallows the release).
    pub fn title_release(&mut self) -> bool {
        match self.title_drag.take() {
            Some(d) => {
                // X11 windows are positioned by the WM, so push the final
                // post-move geometry back to Xwayland (xdg clients don't need
                // this — they never control their own position). No-op for xdg.
                self.x11_push_geometry(d.id);
                true
            }
            None => false,
        }
    }

    /// The `ToplevelSurface` backing `id`, if it's an xdg toplevel (not X11).
    fn toplevel_for(&self, id: WindowId) -> Option<ToplevelSurface> {
        let s = self.surface_for_window(id)?;
        self.xdg_shell_state
            .toplevel_surfaces()
            .iter()
            .find(|t| t.wl_surface() == &s)
            .cloned()
    }

    /// Two-finger touch down. When the second finger lands, pick the window
    /// under the centroid as the gesture target and snapshot its geometry so a
    /// drag is an absolute `start + delta`.
    pub fn two_finger_down(&mut self, slot: i32, x: f32, y: f32, now_ms: u64) {
        let was_active = self.two_finger.is_active();
        self.two_finger.down(slot, x, y, now_ms);
        if !was_active && self.two_finger.is_active() {
            if let Some((cx, cy)) = self.two_finger.live_centroid() {
                self.two_finger_target = self.wm.hit_test(cx, cy);
                self.two_finger_win_start = self
                    .two_finger_target
                    .and_then(|id| self.wm.get(id).ok())
                    .map(|w| w.geom)
                    .unwrap_or_else(|| Rect::new(0.0, 0.0, 0.0, 0.0));
                // Bring the grabbed window to the front so the move is visible.
                if let Some(id) = self.two_finger_target {
                    let _ = self.wm.focus(id);
                }
            }
        }
    }

    /// Two-finger motion. Moves the target window to its drag-start geometry
    /// plus the centroid displacement. Returns `true` if the window moved.
    pub fn two_finger_motion(&mut self, slot: i32, x: f32, y: f32) -> bool {
        let Some(TwoFingerOut::MoveTo { dx, dy }) = self.two_finger.motion(slot, x, y) else {
            return false;
        };
        let Some(id) = self.two_finger_target else {
            return false;
        };
        let mut g = self.two_finger_win_start;
        g.x += dx;
        g.y += dy;
        let _ = self.wm.r#move(id, g);
        true
    }

    /// Two-finger up. A two-finger double-tap toggles fullscreen / restore on
    /// the target window. Returns `true` if anything changed (redraw warranted).
    pub fn two_finger_up(&mut self, slot: i32, now_ms: u64) -> bool {
        let out = self.two_finger.up(slot, now_ms);
        let mut changed = false;
        if let Some(TwoFingerOut::DoubleTap) = out {
            if let Some(id) = self.two_finger_target {
                self.toggle_maximize(id);
                changed = true;
            }
        }
        if !self.two_finger.is_active() {
            self.two_finger_target = None;
        }
        changed
    }

    /// Drop any in-flight two-finger gesture state (touch cancel).
    pub fn two_finger_cancel(&mut self) {
        self.two_finger.cancel();
        self.two_finger_target = None;
    }

    /// Toggle maximise (server-side double-click on the title bar). Compositor-
    /// initiated, reserving the title-bar strip for decorated windows. Works for
    /// both xdg toplevels and X11 windows (the client notification differs).
    pub fn toggle_maximize(&mut self, id: WindowId) {
        let Ok(w) = self.wm.get(id) else { return };
        let is_max = matches!(w.state, WinState::Maximized)
            || matches!(w.state, WinState::Snapped(SnapZone::Maximize));
        if is_max {
            let restore = self.maximize_restore.remove(&id).unwrap_or(w.geom);
            let _ = self.wm.r#move(id, restore);
            self.notify_maximize(id, false, restore);
        } else {
            let Some(mon) = self.wm.monitor_for_window(id) else { return };
            self.maximize_restore.insert(id, w.geom);
            let area = mon.work_area;
            let _ = self.wm.snap(id, SnapZone::Maximize);
            let bar = if self.decorated.contains(&id) {
                crate::decoration::BAR_H
            } else {
                0.0
            };
            let ch = (area.h - bar).max(1.0);
            let target = Rect::new(area.x, area.y + bar, area.w, ch);
            if bar > 0.0 {
                let _ = self.wm.r#move(id, target);
            }
            self.notify_maximize(id, true, target);
        }
        // Reflect the maximize/unmaximize to wlr foreign-toplevel taskbars.
        self.ftl_refresh_all_state();
    }

    /// Notify the client of a maximise change: xdg via a configure carrying the
    /// state + size; X11 via the X11 surface. `geom` is the new content rect.
    fn notify_maximize(&self, id: WindowId, maximized: bool, geom: Rect) {
        if let Some(top) = self.toplevel_for(id) {
            top.with_pending_state(|s| {
                if maximized {
                    s.states.set(xdg_toplevel::State::Maximized);
                } else {
                    s.states.unset(xdg_toplevel::State::Maximized);
                }
                s.size = Some((geom.w as i32, geom.h as i32).into());
            });
            top.send_configure();
        } else {
            self.x11_apply_maximized(id, maximized, geom);
        }
    }

    pub(crate) fn close_window(&self, id: WindowId) {
        let Some(s) = self.surface_for_window(id) else { return };
        for t in self.xdg_shell_state.toplevel_surfaces() {
            if t.wl_surface() == &s {
                t.send_close();
                return;
            }
        }
        // Not an xdg toplevel → X11 window: send `WM_DELETE_WINDOW`.
        if let Some(x11) = self.x11_windows.get(&id) {
            let _ = x11.close();
        }
    }

    /// Keyboard activation of a dock slot — fires the same path as a
    /// click on that tile (focus / restore / minimise-toggle for a
    /// running window, launch with debounce for a pinned-not-running
    /// app). Returns whether anything ran; `false` when `output` has
    /// no dock or `index` is past the last tile.
    pub fn activate_dock_index(&mut self, output: OutputId, index: usize) -> bool {
        let Some(entry) = self.dock_tiles_for(output).get(index).cloned() else {
            return false;
        };
        self.activate_entry(&entry);
        true
    }

    /// Drop the click-routing logic in one place so press-on-non-pinned
    /// (immediate) and release-without-drag (deferred) behave
    /// identically.
    fn activate_entry(&mut self, entry: &DockEntry) {
        // The apps button toggles the applications grid menu instead of
        // launching/focusing. Resolve its output from the tile centre.
        if entry.app == APPS_BUTTON_APP {
            if let Some(out) = self.wm.output_at(
                entry.rect.x + entry.rect.w / 2.0,
                entry.rect.y + entry.rect.h / 2.0,
            ) {
                self.toggle_apps_menu(out);
            }
            return;
        }
        if entry.app == SETTINGS_BUTTON_APP {
            if let Some(out) = self.wm.output_at(
                entry.rect.x + entry.rect.w / 2.0,
                entry.rect.y + entry.rect.h / 2.0,
            ) {
                self.toggle_control_center(out);
            }
            return;
        }
        if entry.app == RECENTS_BUTTON_APP {
            if let Some(out) = self.wm.output_at(
                entry.rect.x + entry.rect.w / 2.0,
                entry.rect.y + entry.rect.h / 2.0,
            ) {
                self.toggle_overview(out);
            }
            return;
        }
        // Launching or focusing a real app from the dock tucks the dock away
        // again (window-presence model): a launch maps a window (handled by
        // the rising-edge clear), but focusing an existing one needs this.
        self.dock_force_shown = false;
        match entry.window {
            Some(id) => {
                let win = self.wm.get(id).ok();
                let focused = win.as_ref().map(|w| w.focused).unwrap_or(false);
                let minimized = matches!(
                    win.as_ref().map(|w| &w.state),
                    Some(WinState::Minimized)
                );
                let pending_min = matches!(
                    self.animations.get(&id).map(|a| a.end),
                    Some(AnimEnd::Minimize { .. })
                );
                if focused && !minimized && !pending_min {
                    self.minimize_window_animated(id);
                } else {
                    self.restore_window_animated(id);
                }
            }
            None => {
                let app = entry.app.clone();
                self.prune_pending_launches();
                if !self.pending_launches.contains_key(&app)
                    && self.launch_app(&app)
                {
                    self.pending_launches.insert(app, Instant::now());
                }
            }
        }
    }

    /// Drop pending-launch entries that have either timed out
    /// ([`LAUNCH_DEBOUNCE`]) or whose app now has a running window (the
    /// launch succeeded; the slot will bind that window from here on).
    /// Keeps the map bounded and the "starting" cue honest.
    fn prune_pending_launches(&mut self) {
        let now = Instant::now();
        let running: Vec<String> =
            self.wm.all_windows().into_iter().map(|w| w.app).collect();
        self.pending_launches.retain(|app, started| {
            now.duration_since(*started) < LAUNCH_DEBOUNCE
                && !running.iter().any(|r| r == app)
        });
    }

    /// Whether `app` has a launch in flight (spawned from a pinned
    /// tile, no window yet, within [`LAUNCH_DEBOUNCE`]). Read-only —
    /// the renderer uses it to draw the pulsing "starting" cue.
    pub fn launch_pending(&self, app: &str) -> bool {
        self.pending_launches
            .get(app)
            .is_some_and(|t| t.elapsed() < LAUNCH_DEBOUNCE)
    }

    /// Any launch still in flight. A damage-driven backend (udev) polls
    /// this to keep redrawing while the pinned-tile "starting" pulse is
    /// animating; winit already renders every loop iteration.
    pub fn has_pending_launches(&self) -> bool {
        self.pending_launches
            .values()
            .any(|t| t.elapsed() < LAUNCH_DEBOUNCE)
    }

    /// Any window currently flagged urgent — the damage-driven backend
    /// keeps redrawing while true so the alert pulse on the dock tile
    /// animates.
    pub fn has_urgent_windows(&self) -> bool {
        self.wm.all_windows().iter().any(|w| w.urgent)
    }

    /// The dock tile whose hover has just crossed
    /// [`DOCK_HOVER_DWELL`] — what the renderer uses to promote the
    /// tooltip to a live window thumbnail. `None` while a drag is in
    /// flight (the drag preview owns the bar).
    pub fn dock_thumbnail_ready(&self) -> Option<(OutputId, usize)> {
        if self.dock_drag.is_some() {
            return None;
        }
        let (out, idx, start) = self.dock_hover_started.as_ref()?;
        if start.elapsed() < DOCK_HOVER_DWELL {
            return None;
        }
        Some((*out, *idx))
    }

    /// True while a dock hover is on its dwell countdown — the udev
    /// backend keeps redrawing so the thumbnail can actually appear
    /// when the timer elapses, even with the pointer motionless.
    pub fn has_pending_hover(&self) -> bool {
        self.dock_hover_started
            .as_ref()
            .is_some_and(|(_, _, t)| t.elapsed() < DOCK_HOVER_DWELL)
    }

    /// Pinned-launcher core: resolve `app_id`'s `.desktop` `Exec=` and
    /// spawn it detached. Returns whether a child actually started; a
    /// missing entry / binary is logged, never fatal. The dock-click
    /// path that calls this for a pinned tile whose app isn't running
    /// yet is wired in Phase 2 — for now this is the tested, callable
    /// engine with no UI trigger.
    pub fn launch_app(&self, app_id: &str) -> bool {
        match crate::icons::resolve_launch(app_id) {
            Some(argv) => crate::launcher::spawn_detached(&argv),
            None => {
                tracing::warn!(app_id, "pinned app has no resolvable .desktop Exec");
                false
            }
        }
    }

    /// Minimise `id` with a shrink-to-dock animation: the window
    /// springs from its current rect down to the dock slot, and is
    /// only marked [`WinState::Minimized`] once the spring settles (so
    /// it stays visible while shrinking). No-op if the window is
    /// missing or already minimised.
    pub fn minimize_window_animated(&mut self, id: WindowId) {
        let Ok(win) = self.wm.get(id) else { return };
        if matches!(win.state, WinState::Minimized) {
            return;
        }
        let start = win.geom;
        let Some(target) = self.dock_slot_for(id) else { return };
        self.animations.insert(
            id,
            WindowAnim::to_rect(id, start, target, AnimEnd::Minimize { restore: start }),
        );
    }

    /// Un-minimise `id` with a grow-from-dock animation, and focus it.
    ///
    /// Works whether the window is fully minimised (settled) *or* still
    /// mid-shrink: in the latter case the true restore rect is read
    /// from the pending [`AnimEnd::Minimize`] so an interrupted
    /// minimise reverses smoothly from wherever it currently is. If the
    /// window isn't minimising at all, this is just a plain focus.
    pub fn restore_window_animated(&mut self, id: WindowId) {
        let Ok(win) = self.wm.get(id) else { return };

        // Are we minimised, or part-way into a minimise?
        let pending_min = matches!(
            self.animations.get(&id).map(|a| a.end),
            Some(AnimEnd::Minimize { .. })
        );
        let should_restore =
            matches!(win.state, WinState::Minimized) || pending_min;

        // Un-minimise + focus + raise z. (`wm.focus` flips Minimized →
        // Floating; on a mid-shrink window it's already Floating.)
        let _ = self.wm.focus(id);
        let surface = self.surface_for_window(id);
        self.set_keyboard_focus(surface);

        if !should_restore {
            return;
        }

        // Target = the window's full rect. After a settled minimise
        // that's `win.geom` (restored on settle); mid-shrink it lives
        // in the pending Minimize end instead.
        let target = match self.animations.get(&id).map(|a| a.end) {
            Some(AnimEnd::Minimize { restore }) => restore,
            _ => win.geom,
        };
        // Start from wherever it visually is now (dock slot when
        // settled, or part-way when interrupted) for a smooth reversal.
        let start = if matches!(win.state, WinState::Minimized) {
            match self.dock_slot_for(id) {
                Some(s) => s,
                None => return,
            }
        } else {
            win.geom
        };
        let _ = self.wm.r#move(id, start);
        self.animations
            .insert(id, WindowAnim::to_rect(id, start, target, AnimEnd::None));
    }

    /// `Super+H` toggle. If a visible window sits under the pointer,
    /// minimise it; otherwise un-minimise the topmost minimised window
    /// on the pointer's output. With no dock yet this is the keyboard
    /// round-trip for hide/show.
    pub fn toggle_minimize_at_pointer(&mut self) {
        let (px, py) = self.pointer_position;
        if let Some(id) = self.wm.hit_test(px as f32, py as f32) {
            // `hit_test` skips minimised windows, so a hit is always a
            // visible window → hide it.
            self.minimize_window_animated(id);
            return;
        }
        // Nothing under the pointer → bring one back. Prefer the
        // most-recently-used minimised window (MRU); fall back to the
        // topmost-by-z one if MRU has nothing (e.g. a window minimised
        // before it was ever focused).
        let Some(output) = self.pointer_output() else { return };
        let Some(ws) = self.wm.active_workspace_on(output) else { return };
        let mru_pick = self.focus_history.most_recent_matching(|id| {
            self.wm
                .get(id)
                .map(|w| w.workspace == ws && matches!(w.state, WinState::Minimized))
                .unwrap_or(false)
        });
        if let Some(id) = mru_pick.or_else(|| self.wm.topmost_minimized_on(ws)) {
            self.restore_window_animated(id);
        }
    }

    /// Advance every live window animation by the wall-clock delta since the
    /// previous call. Returns `true` while at least one animation is still
    /// in motion — backends use this to keep `needs_redraw` flagged.
    ///
    /// On settle, each animation's `commit_zone` (if any) is folded into the
    /// WM via [`WindowManager::snap`], so the post-animation state record is
    /// consistent with what's on screen.
    pub fn tick_animations(&mut self, now: Instant) -> bool {
        let dt = now.saturating_duration_since(self.last_anim_tick).as_secs_f64();
        self.last_anim_tick = now;
        // The carousel keeps the loop alive while its scroll spring is moving
        // or a drag is in progress (motion drives `pos` directly, but we still
        // want per-frame redraws).
        let dragging_h =
            matches!(self.overview_drag.as_ref().map(|d| d.axis), Some(DragAxis::Horizontal));
        let ov_animating = self
            .overview
            .as_ref()
            .map(|o| !o.scroll.is_settled() || o.dismiss.is_some() || o.error.is_some())
            .unwrap_or(false)
            || self.overview_drag.is_some();
        // Apps-menu scroll inertia coasts while its spring is moving and no
        // finger is actively driving it (the drag path drives `pos` directly
        // and flags its own redraws).
        let apps_scroll_animating = self.apps_menu_drag.is_none()
            && self
                .apps_menu
                .as_ref()
                .map(|m| !m.scroll.is_settled())
                .unwrap_or(false);
        // A background Wi-Fi scan/connect keeps the loop alive so its status
        // line updates even with no other input.
        let wifi_busy = self
            .wifi_panel
            .as_ref()
            .map(|w| w.connecting.is_some() || w.scanning)
            .unwrap_or(false);
        let cc_pending = self.cc_rx.is_some();
        // The Bluetooth panel polls its coprocess every tick while open.
        let bt_open = self.bt_panel.is_some();
        // Desktop settings background auth/action worker.
        let ds_pending = self.ds_rx.is_some();
        if self.animations.is_empty()
            && self.workspace_slides.is_empty()
            && self.switcher_fade.is_none()
            && !ov_animating
            && !apps_scroll_animating
            && self.ws_bounce.is_none()
            && self.ws_swipe.is_none()
            && self.flash.is_none()
            && self.toast.is_none()
            && !wifi_busy
            && !cc_pending
            && !bt_open
            && !ds_pending
        {
            return false;
        }

        let mut still_running = false;

        // --- Bluetooth coprocess events ---------------------------------
        if bt_open {
            self.bt_poll();
            still_running = true; // keep polling the coprocess while open
        }

        // --- Desktop Settings background worker -------------------------
        if ds_pending {
            still_running |= self.ds_tick();
        }

        // --- Control Center background state ----------------------------
        if cc_pending {
            let snap = self.cc_rx.as_ref().and_then(|rx| rx.try_recv().ok());
            match snap {
                Some(s) => {
                    self.cc_rx = None;
                    if self.control_center.is_some() {
                        self.cc_apply_snapshot(s);
                    }
                    still_running = true;
                }
                None => still_running = true, // keep polling
            }
        }

        // --- Wi-Fi async scan / connect ---------------------------------
        if wifi_busy {
            let msg = self.wifi_rx.as_ref().and_then(|rx| rx.try_recv().ok());
            match msg {
                Some(WifiMsg::ScannedPartial(nets)) => {
                    // First paint from cache; keep `wifi_rx` open + scanning so
                    // the fresh pass (real signals) still lands.
                    if let Some(out) = self.wifi_panel.as_ref().map(|w| w.output) {
                        self.build_wifi_panel(out, nets, Some("Taranıyor…".to_string()), true, true);
                    }
                    still_running = true;
                }
                Some(WifiMsg::Scanned(nets)) => {
                    self.wifi_rx = None;
                    if let Some(out) = self.wifi_panel.as_ref().map(|w| w.output) {
                        self.build_wifi_panel(out, nets, None, false, true);
                    }
                    still_running = true;
                }
                Some(WifiMsg::Details(d)) => {
                    self.wifi_rx = None;
                    if let Some((out, eth)) = self.wifi_panel.as_ref().map(|w| (w.output, w.eth)) {
                        self.build_wifi_details_panel(out, d, false, eth);
                    }
                    still_running = true;
                }
                Some(WifiMsg::Connected { ssid, ok, msg }) => {
                    self.wifi_rx = None;
                    let info = self
                        .wifi_panel
                        .as_ref()
                        .map(|p| (p.output, p.pw_for.is_some(), p.nets.clone()));
                    if let Some((out, was_pw, mut nets)) = info {
                        if let Some(p) = self.wifi_panel.as_mut() {
                            p.connecting = None;
                        }
                        if ok {
                            // Reflect the new connection and return to the list.
                            for n in nets.iter_mut() {
                                n.active = n.ssid == ssid;
                            }
                            self.build_wifi_panel(
                                out,
                                nets,
                                Some(format!("Bağlandı: {ssid}")),
                                false,
                                true,
                            );
                        } else if was_pw {
                            // Re-prompt with a fresh keyboard, showing the real
                            // nmcli reason (wrong key / WPA3 / timeout / …).
                            self.enter_wifi_password(out, ssid);
                            self.wifi_set_status(&msg);
                        } else {
                            self.wifi_set_status(&format!("Bağlanamadı: {msg}"));
                        }
                    }
                    still_running = true; // one more frame to show the result
                }
                None => still_running = true, // keep polling
            }
        }

        // Camera flash: purely time-based; keep ticking until it has faded out.
        if let Some((_, start)) = self.flash {
            if start.elapsed().as_millis() as u64 >= FLASH_MS {
                self.flash = None;
            } else {
                still_running = true;
            }
        }

        // Toast banner: time-based; keep ticking (for the fade) until it expires.
        if let Some(t) = self.toast.as_ref() {
            if t.start.elapsed().as_millis() as u64 >= TOAST_MS {
                self.toast = None;
            } else {
                still_running = true;
            }
        }

        // --- apps-menu scroll inertia -----------------------------------
        if apps_scroll_animating {
            if let Some(menu) = self.apps_menu.as_mut() {
                menu.scroll.step(dt);
                // Hard-stop at the content edges (no rubber-band): if a fling
                // overshoots, clamp and kill the velocity so it rests flush.
                let max = menu.max_scroll_y() as f64;
                if menu.scroll.pos < 0.0 {
                    menu.scroll.pos = 0.0;
                    menu.scroll.target = 0.0;
                    menu.scroll.vel = 0.0;
                } else if menu.scroll.pos > max {
                    menu.scroll.pos = max;
                    menu.scroll.target = max;
                    menu.scroll.vel = 0.0;
                }
                still_running = true;
            }
        }

        // --- overview carousel scroll -----------------------------------
        if let Some(ov) = self.overview.as_mut() {
            if !dragging_h && !ov.scroll.is_settled() {
                ov.scroll.step(dt);
                still_running = true;
            }
        }
        if self.overview_drag.is_some() {
            still_running = true; // redraw live while dragging
        }

        // --- overview card dismiss / snap-back --------------------------
        // Phase machine (close-refusal aware): Closing flies off → requests the
        // close + parks (Pending); the window's actual destroy drops the card
        // (`overview_window_closed`); if it refuses past the timeout it springs
        // back (Refused) + a red error pulse. SnapBack just returns to 0.
        enum DismissDo {
            SendClose(WindowId),
            FlagError(WindowId),
        }
        let mut act: Option<DismissDo> = None;
        if let Some(ov) = self.overview.as_mut() {
            if let Some(da) = ov.dismiss.as_mut() {
                match da.kind {
                    DismissKind::SnapBack | DismissKind::Refused => {
                        if da.dy.step(dt) {
                            still_running = true;
                        } else {
                            ov.dismiss = None;
                        }
                    }
                    DismissKind::Closing => {
                        let moving = da.dy.step(dt);
                        if !moving || da.dy.pos as f32 >= da.off_at {
                            da.dy.pos = da.off_at as f64; // park off-screen
                            da.kind = DismissKind::Pending(now);
                            act = Some(DismissDo::SendClose(da.id));
                        }
                        still_running = true;
                    }
                    DismissKind::Pending(since) => {
                        still_running = true; // poll for the timeout
                        if now.saturating_duration_since(since) > DISMISS_CONFIRM_TIMEOUT {
                            // Refused: spring the card back into its slot.
                            let from = da.dy.pos;
                            da.dy = Spring::settle_to(from, 0.0);
                            da.kind = DismissKind::Refused;
                            act = Some(DismissDo::FlagError(da.id));
                        }
                    }
                }
            }
        }
        match act {
            Some(DismissDo::SendClose(id)) => self.close_window(id),
            Some(DismissDo::FlagError(id)) => {
                if let Some(ov) = self.overview.as_mut() {
                    ov.error = Some((id, now));
                }
            }
            None => {}
        }
        // Error pulse expiry.
        if let Some(ov) = self.overview.as_mut() {
            match ov.error {
                Some((_, t)) if now.saturating_duration_since(t).as_millis() > DISMISS_ERROR_MS => {
                    ov.error = None;
                }
                Some(_) => still_running = true,
                None => {}
            }
        }

        // --- window springs ---------------------------------------------
        let mut settled: Vec<WindowId> = Vec::new();
        for (id, anim) in self.animations.iter_mut() {
            let alive = anim.step(dt);
            let rect = anim.current_rect();
            // Always reflect the current spring position into the WM so
            // the render path sees the in-flight geometry.
            let _ = self.wm.r#move(*id, rect);
            if alive {
                still_running = true;
            } else {
                settled.push(*id);
            }
        }
        for id in settled {
            if let Some(anim) = self.animations.remove(&id) {
                match anim.end {
                    AnimEnd::None => {}
                    AnimEnd::Snap(zone) => {
                        // Re-snap on landing so the WM records the final
                        // state as Snapped(zone) rather than Floating.
                        let _ = self.wm.snap(id, zone);
                        // Decorated windows reserve BAR_H at the top of the
                        // zone so the title bar stays on-screen (matches the
                        // grab's animation target and the maximise path).
                        if self.decorated.contains(&id) {
                            if let Ok(w) = self.wm.get(id) {
                                let b = crate::decoration::BAR_H;
                                let _ = self.wm.r#move(
                                    id,
                                    Rect::new(
                                        w.geom.x,
                                        w.geom.y + b,
                                        w.geom.w,
                                        (w.geom.h - b).max(1.0),
                                    ),
                                );
                            }
                        }
                    }
                    AnimEnd::Minimize { restore } => {
                        // The spring shrank it toward the dock for show;
                        // now actually minimize and restore the logical
                        // rect so a future un-minimize expands sanely.
                        let _ = self.wm.minimize(id);
                        let _ = self.wm.r#move(id, restore);
                        // The window is *now* actually minimized — reflect it to
                        // wlr foreign-toplevel taskbars (state lagged the anim).
                        self.ftl_refresh_all_state();
                    }
                }
            }
        }

        // --- workspace slides -------------------------------------------
        // A live 3-finger swipe pins its slide's progress to the finger;
        // don't let the spring fight it. Everything else springs normally.
        let live_swipe_out = self.ws_swipe.as_ref().filter(|s| s.armed).map(|s| s.output);
        let mut settled_outputs: Vec<OutputId> = Vec::new();
        // (output, prev_ws) for slides that sprang *back* to 0 — a
        // cancelled live swipe. The active workspace was switched eagerly
        // when the slide began, so restore it now that we've reversed.
        let mut reverse_restore: Vec<(OutputId, WorkspaceId)> = Vec::new();
        for (output, slide) in self.workspace_slides.iter_mut() {
            if Some(*output) == live_swipe_out {
                still_running = true; // finger-driven this frame
                continue;
            }
            if slide.step(dt) {
                still_running = true;
            } else {
                if slide.progress.target < 0.5 {
                    reverse_restore.push((*output, slide.prev_ws));
                }
                settled_outputs.push(*output);
            }
        }
        for o in settled_outputs {
            self.workspace_slides.remove(&o);
        }
        for (output, prev_ws) in reverse_restore {
            let _ = self.wm.switch_workspace_on(output, prev_ws);
            self.auto_focus_workspace(prev_ws);
        }

        // --- edge rubber-band bounce ------------------------------------
        let bounce_live_out = self.ws_swipe.as_ref().map(|s| s.output);
        let mut clear_bounce = false;
        if let Some(b) = self.ws_bounce.as_mut() {
            if bounce_live_out == Some(b.output) {
                still_running = true; // finger driving it; pos set in update
            } else if b.offset.step(dt) {
                still_running = true;
            } else {
                clear_bounce = true;
            }
        }
        if clear_bounce {
            self.ws_bounce = None;
        }

        // --- switcher fade ----------------------------------------------
        if let Some(f) = self.switcher_fade.as_mut() {
            // The cycle can die without a commit/cancel (e.g. a window
            // close drops it below two candidates). Treat that as a
            // close so the overlay fades out instead of vanishing.
            if !f.closing && !self.focus_history.is_cycling() {
                f.closing = true;
                f.alpha.retarget(0.0);
            }
            let moving = f.alpha.step(dt);
            if moving {
                still_running = true;
            } else if f.closing {
                // Fully faded out — drop it.
                self.switcher_fade = None;
            }
        }

        still_running
    }

    /// Output the switcher overlay is pinned to, or `None` when there's
    /// no overlay (not opening, not fading out). The render gate uses
    /// this so the overlay is drawn on exactly one screen.
    pub fn switcher_output(&self) -> Option<OutputId> {
        self.switcher_fade.as_ref().map(|f| f.output)
    }

    /// What the switcher renderer needs this frame:
    /// `(alpha, candidates, selected)`. Candidates come live from the
    /// cycle while it's open, or from the captured snapshot during
    /// fade-out. `None` when there's nothing to draw.
    pub fn switcher_render(&self) -> Option<(f32, Vec<WindowId>, Option<WindowId>)> {
        let f = self.switcher_fade.as_ref()?;
        let alpha = f.alpha.pos.clamp(0.0, 1.0) as f32;
        let (cands, sel) = match &f.frozen {
            Some((c, s)) => (c.clone(), *s),
            None => (
                self.focus_history.cycle_candidates()?.to_vec(),
                self.focus_history.current_cycle_window(),
            ),
        };
        if cands.is_empty() {
            return None;
        }
        Some((alpha, cands, sel))
    }

    /// Switch the workspace shown on `output` to `target_ws` and kick off
    /// a horizontal slide animation. `direction` controls the visual:
    /// `+1.0` slides the new workspace in from the right (matches "show
    /// what's to the right" — i.e. a 3-finger left swipe); `-1.0`
    /// from the left.
    ///
    /// The WM-side switch is immediate; the animation lives in
    /// [`workspace_slides`] until the spring settles. Calling this with
    /// `target_ws == active_workspace_on(output)` is a no-op.
    pub fn start_workspace_slide(
        &mut self,
        output: OutputId,
        target_ws: WorkspaceId,
        direction: f32,
    ) -> Result<(), WmError> {
        let prev_ws = self
            .wm
            .active_workspace_on(output)
            .ok_or(WmError::NoOutput(output))?;
        if prev_ws == target_ws {
            return Ok(());
        }
        self.wm.switch_workspace_on(output, target_ws)?;
        self.workspace_slides
            .insert(output, SlideAnim::new(output, prev_ws, direction));
        // Without this, keyboard input would keep going to a window on
        // the workspace we just slid away from — invisible to the user.
        self.auto_focus_workspace(target_ws);
        Ok(())
    }

    /// Move the currently focused window to the workspace at zero-based
    /// `index` on the **window's own** output (not the pointer's — the
    /// user typically wants "send this thing one workspace over",
    /// regardless of where the cursor wandered). Missing workspaces
    /// are auto-created like in [`switch_workspace_index`].
    ///
    /// No-ops silently when no window is focused or when the window is
    /// already on the target workspace; this keeps the keyboard
    /// shortcut idempotent.
    pub fn move_focused_window_to_index(&mut self, index: usize) -> Result<(), WmError> {
        // Find the focused window. We use `list_visible` (all active
        // workspaces) — a window on an off-screen workspace can't be
        // "focused" anyway because the WM clears focus when a window
        // moves out of view.
        let Some(win) = self.wm.list_visible().into_iter().find(|w| w.focused) else {
            return Ok(());
        };
        let src_ws = win.workspace;
        let Some(output) = self.wm.output_for_window(win.id).map(|o| o.id) else {
            return Ok(());
        };

        let mut wss = self.wm.workspaces_for(output);
        if wss.is_empty() {
            return Err(WmError::NoOutput(output));
        }
        wss.sort_by_key(|w| w.id);

        while wss.len() <= index {
            let name = format!("Workspace {}", wss.len() + 1);
            self.wm.add_workspace(output, name)?;
            wss = self.wm.workspaces_for(output);
            wss.sort_by_key(|w| w.id);
        }

        let target = wss[index].id;
        if target == src_ws {
            return Ok(());
        }
        self.wm.move_window_to_workspace(win.id, target)?;
        // Promote another window on the source workspace — the user
        // sent something *away*, they don't want focus to follow into
        // the void.
        self.auto_focus_workspace(src_ws);
        Ok(())
    }

    /// Switch to the workspace at zero-based `index` in this output's
    /// id-sorted workspace list. Missing workspaces between `len` and
    /// `index` are auto-created (named `"Workspace N"`), so a fresh
    /// session that only has one workspace can still answer
    /// `Super+5` by spawning four new empty workspaces first.
    ///
    /// The slide direction is inferred from where `index` sits
    /// relative to the current active: a forward jump slides in from
    /// the right, a backward jump from the left.
    pub fn switch_workspace_index(
        &mut self,
        output: OutputId,
        index: usize,
    ) -> Result<(), WmError> {
        // Fixed set of virtual desktops — clamp into it, never create more.
        let index = index.min(crate::wm::WORKSPACE_COUNT - 1);
        let mut wss = self.wm.workspaces_for(output);
        if wss.is_empty() {
            return Err(WmError::NoOutput(output));
        }
        wss.sort_by_key(|w| w.id);
        let Some(target_ws) = wss.get(index) else {
            return Ok(()); // index beyond what exists — no-op
        };
        let target = target_ws.id;
        let current = self
            .wm
            .active_workspace_on(output)
            .ok_or(WmError::NoOutput(output))?;
        if current == target {
            return Ok(());
        }
        let current_idx = wss.iter().position(|w| w.id == current);
        let direction = match current_idx {
            Some(ci) if ci < index => 1.0,
            Some(ci) if ci > index => -1.0,
            // Active workspace fell off the list (shouldn't happen since
            // switch_workspace_on rejects unknown ws) — pick the same
            // direction the user usually means by "jump forward".
            _ => 1.0,
        };
        self.start_workspace_slide(output, target, direction)
    }

    /// Switch to the neighbouring workspace by id-order: `delta = +1`
    /// goes to the next, `-1` to the previous. Edges don't wrap (a
    /// follow-up could expose wrap as a config). Returns `Ok(())`
    /// without animating if there's no neighbour in the requested
    /// direction.
    pub fn switch_workspace_relative(
        &mut self,
        output: OutputId,
        delta: i32,
    ) -> Result<(), WmError> {
        let mut wss = self.wm.workspaces_for(output);
        if wss.is_empty() {
            return Err(WmError::NoOutput(output));
        }
        wss.sort_by_key(|w| w.id);
        let current = self
            .wm
            .active_workspace_on(output)
            .ok_or(WmError::NoOutput(output))?;
        let Some(idx) = wss.iter().position(|w| w.id == current) else {
            return Ok(());
        };
        let new_idx = idx as i32 + delta;
        if new_idx < 0 || new_idx as usize >= wss.len() {
            return Ok(()); // nothing to slide to
        }
        let target = wss[new_idx as usize].id;
        let direction = if delta > 0 { 1.0 } else { -1.0 };
        self.start_workspace_slide(output, target, direction)
    }

    // --- live 3-finger touchpad workspace swipe (udev) -----------------
    //
    // Reuses the proven [`SlideAnim`] render path, but instead of letting
    // the spring run on its own we pin its `progress.pos` to the finger
    // every `GestureSwipeUpdate` (and gate the spring step in
    // `tick_animations`). On `GestureSwipeEnd` we hand control back to the
    // spring: commit (→ 1.0) or cancel (→ 0.0, restoring the eagerly-
    // switched active workspace once it settles). At a workspace edge
    // there's no neighbour, so we run a rubber-band bounce instead.

    /// Begin a touchpad swipe. Only 3-finger swipes drive workspaces
    /// (2-finger is reserved for scrolling); anything else clears any
    /// stale gesture and is otherwise ignored.
    pub fn ws_swipe_begin(&mut self, fingers: u32) {
        if fingers != 3 {
            self.ws_swipe = None;
            return;
        }
        let Some(output) = self.pointer_output() else {
            return;
        };
        let width = self
            .wm
            .output(output)
            .map(|o| o.bounds.w as f32)
            .unwrap_or(0.0);
        if width <= 0.0 {
            return;
        }
        self.ws_swipe = Some(WsSwipe {
            output,
            accum: 0.0,
            width,
            vel: 0.0,
            armed: false,
        });
    }

    /// Feed a frame of finger movement into the active swipe.
    pub fn ws_swipe_update(&mut self, dx: f32, _dy: f32) {
        let Some(mut sw) = self.ws_swipe else {
            return;
        };
        sw.accum += dx;
        sw.vel = 0.7 * sw.vel + 0.3 * dx; // light EMA for fling detection

        if !sw.armed {
            if sw.accum.abs() < WS_SWIPE_START_PX {
                self.ws_swipe = Some(sw);
                return;
            }
            // Crossed the threshold. A swipe left (accum < 0) reveals the
            // workspace to the right ("next", delta +1); a swipe right
            // reveals the previous one.
            let delta = if sw.accum < 0.0 { 1 } else { -1 };
            let had_slide = self.workspace_slides.contains_key(&sw.output);
            let _ = self.switch_workspace_relative(sw.output, delta);
            if !had_slide {
                if let Some(slide) = self.workspace_slides.get_mut(&sw.output) {
                    // Pin it at the start; the finger drives it from here.
                    slide.progress.pos = 0.0;
                    slide.progress.vel = 0.0;
                }
            }
            sw.armed = true;
        }

        if let Some(slide) = self.workspace_slides.get_mut(&sw.output) {
            let travel = (sw.accum.abs() - WS_SWIPE_START_PX).max(0.0);
            slide.progress.pos = (travel / sw.width).clamp(0.0, 1.0) as f64;
            slide.progress.vel = 0.0;
        } else {
            // No neighbour in this direction — rubber-band the edge.
            let off = Self::ws_rubber_band(sw.accum) as f64;
            let b = self.ws_bounce.get_or_insert(WsBounce {
                output: sw.output,
                offset: Spring::settle_to(0.0, 0.0),
            });
            b.output = sw.output;
            b.offset.pos = off;
            b.offset.vel = 0.0;
            b.offset.target = 0.0;
        }
        self.ws_swipe = Some(sw);
    }

    /// Resolve the swipe on lift-off: commit or spring back.
    pub fn ws_swipe_end(&mut self) {
        let Some(sw) = self.ws_swipe.take() else {
            return;
        };
        if sw.armed {
            if let Some(slide) = self.workspace_slides.get_mut(&sw.output) {
                let travel = (sw.accum.abs() - WS_SWIPE_START_PX).max(0.0);
                let t = (travel / sw.width).clamp(0.0, 1.0);
                // A flick commits even from a short drag, provided it's
                // still moving in the reveal direction (finger dir = -dir).
                let fling = sw.vel.abs() >= WS_SWIPE_FLING_VEL
                    && sw.vel.signum() == (-slide.direction).signum();
                if t >= WS_SWIPE_COMMIT_FRAC || fling {
                    slide.progress.retarget(1.0); // finish; active already target
                } else {
                    slide.progress.retarget(0.0); // reverse; tick restores active
                }
            }
        }
        // Any edge bounce springs back regardless.
        if let Some(b) = self.ws_bounce.as_mut() {
            b.offset.retarget(0.0);
        }
    }

    /// Diminishing-returns rubber-band: the offset approaches
    /// [`WS_BOUNCE_MAX`] as the over-swipe grows, so the edge feels firm.
    fn ws_rubber_band(accum: f32) -> f32 {
        let sign = if accum < 0.0 { -1.0 } else { 1.0 };
        let x = accum.abs();
        sign * WS_BOUNCE_MAX * (1.0 - 1.0 / (1.0 + x / (WS_BOUNCE_MAX * 2.0)))
    }

    /// Horizontal pixel offset the renderer adds to `output`'s active
    /// workspace for an in-progress / settling edge bounce. 0 when none.
    pub fn ws_bounce_offset(&self, output: OutputId) -> i32 {
        self.ws_bounce
            .as_ref()
            .filter(|b| b.output == output)
            .map(|b| b.offset.pos.round() as i32)
            .unwrap_or(0)
    }

    /// Convenience: build a fresh `Arc<ClientState>` to hand to
    /// `DisplayHandle::insert_client`. Centralised here so the per-client
    /// pieces always grow in one place.
    pub fn new_client_state() -> Arc<ClientState> {
        Arc::new(ClientState::default())
    }

    /// Look up the Bacak window id behind a Wayland surface, if any.
    pub fn window_for(&self, surface: &WlSurface) -> Option<WindowId> {
        self.windows.get(surface).copied()
    }
}

#[cfg(test)]
mod ws_swipe_tests {
    //! Headless tests for the live 3-finger touchpad workspace swipe.
    //! `BacakState::new` already seeds output 0 with [`WORKSPACE_COUNT`]
    //! workspaces, so we drive the real begin/update/end methods and the
    //! real `tick_animations` slide settling — no mock.
    use super::*;
    use crate::wm::WORKSPACE_COUNT;
    use smithay::reexports::wayland_server::Display;
    use std::time::{Duration, Instant};

    fn test_state() -> BacakState {
        let display: Display<BacakState> = Display::new().unwrap();
        let mut state = BacakState::new(&display, "ws-swipe-test-seat");
        // Hermetic: `BacakState::new` calls `restore_session`, which reads the
        // real `~/.config/bacak/session.json` and can leave a non-zero active
        // workspace. Pin to index 0 (settling any restore slide) so these
        // tests never depend on on-disk session state.
        if let Some(out) = state.wm.primary_output() {
            let _ = state.switch_workspace_index(out, 0);
            settle(&mut state);
        }
        state
    }

    /// Step `tick_animations` with a fixed 16 ms dt until it reports
    /// everything settled (or fail). Returns when idle.
    fn settle(state: &mut BacakState) {
        let t0 = Instant::now();
        state.last_anim_tick = t0;
        for i in 1..=600 {
            if !state.tick_animations(t0 + Duration::from_millis(16 * i)) {
                return;
            }
        }
        panic!("animations failed to settle within 600 frames");
    }

    fn workspaces(state: &BacakState, out: OutputId) -> Vec<WorkspaceId> {
        let mut wss = state.wm.workspaces_for(out);
        wss.sort_by_key(|w| w.id);
        wss.into_iter().map(|w| w.id).collect()
    }

    #[test]
    fn three_finger_swipe_past_threshold_commits() {
        let mut state = test_state();
        let out = state.wm.primary_output().unwrap();
        let width = state.wm.output(out).unwrap().bounds.w;
        let ws = workspaces(&state, out);
        assert_eq!(state.wm.active_workspace_on(out), Some(ws[0]));

        // Swipe left far enough to clear the commit fraction.
        let total = WS_SWIPE_START_PX + WS_SWIPE_COMMIT_FRAC * width + 20.0;
        state.ws_swipe_begin(3);
        for _ in 0..20 {
            state.ws_swipe_update(-total / 20.0, 0.0);
        }
        // Active flips eagerly to the next workspace while the slide runs.
        assert_eq!(state.wm.active_workspace_on(out), Some(ws[1]));
        assert!(state.workspace_slides.contains_key(&out));

        state.ws_swipe_end();
        settle(&mut state);
        assert_eq!(state.wm.active_workspace_on(out), Some(ws[1]));
        assert!(state.workspace_slides.is_empty());
    }

    #[test]
    fn short_swipe_springs_back_and_restores_active() {
        let mut state = test_state();
        let out = state.wm.primary_output().unwrap();
        let ws = workspaces(&state, out);

        // Arm (cross the start threshold) but stay well under the commit
        // fraction, with small per-frame deltas so the release velocity is
        // low (a single huge delta would read as a fling). It must reverse.
        state.ws_swipe_begin(3);
        for _ in 0..12 {
            state.ws_swipe_update(-4.0, 0.0); // 48px total, ~4px/frame velocity
        }
        assert_eq!(state.wm.active_workspace_on(out), Some(ws[1])); // eager switch
        state.ws_swipe_end();
        settle(&mut state);
        assert_eq!(state.wm.active_workspace_on(out), Some(ws[0])); // restored
        assert!(state.workspace_slides.is_empty());
    }

    #[test]
    fn two_finger_swipe_is_ignored() {
        let mut state = test_state();
        let out = state.wm.primary_output().unwrap();
        let ws = workspaces(&state, out);

        state.ws_swipe_begin(2); // scroll, not a workspace gesture
        state.ws_swipe_update(-500.0, 0.0);
        assert!(state.ws_swipe.is_none());
        assert!(state.workspace_slides.is_empty());
        assert_eq!(state.wm.active_workspace_on(out), Some(ws[0]));
    }

    #[test]
    fn edge_swipe_bounces_without_switching() {
        let mut state = test_state();
        let out = state.wm.primary_output().unwrap();
        let ws = workspaces(&state, out);

        // Jump to the last workspace and let the slide settle.
        state.switch_workspace_index(out, WORKSPACE_COUNT - 1).unwrap();
        settle(&mut state);
        assert_eq!(state.wm.active_workspace_on(out), Some(ws[WORKSPACE_COUNT - 1]));

        // Swipe left again ("next") — there's no neighbour, so it bounces.
        state.ws_swipe_begin(3);
        for _ in 0..20 {
            state.ws_swipe_update(-15.0, 0.0);
        }
        assert!(state.workspace_slides.is_empty(), "no slide at the edge");
        assert!(state.ws_bounce.is_some());
        assert!(state.ws_bounce_offset(out) < 0, "edge follows the finger left");
        assert_eq!(state.wm.active_workspace_on(out), Some(ws[WORKSPACE_COUNT - 1]));

        state.ws_swipe_end();
        settle(&mut state);
        assert!(state.ws_bounce.is_none());
        assert_eq!(state.wm.active_workspace_on(out), Some(ws[WORKSPACE_COUNT - 1]));
    }

    #[test]
    fn rubber_band_is_bounded_and_signed() {
        // Saturates toward ±WS_BOUNCE_MAX, never exceeds it, tracks sign.
        assert!(BacakState::ws_rubber_band(0.0).abs() < 0.001);
        assert!(BacakState::ws_rubber_band(-50.0) < 0.0);
        assert!(BacakState::ws_rubber_band(50.0) > 0.0);
        for x in [10.0, 100.0, 1000.0, 100000.0] {
            assert!(BacakState::ws_rubber_band(x) < WS_BOUNCE_MAX);
            assert!(BacakState::ws_rubber_band(-x) > -WS_BOUNCE_MAX);
        }
        // Monotonic: a larger over-swipe gives a larger (abs) offset.
        assert!(BacakState::ws_rubber_band(200.0) > BacakState::ws_rubber_band(50.0));
    }
}

#[cfg(test)]
mod overview_uniform_tests {
    //! End-to-end (headless) check of the uniform-card invariant through the
    //! *real* open path: `toggle_overview` → `open_overview` → `rebuild_overview`
    //! → cards, then the same `carousel::card_transform` the renderer uses for
    //! every card. This is the host-free analog of "open the Overview and verify
    //! uniform cards" (the nested winit run needs a live host compositor).
    use super::*;
    use smithay::reexports::wayland_server::Display;

    #[test]
    fn open_overview_builds_uniform_cards_for_mixed_window_shapes() {
        let display: Display<BacakState> = Display::new().unwrap();
        let mut state = BacakState::new(&display, "ov-uniform-test-seat");
        let out = state.wm.primary_output().unwrap();
        let bounds = state.wm.output(out).unwrap().bounds;

        // Deliberately heterogeneous source windows — the exact "no exceptions"
        // cases from the spec: wide browser, tall portrait, tiny keyboard strip,
        // and a fullscreen-sized window.
        state.wm.open("browser", "Web", Rect::new(0.0, 0.0, 1600.0, 900.0));
        state.wm.open("foot", "Terminal", Rect::new(0.0, 0.0, 600.0, 820.0));
        state.wm.open("onboard", "Keyboard", Rect::new(0.0, 0.0, 1200.0, 260.0));
        state.wm.open("viewer", "Fullscreen", Rect::new(0.0, 0.0, bounds.w, bounds.h));

        state.toggle_overview(out);
        let ov = state
            .overview
            .as_ref()
            .expect("overview should open when windows exist");
        let n = ov.cards.len();
        assert_eq!(n, 4, "one card per open window");

        // The renderer derives every card's outer frame solely from
        // card_transform — so verify the frame is identical for all cards
        // regardless of the source window's aspect ratio.
        let scroll = ov.scroll.target; // where the open animation settles
        let expect_w = carousel::CARD_W_FRAC * bounds.w;
        let expect_h = carousel::CARD_H_FRAC * bounds.h;
        for i in 0..n {
            let t = carousel::card_transform(i, scroll, bounds);
            assert_eq!(t.scale, 1.0, "card {i}: no scaling");
            assert_eq!(t.opacity, 1.0, "card {i}: no fade");
            assert_eq!(t.rect.w, expect_w, "card {i}: identical width");
            assert_eq!(t.rect.h, expect_h, "card {i}: identical height");
        }

        // Equal spacing: adjacent card centres differ by exactly `spacing`,
        // and adjacent frames don't overlap (gap > 0).
        let sp = carousel::spacing(bounds);
        let c0 = carousel::card_transform(0, scroll, bounds).rect;
        let c1 = carousel::card_transform(1, scroll, bounds).rect;
        let step = (c1.x + c1.w / 2.0) - (c0.x + c0.w / 2.0);
        assert!((step as f64 - sp).abs() < 0.5, "equal centre-to-centre spacing");
        assert!(c1.x > c0.x + c0.w, "uniform frames must not overlap");
    }
}

// ---------------------------------------------------------------------------
// Desktop Settings panel
// ---------------------------------------------------------------------------

fn ds_rasterize(
    font: &Option<crate::text::TextRenderer>,
    s: &str,
    px: f32,
    rgb: [u8; 3],
    max_w: usize,
) -> Option<(MemoryRenderBuffer, usize, usize)> {
    use smithay::utils::Transform;
    let f = font.as_ref()?;
    let ss = LABEL_SUPERSAMPLE;
    let (rgba, w, h) = f.rasterize_line(s, px * ss as f32, rgb, max_w * ss as usize)?;
    let buf = MemoryRenderBuffer::from_slice(
        &rgba, Fourcc::Abgr8888, (w as i32, h as i32), ss, Transform::Normal, None,
    );
    Some((buf, w / ss as usize, h / ss as usize))
}

impl BacakState {
    // -- open / close --------------------------------------------------------

    pub fn open_desktop_settings(&mut self, out: OutputId) {
        tracing::info!("open_desktop_settings: output={:?} text={}", out, self.text.is_some());
        self.apps_menu = None;
        self.wifi_panel = None;
        self.bt_panel = None;
        self.close_overview();

        let bounds = self
            .wm
            .output(out)
            .map(|o| o.bounds)
            .unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0));

        const W: f32 = 400.0;
        const PAD: f32 = 20.0;
        const GAP: f32 = 10.0;
        const ROW_H: f32 = 52.0;
        const HDR_H: f32 = 32.0;  // section header
        const SWATCH_ROW: f32 = 60.0; // wallpaper swatches
        const TITLE_H: f32 = 38.0;

        let inner = W - 2.0 * PAD;
        let iw = inner as usize;

        // Current state snapshots
        let auto_login_on =
            std::fs::read_to_string("/etc/bacak/autologin").map(|s| !s.trim().is_empty()).unwrap_or(false);
        let hostname = std::fs::read_to_string("/etc/hostname")
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "bacakos".to_string());

        const LABEL: [u8; 3] = [232, 236, 244];
        const SUB:   [u8; 3] = [170, 178, 196];
        const HDR:   [u8; 3] = [130, 145, 175];
        let text = &self.text;

        let mut rows: Vec<DsRow> = Vec::new();
        let mut y = 0.0_f32; // relative to panel content origin

        // ── Görünüm ──
        rows.push(DsRow {
            rect: Rect::new(0.0, y, inner, HDR_H),
            action: DsAction::ToggleDarkMode, // not tappable — section header
            label: ds_rasterize(text, "── Görünüm ──", 12.0, HDR, iw),
            value_label: None,
            swatch: None,
            is_toggle: false,
            toggled: false,
        });
        y += HDR_H + 4.0;

        rows.push(DsRow {
            rect: Rect::new(0.0, y, inner, ROW_H),
            action: DsAction::ToggleDarkMode,
            label: ds_rasterize(text, "Karanlık Mod", 15.0, LABEL, iw / 2),
            value_label: None,
            swatch: None,
            is_toggle: true,
            toggled: self.dark_mode,
        });
        y += ROW_H + GAP;

        rows.push(DsRow {
            rect: Rect::new(0.0, y, inner, HDR_H),
            action: DsAction::SetWallpaper(0), // placeholder — section label
            label: ds_rasterize(text, "Duvar Kağıdı", 13.0, SUB, iw),
            value_label: None,
            swatch: None,
            is_toggle: false,
            toggled: false,
        });
        y += HDR_H + 4.0;

        // 8 swatches in one row
        let sw = (inner - 7.0 * GAP) / 8.0;
        for (i, &color) in crate::plugins::desktop_settings::WALLPAPER_PRESETS.iter().enumerate() {
            rows.push(DsRow {
                rect: Rect::new(i as f32 * (sw + GAP), y, sw, SWATCH_ROW),
                action: DsAction::SetWallpaper(i),
                label: None,
                value_label: None,
                swatch: Some(color),
                is_toggle: false,
                toggled: self.config.wallpaper_color == color,
            });
        }
        y += SWATCH_ROW + GAP;

        // Wallpaper image path row
        let img_val = self.config.wallpaper_image.as_deref().unwrap_or("Seçilmedi");
        let img_val_label = ds_rasterize(text, img_val, 12.0, SUB, iw / 2);
        rows.push(DsRow {
            rect: Rect::new(0.0, y, inner, ROW_H),
            action: DsAction::SetWallpaperImage,
            label: ds_rasterize(text, "Resim Yolu", 15.0, LABEL, iw / 2),
            value_label: img_val_label,
            swatch: None,
            is_toggle: false,
            toggled: false,
        });
        y += ROW_H + GAP;

        // ── Sistem (root korumalı) ──
        rows.push(DsRow {
            rect: Rect::new(0.0, y, inner, HDR_H),
            action: DsAction::EditHostname,
            label: ds_rasterize(text, "── Sistem ──", 12.0, HDR, iw),
            value_label: None,
            swatch: None,
            is_toggle: false,
            toggled: false,
        });
        y += HDR_H + 4.0;

        let hn_val = ds_rasterize(text, &hostname, 13.0, SUB, iw / 2);
        rows.push(DsRow {
            rect: Rect::new(0.0, y, inner, ROW_H),
            action: DsAction::EditHostname,
            label: ds_rasterize(text, "Bilgisayar Adı", 15.0, LABEL, iw / 2),
            value_label: hn_val,
            swatch: None,
            is_toggle: false,
            toggled: false,
        });
        y += ROW_H + GAP;

        rows.push(DsRow {
            rect: Rect::new(0.0, y, inner, ROW_H),
            action: DsAction::ToggleAutoLogin,
            label: ds_rasterize(text, "Otomatik Giriş", 15.0, LABEL, iw / 2),
            value_label: None,
            swatch: None,
            is_toggle: true,
            toggled: auto_login_on,
        });
        y += ROW_H + GAP;

        // ── Güvenlik (root korumalı) ──
        rows.push(DsRow {
            rect: Rect::new(0.0, y, inner, HDR_H),
            action: DsAction::ChangePassword,
            label: ds_rasterize(text, "── Güvenlik ──", 12.0, HDR, iw),
            value_label: None,
            swatch: None,
            is_toggle: false,
            toggled: false,
        });
        y += HDR_H + 4.0;

        rows.push(DsRow {
            rect: Rect::new(0.0, y, inner, ROW_H),
            action: DsAction::ChangePassword,
            label: ds_rasterize(text, "Parola Değiştir", 15.0, LABEL, iw),
            value_label: None,
            swatch: None,
            is_toggle: false,
            toggled: false,
        });
        y += ROW_H + GAP;

        let content_h = y;
        let panel_h = TITLE_H + PAD + content_h + PAD;
        let panel_h = panel_h.min(bounds.h * 0.85);
        let panel_w = W;
        let px = bounds.x + (bounds.w - panel_w) / 2.0;
        let py = bounds.y + (bounds.h - panel_h) / 2.0;
        let panel = Rect::new(px, py, panel_w, panel_h);
        let cx = px + PAD;
        let cy_content = py + TITLE_H + PAD;

        // Offset all row rects by content origin
        for row in &mut rows {
            row.rect.x += cx;
            row.rect.y += cy_content;
        }

        // Auth overlay geometry (centred inside panel)
        const A_W: f32 = W - 40.0;
        const A_H: f32 = 180.0;
        let ax = px + (panel_w - A_W) / 2.0;
        let ay = py + (panel_h - A_H) / 2.0;
        let auth_field = Rect::new(ax + 16.0, ay + 70.0, A_W - 32.0, 44.0);
        let btn_w = (A_W - 32.0 - 10.0) / 2.0;
        let auth_ok_rect = Rect::new(ax + 16.0 + btn_w + 10.0, ay + A_H - 56.0, btn_w, 40.0);
        let auth_cancel_rect = Rect::new(ax + 16.0, ay + A_H - 56.0, btn_w, 40.0);

        // Entry sub-flow geometry
        let entry_field = Rect::new(ax + 16.0, ay + 70.0, A_W - 32.0, 44.0);
        let entry_ok_rect = auth_ok_rect;
        let entry_cancel_rect = auth_cancel_rect;

        let title = ds_rasterize(text, "Masaüstü Ayarları", 16.0, LABEL, iw);
        let auth_title_label = ds_rasterize(text, "Root Parolası Gerekli", 15.0, LABEL, A_W as usize);
        let auth_ok_label = ds_rasterize(text, "Onayla", 14.0, LABEL, btn_w as usize);
        let auth_cancel_label = ds_rasterize(text, "İptal", 14.0, LABEL, btn_w as usize);
        let entry_ok_label = auth_ok_label.clone();
        let entry_cancel_label = auth_cancel_label.clone();

        self.desktop_settings = Some(DesktopSettingsPanel {
            output: out,
            panel,
            rows,
            mode: DsMode::Main,
            text_buf: String::new(),
            auth_buf: String::new(),
            root_pw: String::new(),
            title,
            status: None,
            status_ok: false,
            auth_title_label,
            auth_field,
            auth_pw_label: None,
            auth_ok_rect,
            auth_cancel_rect,
            auth_ok_label,
            auth_cancel_label,
            entry_title_label: None,
            entry_field,
            entry_field_label: None,
            entry_ok_rect,
            entry_cancel_rect,
            entry_ok_label,
            entry_cancel_label,
        });
    }

    pub fn close_desktop_settings(&mut self) {
        self.desktop_settings = None;
        self.ds_rx = None;
        if self.osk.is_visible() {
            self.osk_hide();
            self.osk_dirty = true;
        }
    }

    // -- press handler -------------------------------------------------------

    pub fn ds_panel_press(&mut self, px: f32, py: f32) -> bool {
        let Some(ds) = self.desktop_settings.as_ref() else { return false };
        if !ds.panel.contains(px, py) {
            self.close_desktop_settings();
            return true;
        }

        match &ds.mode {
            DsMode::Auth { .. } => return self.ds_auth_press(px, py),
            DsMode::HostnameEntry { .. } | DsMode::PwChange { .. } | DsMode::WallpaperImageEntry => return self.ds_entry_press(px, py),
            DsMode::Main => {}
        }

        // Find hit row
        let hit = self.desktop_settings.as_ref().and_then(|ds| {
            ds.rows.iter().find(|r| r.rect.contains(px, py)).map(|r| r.action.clone())
        });
        let Some(action) = hit else { return true };

        match action {
            DsAction::ToggleDarkMode => {
                self.dark_mode = !self.dark_mode;
                let dark = self.dark_mode;
                if let Some(ds) = self.desktop_settings.as_mut() {
                    for row in &mut ds.rows {
                        if matches!(row.action, DsAction::ToggleDarkMode) {
                            row.toggled = dark;
                        }
                    }
                }
            }
            DsAction::SetWallpaper(i) => {
                let color = crate::plugins::desktop_settings::WALLPAPER_PRESETS[i];
                self.config.wallpaper_color = color;
                let _ = self.config.save();
                if let Some(ds) = self.desktop_settings.as_mut() {
                    for row in &mut ds.rows {
                        if let DsAction::SetWallpaper(j) = row.action {
                            row.toggled = j == i;
                        }
                    }
                }
            }
            DsAction::SetWallpaperImage => {
                let out = self.desktop_settings.as_ref().map(|d| d.output);
                if let Some(out) = out {
                    self.open_file_browser(out);
                }
            }
            DsAction::EditHostname => self.ds_require_auth(DsAction::EditHostname),
            DsAction::ToggleAutoLogin => self.ds_require_auth(DsAction::ToggleAutoLogin),
            DsAction::ChangePassword => self.ds_require_auth(DsAction::ChangePassword),
        }
        true
    }

    fn ds_require_auth(&mut self, action: DsAction) {
        let Some(ds) = self.desktop_settings.as_mut() else { return };
        ds.auth_buf.clear();
        ds.auth_pw_label = None;
        ds.status = None;
        ds.mode = DsMode::Auth { pending: action };
        // Open OSK for auth entry
        let out = ds.output;
        let field_rect = ds.auth_field;
        drop(ds);
        if !self.osk.is_visible() {
            self.osk.bind(self.wm.clone(), out);
            let field = FocusedField {
                rect: OskRect { x: field_rect.x, y: field_rect.y, w: field_rect.w, h: field_rect.h },
                mode: InputMode::Password,
                has_hw_keyboard: false,
            };
            self.osk.open_for(field);
            let _ = self.osk.confirm_open();
            self.apply_osk_xkb();
            self.osk_dirty = true;
        }
    }

    fn ds_auth_press(&mut self, px: f32, py: f32) -> bool {
        let Some(ds) = self.desktop_settings.as_ref() else { return false };
        let ok_hit = ds.auth_ok_rect.contains(px, py);
        let cancel_hit = ds.auth_cancel_rect.contains(px, py);

        if cancel_hit {
            self.osk_hide();
            self.osk_dirty = true;
            if let Some(ds) = self.desktop_settings.as_mut() {
                ds.mode = DsMode::Main;
                ds.auth_buf.clear();
                ds.status = None;
            }
            return true;
        }
        if ok_hit {
            let pw = self.desktop_settings.as_ref().map(|d| d.auth_buf.clone()).unwrap_or_default();
            let action = match self.desktop_settings.as_ref().map(|d| &d.mode) {
                Some(DsMode::Auth { pending }) => pending.clone(),
                _ => return true,
            };
            self.ds_verify_auth(pw, action);
            return true;
        }
        true // stay open, consume
    }

    fn ds_verify_auth(&mut self, pw: String, action: DsAction) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let ok = verify_root_password(&pw);
            if ok {
                let _ = tx.send(DsResult::AuthOk(action, pw));
            } else {
                let _ = tx.send(DsResult::AuthFail);
            }
        });
        self.ds_rx = Some(rx);
        if let Some(ds) = self.desktop_settings.as_mut() {
            ds.status = ds_rasterize(&None, "Doğrulanıyor…", 13.0, [170, 178, 196], 300);
        }
    }

    fn ds_entry_press(&mut self, px: f32, py: f32) -> bool {
        let Some(ds) = self.desktop_settings.as_ref() else { return false };
        let ok_hit = ds.entry_ok_rect.contains(px, py);
        let cancel_hit = ds.entry_cancel_rect.contains(px, py);

        if cancel_hit {
            self.osk_hide();
            self.osk_dirty = true;
            if let Some(ds) = self.desktop_settings.as_mut() {
                ds.mode = DsMode::Main;
                ds.text_buf.clear();
                ds.status = None;
            }
            return true;
        }
        if ok_hit {
            let buf = self.desktop_settings.as_ref().map(|d| d.text_buf.clone()).unwrap_or_default();
            // Wallpaper image path — no root auth needed.
            if matches!(self.desktop_settings.as_ref().map(|d| &d.mode), Some(DsMode::WallpaperImageEntry)) {
                let path = buf.trim().to_string();
                self.osk_hide();
                self.osk_dirty = true;
                if path.is_empty() {
                    // Clear wallpaper image.
                    self.config.wallpaper_image = None;
                    self.wallpaper_cache.clear();
                    let _ = self.config.save();
                    if let Some(ds) = self.desktop_settings.as_mut() {
                        ds.mode = DsMode::Main;
                        ds.status = ds_rasterize(&self.text, "Resim kaldırıldı", 13.0, [170, 178, 196], 300);
                        ds.status_ok = true;
                        // Update row value label.
                        for row in &mut ds.rows {
                            if matches!(row.action, DsAction::SetWallpaperImage) {
                                row.value_label = ds_rasterize(&self.text, "Seçilmedi", 12.0, [170, 178, 196], 200);
                            }
                        }
                    }
                } else {
                    // Load image on background thread.
                    let out_bounds = self.wm.output(self.desktop_settings.as_ref().map(|d| d.output).unwrap_or_default())
                        .map(|o| (o.bounds.w as u32, o.bounds.h as u32))
                        .unwrap_or((1920, 1080));
                    let (tx, rx) = std::sync::mpsc::channel::<DsResult>();
                    let path2 = path.clone();
                    let (ow, oh) = out_bounds;
                    std::thread::spawn(move || {
                        let result = crate::state::load_wallpaper_buffer(&path2, ow, oh)
                            .map(|buf| (buf, ow, oh))
                            .ok_or(());
                        let _ = tx.send(DsResult::WallpaperLoaded(result));
                    });
                    self.ds_rx = Some(rx);
                    self.config.wallpaper_image = Some(path.clone());
                    if let Some(ds) = self.desktop_settings.as_mut() {
                        ds.mode = DsMode::Main;
                        ds.status = ds_rasterize(&self.text, "Resim yükleniyor…", 13.0, [170, 178, 196], 300);
                        ds.status_ok = false;
                        for row in &mut ds.rows {
                            if matches!(row.action, DsAction::SetWallpaperImage) {
                                let short = std::path::Path::new(&path)
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or(&path);
                                row.value_label = ds_rasterize(&self.text, short, 12.0, [170, 178, 196], 200);
                            }
                        }
                    }
                }
                return true;
            }
            let mode = match self.desktop_settings.as_ref().map(|d| &d.mode) {
                Some(DsMode::HostnameEntry { .. }) => "hostname",
                Some(DsMode::PwChange { phase, new_pw }) => {
                    let phase = *phase;
                    let new_pw = new_pw.clone();
                    if phase == 0 {
                        if let Some(ds) = self.desktop_settings.as_mut() {
                            ds.mode = DsMode::PwChange { phase: 1, new_pw: buf };
                            ds.text_buf.clear();
                            ds.entry_field_label = ds_rasterize(&self.text, "Yeni Parolayı Onayla", 13.0, [170, 178, 196], 300);
                        }
                        return true;
                    }
                    // phase 1: confirm
                    if buf != new_pw {
                        if let Some(ds) = self.desktop_settings.as_mut() {
                            let col = [230, 80, 80];
                            ds.status = ds_rasterize(&self.text, "Parolalar eşleşmiyor", 13.0, col, 300);
                            ds.status_ok = false;
                        }
                        return true;
                    }
                    let (tx, rx) = std::sync::mpsc::channel::<DsResult>();
                    let user = std::env::var("USER").unwrap_or_else(|_| "user".to_string());
                    let new_pw2 = new_pw.clone();
                    let rpw = self.desktop_settings.as_ref().map(|d| d.root_pw.clone()).unwrap_or_default();
                    std::thread::spawn(move || {
                        let ok = ds_set_password(&user, &new_pw2, &rpw);
                        let _ = tx.send(DsResult::PasswordSet(ok));
                    });
                    self.ds_rx = Some(rx);
                    self.osk_hide();
                    self.osk_dirty = true;
                    return true;
                }
                _ => return true,
            };
            if mode == "hostname" {
                let new_hn = buf.trim().to_string();
                if new_hn.is_empty() { return true; }
                let rpw = self.desktop_settings.as_ref().map(|d| d.root_pw.clone()).unwrap_or_default();
                let (tx, rx) = std::sync::mpsc::channel::<DsResult>();
                std::thread::spawn(move || {
                    let ok = ds_set_hostname(&new_hn, &rpw);
                    let _ = tx.send(DsResult::HostnameSet(ok));
                });
                self.ds_rx = Some(rx);
                self.osk_hide();
                self.osk_dirty = true;
            }
        }
        true
    }

    /// Apply a result from the background ds_rx.
    pub fn ds_apply_result(&mut self, result: DsResult) {
        match result {
            DsResult::AuthOk(action, root_pw) => {
                self.osk_hide();
                self.osk_dirty = true;
                if let Some(ds) = self.desktop_settings.as_mut() {
                    ds.root_pw = root_pw.clone();
                }
                match &action {
                    DsAction::EditHostname => {
                        let hostname = std::fs::read_to_string("/etc/hostname")
                            .map(|s| s.trim().to_string())
                            .unwrap_or_default();
                        let out = self.desktop_settings.as_ref().map(|d| d.output);
                        if let (Some(ds), Some(out)) = (self.desktop_settings.as_mut(), out) {
                            ds.text_buf = hostname;
                            ds.mode = DsMode::HostnameEntry { original: ds.text_buf.clone() };
                            ds.entry_field_label = ds_rasterize(&self.text, "Bilgisayar Adı", 13.0, [170, 178, 196], 300);
                            ds.status = None;
                            drop(ds);
                            // Open OSK for entry
                            self.ds_open_entry_osk(out);
                        }
                    }
                    DsAction::ToggleAutoLogin => {
                        let auto_on = std::fs::read_to_string("/etc/bacak/autologin")
                            .map(|s| !s.trim().is_empty())
                            .unwrap_or(false);
                        let user = if auto_on {
                            String::new()
                        } else {
                            std::env::var("USER").unwrap_or_else(|_| "user".to_string())
                        };
                        let rpw = root_pw.clone();
                        let (tx, rx) = std::sync::mpsc::channel::<DsResult>();
                        std::thread::spawn(move || {
                            let ok = ds_set_autologin(&user, &rpw);
                            let _ = tx.send(DsResult::AutoLoginSet(ok));
                        });
                        self.ds_rx = Some(rx);
                        if let Some(ds) = self.desktop_settings.as_mut() {
                            ds.mode = DsMode::Main;
                            ds.status = ds_rasterize(&self.text, "Uygulanıyor…", 13.0, [170, 178, 196], 300);
                        }
                    }
                    DsAction::ChangePassword => {
                        let out = self.desktop_settings.as_ref().map(|d| d.output);
                        if let (Some(ds), Some(out)) = (self.desktop_settings.as_mut(), out) {
                            ds.text_buf.clear();
                            ds.mode = DsMode::PwChange { phase: 0, new_pw: String::new() };
                            ds.entry_field_label = ds_rasterize(&self.text, "Yeni Parola", 13.0, [170, 178, 196], 300);
                            ds.status = None;
                            drop(ds);
                            self.ds_open_entry_osk(out);
                        }
                    }
                    _ => {}
                }
            }
            DsResult::AuthFail => {
                if let Some(ds) = self.desktop_settings.as_mut() {
                    let col = [230, 80, 80];
                    ds.status = ds_rasterize(&self.text, "Yanlış parola", 13.0, col, 300);
                    ds.status_ok = false;
                    ds.auth_buf.clear();
                    ds.auth_pw_label = None;
                }
            }
            DsResult::HostnameSet(ok) => {
                if let Some(ds) = self.desktop_settings.as_mut() {
                    let (text, col) = if ok {
                        ("Bilgisayar adı güncellendi", [80, 200, 120_u8])
                    } else {
                        ("Hata: değiştirilemedi", [230_u8, 80, 80])
                    };
                    ds.mode = DsMode::Main;
                    ds.text_buf.clear();
                    ds.status = ds_rasterize(&self.text, text, 13.0, col, 300);
                    ds.status_ok = ok;
                }
            }
            DsResult::AutoLoginSet(ok) => {
                if let Some(ds) = self.desktop_settings.as_mut() {
                    let auto_on = std::fs::read_to_string("/etc/bacak/autologin")
                        .map(|s| !s.trim().is_empty())
                        .unwrap_or(false);
                    for row in &mut ds.rows {
                        if matches!(row.action, DsAction::ToggleAutoLogin) {
                            row.toggled = auto_on;
                        }
                    }
                    let (text, col) = if ok {
                        ("Otomatik giriş güncellendi", [80, 200, 120_u8])
                    } else {
                        ("Hata: değiştirilemedi", [230_u8, 80, 80])
                    };
                    ds.status = ds_rasterize(&self.text, text, 13.0, col, 300);
                    ds.status_ok = ok;
                }
            }
            DsResult::PasswordSet(ok) => {
                if let Some(ds) = self.desktop_settings.as_mut() {
                    ds.mode = DsMode::Main;
                    ds.text_buf.clear();
                    let (text, col) = if ok {
                        ("Parola güncellendi", [80, 200, 120_u8])
                    } else {
                        ("Hata: parola değiştirilemedi", [230_u8, 80, 80])
                    };
                    ds.status = ds_rasterize(&self.text, text, 13.0, col, 300);
                    ds.status_ok = ok;
                }
            }
            DsResult::WallpaperLoaded(result) => {
                match result {
                    Ok((buf, w, h)) => {
                        self.wallpaper_cache.insert((w, h), buf);
                        let _ = self.config.save();
                        if let Some(ds) = self.desktop_settings.as_mut() {
                            ds.status = ds_rasterize(&self.text, "Resim uygulandı", 13.0, [80, 200, 120], 300);
                            ds.status_ok = true;
                        }
                    }
                    Err(()) => {
                        self.config.wallpaper_image = None;
                        let _ = self.config.save();
                        if let Some(ds) = self.desktop_settings.as_mut() {
                            ds.status = ds_rasterize(&self.text, "Hata: resim yüklenemedi", 13.0, [230, 80, 80], 300);
                            ds.status_ok = false;
                            for row in &mut ds.rows {
                                if matches!(row.action, DsAction::SetWallpaperImage) {
                                    row.value_label = ds_rasterize(&self.text, "Seçilmedi", 12.0, [170, 178, 196], 200);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn ds_open_entry_osk(&mut self, out: OutputId) {
        if let Some(ds) = self.desktop_settings.as_ref() {
            let field_rect = ds.entry_field;
            let mode = match &ds.mode {
                DsMode::PwChange { .. } => InputMode::Password,
                _ => InputMode::Text,
            };
            drop(ds);
            self.osk.bind(self.wm.clone(), out);
            let field = FocusedField {
                rect: OskRect { x: field_rect.x, y: field_rect.y, w: field_rect.w, h: field_rect.h },
                mode,
                has_hw_keyboard: false,
            };
            self.osk.open_for(field);
            let _ = self.osk.confirm_open();
            self.apply_osk_xkb();
            self.osk_dirty = true;
        }
    }

    /// Route an OSK character to the desktop settings panel.
    pub fn ds_type_char(&mut self, c: char) {
        if c.is_control() { return; }
        let Some(ds) = self.desktop_settings.as_mut() else { return };
        match ds.mode {
            DsMode::Auth { .. } => {
                ds.auth_buf.push(c);
                let masked: String = "●".repeat(ds.auth_buf.chars().count());
                ds.auth_pw_label = ds_rasterize(&self.text, &masked, 14.0, [170, 178, 196], 300);
            }
            DsMode::HostnameEntry { .. } | DsMode::PwChange { .. } | DsMode::WallpaperImageEntry => {
                ds.text_buf.push(c);
                let show: String = match &ds.mode {
                    DsMode::PwChange { .. } => "●".repeat(ds.text_buf.chars().count()),
                    _ => ds.text_buf.clone(),
                };
                ds.entry_field_label = ds_rasterize(&self.text, &show, 14.0, [200, 210, 230], 300);
            }
            DsMode::Main => {}
        }
    }

    /// Route a backspace to the desktop settings panel.
    pub fn ds_backspace(&mut self) {
        let Some(ds) = self.desktop_settings.as_mut() else { return };
        match ds.mode {
            DsMode::Auth { .. } => {
                ds.auth_buf.pop();
                let masked: String = "●".repeat(ds.auth_buf.chars().count());
                ds.auth_pw_label = if masked.is_empty() {
                    None
                } else {
                    ds_rasterize(&self.text, &masked, 14.0, [170, 178, 196], 300)
                };
            }
            DsMode::HostnameEntry { .. } | DsMode::PwChange { .. } | DsMode::WallpaperImageEntry => {
                ds.text_buf.pop();
                let show: String = match &ds.mode {
                    DsMode::PwChange { .. } => "●".repeat(ds.text_buf.chars().count()),
                    _ => ds.text_buf.clone(),
                };
                ds.entry_field_label = if show.is_empty() {
                    None
                } else {
                    ds_rasterize(&self.text, &show, 14.0, [200, 210, 230], 300)
                };
            }
            DsMode::Main => {}
        }
    }

    /// Poll ds_rx; apply result if ready.
    pub fn ds_tick(&mut self) -> bool {
        let result = self.ds_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(r) = result {
            self.ds_rx = None;
            self.ds_apply_result(r);
            return true;
        }
        // Keep redrawing while waiting for auth result.
        self.ds_rx.is_some() && self.desktop_settings.is_some()
    }

    // -----------------------------------------------------------------------
    // File browser (opened from Desktop Settings "Resim Yolu" row)

    pub fn open_file_browser(&mut self, out: OutputId) {
        use std::path::PathBuf;
        let initial_dir = dirs::home_dir()
            .map(|h| { let p = h.join("Pictures"); if p.exists() { p } else { h } })
            .unwrap_or_else(|| PathBuf::from("/"));

        let Some(bounds) = self.wm.output(out).map(|o| o.bounds) else { return };

        const PW: f32 = 460.0;
        const PH: f32 = 560.0;
        const TITLE_H: f32 = 44.0;
        const PATH_H: f32 = 28.0;
        const BOT_H: f32 = 50.0;
        const PAD: f32 = 12.0;
        const ROW_H: f32 = 44.0;
        const SB_W: f32 = 14.0; // scrollbar width

        let px = bounds.x as f32 + (bounds.w as f32 - PW) / 2.0;
        let py = bounds.y as f32 + (bounds.h as f32 - PH) / 2.0;
        let panel = Rect { x: px, y: py, w: PW, h: PH };

        let list_x = px + PAD;
        let list_y = py + TITLE_H + PATH_H;
        // Leave SB_W + 2px on the right for the scrollbar
        let list_w = PW - PAD * 2.0 - SB_W - 2.0;
        let list_h = PH - TITLE_H - PATH_H - BOT_H;
        let list_rect = Rect { x: list_x, y: list_y, w: list_w, h: list_h };

        let sb_x = px + PAD + list_w + 2.0;
        let scrollbar_rect = Rect { x: sb_x, y: list_y, w: SB_W, h: list_h };

        let btn_y = py + PH - BOT_H + (BOT_H - 32.0) / 2.0;
        let cancel_rect    = Rect { x: px + PAD,         y: btn_y, w: 80.0, h: 32.0 };
        let home_rect      = Rect { x: px + PAD + 88.0,  y: btn_y, w: 90.0, h: 32.0 };
        let up_rect        = Rect { x: px + PAD + 186.0, y: btn_y, w: 82.0, h: 32.0 };
        let scroll_up_rect = Rect { x: px + PAD + 276.0, y: btn_y, w: 72.0, h: 32.0 };
        let scroll_dn_rect = Rect { x: px + PAD + 356.0, y: btn_y, w: 72.0, h: 32.0 };

        let entries = Self::fb_build_entries_for(&initial_dir, &self.text, ROW_H);
        let scroll_max = (entries.len() as f32 * ROW_H - list_h).max(0.0);

        let title_label      = ds_rasterize(&self.text, "Resim Seç",                        14.0, [220, 230, 255], 400);
        let path_label       = ds_rasterize(&self.text, &Self::fb_short_path(&initial_dir), 11.0, [140, 150, 170], 430);
        let cancel_label     = ds_rasterize(&self.text, "İptal",                            12.0, [200, 210, 230],  80);
        let home_label       = ds_rasterize(&self.text, "Ana Dizin",                        12.0, [170, 210, 255],  90);
        let up_label         = ds_rasterize(&self.text, "Ust Dizin",                        12.0, [170, 200, 255],  82);
        let scroll_up_label  = ds_rasterize(&self.text, "Yukari",                           12.0, [200, 220, 255],  72);
        let scroll_dn_label  = ds_rasterize(&self.text, "Asagi",                            12.0, [200, 220, 255],  72);

        const ICON_SZ: u32 = 28;
        let icon_folder = fb_load_icon("folder",           ICON_SZ);
        let icon_image  = fb_load_icon("image-x-generic",  ICON_SZ);

        self.file_browser = Some(FileBrowserPanel {
            output: out,
            panel,
            list_rect,
            current_dir: initial_dir,
            entries,
            scroll_y: 0.0,
            scroll_max,
            row_h: ROW_H,
            drag_start: None,
            drag_slot: None,
            drag_moved: false,
            pressed_idx: None,
            title_label,
            path_label,
            cancel_rect,
            cancel_label,
            home_rect,
            home_label,
            up_rect,
            up_label,
            scroll_up_rect,
            scroll_up_label,
            scroll_dn_rect,
            scroll_dn_label,
            scrollbar_rect,
            icon_folder,
            icon_image,
        });
    }

    fn fb_short_path(p: &std::path::Path) -> String {
        let s = p.to_string_lossy();
        if s.len() <= 50 { s.into_owned() } else { format!("…{}", &s[s.len()-49..]) }
    }

    fn fb_build_entries_for(
        dir: &std::path::Path,
        text: &Option<crate::text::TextRenderer>,
        row_h: f32,
    ) -> Vec<FbEntry> {
        let _ = row_h;
        let mut entries: Vec<FbEntry> = Vec::new();

        // ".." parent entry
        if let Some(parent) = dir.parent() {
            entries.push(FbEntry {
                label: ds_rasterize(text, "..  (üst dizin)", 12.0, [160, 168, 190], 380),
                name: "..".to_string(),
                path: parent.to_path_buf(),
                is_dir: true,
                is_image: false,
            });
        }

        let Ok(rd) = std::fs::read_dir(dir) else { return entries };
        let mut dirs: Vec<(String, std::path::PathBuf)> = Vec::new();
        let mut images: Vec<(String, std::path::PathBuf)> = Vec::new();
        let mut others: Vec<(String, std::path::PathBuf)> = Vec::new();

        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') { continue; }
            let path = e.path();
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                dirs.push((name, path));
            } else if FileBrowserPanel::is_image_path(&path) {
                images.push((name, path));
            } else {
                others.push((name, path));
            }
        }
        dirs.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
        images.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
        others.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));

        for (name, path) in dirs {
            entries.push(FbEntry {
                label: ds_rasterize(text, &name, 12.0, [150, 190, 255], 380),
                name, path, is_dir: true, is_image: false,
            });
        }
        for (name, path) in images {
            entries.push(FbEntry {
                label: ds_rasterize(text, &name, 12.0, [130, 220, 150], 380),
                name, path, is_dir: false, is_image: true,
            });
        }
        for (name, path) in others {
            entries.push(FbEntry {
                label: ds_rasterize(text, &name, 12.0, [80, 86, 100], 380),
                name, path, is_dir: false, is_image: false,
            });
        }
        entries
    }

    fn fb_navigate(&mut self, path: std::path::PathBuf) {
        let Some(fb) = self.file_browser.as_mut() else { return };
        let row_h = fb.row_h;
        let list_h = fb.list_rect.h as f32;
        fb.current_dir = path.clone();
        fb.entries = Self::fb_build_entries_for(&path, &self.text, row_h);
        fb.scroll_max = (fb.entries.len() as f32 * row_h - list_h).max(0.0);
        fb.scroll_y = 0.0;
        fb.drag_start = None;
        fb.drag_slot = None;
        fb.drag_moved = false;
        fb.pressed_idx = None;
        fb.path_label = ds_rasterize(&self.text, &Self::fb_short_path(&path), 11.0, [140, 150, 170], 430);
    }

    fn fb_activate(&mut self, idx: usize) -> bool {
        let entry = self.file_browser.as_ref()
            .and_then(|fb| fb.entries.get(idx))
            .map(|e| (e.path.clone(), e.is_dir, e.is_image));
        let Some((path, is_dir, is_image)) = entry else { return true };
        if is_dir {
            self.fb_navigate(path);
        } else if is_image {
            let path_str = path.to_string_lossy().into_owned();
            self.config.wallpaper_image = Some(path_str.clone());
            self.file_browser = None;
            // Kick off async load into wallpaper_cache
            let (tx, rx) = std::sync::mpsc::channel();
            let out_dims = self.wm.outputs().into_iter().next().map(|o| (o.bounds.w as u32, o.bounds.h as u32));
            if let Some((ow, oh)) = out_dims {
                let path2 = path_str;
                std::thread::spawn(move || {
                    let result = crate::state::load_wallpaper_buffer(&path2, ow, oh)
                        .map(|buf| (buf, ow, oh))
                        .ok_or(());
                    let _ = tx.send(DsResult::WallpaperLoaded(result));
                });
                self.ds_rx = Some(rx);
                if let Some(ds) = self.desktop_settings.as_mut() {
                    ds.status = ds_rasterize(&self.text, "Resim yükleniyor...", 13.0, [170, 178, 196], 300);
                    ds.status_ok = true;
                }
            }
        }
        true
    }

    pub fn fb_press(&mut self, px: f32, py: f32) -> bool {
        let Some(fb) = self.file_browser.as_ref() else { return false };

        // Cancel
        let cr = fb.cancel_rect;
        if Self::rect_hit(cr, px, py) { self.file_browser = None; return true; }
        // Home
        let hr = fb.home_rect;
        if Self::rect_hit(hr, px, py) {
            let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"));
            self.fb_navigate(home); return true;
        }
        // Up (parent dir)
        let ur = fb.up_rect;
        if Self::rect_hit(ur, px, py) {
            let parent = fb.current_dir.parent().map(|p| p.to_path_buf());
            if let Some(p) = parent { self.fb_navigate(p); } return true;
        }
        // Scroll up (3 rows)
        let su = fb.scroll_up_rect;
        if Self::rect_hit(su, px, py) {
            if let Some(fb) = self.file_browser.as_mut() {
                fb.scroll_y = (fb.scroll_y - fb.row_h * 3.0).max(0.0);
            }
            return true;
        }
        // Scroll down (3 rows)
        let sd = fb.scroll_dn_rect;
        if Self::rect_hit(sd, px, py) {
            if let Some(fb) = self.file_browser.as_mut() {
                fb.scroll_y = (fb.scroll_y + fb.row_h * 3.0).min(fb.scroll_max);
            }
            return true;
        }
        // List area
        let lr = fb.list_rect;
        if Self::rect_hit(lr, px, py) {
            let fb = self.file_browser.as_mut().unwrap();
            let rel_y = (py - lr.y) + fb.scroll_y;
            let idx = (rel_y / fb.row_h) as usize;
            fb.pressed_idx = if idx < fb.entries.len() { Some(idx) } else { None };
            fb.drag_start = Some((py, fb.scroll_y));
            fb.drag_moved = false;
            return true;
        }
        // Click outside panel → close
        let p = fb.panel;
        if !Self::rect_hit(p, px, py) { self.file_browser = None; }
        true
    }

    pub fn fb_touch_press(&mut self, px: f32, py: f32, slot: i32) -> bool {
        if let Some(fb) = self.file_browser.as_mut() { fb.drag_slot = Some(slot); }
        self.fb_press(px, py)
    }

    pub fn fb_pointer_motion(&mut self, gy: f32) -> bool {
        let Some(fb) = self.file_browser.as_mut() else { return false };
        if let Some((start_y, scroll_start)) = fb.drag_start {
            let delta = start_y - gy;
            if delta.abs() > 8.0 { fb.drag_moved = true; }
            fb.scroll_y = (scroll_start + delta).clamp(0.0, fb.scroll_max);
            return true;
        }
        false
    }

    pub fn fb_pointer_release(&mut self, _px: f32, _py: f32) -> bool {
        let Some(fb) = self.file_browser.as_ref() else { return false };
        let moved = fb.drag_moved;
        let idx   = fb.pressed_idx;
        let fb = self.file_browser.as_mut().unwrap();
        fb.drag_start = None;
        fb.pressed_idx = None;
        fb.drag_moved = false;
        if !moved {
            if let Some(i) = idx { return self.fb_activate(i); }
        }
        true
    }

    pub fn fb_touch_motion(&mut self, ty: f32, slot: i32) -> bool {
        if self.file_browser.as_ref().map_or(true, |fb| fb.drag_slot != Some(slot)) { return false; }
        self.fb_pointer_motion(ty)
    }

    pub fn fb_touch_up(&mut self, slot: i32) -> bool {
        let Some(fb) = self.file_browser.as_ref() else { return false };
        if fb.drag_slot != Some(slot) { return false; }
        let moved = fb.drag_moved;
        let idx   = fb.pressed_idx;
        let fb = self.file_browser.as_mut().unwrap();
        fb.drag_start = None;
        fb.drag_slot = None;
        fb.drag_moved = false;
        fb.pressed_idx = None;
        if !moved {
            if let Some(i) = idx { return self.fb_activate(i); }
        }
        true
    }

    fn rect_hit(r: Rect, px: f32, py: f32) -> bool {
        px >= r.x && px < r.x + r.w && py >= r.y && py < r.y + r.h
    }
}

/// Verify `pw` is the root password by running `sudo -S true` with it.
fn verify_root_password(pw: &str) -> bool {
    use std::io::Write;
    let mut child = match std::process::Command::new("sudo")
        .args(["-S", "-u", "root", "true"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(format!("{pw}\n").as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

/// Change hostname via hostnamectl (pipes root password to sudo -S).
fn ds_set_hostname(name: &str, root_pw: &str) -> bool {
    use std::io::Write;
    let mut child = match std::process::Command::new("sudo")
        .args(["-S", "hostnamectl", "set-hostname", name])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(format!("{root_pw}\n").as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

/// Write/clear /etc/bacak/autologin (pipes root password to sudo -S tee).
fn ds_set_autologin(user: &str, root_pw: &str) -> bool {
    use std::io::Write;
    let content = format!("{user}\n");
    // sudo -S reads password from stdin, but tee also reads from stdin.
    // Pipe password via a wrapper: echo "<pw>" | sudo -S tee won't work directly.
    // Instead spawn with stdin piped and write password + \n first, then content.
    // sudo -S: reads one line from stdin for the password, then the subprocess
    // (tee) reads the rest of stdin as data.
    let mut child = match std::process::Command::new("sudo")
        .args(["-S", "tee", "/etc/bacak/autologin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(format!("{root_pw}\n").as_bytes());
        let _ = stdin.write_all(content.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

/// Change a user's password via chpasswd (pipes root password to sudo -S).
fn ds_set_password(user: &str, new_pw: &str, root_pw: &str) -> bool {
    use std::io::Write;
    let mut child = match std::process::Command::new("sudo")
        .args(["-S", "chpasswd"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(format!("{root_pw}\n").as_bytes());
        let _ = stdin.write_all(format!("{user}:{new_pw}\n").as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

/// Decode a wallpaper image (PNG/JPEG) from `path`, scale it to cover
/// `(out_w, out_h)` maintaining aspect ratio, and return a
/// `MemoryRenderBuffer` ready for blitting.  Returns `None` on any
/// decode or I/O failure.
pub(crate) fn load_wallpaper_buffer(
    path: &str,
    out_w: u32,
    out_h: u32,
) -> Option<smithay::backend::renderer::element::memory::MemoryRenderBuffer> {
    use image::imageops::FilterType;
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
    use smithay::utils::Transform;

    let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
    let (iw, ih) = (img.width(), img.height());

    // Scale to cover: pick the axis that needs the larger factor.
    let sx = out_w as f32 / iw as f32;
    let sy = out_h as f32 / ih as f32;
    let s  = sx.max(sy);
    let sw = (iw as f32 * s).ceil() as u32;
    let sh = (ih as f32 * s).ceil() as u32;

    // Resize then centre-crop to exact output size.
    let scaled = image::imageops::resize(&img.to_rgba8(), sw, sh, FilterType::Triangle);
    let cx = (sw.saturating_sub(out_w)) / 2;
    let cy = (sh.saturating_sub(out_h)) / 2;
    let cropped = image::imageops::crop_imm(&scaled, cx, cy, out_w, out_h).to_image();

    // Abgr8888 = [R,G,B,A] in memory — matches image crate's to_rgba8() directly.
    let rgba = cropped.into_raw();

    Some(MemoryRenderBuffer::from_slice(
        &rgba,
        Fourcc::Abgr8888,
        (out_w as i32, out_h as i32),
        1,
        Transform::Normal,
        None,
    ))
}

/// Load a named icon from the system icon theme and scale it to `size`×`size`.
/// Returns a MemoryRenderBuffer in Abgr8888 ([R,G,B,A]) format, or None.
pub(crate) fn fb_load_icon(name: &str, size: u32) -> Option<MemoryRenderBuffer> {
    use image::imageops::FilterType;
    use smithay::utils::Transform;
    let (rgba, w, h) = crate::icons::resolve_icon_rgba(name)?;
    let img = image::RgbaImage::from_raw(w, h, rgba)?;
    let scaled = image::imageops::resize(&img, size, size, FilterType::Triangle);
    let raw = scaled.into_raw();
    Some(MemoryRenderBuffer::from_slice(
        &raw, Fourcc::Abgr8888, (size as i32, size as i32), 1, Transform::Normal, None,
    ))
}

#[cfg(test)]
mod dock_autohide_tests {
    //! Headless tests for the window-presence dock auto-hide + floating-button
    //! toggle (2026-05-27). Uses the real `BacakState` + WM; `dock_reveal_should
    //! _show_for` / `has_visible_window_on` are private but reachable from this
    //! child module.
    use super::*;
    use smithay::reexports::wayland_server::Display;
    use std::time::{Duration, Instant};

    fn dock_state() -> (BacakState, OutputId) {
        let display: Display<BacakState> = Display::new().unwrap();
        let mut state = BacakState::new(&display, "dock-autohide-test-seat");
        state.config.dock = true;
        state.config.dock_autohide = true;
        let out = state.wm.primary_output().unwrap();
        (state, out)
    }

    #[test]
    fn empty_desktop_shows_dock_and_floating_button_exists() {
        let (state, out) = dock_state();
        assert!(state.dock_reveal_should_show_for(out), "no windows → dock shown");
        assert!(
            state.dock_floating_button_rect(out).is_some(),
            "floating button present when the dock can hide"
        );
    }

    #[test]
    fn open_window_hides_dock_then_button_summons_it() {
        let (mut state, out) = dock_state();
        state.wm.open("app", "A", Rect::new(0.0, 0.0, 800.0, 600.0));
        assert!(!state.dock_reveal_should_show_for(out), "a window hides the dock");

        // The floating button toggles the override → summoned.
        let r = state.dock_floating_button_rect(out).unwrap();
        assert!(state.dock_floating_button_press(r.x + r.w / 2.0, r.y + r.h / 2.0));
        assert!(state.dock_force_shown);
        assert!(state.dock_reveal_should_show_for(out), "summoned over a window");

        // A press that misses the button does nothing.
        assert!(!state.dock_floating_button_press(r.x + 10_000.0, r.y));
    }

    #[test]
    fn opening_an_app_clears_the_summon() {
        let (mut state, out) = dock_state();
        state.wm.open("a", "A", Rect::new(0.0, 0.0, 800.0, 600.0));
        state.toggle_dock();
        assert!(state.dock_reveal_should_show_for(out));

        // Launching/restoring another window is a rising edge → drops override.
        state.wm.open("b", "B", Rect::new(0.0, 0.0, 800.0, 600.0));
        let t0 = Instant::now();
        state.last_reveal_tick = t0;
        state.tick_dock_reveal(t0 + Duration::from_millis(16));
        assert!(!state.dock_force_shown, "opening an app re-tucks a summoned dock");
        assert!(!state.dock_reveal_should_show_for(out));
    }

    #[test]
    fn no_floating_button_without_autohide() {
        let (mut state, out) = dock_state();
        state.config.dock_autohide = false;
        assert!(state.dock_floating_button_rect(out).is_none());
    }

    /// A state with a scrollable apps menu (60 synthetic apps) on the
    /// primary output, built without touching the filesystem.
    fn apps_menu_state() -> BacakState {
        let display: Display<BacakState> = Display::new().unwrap();
        let mut state = BacakState::new(&display, "apps-menu-test-seat");
        let out = state.wm.primary_output().unwrap();
        let all: Vec<(String, String, u8)> = (0..60)
            .map(|i| (format!("app{i}"), format!("App {i}"), 0u8))
            .collect();
        state.apps_menu = Some(state.build_apps_menu(out, all, String::new(), 0, 0.0, 0.0));
        state
    }

    #[test]
    fn apps_menu_single_finger_drag_scrolls_smoothly() {
        let mut state = apps_menu_state();
        let (cell, gx, gy, max) = {
            let m = state.apps_menu.as_ref().unwrap();
            (m.cell, m.grid_x, m.grid_y, m.max_scroll_y())
        };
        assert!(max > 0.0, "list must overflow the viewport to be scrollable");

        // Press inside the grid → claims a drag (does not launch yet).
        let (px, py) = (gx + cell * 0.5, gy + cell * 0.5);
        assert_eq!(state.apps_menu_touch_down(px, py), AppsTouch::Drag);
        assert!(state.apps_menu_drag.is_some());

        // Drag up by 30px → grid follows the finger by exactly 30px (sub-row,
        // not quantised).
        state.apps_menu_touch_motion(py - 30.0);
        assert!((state.apps_menu.as_ref().unwrap().scroll.pos - 30.0).abs() < 0.01);

        // It moved past the slop → release is a scroll, not a launch, and
        // the menu stays open.
        assert!(!state.apps_menu_touch_up());
        assert!(state.apps_menu.is_some());
        assert!(state.apps_menu_drag.is_none());
    }

    #[test]
    fn apps_menu_drag_scroll_clamps_at_both_ends() {
        let mut state = apps_menu_state();
        let (cell, gx, gy, max) = {
            let m = state.apps_menu.as_ref().unwrap();
            (m.cell, m.grid_x, m.grid_y, m.max_scroll_y())
        };
        // Huge upward drag saturates at max_scroll_y, never beyond.
        let (px, py) = (gx + cell * 0.5, gy + cell * 0.5);
        assert_eq!(state.apps_menu_touch_down(px, py), AppsTouch::Drag);
        state.apps_menu_touch_motion(py - 999.0 * cell);
        assert!((state.apps_menu.as_ref().unwrap().scroll.pos - max as f64).abs() < 0.01);
        // And a downward drag from there can't go below zero.
        state.apps_menu_touch_motion(py + 999.0 * cell);
        assert_eq!(state.apps_menu.as_ref().unwrap().scroll.pos, 0.0);
    }

    #[test]
    fn apps_menu_flick_release_coasts_with_inertia() {
        let mut state = apps_menu_state();
        let (cell, gx, gy) = {
            let m = state.apps_menu.as_ref().unwrap();
            (m.cell, m.grid_x, m.grid_y)
        };
        let (px, py) = (gx + cell * 0.5, gy + cell * 0.5);
        assert_eq!(state.apps_menu_touch_down(px, py), AppsTouch::Drag);

        // Fast upward flick: a few motions building velocity.
        let mut y = py;
        for _ in 0..5 {
            y -= 10.0;
            state.apps_menu_touch_motion(y);
        }
        let released_at = state.apps_menu.as_ref().unwrap().scroll.pos;
        assert!(
            state.apps_menu_drag.as_ref().unwrap().velocity > 0.0,
            "an upward flick builds positive scroll velocity"
        );

        // Release → fling seeds a target ahead of where the finger left off.
        assert!(!state.apps_menu_touch_up());
        let target = state.apps_menu.as_ref().unwrap().scroll.target;
        assert!(target > released_at, "the fling projects the target forward");
        assert!(!state.apps_menu.as_ref().unwrap().scroll.is_settled());

        // Inertia coasts further over the next frame.
        let t0 = Instant::now();
        state.last_anim_tick = t0;
        assert!(state.tick_animations(t0 + Duration::from_millis(16)));
        assert!(
            state.apps_menu.as_ref().unwrap().scroll.pos > released_at,
            "scroll keeps moving after the finger lifts"
        );
    }

    #[test]
    fn apps_menu_tap_in_grid_is_not_a_scroll() {
        let mut state = apps_menu_state();
        let r = state.apps_menu.as_ref().unwrap().item_rect(0);
        // Press + lift without crossing the slop = a tap. It launches the
        // pressed cell (synthetic id won't spawn) and closes the menu.
        assert_eq!(
            state.apps_menu_touch_down(r.x + r.w * 0.5, r.y + r.h * 0.5),
            AppsTouch::Drag
        );
        assert!(state.apps_menu_touch_up());
        assert!(state.apps_menu.is_none());
        assert!(state.apps_menu_drag.is_none());
    }

    #[test]
    fn apps_menu_touch_outside_dismisses() {
        let mut state = apps_menu_state();
        let p = state.apps_menu.as_ref().unwrap().panel;
        // A press clearly outside the panel dismisses immediately.
        assert_eq!(
            state.apps_menu_touch_down(p.x - 50.0, p.y - 50.0),
            AppsTouch::Consumed
        );
        assert!(state.apps_menu.is_none());
    }
}

#[cfg(test)]
mod selection_menu_tests {
    //! Floating action-menu layout + hit-test, on the real `BacakState`/WM.
    //! Key synthesis is a no-op here (no keyboard is added to the seat outside
    //! a backend), so `selection_menu_press` exercises routing without input.
    use super::*;
    use smithay::reexports::wayland_server::Display;

    fn state() -> BacakState {
        let display: Display<BacakState> = Display::new().unwrap();
        BacakState::new(&display, "selection-menu-test-seat")
    }

    #[test]
    fn open_lays_out_four_buttons_left_to_right() {
        let mut s = state();
        s.open_selection_menu(960.0, 540.0);
        let menu = s.floating_menu.as_ref().expect("menu opened");
        assert_eq!(menu.items.len(), 4);
        assert_eq!(menu.buttons.len(), 4);
        // Buttons tile the plaque with no gaps and stay within it.
        assert!((menu.buttons[0].x - menu.rect.x).abs() < 0.01);
        for w in menu.buttons.windows(2) {
            assert!((w[1].x - (w[0].x + w[0].w)).abs() < 0.01, "buttons are contiguous");
        }
        let last = menu.buttons.last().unwrap();
        assert!(last.x + last.w <= menu.rect.x + menu.rect.w + 0.01);
    }

    #[test]
    fn item_at_maps_points_to_actions() {
        let mut s = state();
        s.open_selection_menu(960.0, 540.0);
        let menu = s.floating_menu.clone().unwrap();
        // Centre of the first button → Copy; centre of the third → Select all.
        let b0 = menu.buttons[0];
        assert_eq!(menu.item_at(b0.x + b0.w / 2.0, b0.y + b0.h / 2.0), Some(0));
        assert_eq!(menu.items[0].action, SelectionAction::Copy);
        let b2 = menu.buttons[2];
        assert_eq!(menu.items[menu.item_at(b2.x + 1.0, b2.y + 1.0).unwrap()].action,
                   SelectionAction::SelectAll);
        // A point clearly outside is no button.
        assert_eq!(menu.item_at(menu.rect.x - 50.0, menu.rect.y), None);
    }

    #[test]
    fn press_on_button_consumes_and_closes() {
        let mut s = state();
        s.open_selection_menu(960.0, 540.0);
        let b1 = s.floating_menu.as_ref().unwrap().buttons[1];
        assert!(s.selection_menu_press(b1.x + b1.w / 2.0, b1.y + b1.h / 2.0));
        assert!(s.floating_menu.is_none(), "menu closes after an action");
    }

    #[test]
    fn off_menu_press_dismisses_and_consumes() {
        let mut s = state();
        s.open_selection_menu(960.0, 540.0);
        let off_y = s.floating_menu.as_ref().unwrap().rect.y - 80.0;
        assert!(s.selection_menu_press(960.0, off_y), "press is consumed");
        assert!(s.floating_menu.is_none(), "off-menu press dismisses");
    }

    #[test]
    fn press_with_no_menu_is_not_consumed() {
        let mut s = state();
        assert!(!s.selection_menu_press(10.0, 10.0));
    }

    #[test]
    fn text_panel_toggles() {
        let mut s = state();
        let out = s.wm.primary_output().unwrap();
        s.toggle_text_panel(out);
        assert!(s.text_panel.is_some(), "Super+T opens the panel");
        s.toggle_text_panel(out);
        assert!(s.text_panel.is_none(), "again closes it");
    }

    #[test]
    fn select_all_then_copy_puts_demo_text_on_clipboard() {
        // Font-independent: select_all + selected_text read the logical line
        // buffer, not glyph geometry, so this runs without a system font.
        let mut s = state();
        let out = s.wm.primary_output().unwrap();
        s.toggle_text_panel(out);
        s.run_selection_action(SelectionAction::SelectAll);
        assert!(s.text_panel.as_ref().unwrap().native.has_selection());
        s.run_selection_action(SelectionAction::Copy);
        assert_eq!(s.clipboard_text.as_deref(), Some(SEL_PANEL_DEMO));
    }

    #[test]
    fn off_panel_press_dismisses_panel() {
        let mut s = state();
        let out = s.wm.primary_output().unwrap();
        s.toggle_text_panel(out);
        let r = s.text_panel.as_ref().unwrap().rect;
        // A press well outside the plaque closes the panel and is consumed.
        assert!(s.text_panel_press(r.x - 100.0, r.y - 100.0));
        assert!(s.text_panel.is_none());
    }

    #[test]
    fn open_panel_caches_a_glyph_buffer_when_a_font_is_present() {
        let mut s = state();
        let out = s.wm.primary_output().unwrap();
        s.toggle_text_panel(out);
        // With a system font the rasteriser yields a cached upload buffer; with
        // none it's `None` and the panel still works (text just doesn't draw).
        let panel = s.text_panel.as_ref().unwrap();
        if let Some((_buf, w, h)) = panel.bitmap.as_ref() {
            assert!(*w > 0 && *h > 0, "cached glyph buffer has a real size");
        }
    }

    #[test]
    fn double_tap_in_panel_selects_a_word() {
        // Full path: recogniser → route_tap_gesture → NativeText word select.
        // Needs a font for hit-testing, so skip if none is installed.
        let mut s = state();
        let out = s.wm.primary_output().unwrap();
        s.toggle_text_panel(out);
        let (has_font, ox, oy) = {
            let p = s.text_panel.as_ref().unwrap();
            (p.bitmap.is_some(), p.text_origin.0, p.text_origin.1)
        };
        if !has_font {
            return;
        }
        // Two quick taps at the same spot inside the first word (the test runs
        // far inside the 320 ms multi-tap window).
        let (x, y) = (ox + 24.0, oy + 6.0);
        s.panel_input_down(x, y);
        s.panel_input_up();
        s.panel_input_down(x, y);
        s.panel_input_up();
        assert!(
            s.text_panel.as_ref().unwrap().native.has_selection(),
            "double-tap selects a word"
        );
    }
}

#[cfg(test)]
mod terminal_copy_tests {
    //! `focused_is_terminal` decides whether the floating-menu / right-click
    //! Copy+Paste synthesise Ctrl+Shift+C/V (terminals) instead of plain
    //! Ctrl+C/V. Plain Ctrl+C in a terminal is SIGINT — it interrupts the
    //! running command instead of copying — which is exactly the "right-click
    //! copy fires Ctrl+C and doesn't copy" report this guards against.
    use super::*;
    use smithay::reexports::wayland_server::Display;

    fn test_state() -> BacakState {
        let display: Display<BacakState> = Display::new().unwrap();
        BacakState::new(&display, "term-copy-test-seat")
    }

    fn open_focused(state: &mut BacakState, app: &str) {
        let id = state.wm.open(app, "t", Rect::new(0.0, 0.0, 100.0, 100.0));
        let _ = state.wm.focus(id);
    }

    #[test]
    fn common_terminal_app_ids_are_detected() {
        for app in [
            "mate-terminal",
            "Mate-terminal",
            "org.gnome.Terminal",
            "konsole",
            "Alacritty",
            "foot",
            "xterm",
            "kitty",
        ] {
            let mut state = test_state();
            open_focused(&mut state, app);
            assert!(state.focused_is_terminal(), "expected terminal for app_id {app:?}");
        }
    }

    #[test]
    fn non_terminal_app_is_not_detected() {
        let mut state = test_state();
        open_focused(&mut state, "firefox");
        assert!(!state.focused_is_terminal());
    }

    #[test]
    fn no_focused_window_is_not_terminal() {
        let state = test_state();
        assert!(!state.focused_is_terminal());
    }

    #[test]
    fn flash_alpha_is_output_gated_and_fades() {
        let mut state = test_state();
        assert!(state.flash_alpha(0).is_none(), "no flash → no alpha");
        state.flash = Some((0u32, std::time::Instant::now()));
        let a = state.flash_alpha(0).expect("flash on output 0 is visible");
        assert!(a > 0.0 && a <= 0.6, "alpha within (0, peak]: {a}");
        assert!(state.flash_alpha(7).is_none(), "flash is gated to its output");
        // After the duration it has faded out.
        state.flash = Some((0u32, std::time::Instant::now() - std::time::Duration::from_millis(crate::state::FLASH_MS + 50)));
        assert!(state.flash_alpha(0).is_none(), "expired flash → no alpha");
    }

    #[test]
    fn window_shot_rect_includes_title_bar_when_decorated() {
        use crate::decoration::BAR_H;
        let mut state = test_state();
        let id = state.wm.open("app", "w", Rect::new(100.0, 100.0, 400.0, 300.0));
        // Undecorated → just the content rect.
        let r = state.window_shot_rect(id).unwrap();
        assert_eq!((r.x, r.y, r.w, r.h), (100.0, 100.0, 400.0, 300.0));
        // Decorated → grows upward by BAR_H to include the SSD title bar.
        state.decorated.insert(id);
        let r = state.window_shot_rect(id).unwrap();
        assert_eq!((r.x, r.y, r.w, r.h), (100.0, 100.0 - BAR_H, 400.0, 300.0 + BAR_H));
    }

    #[test]
    fn region_shot_rect_normalises_drag_direction() {
        // Dragging up-left (anchor below-right of cur) must still yield a
        // positive-size rect at the top-left corner — the capture region.
        let rs = RegionShot { output: 0u32, anchor: Some((300.0, 200.0)), cur: (100.0, 50.0) };
        let r = rs.rect().expect("anchored drag has a rect");
        assert_eq!((r.x, r.y, r.w, r.h), (100.0, 50.0, 200.0, 150.0));

        // No press yet → no rect.
        let pending = RegionShot { output: 0u32, anchor: None, cur: (10.0, 10.0) };
        assert!(pending.rect().is_none());

        // A bare click (no drag) clamps to a 1px minimum, not zero.
        let click = RegionShot { output: 0u32, anchor: Some((5.0, 5.0)), cur: (5.0, 5.0) };
        let r = click.rect().unwrap();
        assert!(r.w >= 1.0 && r.h >= 1.0);
    }

    #[test]
    fn raising_parent_keeps_child_dialog_on_top() {
        // A transient dialog must stay above its parent even after the parent is
        // (re)focused — else a "Save changes?" prompt hides behind the window
        // and the app can't be closed. `focus(parent)` puts the parent on top;
        // `raise_child_dialogs` must pull the dialog back above it.
        let mut state = test_state();
        let parent = state.wm.open("app", "main", Rect::new(0.0, 0.0, 600.0, 400.0));
        let dialog = state.wm.open("app", "Save?", Rect::new(100.0, 100.0, 200.0, 120.0));
        state.dialog_parent.insert(dialog, parent);

        // Simulate the parent being raised over the dialog (click / FFP).
        let _ = state.wm.focus(parent);
        assert!(
            state.wm.get(parent).unwrap().z > state.wm.get(dialog).unwrap().z,
            "focusing the parent raises it above the dialog (the bug)"
        );

        // The fix restores the invariant.
        state.raise_child_dialogs(parent);
        assert!(
            state.wm.get(dialog).unwrap().z > state.wm.get(parent).unwrap().z,
            "child dialog must be stacked back above its parent"
        );
    }

    #[test]
    fn open_selection_menu_records_target_window_for_refocus() {
        // Copy/Paste re-focus `selection_menu_target` before synthesising, so a
        // menu opened over a window must capture that window — otherwise the
        // synthesised chord lands nowhere (the observed `has_kbd_focus=false`
        // copy failure over an XWayland terminal).
        let mut state = test_state();
        let id = state.wm.open("mate-terminal", "t", Rect::new(0.0, 0.0, 500.0, 400.0));
        let _ = state.wm.focus(id);
        state.open_selection_menu(100.0, 100.0);
        assert_eq!(state.selection_menu_target, Some(id), "menu must remember its target window");
        state.close_selection_menu();
        assert_eq!(state.selection_menu_target, None, "close clears the target");
    }
}
