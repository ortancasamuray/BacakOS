//! Native-session backend: libseat + libudev + libinput + DRM/KMS + GBM + EGL.
//!
//! This is the "real" runtime — the one that boots the compositor without a
//! parent display server. It mirrors Smithay's `anvil` udev backend but pares
//! the scope down to **one GPU, one connected output, no hot-plug**, which is
//! what Bacak needs to drive a tablet / laptop session today. Multi-output and
//! hot-plug are tracked in follow-up milestones; the architecture here was
//! chosen so they slot in without rewiring the event loop.
//!
//! # Pipeline
//!
//! ```text
//!   libseat session ──► open(/dev/dri/cardN) ──► DrmDevice ──┐
//!         │                                                   │
//!         ├──► open(/dev/input/eventN) via libinput ──► seat  │
//!         │                                                   ▼
//!         └──► UdevBackend (used only for primary_gpu lookup) │
//!                                                             │
//!   gbm::Device(card_fd) ──► EGLDisplay ──► EGLContext ──► GlesRenderer
//!                                                             │
//!   DrmSurface(connector, mode, crtc) + GbmAllocator ──► DrmCompositor
//!                                                             │
//!   ListeningSocket(wayland-bacak-0) ◄── Display<BacakState> ─┘
//! ```
//!
//! Compiled only with the `udev` feature.

#![cfg(feature = "udev")]

use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use calloop::{EventLoop, LoopHandle, LoopSignal};
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::drm::compositor::{DrmCompositor, FrameFlags};
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmEvent};
use smithay::wayland::drm_syncobj::{
    supports_syncobj_eventfd, DrmSyncPointSource, DrmSyncobjState,
};
use smithay::backend::egl::context::EGLContext;
use smithay::backend::egl::display::EGLDisplay;
use smithay::backend::input::{
    AbsolutePositionEvent, ButtonState, Event as _, GestureBeginEvent, GestureEndEvent as _,
    GesturePinchUpdateEvent as _, GestureSwipeUpdateEvent, InputEvent, KeyState, KeyboardKeyEvent,
    PointerButtonEvent, PointerMotionEvent, ProximityState, TabletToolButtonEvent, TabletToolEvent,
    TabletToolProximityEvent, TabletToolTipEvent, TabletToolTipState, TouchEvent,
};
use smithay::wayland::tablet_manager::{TabletDescriptor, TabletSeatTrait};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::{Bind, Color32F, Offscreen};
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev::{primary_gpu, UdevBackend, UdevEvent};
use smithay::input::keyboard::{FilterResult, KeyboardHandle};
use smithay::input::pointer::{ButtonEvent, MotionEvent, PointerHandle};
use smithay::input::touch::{DownEvent, MotionEvent as TouchMotionEvent, TouchHandle, UpEvent};
use smithay::output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel};
use smithay::backend::allocator::Fourcc as DrmFourcc;
use smithay::wayland::dmabuf::DmabufFeedbackBuilder;
use smithay::reexports::drm::control::{connector, crtc, Device as DrmControlDevice, ModeTypeFlags};
use smithay::reexports::input::Libinput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Display, DisplayHandle, ListeningSocket};
use smithay::wayland::compositor::CompositorHandler;
use smithay::utils::{
    Buffer as BufferCoord, DeviceFd, Logical, Physical, Point, Rectangle, Scale, SERIAL_COUNTER,
    Size as UtilsSize, Transform,
};
use smithay::wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState};
use smithay::wayland::selection::SelectionTarget;
use smithay::xwayland::xwm::{Reorder, ResizeEdge, WmWindowProperty, XwmId};
use smithay::xwayland::{X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler};
use tracing::{error, info, warn};

use std::collections::HashSet;

use crate::config::{CompositorConfig, ConfigWatcher};
use crate::hotplug::{ConnectorSnapshot, HotplugChange, OutputRegistry};
use crate::input::Gesture;
use crate::render::{build_output_frame, build_styled_elements_for_output, BacakElements};
use crate::state::BacakState;
use crate::wm::{FocusPolicy, OutputId, Rect as WmRect};

/// Wayland socket name (matches `crate::runtime::SOCKET`). Clients use it via
/// the `WAYLAND_DISPLAY` env var.
const SOCKET: &str = "wayland-bacak-0";

/// Aegean deep — same clear colour as the winit backend so the desktop
/// presentation is identical across both paths.
const CLEAR_COLOR: Color32F = Color32F::new(0.024, 0.165, 0.239, 1.0);

/// `BTN_LEFT` from `linux/input-event-codes.h` — the primary click
/// that activates a dock tile.
const BTN_LEFT: u32 = 0x110;
/// `BTN_RIGHT` from `linux/input-event-codes.h` — opens the dock's
/// per-tile context menu.
const BTN_RIGHT: u32 = 0x111;
/// `BTN_MIDDLE` — middle-click closes the hovered card in the Overview.
const BTN_MIDDLE: u32 = 0x112;


/// Concrete DrmCompositor we instantiate. The four generics are:
///
/// * `A = GbmAllocator<DrmDeviceFd>` — primary-plane swapchain allocator,
///   shares the same DRM fd as the device.
/// * `F = GbmFramebufferExporter<DrmDeviceFd>` — turns GBM BOs into DRM
///   framebuffers for scan-out.
/// * `U = ()` — no per-frame user data on this generation.
/// * `G = DrmDeviceFd` — fd type backing the cursor-plane GBM device.
type DrmComp = DrmCompositor<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    (),
    DrmDeviceFd,
>;

/// One render destination: a connector + its CRTC + the DrmCompositor
/// that pushes frames to it. The udev backend can carry several of these
/// in parallel — one per active monitor — and the renderer fans frames
/// across them.
struct RenderTarget {
    /// WM-side OutputId; lets us look up bounds / struts cheaply.
    output_id: crate::wm::OutputId,
    /// DRM connector handle as a raw u32. Hot-plug uses this to drop the
    /// right target when its connector vanishes.
    connector: u32,
    /// CRTC driving this connector. Used to route `DrmEvent::VBlank`.
    crtc: crtc::Handle,
    /// Smithay output advertised to clients. Kept here so its Drop fires
    /// when the target is dropped — clients see wl_output destroyed.
    _output: Output,
    /// The composition pipeline for this output.
    compositor: DrmComp,
    /// Output buffer geometry in physical pixels.
    output_size: UtilsSize<i32, smithay::utils::Physical>,
    /// Integer HiDPI scale for this output. Logical size is
    /// `output_size / scale`; the render path multiplies logical coords
    /// by this to reach physical pixels, and it's advertised to clients
    /// via `wl_output` so they paint at the matching buffer scale.
    scale: i32,
    /// Set when a `queue_frame` is awaiting page-flip ack on this CRTC —
    /// rendering again before then would over-queue.
    flip_pending: bool,
    /// Per-target redraw flag. We try to keep targets independent so a
    /// slow monitor doesn't block a fast one.
    needs_redraw: bool,
}

/// Shared dependencies for [`try_bringup_target`]. Lives at the top of
/// the event loop and is reused for every new connector bring-up.
struct RenderEnv {
    gbm: GbmDevice<DrmDeviceFd>,
    renderer_formats: smithay::backend::allocator::format::FormatSet,
}

/// Aggregate state ferried through every calloop callback. Splitting the
/// compositor's "session" pieces from `BacakState` keeps the borrow checker
/// happy — each callback typically wants both `&mut state.state` (to drive
/// Wayland protocols) and another field at the same time.
struct LoopData {
    /// Protocol & Bacak-side state (xdg-shell, seat, WM, …).
    state: BacakState,
    /// Smithay's wayland-server display object.
    display: Display<BacakState>,
    /// Listening socket. We accept new clients each main-loop tick.
    socket: ListeningSocket,
    /// DRM device handle. Kept alive — closing it disables KMS scan-out.
    /// Also used by the hot-plug path to re-enumerate connectors when udev
    /// notifies us of a `change` event, and by [`try_bringup_target`] to
    /// open a fresh DrmSurface for newly connected monitors.
    drm: DrmDevice,
    /// Shared GBM / format set used by every render target.
    env: RenderEnv,
    /// GLES renderer driving every primary plane. A single EGL context
    /// can present to multiple DrmCompositors in sequence; we make_current
    /// per-render-pass via Smithay's render_frame.
    renderer: GlesRenderer,
    /// libseat session — kept alive so the underlying fds keep working.
    _session: LibSeatSession,
    /// Active render targets, one per connected monitor.
    targets: Vec<RenderTarget>,
    /// Seat-side input handles.
    keyboard: KeyboardHandle<BacakState>,
    pointer: PointerHandle<BacakState>,
    touch: TouchHandle<BacakState>,
    /// `connector::Handle → OutputId` bridge. Hot-plug events feed snapshots
    /// in here and the registry mutates the WM in turn.
    output_registry: OutputRegistry,
    /// `st_rdev` of the GPU we opened. Used to filter UdevEvent::Changed
    /// down to just our card — a multi-GPU laptop can otherwise wake us up
    /// for the sibling iGPU's events.
    primary_gpu_id: u64,
    /// Lazily-compiled blur kernel (experimental, `BACAK_BLUR`). `None`
    /// until first attempt; `blur_attempted` guards against recompiling
    /// every frame if the shader fails to link.
    blur: Option<crate::blur::Blur>,
    blur_attempted: bool,
    /// Config hot-reload watcher (shared impl with the winit backend).
    /// `None` when hot-reload couldn't be set up.
    config_watch: Option<ConfigWatcher>,
    /// Calloop signal — used to break the main loop on fatal errors.
    signal: LoopSignal,
}

impl LoopData {
    /// Find the target driving `crtc`. VBlank routing hot-path; the linear
    /// scan is fine because we never expect more than a handful of
    /// outputs (typical desktop: 1–4).
    fn target_by_crtc_mut(&mut self, crtc: crtc::Handle) -> Option<&mut RenderTarget> {
        self.targets.iter_mut().find(|t| t.crtc == crtc)
    }

    /// CRTCs currently bound by an active target. Bring-up consults this
    /// to avoid stealing a CRTC from another connector.
    fn used_crtcs(&self) -> std::collections::HashSet<crtc::Handle> {
        self.targets.iter().map(|t| t.crtc).collect()
    }

    /// Primary output's WM-global origin (the `bounds.x`/`y` of its
    /// rect), physical size, and integer scale. Touch coords come in
    /// primary-output-local (libinput → `position_transformed`), so the
    /// WM-global *logical* position is `local.to_logical(scale) + origin`.
    /// We bind to the primary target (not "the target under the pointer")
    /// because libinput touch devices are tied to a specific output by
    /// the OS — the primary's geometry is the right answer for
    /// single-touchscreen laptops. On the canonical layout primary is at
    /// `(0, 0)` and the origin offset is a no-op.
    fn primary_origin_size(
        &self,
    ) -> Option<(f64, f64, UtilsSize<i32, smithay::utils::Physical>, i32)> {
        let primary = self.state.wm.primary_output()?;
        let t = self
            .targets
            .iter()
            .find(|t| t.output_id == primary)
            .or_else(|| self.targets.first())?;
        let o = self.state.wm.output(primary)?;
        Some((o.bounds.x as f64, o.bounds.y as f64, t.output_size, t.scale))
    }
}

/// Bring up the native session backend and run until the event loop exits.
pub fn run() -> Result<()> {
    // ---- 1. session -----------------------------------------------------
    let (session, session_notifier) = LibSeatSession::new()
        .map_err(|e| anyhow!("LibSeatSession::new failed: {e}"))?;
    let seat_name = session.seat();
    info!(seat = %seat_name, "opened libseat session");

    // Wait until logind makes our session the active one on the seat before we
    // drive KMS — otherwise the initial modeset runs with no DRM master. No-op
    // (returns immediately) on the normal foreground-launch path.
    let session_notifier = wait_until_session_active(&session, session_notifier)?;

    // ---- 2. find + open primary GPU ------------------------------------
    let gpu_path: PathBuf = primary_gpu(&seat_name)
        .map_err(|e| anyhow!("udev primary_gpu lookup failed: {e}"))?
        .context("no DRM devices found on this seat")?;
    info!(gpu = %gpu_path.display(), "primary GPU selected");

    // ---- 3. DrmDevice + scan resources ---------------------------------
    // Acquire the DRM master with a short bounded retry so transient contention
    // (a just-killed previous compositor still releasing the GPU, or a seat
    // hand-off race) doesn't kill us at startup only to be respawned into the
    // same race — the flashing-black-screen loop.
    let (mut drm_device, device_fd, drm_notifier) =
        open_drm_with_retry(&session, &gpu_path)?;

    let boot_connector_raw = pick_first_connected_connector(&drm_device)?;
    info!(connector = boot_connector_raw, "boot connector selected");

    // ---- 4. GBM + EGL + GLES -------------------------------------------
    let gbm = GbmDevice::new(device_fd.clone())
        .map_err(|e| anyhow!("GbmDevice::new failed: {e}"))?;
    // `EGLDisplay::new` is unsafe — the contract is: the EGLNativeDisplay
    // (here gbm::Device) must outlive the returned display. We satisfy that
    // by keeping gbm cloned into the allocator / exporter / cursor.
    let egl_display = unsafe { EGLDisplay::new(gbm.clone()) }
        .map_err(|e| anyhow!("EGLDisplay::new failed: {e}"))?;
    let egl_context = EGLContext::new(&egl_display)
        .map_err(|e| anyhow!("EGLContext::new failed: {e}"))?;
    // Two distinct format sets — confusing them makes GPU clients render black:
    //   • render formats  → formats the GPU can render *into* (scanout/FBO).
    //     What the DrmCompositor negotiates for its own framebuffers.
    //   • texture formats → formats the GlesRenderer can *import as a texture*
    //     (`ImportDma::dmabuf_formats` == `egl.dmabuf_texture_formats`). This is
    //     what the dma-buf global must advertise, because the compositor samples
    //     client buffers as textures. Advertising render formats let clients
    //     pick a (format, modifier) we can't import → silent per-frame import
    //     failure → black (LibreOffice/Skia toolbar + document area).
    // `egl_context` is consumed by `GlesRenderer::new`, so clone both now.
    //
    // VirtualBox (vboxvideo/vmwgfx) and VMware SVGA drivers reject tiled GBM
    // modifiers on the primary DRM plane — the atomic commit succeeds but the
    // hardware scans out black pixels. Detect the vendor via DMI and restrict
    // the DrmCompositor's swapchain to DRM_FORMAT_MOD_LINEAR so the framebuffer
    // is always a simple raster that every virtual GPU driver can display.
    let virtual_gpu = std::fs::read_to_string("/sys/class/dmi/id/sys_vendor")
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v.contains("innotek") || v.contains("vmware") || v.contains("virtualbox")
        })
        .unwrap_or(false);
    let renderer_formats: smithay::backend::allocator::format::FormatSet = if virtual_gpu {
        use smithay::backend::allocator::Modifier;
        let linear: smithay::backend::allocator::format::FormatSet = egl_context
            .dmabuf_render_formats()
            .iter()
            .filter(|f| f.modifier == Modifier::Linear)
            .copied()
            .collect();
        let count = linear.iter().count();
        if count == 0 {
            // llvmpipe on this driver version doesn't advertise LINEAR render
            // formats — fall back to the full set and accept the black-frame risk.
            warn!("virtual GPU detected but no LINEAR render formats available — using full format set");
            egl_context.dmabuf_render_formats().clone()
        } else {
            warn!(
                count,
                "virtual GPU detected (VirtualBox/VMware) — restricting framebuffer modifiers to DRM_FORMAT_MOD_LINEAR"
            );
            linear
        }
    } else {
        egl_context.dmabuf_render_formats().clone()
    };
    // Drop Intel render-compression (CCS) modifiers from what we advertise.
    // EGL reports them as texture-importable so `import_dmabuf` *succeeds*, but
    // our GLES sampler reads the compressed buffer as BLACK (the aux CCS plane's
    // compression is never resolved) — that's the LibreOffice/Skia + XWayland
    // GLAMOR "black window" bug. By not advertising `*_ccs` modifiers, clients
    // (and XWayland) renegotiate a plain Y-tiled/linear modifier for the same
    // fourcc, which we sample correctly. Matched by name so every CCS variant
    // (y_tiled_ccs, gen12_rc/mc_ccs, 4_tiled_*_ccs, …) is covered.
    let dropped_ccs = egl_context
        .dmabuf_texture_formats()
        .iter()
        .filter(|f| format!("{:?}", f.modifier).to_ascii_lowercase().contains("ccs"))
        .count();
    let texture_formats: Vec<_> = egl_context
        .dmabuf_texture_formats()
        .iter()
        .copied()
        .filter(|f| !format!("{:?}", f.modifier).to_ascii_lowercase().contains("ccs"))
        .collect();
    info!(dropped_ccs, kept = texture_formats.len(), "filtered CCS modifiers from advertised dmabuf formats");
    let renderer = unsafe { GlesRenderer::new(egl_context) }
        .map_err(|e| anyhow!("GlesRenderer::new failed: {e}"))?;

    let env = RenderEnv { gbm: gbm.clone(), renderer_formats };

    // ---- 5. Wayland display + protocol state ---------------------------
    let display: Display<BacakState> = Display::new()?;
    let mut state = BacakState::new(&display, "bacak-seat0");
    // FocusFollowsPointer is the better default for a touch-first session
    // host. Click-to-focus is still available via the runtime API.
    state.focus_policy = FocusPolicy::FocusFollowsPointer;

    let keyboard = state
        .seat
        .add_keyboard(Default::default(), 200, 25)
        .map_err(|e| anyhow!("seat.add_keyboard: {e}"))?;
    let pointer = state.seat.add_pointer();
    let touch = state.seat.add_touch();

    // Advertise zwp_linux_dmabuf_v1 with the renderer's *texture import* formats
    // (NOT the render formats — see the note where they're cloned above), so GPU
    // clients (LibreOffice Skia/GL, GTK GL, …) only negotiate (format, modifier)
    // pairs the GlesRenderer can actually import as a texture — instead of
    // rendering black. Created here (not in `BacakState::new`) because it needs
    // the renderer's formats. The per-frame import happens in the render path.
    {
        let dh = state.display_handle.clone();
        // linux-dmabuf **v4 with feedback**: advertise the render device +
        // format/modifier tranches so clients — notably XWayland's GLAMOR
        // backend — can set up GPU acceleration. A plain v3 global (no feedback)
        // gives XWayland no main-device hint, so GL-heavy X11 apps (e.g.
        // OnlyOffice's bundled CEF/Chromium) couldn't get a GPU and crashed /
        // needed a software-GL fallback. This is how cosmic-comp (same Smithay
        // base) makes accelerated X11 apps work. v3 clients still bind and read
        // the main tranche, so nothing regresses.
        let main_device = std::fs::metadata(&gpu_path)
            .map(|m| m.rdev())
            .with_context(|| format!("stat {} for dmabuf main_device", gpu_path.display()))?;
        let feedback = DmabufFeedbackBuilder::new(main_device, texture_formats.iter().copied())
            .build()
            .map_err(|e| anyhow!("dmabuf feedback build failed: {e}"))?;
        let global = state
            .dmabuf_state
            .create_global_with_default_feedback::<BacakState>(&dh, &feedback);
        state.dmabuf_global = Some(global);
        info!(
            formats = texture_formats.iter().count(),
            main_device, "zwp_linux_dmabuf_v1 advertised (v4 + feedback)"
        );
    }

    let socket = ListeningSocket::bind(SOCKET)?;
    std::env::set_var("WAYLAND_DISPLAY", SOCKET);
    info!(socket = SOCKET, "wayland listening socket bound");
    // Make native-Wayland apps started by D-Bus activation / systemd-user (not
    // by our own launcher) see the socket. DISPLAY is exported later, once
    // XWayland is ready. See launcher::export_to_session.
    crate::launcher::export_to_session(&["WAYLAND_DISPLAY"]);
    // Hand off to the session manager's startup client ($BACAK_STARTUP), e.g.
    // the BDM greeter, now that the Wayland socket is live.
    crate::launcher::spawn_startup();

    // ---- 9. libinput pump ----------------------------------------------
    let mut libinput =
        Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
    libinput
        .udev_assign_seat(&seat_name)
        .map_err(|_| anyhow!("libinput udev_assign_seat({seat_name}) failed"))?;
    let input_backend = LibinputInputBackend::new(libinput);

    // ---- 7. bring up boot target ----------------------------------------
    let primary_oid = state
        .wm
        .primary_output()
        .expect("WindowManager::new always sets a primary output");
    let mut output_registry = OutputRegistry::new();
    let dh = state.display_handle.clone();
    let boot_target = try_bringup_target(
        &mut drm_device,
        &env,
        &HashSet::new(),
        boot_connector_raw,
        primary_oid,
        (0, 0),
        &dh,
        &state.config,
    )
    .context("boot target bring-up failed")?;
    // Sync the WM's primary bounds with the actual mode we just lit up;
    // BacakState::new uses a placeholder. Bounds are LOGICAL (physical /
    // scale), so the whole WM works in logical pixels and the render path
    // multiplies back up by the output scale.
    let boot_bounds = WmRect::new(
        0.0,
        0.0,
        (boot_target.output_size.w / boot_target.scale) as f32,
        (boot_target.output_size.h / boot_target.scale) as f32,
    );
    state
        .wm
        .set_output_bounds(primary_oid, boot_bounds)
        .expect("primary output exists immediately after WindowManager::new");
    // Mirror the chosen render scale onto the WM output so wp_fractional_scale
    // advertises the right value (reporting the default 1.0 on a scale-2 panel
    // would tell fractional-scale-aware clients to render at 1× → blurry).
    state.wm.set_output_scale(primary_oid, boot_target.scale as f64);
    // Register the smithay Output so layer-shell / render / input can reach its
    // LayerMap (the backend's own copy lives in the RenderTarget).
    state.register_output(primary_oid, boot_target._output.clone());
    output_registry.adopt(boot_connector_raw, primary_oid);
    info!(
        connector = boot_connector_raw,
        output = primary_oid,
        w = boot_target.output_size.w,
        h = boot_target.output_size.h,
        "adopted boot connector"
    );

    let mut targets: Vec<RenderTarget> = vec![boot_target];

    // ---- 7b. discover any other connectors that are already connected --
    // Reconcile against the current connector state. The boot connector
    // is already in the registry so it shouldn't appear in `changes`;
    // anything else is a secondary monitor that was plugged in before
    // the session started.
    let snapshots = scan_connectors(&drm_device, &output_registry, &state.wm);
    let extra_changes = output_registry.reconcile(&snapshots, &state.wm);
    for change in &extra_changes {
        if let HotplugChange::Added { connector, output } = change {
            let bounds = state.wm.output(*output).map(|o| o.bounds).unwrap_or_default();
            let used: HashSet<crtc::Handle> = targets.iter().map(|t| t.crtc).collect();
            match try_bringup_target(
                &mut drm_device,
                &env,
                &used,
                *connector,
                *output,
                (bounds.x as i32, bounds.y as i32),
                &dh,
                &state.config,
            ) {
                Ok(t) => {
                    info!(connector, output, "secondary connector brought up at boot");
                    state.wm.set_output_scale(*output, t.scale as f64);
                    state.register_output(*output, t._output.clone());
                    targets.push(t);
                }
                Err(err) => warn!(?err, connector, "failed to bring up secondary at boot"),
            }
        }
    }

    // ---- 8. udev backend (hot-plug) ------------------------------------
    // The same seat-bound monitor that picks up new GPUs also notifies us
    // when an *already-open* card has a connector flip state — a monitor
    // being plugged in or yanked. We filter Changed events by st_rdev so
    // a sibling iGPU on a multi-GPU laptop doesn't wake us up.
    let udev_backend = UdevBackend::new(&seat_name)
        .map_err(|e| anyhow!("UdevBackend::new({seat_name}): {e}"))?;
    let primary_gpu_id: u64 = std::fs::metadata(&gpu_path)
        .with_context(|| format!("stat {}", gpu_path.display()))?
        .rdev();

    // ---- 9. event loop --------------------------------------------------
    let mut event_loop: EventLoop<LoopData> = EventLoop::try_new()
        .map_err(|e| anyhow!("EventLoop::try_new failed: {e}"))?;
    let signal = event_loop.get_signal();

    let config_watch = ConfigWatcher::start();

    let mut loop_data = LoopData {
        state,
        display,
        socket,
        drm: drm_device,
        env,
        renderer,
        _session: session,
        targets,
        keyboard,
        pointer,
        touch,
        output_registry,
        primary_gpu_id,
        blur: None,
        blur_attempted: false,
        config_watch,
        signal,
    };

    register_sources(
        &event_loop.handle(),
        session_notifier,
        drm_notifier,
        input_backend,
        udev_backend,
    )?;

    // Spawn Xwayland + attach the X11 window manager. Best-effort: logs and
    // falls back to Wayland-only if the `Xwayland` binary is missing.
    setup_xwayland(&event_loop.handle(), &loop_data.state.display_handle);
    // Teach the selection bridge how to drive X11→Wayland transfers on this
    // backend's loop (see `X11SelectionSink`).
    loop_data.state.x11_selection_sink = Some(Box::new(UdevX11Sink(event_loop.handle())));
    // Explicit sync (wp_linux_drm_syncobj_v1): advertise it so GPU clients that
    // present with an acquire fence (Vulkan/Skia — LibreOffice, OnlyOffice's
    // Chromium) are composited only after their render fence signals, instead of
    // being sampled mid-render and showing black. Only enable if the DRM device
    // supports the syncobj-eventfd needed to build the acquire blocker. The
    // pre-commit hook (handlers.rs) inserts each fence source via this bridge.
    if supports_syncobj_eventfd(&device_fd) {
        let dh = loop_data.state.display_handle.clone();
        loop_data.state.syncobj_state =
            Some(DrmSyncobjState::new::<BacakState>(&dh, device_fd.clone()));
        loop_data.state.syncobj_loop = Some(Box::new(UdevSyncobjLoop(event_loop.handle())));
        info!("explicit sync (wp_linux_drm_syncobj_v1) enabled");
    } else {
        warn!("DRM device lacks syncobj-eventfd; explicit sync disabled (GPU apps may render black)");
    }
    // Tier C accessibility bridge (AT-SPI2) — drives the draggable text-selection
    // handles over foreign apps. ON BY DEFAULT now (touch text selection needs
    // it); set `BACAK_ATSPI=0` to opt out of the a11y-tree overhead. See
    // `crate::atspi`.
    let atspi_disabled = std::env::var("BACAK_ATSPI")
        .map(|v| v == "0" || v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("false"))
        .unwrap_or(false);
    if !atspi_disabled {
        loop_data.state.atspi = crate::atspi::AtspiBridge::start();
        info!(
            started = loop_data.state.atspi.is_some(),
            "AT-SPI (Tier C selection) bridge"
        );
    }

    crate::signals::install();
    crate::launcher::init();

    info!(
        targets = loop_data.targets.len(),
        "entering compositor main loop (udev + drm + wayland)"
    );
    // We tick every ~16 ms so that even quiet sessions still drive frame
    // callbacks for animating clients. The dispatch timeout is a budget,
    // not a wait — calloop returns as soon as any source fires.
    loop {
        if crate::signals::shutdown_requested() {
            loop_data.state.persist_session_now();
            // Release the DRM master explicitly so a successor compositor can
            // become master immediately, instead of waiting for our fds to close
            // on process exit. `pause()` drops the master lock (when privileged)
            // and marks the device inactive, which also stops the subsequent
            // drop from making master-requiring KMS calls with no master.
            loop_data.drm.pause();
            info!("released DRM master; shutdown signal received, exiting");
            return Ok(());
        }

        event_loop
            .dispatch(Some(Duration::from_millis(16)), &mut loop_data)
            .map_err(|e| anyhow!("event_loop.dispatch: {e}"))?;

        accept_pending_clients(&mut loop_data)?;
        // Insert any wp_security_context listeners the protocol queued, tagging
        // clients that connect through them as sandboxed.
        drain_security_listeners(&event_loop.handle(), &mut loop_data);
        loop_data.display.dispatch_clients(&mut loop_data.state)?;
        loop_data.display.flush_clients()?;

        maybe_reload_config(&mut loop_data);

        // Drive spring animations and arm a redraw on every target if any
        // animation is still moving — we can't know which output it's on
        // without inspecting each window, so we conservatively wake all.
        // The alt+tab switcher gets the same treatment: while a cycle is
        // up we redraw every tick so thumbnails of self-animating
        // windows (video, terminals) stay live and the selection reads
        // fluid between Tab presses.
        let now = Instant::now();
        // Poll the selection recogniser for a live long-press (fires mid-hold,
        // not on release). The 16 ms tick budget means a still finger still
        // crosses the threshold even with no further input events.
        let now_ms = now
            .saturating_duration_since(loop_data.state.start_time)
            .as_millis() as u64;
        // Long-press policy (let the app own it): the compositor acts ONLY on
        // its own text fields and on X11 windows. Wayland apps (Firefox,
        // LibreOffice-Wayland) keep their native touch long-press — we don't
        // forward a competing gesture, so their selection + context menu run
        // cleanly. Fighting them was the source of the "selection + right-click
        // both fire" mess.
        if let Some(crate::gestures::SelectionGesture::LongPress { x, y }) =
            loop_data.state.selection_recognizer.tick(now_ms)
        {
            // Altay kendi uzun basış / dosya seçme mantığını yönetir.
            let touched_is_altay = loop_data
                .state
                .wm
                .hit_test(x, y)
                .and_then(|id| loop_data.state.wm.get(id).ok())
                .map(|w| {
                    let app = w.app.to_ascii_lowercase();
                    let title = w.title.to_ascii_lowercase();
                    app.contains("altay") || title.contains("altay")
                })
                .unwrap_or(false);
            if touched_is_altay {
                // Altay'ın kendi Timer'ı devreye girer; compositor menüsü gösterilmez.
            } else if loop_data.state.text_panel_long_press(x, y) {
                // Tier A: a compositor-owned text field handled it.
                loop_data.state.loupe = Some((x, y)); // magnifier for the drag
            } else if loop_data.state.atspi_select_word_at(x, y) {
                // Tier C: AT-SPI selects the word and shows the draggable Android
                // handles (the user drags them to grow/shrink); the action menu
                // is opened by `atspi_select_word_at`. Revoke the app's own touch.
                info!(x, y, "AT-SPI (Tier C) word selection + handles");
                loop_data.touch.cancel(&mut loop_data.state);
                if loop_data.state.touch_pointer_slot.take().is_some() {
                    loop_data.pointer.button(
                        &mut loop_data.state,
                        &ButtonEvent {
                            button: BTN_LEFT,
                            state: ButtonState::Released,
                            serial: SERIAL_COUNTER.next_serial(),
                            time: 0,
                        },
                    );
                    loop_data.pointer.frame(&mut loop_data.state);
                }
            } else {
                // No AT-SPI text here (bridge down, or a non-accessible app):
                // fall back to a MOUSE-emulated selection (double-click + held
                // drag extends), with the Copy/Paste menu popped on release.
                warn!(
                    x,
                    y,
                    atspi = loop_data.state.atspi.is_some(),
                    "AT-SPI select unavailable; mouse-emulation fallback"
                );
                let loc = Point::<f64, smithay::utils::Logical>::from((x as f64, y as f64));
                let focus = loop_data.state.surface_at(x as f64, y as f64);
                // Drop the in-flight touch and any held emulated button first.
                loop_data.touch.cancel(&mut loop_data.state);
                if loop_data.state.touch_pointer_slot.take().is_some() {
                    loop_data.pointer.button(
                        &mut loop_data.state,
                        &ButtonEvent {
                            button: BTN_LEFT,
                            state: ButtonState::Released,
                            serial: SERIAL_COUNTER.next_serial(),
                            time: 0,
                        },
                    );
                }
                loop_data.pointer.motion(
                    &mut loop_data.state,
                    focus,
                    &MotionEvent { location: loc, serial: SERIAL_COUNTER.next_serial(), time: 0 },
                );
                // press, release, press(HELD): a double-click selects the word,
                // and leaving the 2nd press down turns the follow-up drag into a
                // word-by-word selection extend.
                for st in [
                    ButtonState::Pressed,
                    ButtonState::Released,
                    ButtonState::Pressed,
                ] {
                    loop_data.pointer.button(
                        &mut loop_data.state,
                        &ButtonEvent {
                            button: BTN_LEFT,
                            state: st,
                            serial: SERIAL_COUNTER.next_serial(),
                            time: 0,
                        },
                    );
                }
                loop_data.pointer.frame(&mut loop_data.state);
                // Route the rest of the finger's motion/up through the emulated-
                // pointer slot so the drag extends and the lift releases.
                loop_data.state.touch_pointer_slot = loop_data.state.single_touch_slot;
                loop_data.state.loupe = Some((x, y)); // magnifier follows the drag
                loop_data.state.mouse_select = true; // → Copy/Paste menu on release
            }
            for t in loop_data.targets.iter_mut() {
                t.needs_redraw = true;
            }
        }
        // Resync the Tier C overlay when the focused app's selection changed.
        if loop_data.state.atspi.as_ref().is_some_and(|b| b.take_selection_dirty()) {
            loop_data.state.atspi_sync_selection();
            for t in loop_data.targets.iter_mut() {
                t.needs_redraw = true;
            }
        }
        // Flush a coalesced Tier C drag point if its debounce has elapsed.
        if loop_data.state.atspi_selection.as_ref().is_some_and(|s| s.pending.is_some()) {
            loop_data.state.atspi_drag_tick();
            for t in loop_data.targets.iter_mut() {
                t.needs_redraw = true;
            }
        }
        // Tick the shell plugins (OSK debounced hide, delayed screenshot, …).
        // Combined with the event-driven `osk_dirty` flag, force a redraw if
        // anything changed.
        let plugins_dirty = crate::plugins::tick_all(&mut loop_data.state, now);
        if std::mem::take(&mut loop_data.state.osk_dirty)
            || std::mem::take(&mut loop_data.state.surface_committed)
            || plugins_dirty
        {
            for t in loop_data.targets.iter_mut() {
                t.needs_redraw = true;
            }
        }
        let anim_running = loop_data.state.tick_animations(now);
        let reveal_moving = loop_data.state.tick_dock_reveal(now);
        loop_data.state.maybe_persist_session(now);
        if anim_running
            || reveal_moving
            || loop_data.state.focus_history.is_cycling()
            || loop_data.state.has_pending_launches()
            || loop_data.state.has_urgent_windows()
            || loop_data.state.has_pending_hover()
        {
            for t in loop_data.targets.iter_mut() {
                t.needs_redraw = true;
            }
        }

        render_dirty_targets(&mut loop_data)?;
    }
}

// ---------------------------------------------------------------------------
// Setup helpers
// ---------------------------------------------------------------------------

/// Open a path through the seat manager — this is how libseat hands us an fd
/// without requiring root or a setuid wrapper. The flag argument is ignored
/// by `LibSeatSession::open` (libseat always opens RW) but the trait still
/// requires us to pass it.
fn open_via_session(mut session: LibSeatSession, path: &std::path::Path) -> Result<OwnedFd> {
    use smithay::reexports::rustix::fs::OFlags;
    session
        .open(path, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NONBLOCK)
        .map_err(|e| anyhow!("libseat failed to open {}: {e}", path.display()))
}

/// Open the GPU and create the [`DrmDevice`], retrying briefly while the DRM
/// master is still held by another process.
///
/// `DrmDevice::new(.., true)` does an initial modeset that needs the DRM master.
/// If a previous compositor is still releasing the GPU (e.g. it was just killed
/// on a session-manager restart) or there is a seat hand-off race, that fails
/// with EACCES/EPERM/EBUSY. Dying here is pointless: the session manager just
/// respawns us straight back into the same race, which is exactly the "flashing
/// black screen" loop. The kernel drops a dead process's master within
/// milliseconds, so a short bounded retry rides out the contention. A genuinely
/// fatal error (no GPU, bad fd) simply exhausts the window and is returned as
/// before.
fn open_drm_with_retry(
    session: &LibSeatSession,
    gpu_path: &std::path::Path,
) -> Result<(
    DrmDevice,
    DrmDeviceFd,
    smithay::backend::drm::DrmDeviceNotifier,
)> {
    const ATTEMPTS: u32 = 15;
    const DELAY: std::time::Duration = std::time::Duration::from_millis(300);

    let mut last_err = anyhow!("DRM acquisition never attempted");
    for attempt in 1..=ATTEMPTS {
        match open_via_session(session.clone(), gpu_path)
            .map(|fd| DrmDeviceFd::new(DeviceFd::from(fd)))
            .and_then(|device_fd| {
                DrmDevice::new(device_fd.clone(), true)
                    .map(|(drm, notifier)| (drm, device_fd, notifier))
                    .map_err(|e| anyhow!("DrmDevice::new failed: {e}"))
            }) {
            Ok(acquired) => {
                if attempt > 1 {
                    info!(attempt, "acquired DRM master after transient contention");
                }
                return Ok(acquired);
            }
            Err(e) => {
                warn!(
                    attempt,
                    max = ATTEMPTS,
                    error = %e,
                    "DRM master unavailable (held by another process?); retrying"
                );
                // Identify the blocker once (not on every retry).
                if attempt == 1 {
                    log_drm_device_holders(gpu_path);
                }
                last_err = e;
                std::thread::sleep(DELAY);
            }
        }
    }
    Err(last_err.context("DRM master still unavailable after retrying"))
}

/// Wait until our libseat session is the active one on the seat (so we hold the
/// seat and can become DRM master), up to a timeout, then hand the notifier
/// back for the main loop.
///
/// `LibSeatSession::new` already reflects the seat state at creation, so when
/// the compositor is launched as the foreground session — BDM's normal path —
/// `is_active()` is already true and this returns immediately (zero overhead,
/// zero risk for the common case). The slow path matters only when we're
/// started while another session still owns the seat (a VT / seat hand-off
/// race): libseat reports `Enable` only once logind switches to us, and that
/// event is delivered by *dispatching the notifier*. We drive it on a throwaway
/// event loop until active (or the timeout), then return the notifier so the
/// main loop keeps receiving pause/resume (VT-switch) events.
fn wait_until_session_active(
    session: &LibSeatSession,
    notifier: smithay::backend::session::libseat::LibSeatSessionNotifier,
) -> Result<smithay::backend::session::libseat::LibSeatSessionNotifier> {
    if session.is_active() {
        return Ok(notifier);
    }
    info!("session not active yet; waiting for the seat (ActivateSession)…");

    let mut wait_loop: EventLoop<()> =
        EventLoop::try_new().map_err(|e| anyhow!("wait EventLoop::try_new failed: {e}"))?;
    let dispatcher = calloop::Dispatcher::new(notifier, |event, _, _: &mut ()| {
        if let SessionEvent::ActivateSession = event {
            info!("session activated");
        }
    });
    let token = wait_loop
        .handle()
        .register_dispatcher(dispatcher.clone())
        .map_err(|e| anyhow!("register session notifier for wait: {e}"))?;

    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
    let start = std::time::Instant::now();
    let mut data = ();
    while !session.is_active() {
        if start.elapsed() >= TIMEOUT {
            warn!("timed out waiting for session activation; proceeding (DRM acquire will retry)");
            break;
        }
        wait_loop
            .dispatch(std::time::Duration::from_millis(250), &mut data)
            .map_err(|e| anyhow!("wait_loop.dispatch: {e}"))?;
    }

    // Reclaim the notifier for the main loop. `into_source_inner` panics if any
    // other `Rc` to the dispatcher survives, so unregister it AND drop the temp
    // loop first — then our `dispatcher` is the sole owner.
    wait_loop.handle().remove(token);
    drop(wait_loop);
    Ok(dispatcher.into_source_inner())
}

/// Best-effort diagnostic: log which process is holding the DRM device (and so,
/// most likely, the DRM master we can't get). Called when master acquisition
/// fails.
///
/// The usual culprit is an orphaned previous compositor running as the *same*
/// (greeter) user, so a `/proc/<pid>/fd` scan finds it — we can read the fd
/// links of our own user's processes. A holder owned by another user/seat is
/// not visible to an unprivileged compositor; that case is noted. As a bonus,
/// if debugfs is readable (root / dev runs) we dump the kernel's authoritative
/// client table, whose `master` column names the actual master.
fn log_drm_device_holders(gpu_path: &std::path::Path) {
    let want = gpu_path.file_name();
    let self_pid = std::process::id();
    let mut found = false;

    if let Ok(proc) = std::fs::read_dir("/proc") {
        for entry in proc.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            if pid == self_pid {
                continue;
            }
            // EACCES (a process we don't own) just skips — best effort.
            let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
                continue;
            };
            for fd in fds.flatten() {
                if let Ok(target) = std::fs::read_link(fd.path()) {
                    if want.is_some() && target.file_name() == want {
                        let comm = std::fs::read_to_string(entry.path().join("comm"))
                            .map(|s| s.trim().to_owned())
                            .unwrap_or_default();
                        warn!(
                            device = %gpu_path.display(),
                            pid,
                            command = %comm,
                            "DRM device held open by another process — likely the DRM master blocking us"
                        );
                        found = true;
                        break;
                    }
                }
            }
        }
    }

    if !found {
        warn!(
            device = %gpu_path.display(),
            "no same-user process found holding the DRM device; the master may belong to another user/seat (not visible to an unprivileged compositor)"
        );
    }

    // Authoritative kernel view, if debugfs is readable (root / dev runs only).
    if let Some(minor) = want
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("card"))
        .and_then(|n| n.parse::<u32>().ok())
    {
        let clients = format!("/sys/kernel/debug/dri/{minor}/clients");
        if let Ok(table) = std::fs::read_to_string(&clients) {
            warn!(path = %clients, "DRM clients (the row with master=\"y\" is the current master):\n{}", table.trim_end());
        }
    }
}

/// Find the first connected connector and return its raw u32 handle.
/// The caller hands this to [`try_bringup_target`] which picks a mode
/// and CRTC. Returns an error if no monitor is plugged in at startup —
/// a headless boot would be invisible to the user.
fn pick_first_connected_connector(drm: &DrmDevice) -> Result<u32> {
    let res = drm
        .resource_handles()
        .map_err(|e| anyhow!("drm.resource_handles: {e}"))?;

    for &c in res.connectors() {
        let info = match drm.get_connector(c, true) {
            Ok(info) => info,
            Err(err) => {
                warn!(?err, ?c, "skipping connector: get_connector failed");
                continue;
            }
        };
        if info.state() == connector::State::Connected {
            return Ok(c.into());
        }
    }
    Err(anyhow!("no connected connector on this device at boot"))
}

/// Produce a stable, human-readable name for an output based on the
/// connector type — `eDP-1`, `HDMI-A-2`, etc. Smithay only needs *a* name
/// (it doesn't have to match the kernel naming), but matching makes
/// debugging against `xrandr` / `wlr-randr` quieter.
fn connector_interface_name(info: &connector::Info) -> String {
    // Use as_str() so names match sysfs/xrandr: "DP-1", "HDMI-A-1", "eDP-1".
    // {:#?} (Debug) would give "DisplayPort-1" which surprises users.
    format!("{}-{}", info.interface().as_str(), info.interface_id())
}

/// Convert a raw u32 to `connector::Handle`. Returns `None` on zero,
/// which DRM uses as "no handle" — we should never see one here, but
/// the type signature requires us to handle it.
fn connector_handle_from_raw(raw: u32) -> Option<connector::Handle> {
    smithay::reexports::drm::control::from_u32(raw)
}

/// Pick an integer HiDPI scale for a connector.
///
/// `BACAK_SCALE` (1–3) forces a global value and short-circuits the
/// estimate — the reliable escape hatch when a panel's EDID lies or the
/// user just wants a different size. Otherwise we estimate the panel's
/// DPI from its mode resolution and EDID physical size and pick 2 once
/// it crosses the classic "HiDPI" threshold (2× of 96 dpi). Panels with
/// no usable EDID size fall back to 1.
fn pick_scale(mode_px: (u16, u16), phys_mm: (u32, u32)) -> i32 {
    if let Ok(s) = std::env::var("BACAK_SCALE") {
        if let Ok(n) = s.trim().parse::<i32>() {
            if (1..=3).contains(&n) {
                return n;
            }
            warn!(value = %s, "BACAK_SCALE out of range 1..=3; auto-detecting");
        }
    }
    let (pw, ph) = phys_mm;
    if pw == 0 || ph == 0 {
        // No physical size info (e.g. virtual/headless output). Use pixel
        // count as a proxy: 4K resolution almost certainly needs 2×.
        let (w, h) = mode_px;
        return if w >= 3200 || h >= 1800 { 2 } else { 1 };
    }
    let px_diag = ((mode_px.0 as f64).powi(2) + (mode_px.1 as f64).powi(2)).sqrt();
    let mm_diag = ((pw as f64).powi(2) + (ph as f64).powi(2)).sqrt();
    let dpi = px_diag / (mm_diag / 25.4);
    // ≥ 192 DPI: true HiDPI (laptop Retina, small 4K panels) → 3× on
    // extreme cases but 2× is the common sweet spot.
    // 150–192 DPI: 4K on typical 24–27″ monitors at desk distance → 2×.
    // < 150 DPI: Full HD or 4K on large/distant display → 1× (or user
    // can override via `outputs.<connector>.scale` in compositor.json).
    if dpi >= 192.0 {
        2
    } else if dpi >= 150.0 {
        // 4K at ~24–27″: pixel density justifies 2× scaling.
        2
    } else {
        1
    }
}

/// Bring up a render target for one connector: pick a mode, find a free
/// CRTC, create the DrmSurface, build a Smithay `Output`, and wrap the
/// whole thing in a `DrmCompositor`. The location is the output's
/// logical-pixel top-left in the WM's global coordinate space; we plumb
/// it into Smithay's `change_current_state` so client surfaces see the
/// layout that the WM thinks is real.
fn try_bringup_target(
    drm: &mut DrmDevice,
    env: &RenderEnv,
    used_crtcs: &HashSet<crtc::Handle>,
    connector_raw: u32,
    output_id: OutputId,
    location_logical: (i32, i32),
    dh: &DisplayHandle,
    cfg: &crate::config::CompositorConfig,
) -> Result<RenderTarget> {
    let res = drm
        .resource_handles()
        .map_err(|e| anyhow!("drm.resource_handles: {e}"))?;

    let handle = connector_handle_from_raw(connector_raw)
        .ok_or_else(|| anyhow!("connector handle 0 is not a valid id"))?;
    let info = drm
        .get_connector(handle, true)
        .map_err(|e| anyhow!("get_connector({connector_raw}): {e}"))?;
    if info.state() != connector::State::Connected {
        return Err(anyhow!("connector {connector_raw} is not connected"));
    }

    let connector_name = connector_interface_name(&info);
    let output_cfg = cfg.outputs.get(&connector_name);

    let modes = info.modes();
    // Mode selection priority:
    // 1. If compositor.json specifies a mode for this connector, find the
    //    closest DRM mode match (exact WxH, then best refresh, fallback preferred).
    // 2. Otherwise prefer the mode with the PREFERRED flag from EDID/firmware.
    // 3. Fall back to the first mode in the driver list.
    let drm_mode = if let Some(cfg_mode) = output_cfg.and_then(|c| c.mode.as_deref()) {
        if let Some((req_w, req_h, req_hz)) = crate::config::OutputConfig::parse_mode(cfg_mode) {
            let matched = modes.iter().copied().filter(|m| {
                let s = m.size();
                s.0 == req_w && s.1 == req_h && (req_hz == 0 || m.vrefresh() as u32 == req_hz)
            })
            .max_by_key(|m| m.vrefresh());
            if let Some(m) = matched {
                info!(
                    connector = connector_raw,
                    mode = cfg_mode,
                    "using config-specified output mode"
                );
                m
            } else {
                warn!(
                    connector = connector_raw,
                    mode = cfg_mode,
                    "config mode not found in connector's mode list; falling back to preferred"
                );
                modes.iter().copied()
                    .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                    .or_else(|| modes.iter().copied().next())
                    .ok_or_else(|| anyhow!("connector {connector_raw} reports no modes"))?
            }
        } else {
            warn!(connector = connector_raw, mode = cfg_mode, "could not parse configured mode string");
            modes.iter().copied()
                .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                .or_else(|| modes.iter().copied().next())
                .ok_or_else(|| anyhow!("connector {connector_raw} reports no modes"))?
        }
    } else {
        // No config override — prefer the PREFERRED mode (highest-quality mode
        // recommended by the display's EDID), then fall back to first.
        modes.iter().copied()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .or_else(|| modes.iter().copied().next())
            .ok_or_else(|| anyhow!("connected connector {connector_raw} reports no modes"))?
    };
    let mode_size = drm_mode.size();

    // Find a CRTC that's both compatible with this connector's encoders
    // and not currently driving another output.
    let crtc_handle = {
        let mut found = None;
        'outer: for &enc_handle in info.encoders() {
            let Ok(enc) = drm.get_encoder(enc_handle) else { continue };
            let compatible = res.filter_crtcs(enc.possible_crtcs());
            for c in compatible {
                if !used_crtcs.contains(&c) {
                    found = Some(c);
                    break 'outer;
                }
            }
        }
        found.ok_or_else(|| anyhow!("no free CRTC for connector {connector_raw}"))?
    };

    let drm_surface = drm
        .create_surface(crtc_handle, drm_mode, &[handle])
        .map_err(|e| anyhow!("DrmDevice::create_surface: {e}"))?;

    let (phys_w, phys_h) = info.size().unwrap_or((0, 0));
    let smithay_output = Output::new(
        connector_interface_name(&info),
        PhysicalProperties {
            size: (phys_w as i32, phys_h as i32).into(),
            subpixel: Subpixel::Unknown,
            make: "Bacak".into(),
            model: "DRM".into(),
        },
    );
    let smithay_mode = OutputMode {
        size: (mode_size.0 as i32, mode_size.1 as i32).into(),
        refresh: (drm_mode.vrefresh() as i32) * 1000,
    };
    let scale = if let Some(&s) = output_cfg.and_then(|c| c.scale.as_ref()) {
        let clamped = s.clamp(1, 3);
        if clamped != s {
            warn!(connector = connector_raw, requested = s, used = clamped, "scale clamped to 1–3");
        }
        info!(connector = connector_raw, scale = clamped, "using config-specified output scale");
        clamped
    } else {
        pick_scale(mode_size, (phys_w, phys_h))
    };
    info!(
        connector = connector_raw,
        mode_w = mode_size.0,
        mode_h = mode_size.1,
        phys_mm_w = phys_w,
        phys_mm_h = phys_h,
        scale,
        "selected output mode and scale (override via compositor.json outputs.<connector>)"
    );
    smithay_output.set_preferred(smithay_mode);
    smithay_output.change_current_state(
        Some(smithay_mode),
        Some(Transform::Normal),
        Some(smithay::output::Scale::Integer(scale)),
        Some(location_logical.into()),
    );
    // Advertise this monitor as a `wl_output` global (xdg-output rides
    // along via OutputManagerState). Real toolkits — Chromium's
    // Ozone-Wayland backend, Firefox, GTK/Qt — require at least one
    // output and abort/crash without it. The Output is moved into the
    // RenderTarget below, so the global lives for the connector's
    // lifetime.
    smithay_output.create_global::<BacakState>(dh);

    let allocator = GbmAllocator::new(
        env.gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let exporter = GbmFramebufferExporter::new(env.gbm.clone(), None);
    let cursor_size = drm.cursor_size();
    let compositor = DrmCompositor::new(
        &smithay_output,
        drm_surface,
        None,
        allocator,
        exporter,
        [DrmFourcc::Argb8888, DrmFourcc::Xrgb8888],
        env.renderer_formats.clone(),
        cursor_size,
        Some(env.gbm.clone()),
    )
    .map_err(|e| anyhow!("DrmCompositor::new: {e:?}"))?;

    let output_size = UtilsSize::<i32, smithay::utils::Physical>::from((
        mode_size.0 as i32,
        mode_size.1 as i32,
    ));

    Ok(RenderTarget {
        output_id,
        connector: connector_raw,
        crtc: crtc_handle,
        _output: smithay_output,
        compositor,
        output_size,
        scale,
        flip_pending: false,
        needs_redraw: true,
    })
}

// ---------------------------------------------------------------------------
// XWayland
//
// `X11Wm::start_wm` is generic over the calloop data type (`LoopData`), so the
// `XwmHandler` / `XWaylandShellHandler` impls its event source dispatches to
// must live on `LoopData`. They forward straight to the real logic on
// `BacakState` (see `crate::xwayland`).
// ---------------------------------------------------------------------------

impl XWaylandShellHandler for LoopData {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        self.state.xwayland_shell_state()
    }
    fn surface_associated(
        &mut self,
        xwm: XwmId,
        wl_surface: smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        surface: X11Surface,
    ) {
        XWaylandShellHandler::surface_associated(&mut self.state, xwm, wl_surface, surface)
    }
}

/// udev backend's [`crate::state::X11SelectionSink`]: closes over the
/// `LoopData` loop handle so [`X11Wm::send_selection`] runs its async transfer
/// on the loop the X11Wm actually lives on.
struct UdevX11Sink(LoopHandle<'static, LoopData>);

impl crate::state::X11SelectionSink for UdevX11Sink {
    fn send(&self, xwm: &mut X11Wm, selection: SelectionTarget, mime_type: String, fd: OwnedFd) {
        if let Err(err) = xwm.send_selection::<LoopData>(selection, mime_type, fd, self.0.clone()) {
            warn!(?err, "failed to stream X11 selection to Wayland client");
        }
    }
}

/// Inserts explicit-sync acquire-fence sources into the udev event loop on
/// behalf of the pre-commit hook (see [`crate::state::SyncobjLoopHandle`]).
struct UdevSyncobjLoop(LoopHandle<'static, LoopData>);

impl crate::state::SyncobjLoopHandle for UdevSyncobjLoop {
    fn insert_sync_source(&self, source: DrmSyncPointSource, client: Client) {
        // When the acquire fence signals, clear that client's commit blockers so
        // the delayed commit is applied and the now-ready buffer is composited.
        let res = self.0.insert_source(source, move |_, _, data: &mut LoopData| {
            let dh = data.state.display_handle.clone();
            data.state
                .client_compositor_state(&client)
                .blocker_cleared(&mut data.state, &dh);
            Ok(())
        });
        if let Err(err) = res {
            warn!(?err, "failed to insert syncobj acquire-fence source");
        }
    }
}

impl XwmHandler for LoopData {
    fn xwm_state(&mut self, xwm: XwmId) -> &mut X11Wm {
        self.state.xwm_state(xwm)
    }
    fn new_window(&mut self, xwm: XwmId, w: X11Surface) {
        self.state.new_window(xwm, w)
    }
    fn new_override_redirect_window(&mut self, xwm: XwmId, w: X11Surface) {
        self.state.new_override_redirect_window(xwm, w)
    }
    fn map_window_request(&mut self, xwm: XwmId, w: X11Surface) {
        self.state.map_window_request(xwm, w)
    }
    fn mapped_override_redirect_window(&mut self, xwm: XwmId, w: X11Surface) {
        self.state.mapped_override_redirect_window(xwm, w)
    }
    fn unmapped_window(&mut self, xwm: XwmId, w: X11Surface) {
        self.state.unmapped_window(xwm, w)
    }
    fn destroyed_window(&mut self, xwm: XwmId, w: X11Surface) {
        self.state.destroyed_window(xwm, w)
    }
    fn configure_request(
        &mut self,
        xwm: XwmId,
        w: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        width: Option<u32>,
        height: Option<u32>,
        reorder: Option<Reorder>,
    ) {
        self.state
            .configure_request(xwm, w, x, y, width, height, reorder)
    }
    fn configure_notify(
        &mut self,
        xwm: XwmId,
        w: X11Surface,
        geometry: Rectangle<i32, Logical>,
        above: Option<u32>,
    ) {
        self.state.configure_notify(xwm, w, geometry, above)
    }
    fn property_notify(&mut self, xwm: XwmId, w: X11Surface, property: WmWindowProperty) {
        self.state.property_notify(xwm, w, property)
    }
    fn resize_request(&mut self, xwm: XwmId, w: X11Surface, button: u32, edge: ResizeEdge) {
        self.state.resize_request(xwm, w, button, edge)
    }
    fn move_request(&mut self, xwm: XwmId, w: X11Surface, button: u32) {
        self.state.move_request(xwm, w, button)
    }
    fn allow_selection_access(&mut self, xwm: XwmId, sel: SelectionTarget) -> bool {
        self.state.allow_selection_access(xwm, sel)
    }
    fn new_selection(&mut self, xwm: XwmId, sel: SelectionTarget, mimes: Vec<String>) {
        self.state.new_selection(xwm, sel, mimes)
    }
    fn send_selection(&mut self, xwm: XwmId, sel: SelectionTarget, mime: String, fd: OwnedFd) {
        self.state.send_selection(xwm, sel, mime, fd)
    }
    fn cleared_selection(&mut self, xwm: XwmId, sel: SelectionTarget) {
        self.state.cleared_selection(xwm, sel)
    }
}

/// Spawn an Xwayland server and, when it signals `Ready`, attach an [`X11Wm`]
/// so legacy X11 clients map as ordinary Bacak windows. Sets `DISPLAY` for
/// child processes. Best-effort: if `Xwayland` isn't installed the failure is
/// logged and the session simply continues Wayland-only.
fn setup_xwayland(handle: &LoopHandle<'static, LoopData>, dh: &DisplayHandle) {
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
    let res = handle.insert_source(xwayland, move |event, _, data: &mut LoopData| match event {
        XWaylandEvent::Ready {
            x11_socket,
            display_number,
        } => match X11Wm::start_wm(wm_handle.clone(), x11_socket, client.clone()) {
            Ok(wm) => {
                data.state.xwm = Some(wm);
                std::env::set_var("DISPLAY", format!(":{display_number}"));
                // Export DISPLAY to D-Bus/systemd-user so X11/XWayland apps
                // started outside our launcher (e.g. from a mate-terminal shell:
                // onlyoffice-desktopeditors) can reach the X display instead of
                // failing with "Could not connect to an X display".
                crate::launcher::export_to_session(&["DISPLAY", "WAYLAND_DISPLAY"]);
                info!(
                    display = display_number,
                    "XWayland ready; X11 window manager attached"
                );
            }
            Err(e) => error!("failed to attach X11 window manager: {e}"),
        },
        XWaylandEvent::Error => error!("XWayland failed to start"),
    });
    if let Err(e) = res {
        warn!("failed to insert XWayland source into the event loop: {e}");
    }
}

/// Wire every long-lived `EventSource` into the calloop event loop. The
/// closures live for the duration of the loop and are the *only* place a
/// `&mut LoopData` is conjured into being.
fn register_sources(
    handle: &LoopHandle<'static, LoopData>,
    session_notifier: smithay::backend::session::libseat::LibSeatSessionNotifier,
    drm_notifier: smithay::backend::drm::DrmDeviceNotifier,
    input_backend: LibinputInputBackend,
    udev_backend: UdevBackend,
) -> Result<()> {
    // Session: pause + resume KMS access when the user VT-switches away.
    handle
        .insert_source(session_notifier, |event, _, data| match event {
            SessionEvent::ActivateSession => {
                info!("session activated");
                for t in data.targets.iter_mut() {
                    t.needs_redraw = true;
                }
            }
            SessionEvent::PauseSession => {
                info!("session paused");
                // KMS access is suspended while we're off the VT, so any
                // page-flip we'd queued will never get its VBlank ack.
                // Clearing `flip_pending` here means `render_dirty_targets`
                // isn't permanently gated on resume — otherwise the
                // screen stays frozen after VT-switching back.
                for t in data.targets.iter_mut() {
                    t.flip_pending = false;
                }
            }
        })
        .map_err(|e| anyhow!("insert session source: {e}"))?;

    // DRM: page-flip completions release the next render slot. The event
    // carries the CRTC that just flipped — route the ack to the matching
    // RenderTarget so a slow output can't stall a fast one.
    handle
        .insert_source(drm_notifier, |event, _, data| match event {
            DrmEvent::VBlank(crtc) => {
                if let Some(t) = data.target_by_crtc_mut(crtc) {
                    if let Err(err) = t.compositor.frame_submitted() {
                        warn!(?err, ?crtc, "frame_submitted failed");
                    }
                    t.flip_pending = false;
                    t.needs_redraw = true;
                } else {
                    warn!(?crtc, "VBlank for unknown CRTC — target dropped mid-flip?");
                }
            }
            DrmEvent::Error(err) => {
                warn!(?err, "DRM error");
            }
        })
        .map_err(|e| anyhow!("insert drm source: {e}"))?;

    // Input: drive the seat directly off libinput events.
    handle
        .insert_source(input_backend, |event, _, data| {
            forward_libinput_event(data, event)
        })
        .map_err(|e| anyhow!("insert input source: {e}"))?;

    // Udev: connector hot-plug. New connectors get a render target so
    // they actually receive frames; vanished connectors drop theirs.
    handle
        .insert_source(udev_backend, |event, _, data| {
            handle_udev_event(data, event);
        })
        .map_err(|e| anyhow!("insert udev source: {e}"))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Hot-plug
// ---------------------------------------------------------------------------

/// Demultiplex a `UdevEvent`. Whole-GPU add / remove events are out of
/// scope (single-GPU only); we react to `Changed` on the primary card,
/// which is how the kernel signals a connector flip.
fn handle_udev_event(data: &mut LoopData, event: UdevEvent) {
    match event {
        UdevEvent::Added { device_id, path } => {
            info!(device_id, ?path, "udev: GPU added (ignored — single-GPU only)");
        }
        UdevEvent::Removed { device_id } => {
            if device_id == data.primary_gpu_id {
                warn!(device_id, "udev: primary GPU removed — session will become headless");
            } else {
                info!(device_id, "udev: GPU removed (sibling, ignored)");
            }
        }
        UdevEvent::Changed { device_id } => {
            if device_id != data.primary_gpu_id {
                return;
            }
            apply_hotplug(data);
        }
    }
}

/// Re-enumerate connectors, reconcile WM state, then bring up / drop
/// the matching render targets. Both the boot path and `Changed` events
/// converge here so the two diverge only in the seeding of the boot
/// connector.
fn apply_hotplug(data: &mut LoopData) {
    let snapshots = scan_connectors(&data.drm, &data.output_registry, &data.state.wm);
    let changes = data
        .output_registry
        .reconcile(&snapshots, &data.state.wm);
    if changes.is_empty() {
        return;
    }

    let dh = data.state.display_handle.clone();
    for change in &changes {
        match *change {
            HotplugChange::Added { connector, output } => {
                let bounds = data
                    .state
                    .wm
                    .output(output)
                    .map(|o| o.bounds)
                    .unwrap_or_default();
                let used = data.used_crtcs();
                match try_bringup_target(
                    &mut data.drm,
                    &data.env,
                    &used,
                    connector,
                    output,
                    (bounds.x as i32, bounds.y as i32),
                    &dh,
                    &data.state.config,
                ) {
                    Ok(t) => {
                        info!(connector, output, "hotplug: target brought up");
                        data.state.wm.set_output_scale(output, t.scale as f64);
                        data.state.register_output(output, t._output.clone());
                        data.targets.push(t);
                    }
                    Err(err) => warn!(?err, connector, output, "hotplug: bring-up failed"),
                }
            }
            HotplugChange::Removed { connector, output } => {
                data.state.outputs.remove(&output);
                let before = data.targets.len();
                data.targets.retain(|t| t.connector != connector);
                if data.targets.len() < before {
                    info!(connector, output, "hotplug: target torn down");
                } else {
                    info!(
                        connector,
                        output,
                        "hotplug: output removed (no live target was bound)"
                    );
                }
            }
            HotplugChange::Resized { connector, output } => {
                // Mode change → drop the old DrmSurface and bring up a
                // fresh one at the new mode. We drop first so the old
                // target's CRTC is back in the free pool by the time
                // bringup walks it. The logical location is unchanged
                // — `scan_connectors` preserves the previous (x,y) on
                // known connectors, so this Resized is purely w/h.
                let before = data.targets.len();
                data.targets.retain(|t| t.connector != connector);
                if data.targets.len() < before {
                    let bounds = data
                        .state
                        .wm
                        .output(output)
                        .map(|o| o.bounds)
                        .unwrap_or_default();
                    let used = data.used_crtcs();
                    match try_bringup_target(
                        &mut data.drm,
                        &data.env,
                        &used,
                        connector,
                        output,
                        (bounds.x as i32, bounds.y as i32),
                        &dh,
                        &data.state.config,
                    ) {
                        Ok(t) => {
                            info!(connector, output, "hotplug: target re-modeset");
                            data.state.wm.set_output_scale(output, t.scale as f64);
                            data.state.register_output(output, t._output.clone());
                            data.targets.push(t);
                        }
                        Err(err) => {
                            // The old target is already gone. If
                            // bringup fails we don't try to restore
                            // the previous mode — the WM still has
                            // the output, just with no scan-out until
                            // the user fixes the cable / mode picker.
                            warn!(
                                ?err,
                                connector,
                                output,
                                "hotplug: re-modeset failed; output is dark"
                            );
                        }
                    }
                } else {
                    info!(
                        connector,
                        output,
                        "hotplug: resized event for output with no live target"
                    );
                }
            }
        }
    }

    // Re-apply the dock strut so a newly-arrived output gets its
    // bottom band reserved (and a removed-then-re-added one doesn't
    // miss it). `apply_dock_strut` is idempotent across outputs.
    data.state.apply_dock_strut();

    for t in data.targets.iter_mut() {
        t.needs_redraw = true;
    }
}

/// Walk every connector on the DRM device and produce a snapshot for
/// the ones currently in `Connected` state. Disconnected connectors are
/// omitted — their absence is what [`OutputRegistry::reconcile`] uses
/// to drive removes.
///
/// Layout policy: known connectors keep their existing `bounds.{x,y}`
/// (only their `w/h` is refreshed from the current mode, which lets us
/// detect resolution changes). New connectors stack to the right of
/// the rightmost edge of all currently-known outputs. The order in
/// which new connectors land is sorted by their raw handle for
/// determinism across boots.
fn scan_connectors(
    drm: &DrmDevice,
    registry: &OutputRegistry,
    wm: &crate::wm::WindowManager,
) -> Vec<ConnectorSnapshot> {
    let res = match drm.resource_handles() {
        Ok(r) => r,
        Err(err) => {
            warn!(?err, "drm.resource_handles failed during connector rescan");
            return Vec::new();
        }
    };

    // First pass: collect (handle, logical-w, logical-h) for every
    // connector that's actually in Connected state. Sizes are LOGICAL
    // (mode pixels / scale) so the layout the WM builds is in the same
    // logical coordinate space as `try_bringup_target` advertises.
    let mut raw: Vec<(u32, i32, i32)> = Vec::new();
    for &c in res.connectors() {
        let info = match drm.get_connector(c, true) {
            Ok(i) => i,
            Err(err) => {
                warn!(?err, ?c, "get_connector failed during rescan");
                continue;
            }
        };
        if info.state() != connector::State::Connected {
            continue;
        }
        let Some(mode) = info.modes().iter().copied().next() else {
            continue;
        };
        let (w, h) = mode.size();
        let scale = pick_scale((w, h), info.size().unwrap_or((0, 0)));
        let handle: u32 = c.into();
        raw.push((handle, w as i32 / scale, h as i32 / scale));
    }

    // Where new connectors will be placed. We use the rightmost edge of
    // every currently-known output; if a known connector happens to be
    // *missing* from this rescan (it just unplugged) its width still
    // counts here, but reconcile will tear it down before any new
    // connector lands at that x. The minor over-count is harmless.
    let right_edge: f32 = wm
        .outputs()
        .iter()
        .map(|o| o.bounds.x + o.bounds.w)
        .fold(0.0_f32, f32::max);

    // Process known connectors first (so their positions are stable),
    // then new ones in handle order (for determinism).
    raw.sort_by_key(|(h, _, _)| (if registry.knows(*h) { 0 } else { 1 }, *h));

    let mut out = Vec::with_capacity(raw.len());
    let mut next_x = right_edge;
    for (handle, w, h) in raw {
        let bounds = if let Some(oid) = registry.output_for(handle) {
            let prev = wm.output(oid).map(|o| o.bounds).unwrap_or_default();
            // Keep position; refresh size from current mode so a
            // mode-change reconcile picks it up as a Resized.
            WmRect::new(prev.x, prev.y, w as f32, h as f32)
        } else {
            let b = WmRect::new(next_x, 0.0, w as f32, h as f32);
            next_x += w as f32;
            b
        };
        out.push(ConnectorSnapshot { handle, bounds });
    }
    out
}

// ---------------------------------------------------------------------------
// Per-tick work
// ---------------------------------------------------------------------------

/// Drain any pending socket connections. Mirrors the same logic as the winit
/// backend but lives inline since the calloop adapter for `ListeningSocket`
/// adds more ceremony than the inline poll buys us.
/// Insert each queued `wp_security_context` listener into the event loop. When
/// a client connects through one, it's inserted tagged with that security
/// context (so the manager filter excludes it from creating nested contexts).
fn drain_security_listeners(handle: &LoopHandle<'static, LoopData>, data: &mut LoopData) {
    if data.state.pending_security_listeners.is_empty() {
        return;
    }
    for (source, ctx) in std::mem::take(&mut data.state.pending_security_listeners) {
        let res = handle.insert_source(source, move |stream, _, data: &mut LoopData| {
            let cs = crate::state::ClientState {
                security_context: Some(ctx.clone()),
                ..Default::default()
            };
            if let Err(e) = data
                .display
                .handle()
                .insert_client(stream, std::sync::Arc::new(cs))
            {
                warn!(?e, "security-context: insert_client failed");
            }
        });
        if let Err(e) = res {
            warn!(?e, "security-context: failed to insert listener source");
        }
    }
}

fn accept_pending_clients(data: &mut LoopData) -> Result<()> {
    loop {
        match data.socket.accept() {
            Ok(Some(stream)) => {
                info!("wayland client connected");
                let _ = data
                    .display
                    .handle()
                    .insert_client(stream, BacakState::new_client_state())
                    .map_err(|e| anyhow!("insert_client: {e}"))?;
            }
            Ok(None) => return Ok(()),
            Err(e) => {
                warn!(?e, "listener.accept failed");
                return Ok(());
            }
        }
    }
}

/// For every target that's flagged dirty and not waiting on a flip,
/// build its per-output element list, render, and queue. The element
/// list is rebuilt per target so each one only paints the windows on
/// its active workspace, in its own output-local coordinate space.
/// Render the scene (no overlay) for `output_id` into an offscreen
/// texture, then run the two-pass Gaussian over it. The returned
/// texture is full-output-sized; the switcher backdrop samples the
/// crop under the panel.
fn blurred_backdrop(
    data: &mut LoopData,
    output_id: OutputId,
    ow: i32,
    oh: i32,
    scale: i32,
) -> Result<GlesTexture> {
    let scene: Vec<BacakElements> =
        build_output_frame(&data.state, &mut data.renderer, scale, output_id, false, None);

    let bsize = UtilsSize::<i32, BufferCoord>::from((ow, oh));
    let mut off = data
        .renderer
        .create_buffer(DrmFourcc::Abgr8888, bsize)
        .map_err(|e| anyhow!("blur create_buffer: {e:?}"))?;
    {
        let mut fb = data
            .renderer
            .bind(&mut off)
            .map_err(|e| anyhow!("blur bind offscreen: {e:?}"))?;
        let psize = UtilsSize::<i32, Physical>::from((ow, oh));
        let mut dt = OutputDamageTracker::new(psize, Scale::from(scale as f64), Transform::Normal);
        dt.render_output(&mut data.renderer, &mut fb, 0, &scene, CLEAR_COLOR)
            .map_err(|e| anyhow!("blur offscreen render_output: {e:?}"))?;
    }

    let blur = data
        .blur
        .as_ref()
        .ok_or_else(|| anyhow!("blur program unavailable"))?;
    let radius = data.state.config.blur_radius;
    blur.blur(&mut data.renderer, &off, bsize, radius)
        .ok_or_else(|| anyhow!("blur pass returned None"))
}

/// Render the output's scene (no overlay) to an offscreen texture — the source
/// the selection magnifier loupe samples. Mirrors [`blurred_backdrop`]'s
/// capture, minus the blur. `None` on any GL hiccup (caller skips the loupe).
fn capture_scene_to_tex(
    data: &mut LoopData,
    output_id: OutputId,
    ow: i32,
    oh: i32,
    scale: i32,
) -> Option<GlesTexture> {
    let scene: Vec<BacakElements> =
        build_output_frame(&data.state, &mut data.renderer, scale, output_id, false, None);
    let bsize = UtilsSize::<i32, BufferCoord>::from((ow, oh));
    let mut off = data.renderer.create_buffer(DrmFourcc::Abgr8888, bsize).ok()?;
    {
        let mut fb = data.renderer.bind(&mut off).ok()?;
        let psize = UtilsSize::<i32, Physical>::from((ow, oh));
        let mut dt =
            OutputDamageTracker::new(psize, Scale::from(scale as f64), Transform::Normal);
        dt.render_output(&mut data.renderer, &mut fb, 0, &scene, CLEAR_COLOR)
            .ok()?;
    }
    Some(off)
}

/// Drain the shared config watcher; on a change reload
/// `compositor.json` and wake every target so new visuals apply
/// immediately.
fn maybe_reload_config(data: &mut LoopData) {
    let changed = data
        .config_watch
        .as_ref()
        .map(|w| w.poll_changed())
        .unwrap_or(false);
    if !changed {
        return;
    }
    let new = CompositorConfig::load();
    tracing::info!(blur = new.blur, "compositor config reloaded");
    data.state.blur_enabled = new.blur_enabled();
    data.state.config = new;
    // The dock toggle / height may have changed: re-apply the bottom
    // strut so snap math and `work_area` track the new bar.
    data.state.apply_dock_strut();
    for t in data.targets.iter_mut() {
        t.needs_redraw = true;
    }
}

fn render_dirty_targets(data: &mut LoopData) -> Result<()> {
    let mut any_redrew = false;
    // Glassmorphism backdrop blur, resolved once at startup from the
    // persistent config (`compositor.json`, with a `BACAK_BLUR` env
    // override). The default path below is byte-identical when it's
    // off, so the working compositor is never at risk.
    let blur_on = data.state.blur_enabled;
    if blur_on && !data.blur_attempted {
        data.blur = crate::blur::Blur::new(&mut data.renderer);
        data.blur_attempted = true;
    }

    // Refresh Overview window snapshots while windows are mapped, so a window
    // that later minimises still shows a real thumbnail (its live surface is
    // gone by then). Only when something's being drawn this frame — a static
    // desktop's snapshots stay valid — and here, before the per-target build,
    // the renderer is unbound (mirrors the blur backdrop RTT). Throttled +
    // self-pruning inside.
    // Resolve any queued dma-buf imports against the live renderer before we
    // render — rejects un-sampleable buffers so clients renegotiate instead of
    // showing black, and unblocks clients waiting on the import notifier.
    crate::render::process_pending_dmabuf(&mut data.state, &mut data.renderer);
    // Fulfil any screencopy captures against the live renderer.
    crate::render::process_pending_screencopy(&mut data.state, &mut data.renderer);
    // Take a built-in PrintScreen screenshot to disk, if one was requested.
    crate::render::process_pending_screenshot(&mut data.state, &mut data.renderer);

    if data
        .targets
        .iter()
        .any(|t| t.needs_redraw && !t.flip_pending)
    {
        crate::render::capture_window_snapshots(&data.state, &mut data.renderer);
    }

    // Index loop so we can re-borrow the same target mutably (the
    // build_styled_elements call takes an immutable borrow of state +
    // mutable of renderer, no overlap with the target itself).
    for i in 0..data.targets.len() {
        let t = &data.targets[i];
        if !t.needs_redraw || t.flip_pending {
            continue;
        }
        let output_id = t.output_id;
        let (ow, oh) = (t.output_size.w, t.output_size.h);
        let scale = t.scale;

        // Frosted glass for the switcher: render the scene (no overlay)
        // to an offscreen texture, blur it, and feed it back as the
        // switcher backdrop. Only when the switcher is actually up on
        // this output — otherwise there's nothing to frost.
        let elements: Vec<BacakElements> = if blur_on && data.blur.is_some() {
            // Phase 2: always produce the blurred scene texture when
            // blur is on. `build_output_frame` feeds it both to passive
            // windows (frosted glass behind each) and the switcher
            // backdrop.
            match blurred_backdrop(data, output_id, ow, oh, scale) {
                Ok(blurred) => build_output_frame(
                    &data.state,
                    &mut data.renderer,
                    scale,
                    output_id,
                    true,
                    Some(&blurred),
                ),
                Err(e) => {
                    // Any GL hiccup → fall back to the crisp frame this
                    // tick; don't kill the session.
                    warn!(?e, output = output_id, "blur pass failed; crisp frame");
                    build_styled_elements_for_output(
                        &data.state,
                        &mut data.renderer,
                        scale,
                        output_id,
                    )
                }
            }
        } else {
            build_styled_elements_for_output(&data.state, &mut data.renderer, scale, output_id)
        };

        // Selection magnifier: capture the scene to a texture and prepend a
        // zoomed crop above the finger. Only while the loupe is active, and any
        // capture failure just skips it (the crisp frame is untouched).
        let mut elements = elements;
        if let Some((lfx, lfy)) = data.state.loupe {
            let (off_x, off_y) = data
                .state
                .wm
                .output(output_id)
                .map(|o| (o.bounds.x as i32, o.bounds.y as i32))
                .unwrap_or((0, 0));
            if let Some(tex) = capture_scene_to_tex(data, output_id, ow, oh, scale) {
                let mut with_loupe = crate::render::loupe_elements(
                    &mut data.renderer, tex, lfx, lfy, scale, off_x, off_y, ow, oh,
                );
                with_loupe.extend(elements);
                elements = with_loupe;
            }
        }

        let t = &mut data.targets[i];
        // Scanout disabled: with `FrameFlags::DEFAULT`, smithay promotes a
        // fullscreen opaque client (e.g. OpenBoard) onto a DRM plane *above*
        // our composited framebuffer, hiding every compositor-drawn overlay
        // (dock, Control Center, Overview). Forcing a full GL composite makes
        // the DrmCompositor honour our element z-order, so our UI always
        // paints on top of clients — including fullscreen ones. (We use a
        // software cursor element, so we lose no cursor-plane benefit.)
        let result = t
            .compositor
            .render_frame::<_, BacakElements>(
                &mut data.renderer,
                &elements,
                CLEAR_COLOR,
                FrameFlags::empty(),
            )
            .map_err(|e| anyhow!("render_frame(output={output_id}): {e:?}"))?;

        if !result.is_empty {
            t.compositor
                .queue_frame(())
                .map_err(|e| anyhow!("queue_frame(output={output_id}): {e:?}"))?;
            t.flip_pending = true;
        }
        t.needs_redraw = false;
        any_redrew = true;
    }

    // Fire frame callbacks once per tick (not per target) so clients
    // don't get N copies. `send_frames_to_surface_tree` is idempotent
    // per-surface; we let it run even when nothing redrew because a
    // mid-animation client still needs the heartbeat.
    if any_redrew || !data.targets.is_empty() {
        let elapsed_ms = data.state.start_time.elapsed().as_millis() as u32;
        for surface in data.state.xdg_shell_state.toplevel_surfaces() {
            let root = surface.wl_surface();
            crate::runtime::send_frames_to_surface_tree(root, elapsed_ms);
            // Popups (menus, dropdowns) are separate surfaces, not subsurfaces,
            // so the toplevel walk above misses them. Without their own frame
            // callbacks GTK/Qt throttle the popup and it never finishes its
            // first paint / stops updating on hover — menus look frozen.
            for (popup, _) in
                smithay::desktop::PopupManager::popups_for_surface(root)
            {
                crate::runtime::send_frames_to_surface_tree(popup.wl_surface(), elapsed_ms);
            }
        }
        // Layer surfaces (panels, OSK, …) — and their popups — also throttle on
        // frame callbacks.
        for output in data.state.outputs.values() {
            for layer in smithay::desktop::layer_map_for_output(output).layers() {
                let ls = layer.wl_surface();
                crate::runtime::send_frames_to_surface_tree(ls, elapsed_ms);
                for (popup, _) in smithay::desktop::PopupManager::popups_for_surface(ls) {
                    crate::runtime::send_frames_to_surface_tree(popup.wl_surface(), elapsed_ms);
                }
            }
        }
        // IME candidate popups.
        for popup in &data.state.ime_popups {
            crate::runtime::send_frames_to_surface_tree(popup.wl_surface(), elapsed_ms);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Gesture dispatch
// ---------------------------------------------------------------------------

/// Route a recognised [`Gesture`] into the corresponding WM action.
/// For v1 only `SwitchWorkspace` is wired — the others (`TaskOverview`,
/// `ShowDesktop`, …) need shell-side surfaces that don't exist yet.
///
/// The gesture comes from the primary output's touchscreen by
/// convention; libinput does support per-device output binding but we
/// don't surface it yet. For multi-touchscreen setups, this is the
/// place to thread `device_output(id) → output_id`.
fn dispatch_gesture(data: &mut LoopData, gesture: Gesture) {
    // The `GesturePlugin` owns the gesture→action policy (workspace switch,
    // overview, …); a redraw on every target if it handled it. See
    // `crate::plugins`.
    if crate::plugins::dispatch_gesture(&mut data.state, gesture) {
        for t in data.targets.iter_mut() {
            t.needs_redraw = true;
        }
    }
}

// ---------------------------------------------------------------------------
// Input forwarding
// ---------------------------------------------------------------------------

/// Translate one libinput event into BacakState's input pipeline. Mirrors
/// the winit `forward_input` helper in [`crate::runtime`]; kept separate so
/// the two backends can diverge as gesture handling matures (libinput
/// already gives us coalesced swipe / pinch events that winit doesn't).
fn forward_libinput_event(
    data: &mut LoopData,
    ev: InputEvent<LibinputInputBackend>,
) {
    match ev {
        InputEvent::Keyboard { event } => {
            let serial = SERIAL_COUNTER.next_serial();
            let key_state = event.state();
            // The filter callback runs inside Smithay's keyboard state
            // machine. Returning `Intercept` swallows the event so the
            // focused client never sees the keysym — exactly what we
            // want for compositor shortcuts like Super+1..9.
            let intercepted = data
                .keyboard
                .input::<(), _>(
                    &mut data.state,
                    event.key_code(),
                    key_state,
                    serial,
                    (event.time() / 1000) as u32,
                    |state, mods, keysym| {
                        // Commit a running alt+tab cycle the moment Alt
                        // is no longer held. Smithay applies modifier
                        // changes to `mods` *before* invoking the
                        // filter, so `!mods.alt` here means the user
                        // just released the alt key (or never had it
                        // held during the cycle, which can't happen
                        // because the cycle only starts on Alt+Tab).
                        if state.focus_history.is_cycling() && !mods.alt {
                            state.alt_tab_commit();
                            // Fall through — the alt-up itself isn't
                            // a shortcut we want to swallow.
                        }

                        // Bare Super tap → toggle the dock (window-presence
                        // summon). Armed on a lone Super press, disarmed when
                        // any other key joins the chord (Super+digit/H keep
                        // working), fired on Super release. The release is
                        // forwarded (not intercepted) so the client sees a
                        // clean press+release — no stuck modifier.
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

                        if key_state != KeyState::Pressed {
                            return FilterResult::Forward;
                        }

                        // We accept the raw Latin sym so Shift+Tab and
                        // ISO_Left_Tab both look the same as Tab, and
                        // exotic layouts still hand us the canonical
                        // function-key keysyms.
                        let Some(sym) = keysym.raw_latin_sym_or_raw_current_sym() else {
                            return FilterResult::Forward;
                        };
                        let raw = sym.raw();

                        // Escape during an alt+tab cycle cancels and
                        // restores the prior focus. We swallow the
                        // Escape — clients shouldn't see it.
                        const KEY_ESCAPE: u32 = 0xff1b;
                        if state.focus_history.is_cycling() && raw == KEY_ESCAPE {
                            state.alt_tab_cancel();
                            return FilterResult::Intercept(());
                        }

                        // Escape with a dock context menu open just
                        // dismisses the menu — the compositor swallows
                        // the key so the focused client doesn't see it.
                        if raw == KEY_ESCAPE
                            && (state.dock_menu.is_some()
                                || state.apps_menu.is_some()
                                || state.control_center.is_some()
                                || state.overview.is_some()
                                || state.floating_menu.is_some()
                                || state.text_panel.is_some()
                                || state.atspi_selection.is_some()
                                || state.region_shot.is_some()
                                || state.window_pick)
                        {
                            state.dock_close_menu();
                            state.close_apps_menu();
                            state.close_control_center();
                            state.close_overview();
                            state.close_selection_menu();
                            state.close_text_panel();
                            state.clear_atspi_selection();
                            state.region_shot = None; // cancel a region screenshot
                            state.window_pick = false; // cancel window-pick
                            return FilterResult::Intercept(());
                        }

                        // Super+T → toggle the Tier A native text panel (the
                        // selection demo). Swallowed so the client never sees T.
                        {
                            const KEY_T: u32 = 0x74;
                            if mods.logo && !mods.ctrl && !mods.alt && raw == KEY_T {
                                if let Some(out) = state.wm.primary_output() {
                                    state.toggle_text_panel(out);
                                }
                                return FilterResult::Intercept(());
                            }
                        }

                        // PrintScreen → full-output screenshot to a PNG
                        // (~/Pictures). Shift+PrintScreen → interactive region
                        // select (drag a rectangle, release to capture). The key
                        // handler has no renderer; it flags the request and the
                        // render tick captures it.
                        {
                            const KEY_PRINT: u32 = 0xff61;
                            if raw == KEY_PRINT {
                                if mods.alt {
                                    // Alt+PrintScreen → window-pick mode (click a
                                    // window to capture it). Output resolved at click.
                                    state.window_pick = true;
                                } else if let Some(output) =
                                    state.pointer_output().or_else(|| state.wm.primary_output())
                                {
                                    if mods.shift {
                                        state.region_shot = Some(crate::state::RegionShot {
                                            output,
                                            anchor: None,
                                            cur: state.pointer_position,
                                        });
                                    } else {
                                        state.pending_screenshot = Some(crate::state::ScreenshotReq {
                                            output,
                                            region: None,
                                            window: None,
                                        });
                                    }
                                }
                                return FilterResult::Intercept(());
                            }
                        }

                        // Recent-Apps Overview is open → arrows / Tab drive
                        // the carousel selection, Enter activates the centred
                        // card. Takes precedence over the Alt+Tab switcher
                        // below, and swallows the keys so clients don't see
                        // them. (Escape was handled just above.)
                        if state.overview.is_some() {
                            const KEY_LEFT: u32 = 0xff51;
                            const KEY_RIGHT: u32 = 0xff53;
                            const KEY_TAB: u32 = 0xff09;
                            const KEY_ISO_LEFT_TAB: u32 = 0xfe20;
                            const KEY_RETURN: u32 = 0xff0d;
                            const KEY_KP_ENTER: u32 = 0xff8d;
                            match raw {
                                KEY_LEFT => {
                                    state.overview_wheel(-1);
                                    return FilterResult::Intercept(());
                                }
                                KEY_RIGHT => {
                                    state.overview_wheel(1);
                                    return FilterResult::Intercept(());
                                }
                                KEY_TAB | KEY_ISO_LEFT_TAB => {
                                    state.overview_wheel(if mods.shift { -1 } else { 1 });
                                    return FilterResult::Intercept(());
                                }
                                KEY_RETURN | KEY_KP_ENTER => {
                                    state.overview_activate_selected();
                                    return FilterResult::Intercept(());
                                }
                                _ => {}
                            }
                        }

                        // Applications menu open → plain typing filters the
                        // grid, Backspace edits, Enter launches the top match.
                        // Gated to no Ctrl/Super/Alt so real shortcuts still
                        // pass. (Escape was handled above → it closes the menu.)
                        if state.apps_menu.is_some() && !mods.ctrl && !mods.logo && !mods.alt {
                            const KEY_BACKSPACE: u32 = 0xff08;
                            const KEY_RETURN: u32 = 0xff0d;
                            const KEY_KP_ENTER: u32 = 0xff8d;
                            match raw {
                                KEY_BACKSPACE => {
                                    state.apps_menu_backspace();
                                    return FilterResult::Intercept(());
                                }
                                KEY_RETURN | KEY_KP_ENTER => {
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

                        // Wi-Fi picker text entry (password / static IP) from a
                        // physical keyboard — capture into our own buffer.
                        if state.wifi_text_active() && !mods.ctrl && !mods.logo && !mods.alt {
                            let utf = smithay::input::keyboard::xkb::keysym_to_utf8(
                                keysym.modified_sym(),
                            );
                            if state.wifi_physical_key(raw, &utf) {
                                return FilterResult::Intercept(());
                            }
                        }
                        // Bluetooth PIN / passkey entry from a physical keyboard.
                        if state.bt_pin_active() && !mods.ctrl && !mods.logo && !mods.alt {
                            let utf = smithay::input::keyboard::xkb::keysym_to_utf8(
                                keysym.modified_sym(),
                            );
                            if state.bt_pin_key(raw, &utf) {
                                return FilterResult::Intercept(());
                            }
                        }

                        // Alt+Tab / Alt+Shift+Tab — but only when no
                        // other compositor modifier is held. Ctrl+Alt+Tab
                        // and Super+Alt+Tab fall through to clients,
                        // which keeps things like the IDE's compile-time
                        // shortcuts working.
                        const KEY_TAB: u32 = 0xff09;
                        const KEY_ISO_LEFT_TAB: u32 = 0xfe20;
                        if mods.alt
                            && !mods.ctrl
                            && !mods.logo
                            && (raw == KEY_TAB || raw == KEY_ISO_LEFT_TAB)
                        {
                            let reverse = mods.shift;
                            let target = if state.focus_history.is_cycling() {
                                state.alt_tab_advance(reverse)
                            } else {
                                state.alt_tab_start(reverse)
                            };
                            if target.is_none() {
                                tracing::debug!(
                                    "Alt+Tab: nothing to cycle (need ≥2 visible windows)"
                                );
                            }
                            return FilterResult::Intercept(());
                        }

                        // Ctrl+Alt+Left/Right → previous / next workspace.
                        if mods.ctrl && mods.alt && !mods.logo {
                            const KEY_LEFT: u32 = 0xff51;
                            const KEY_RIGHT: u32 = 0xff53;
                            let delta = match raw {
                                KEY_LEFT => Some(-1),
                                KEY_RIGHT => Some(1),
                                _ => None,
                            };
                            if let Some(delta) = delta {
                                if let Some(output) = state.pointer_output() {
                                    let _ = state.switch_workspace_relative(output, delta);
                                }
                                return FilterResult::Intercept(());
                            }
                        }

                        // Super+H: minimise the window under the
                        // pointer, or un-minimise the topmost hidden
                        // one if the pointer's over bare desktop.
                        if mods.logo && !mods.ctrl && !mods.alt && raw == 0x68 {
                            state.toggle_minimize_at_pointer();
                            return FilterResult::Intercept(());
                        }

                        // Super+digit shortcuts. Match the existing
                        // gate exactly (no Ctrl/Alt) so an accidental
                        // Alt+Super+1 doesn't fire either path.
                        if mods.logo && !mods.ctrl && !mods.alt {
                            let index = match raw {
                                0x31..=0x39 => Some((raw - 0x31) as usize),
                                0x30 => Some(9),
                                _ => None,
                            };
                            if let Some(index) = index {
                                let result = if mods.shift {
                                    state.move_focused_window_to_index(index)
                                } else if let Some(output) = state.pointer_output() {
                                    state.switch_workspace_index(output, index)
                                } else {
                                    Ok(())
                                };
                                if let Err(err) = result {
                                    tracing::warn!(
                                        ?err,
                                        index,
                                        shift = mods.shift,
                                        "Super+digit shortcut failed"
                                    );
                                }
                                return FilterResult::Intercept(());
                            }
                        }

                        // Super+Alt+digit: keyboard equivalent of a
                        // click on the n-th tile of the dock under the
                        // pointer's output. 1..9 → slots 0..8, 0 → 10th
                        // (matching the workspace shortcut's wrap).
                        if mods.logo && mods.alt && !mods.ctrl && !mods.shift {
                            let index = match raw {
                                0x31..=0x39 => Some((raw - 0x31) as usize),
                                0x30 => Some(9),
                                _ => None,
                            };
                            if let Some(index) = index {
                                if let Some(output) = state.pointer_output() {
                                    state.activate_dock_index(output, index);
                                }
                                return FilterResult::Intercept(());
                            }
                        }

                        FilterResult::Forward
                    },
                )
                .is_some();

            if intercepted {
                // Slide animations only touch the primary output, but a
                // single-frame wake-up across every target is cheap and
                // avoids missing a redraw on a session that was idle.
                for t in data.targets.iter_mut() {
                    t.needs_redraw = true;
                }
            }
        }

        InputEvent::PointerMotion { event } => {
            // Relative mice: integrate the delta into the WM-global
            // pointer position, then clamp so the cursor can't escape
            // into the void past the rightmost output.
            let delta = event.delta();

            // wp_relative_pointer: forward the raw delta to the focused surface
            // (games/3D mouselook). Always sent; clients that didn't bind it
            // ignore it. Then, if that surface holds an *active locked* pointer
            // constraint, freeze the cursor in place — the client navigates by
            // relative deltas alone — and skip the absolute-motion path.
            let rel_focus = data.state.surface_under_pointer_with_loc();
            {
                let ev = smithay::input::pointer::RelativeMotionEvent {
                    delta,
                    delta_unaccel: event.delta_unaccel(),
                    utime: event.time(),
                };
                data.pointer.relative_motion(&mut data.state, rel_focus.clone(), &ev);
                data.pointer.frame(&mut data.state);
            }
            let locked = rel_focus.as_ref().map_or(false, |(surf, _)| {
                smithay::wayland::pointer_constraints::with_pointer_constraint(
                    surf,
                    &data.pointer,
                    |c| matches!(c, Some(c) if c.is_active()
                        && matches!(&*c, smithay::wayland::pointer_constraints::PointerConstraint::Locked(_))),
                )
            });
            if locked {
                for t in data.targets.iter_mut() {
                    t.needs_redraw = true;
                }
                return;
            }
            let (mut cx, mut cy) = data.state.wm.clamp_to_desktop(
                (data.state.pointer_position.0 + delta.x) as f32,
                (data.state.pointer_position.1 + delta.y) as f32,
            );
            // Confined pointer: if the surface we were over holds an active
            // *confined* constraint, keep the cursor inside that window's rect.
            // (Sub-region confinement isn't enforced — we clamp to the whole
            // window, which covers the common "confine to window" case.)
            let confined = rel_focus.as_ref().map_or(false, |(surf, _)| {
                smithay::wayland::pointer_constraints::with_pointer_constraint(
                    surf,
                    &data.pointer,
                    |c| matches!(c, Some(c) if c.is_active()
                        && matches!(&*c, smithay::wayland::pointer_constraints::PointerConstraint::Confined(_))),
                )
            });
            if confined {
                if let Some(w) = rel_focus
                    .as_ref()
                    .and_then(|(s, _)| data.state.window_for(s))
                    .and_then(|id| data.state.wm.get(id).ok())
                {
                    cx = cx.clamp(w.geom.x, (w.geom.x + w.geom.w - 1.0).max(w.geom.x));
                    cy = cy.clamp(w.geom.y, (w.geom.y + w.geom.h - 1.0).max(w.geom.y));
                }
            }
            data.state.pointer_position = (cx as f64, cy as f64);
            // Plugins owning the pointer (OSK title-strip drag / OSK hover-
            // capture / selection drag) consume motion and swallow it from the
            // client. See `crate::plugins`.
            if crate::plugins::pointer_motion(&mut data.state, cx as f64, cy as f64) {
                for t in data.targets.iter_mut() {
                    t.needs_redraw = true;
                }
                return;
            }
            // A region screenshot tracks the drag; window-pick just needs the
            // highlight to follow. Both swallow client motion while active.
            if data.state.region_shot.is_some() || data.state.window_pick {
                let pp = data.state.pointer_position;
                if let Some(rs) = data.state.region_shot.as_mut() {
                    rs.cur = pp;
                }
                for t in data.targets.iter_mut() {
                    t.needs_redraw = true;
                }
                return;
            }
            data.state.dock_pointer_motion(cx, cy);
            data.state.overview_pointer_motion(cx, cy);
            data.state.title_pointer_motion(cx, cy);

            // Hand the pointer the surface AND its WM-global origin so it
            // derives correct surface-local coordinates. Passing (0,0)
            // used to feed clients the *global* position, so clicks
            // landed on the wrong widget inside the window.
            let focus_pair = data.state.surface_under_pointer_with_loc();
            let focus_target: Option<WlSurface> =
                focus_pair.as_ref().map(|(s, _)| s.clone());
            let serial = SERIAL_COUNTER.next_serial();
            let time = (event.time() / 1000) as u32;
            let location = Point::<f64, smithay::utils::Logical>::from((cx as f64, cy as f64));
            data.pointer.motion(
                &mut data.state,
                focus_pair,
                &MotionEvent { location, serial, time },
            );
            data.pointer.frame(&mut data.state);

            if data.state.focus_policy == FocusPolicy::FocusFollowsPointer {
                data.keyboard.set_focus(&mut data.state, focus_target, serial);
            }
            for t in data.targets.iter_mut() {
                t.needs_redraw = true;
            }
        }

        InputEvent::PointerMotionAbsolute { event } => {
            // Tablets / touch pointers report `[0,1]` device coords; we
            // map them onto the desktop AABB so the cursor can land on
            // *any* output, not just primary. Adding `(db.x, db.y)`
            // back in lets us cope with layouts whose top-left isn't
            // at the origin (rare but legal).
            let db = data.state.wm.desktop_bounds();
            let size = UtilsSize::<i32, smithay::utils::Logical>::from((
                db.w as i32, db.h as i32,
            ));
            let pos = event.position_transformed(size);
            let gx = db.x as f64 + pos.x;
            let gy = db.y as f64 + pos.y;
            data.state.pointer_position = (gx, gy);
            // Plugins owning the pointer (OSK drag/hover, selection drag)
            // consume motion. See `crate::plugins`.
            if crate::plugins::pointer_motion(&mut data.state, gx, gy) {
                for t in data.targets.iter_mut() {
                    t.needs_redraw = true;
                }
                return;
            }
            // Region screenshot tracks the drag; window-pick follows the
            // highlight. Both swallow client motion while active.
            if data.state.region_shot.is_some() || data.state.window_pick {
                if let Some(rs) = data.state.region_shot.as_mut() {
                    rs.cur = (gx, gy);
                }
                for t in data.targets.iter_mut() {
                    t.needs_redraw = true;
                }
                return;
            }
            data.state.dock_pointer_motion(gx as f32, gy as f32);
            data.state.overview_pointer_motion(gx as f32, gy as f32);
            data.state.title_pointer_motion(gx as f32, gy as f32);

            // Same as above: real surface origin → correct surface-local
            // coordinates so client widgets respond where the cursor is.
            let focus_pair = data.state.surface_under_pointer_with_loc();
            let focus_target: Option<WlSurface> =
                focus_pair.as_ref().map(|(s, _)| s.clone());
            let serial = SERIAL_COUNTER.next_serial();
            let time = (event.time() / 1000) as u32;
            let location = Point::<f64, smithay::utils::Logical>::from((gx, gy));
            data.pointer.motion(
                &mut data.state,
                focus_pair,
                &MotionEvent { location, serial, time },
            );
            data.pointer.frame(&mut data.state);

            if data.state.focus_policy == FocusPolicy::FocusFollowsPointer {
                data.keyboard.set_focus(&mut data.state, focus_target, serial);
            }
            // Pointer movement isn't a per-output redraw signal — the
            // cursor lives on the cursor plane. We still wake every
            // target so an animated client under the pointer keeps
            // ticking; cheap given target counts.
            for t in data.targets.iter_mut() {
                t.needs_redraw = true;
            }
        }

        InputEvent::PointerButton { event } => {
            let serial = SERIAL_COUNTER.next_serial();
            let bstate = event.state();

            // Compositor dock owns left-clicks that land on a tile.
            // Press registers (and may start a pinned-tile drag);
            // release commits the drag (or fires the deferred click).
            // Either way the event is swallowed so it never reaches a
            // client or the click-to-focus fallback.
            let (px, py) = (
                data.state.pointer_position.0 as f32,
                data.state.pointer_position.1 as f32,
            );

            // An interactive region screenshot owns all pointer buttons while
            // active: left-press anchors the rectangle, release queues the
            // capture. The event never reaches a client.
            if data.state.region_shot.is_some() {
                if event.button_code() == BTN_LEFT {
                    match bstate {
                        ButtonState::Pressed => {
                            let pp = data.state.pointer_position;
                            if let Some(rs) = data.state.region_shot.as_mut() {
                                rs.anchor = Some(pp);
                                rs.cur = pp;
                            }
                        }
                        ButtonState::Released => {
                            if let Some(rs) = data.state.region_shot.take() {
                                if let Some(region) = rs.rect() {
                                    data.state.pending_screenshot =
                                        Some(crate::state::ScreenshotReq {
                                            output: rs.output,
                                            region: Some(region),
                                            window: None,
                                        });
                                }
                            }
                        }
                    }
                }
                for t in data.targets.iter_mut() {
                    t.needs_redraw = true;
                }
                return;
            }

            // Window-pick screenshot: a left-click captures the window under the
            // pointer (content + SSD title bar via `window_shot_rect`); any other
            // press cancels. The event never reaches a client.
            if data.state.window_pick {
                if event.button_code() == BTN_LEFT && bstate == ButtonState::Pressed {
                    if let Some(id) = data.state.wm.hit_test(px, py) {
                        if let Some(out) = data.state.wm.output_for_window(id).map(|o| o.id) {
                            data.state.pending_screenshot = Some(crate::state::ScreenshotReq {
                                output: out,
                                region: None,
                                window: Some(id), // isolated single-window capture
                            });
                        }
                    }
                    data.state.window_pick = false;
                } else if bstate == ButtonState::Pressed {
                    data.state.window_pick = false; // non-left press cancels
                }
                for t in data.targets.iter_mut() {
                    t.needs_redraw = true;
                }
                return;
            }
            // Right-press opens (or dismisses) the dock context menu;
            // left-press first consults an open menu, then falls
            // through to the normal press/drag/release flow.
            // Right-click: the dock context menu if it's on a tile, otherwise
            // our text action menu (Copy/Paste/…) at the pointer — a desktop
            // context menu. Both press and release are swallowed so the client
            // never sees a stray button.
            if event.button_code() == BTN_RIGHT {
                if bstate == ButtonState::Pressed && !data.state.dock_right_press(px, py) {
                    // Focus the window under the cursor so Copy/Paste's synthesised
                    // keys land in it — but only if there *is* one, else we'd
                    // clear focus and the keys would go nowhere. Then pop the menu.
                    if let Some(target) = data.state.surface_under_pointer() {
                        data.keyboard.set_focus(&mut data.state, Some(target), serial);
                    }
                    data.state.open_text_context_menu(px, py);
                }
                for t in data.targets.iter_mut() {
                    t.needs_redraw = true;
                }
                return;
            }
            // Middle-click closes the hovered Overview card (Android-style).
            if event.button_code() == BTN_MIDDLE
                && bstate == ButtonState::Pressed
                && data.state.overview_middle_click(px, py)
            {
                for t in data.targets.iter_mut() {
                    t.needs_redraw = true;
                }
                return;
            }
            if event.button_code() == BTN_LEFT {
                match bstate {
                    ButtonState::Pressed => {
                        // Shell plugins get first refusal, dispatched by input
                        // priority (keyboard → selection → screenshot → overview
                        // → menus → dock). See `crate::plugins`.
                        match crate::plugins::pointer_press(
                            &mut data.state,
                            px as f64,
                            py as f64,
                        ) {
                            // The keyboard consumed it: a press-and-hold on the
                            // title strip may start a drag — track the button.
                            Some("keyboard") => {
                                data.state.osk_pointer_down = true;
                                for t in data.targets.iter_mut() {
                                    t.needs_redraw = true;
                                }
                                return;
                            }
                            Some(_) => {
                                for t in data.targets.iter_mut() {
                                    t.needs_redraw = true;
                                }
                                return;
                            }
                            None => {}
                        }
                        // Core: window-decoration title-bar move (not a plugin).
                        if data.state.title_press(px, py) {
                            for t in data.targets.iter_mut() {
                                t.needs_redraw = true;
                            }
                            return;
                        }
                    }
                    ButtonState::Released => {
                        // A plugin owning the drag (OSK strip, selection,
                        // overview, dock) settles it.
                        if crate::plugins::pointer_release(
                            &mut data.state,
                            px as f64,
                            py as f64,
                        ) {
                            for t in data.targets.iter_mut() {
                                t.needs_redraw = true;
                            }
                            return;
                        }
                        // Core: window-decoration title-bar move release.
                        if data.state.title_release() {
                            return;
                        }
                    }
                }
            }

            data.pointer.button(
                &mut data.state,
                &ButtonEvent {
                    button: event.button_code(),
                    state: bstate,
                    serial,
                    time: (event.time() / 1000) as u32,
                },
            );
            data.pointer.frame(&mut data.state);

            if bstate == ButtonState::Pressed
                && data.state.focus_policy == FocusPolicy::ClickToFocus
            {
                let target = data.state.surface_under_pointer();
                data.keyboard.set_focus(&mut data.state, target, serial);
            }
        }

        InputEvent::TouchDown { event } => {
            let Some((ox, oy, size, scale)) = data.primary_origin_size() else { return };
            let logical = event.position_transformed(size.to_logical(scale));
            let slot: i32 = event.slot().into();
            // libinput touches are primary-output-local; shift by the
            // output's WM-global origin so click routing, dock
            // hit-tests, and the gesture aggregator all share one
            // coordinate system.
            let (tx, ty) = ((logical.x + ox) as f32, (logical.y + oy) as f32);
            // Shell plugins get first refusal on the touch, dispatched by input
            // priority (keyboard → selection → screenshot → menus → overview →
            // dock). Each plugin records its own drag slot. See `crate::plugins`.
            if crate::plugins::touch_press(&mut data.state, tx, ty, slot).is_some() {
                for t in data.targets.iter_mut() {
                    t.needs_redraw = true;
                }
                return;
            }
            // Feed the aggregator first, then arbitrate: one free finger goes
            // to the client (tap / drag); the second finger claims the whole
            // sequence as a compositor gesture and cancels the client's touch,
            // so a 3-finger swipe no longer rains touch points on the app.
            data.state.touch_aggregator.down(slot, tx, ty, event.time() / 1000);
            // Feed the two-finger window-gesture recogniser every down so it has
            // both fingers' positions when the second lands (it acts only at
            // exactly two fingers; see `BacakState::two_finger_down`).
            data.state.two_finger_down(slot, tx, ty, event.time() / 1000);
            // Feed the single-finger selection recogniser (long-press → action
            // menu). A second finger means a gesture/pinch, not a selection, so
            // drop the pending long-press. Uses the start-time clock so it
            // matches the per-frame `tick` poll in the main loop.
            if data.state.touch_aggregator.fingers() == 1 {
                let now_ms = data.state.start_time.elapsed().as_millis() as u64;
                data.state.selection_recognizer.down(tx, ty, now_ms);
                data.state.single_touch_slot = Some(slot);
            } else {
                data.state.selection_recognizer.cancel();
                data.state.single_touch_slot = None;
            }
            match data.state.touch_arbiter.down(data.state.touch_aggregator.fingers()) {
                crate::input::TouchRoute::Client => {
                    let loc =
                        Point::<f64, smithay::utils::Logical>::from((tx as f64, ty as f64));
                    // X11 (XWayland) apps get `wl_touch` only as XI2 touch,
                    // which core-pointer apps (OpenBoard) ignore. Emulate a
                    // left-button pointer drag from the first finger so they
                    // react to touch like the (working) mouse; Wayland apps
                    // keep native multitouch.
                    if data.state.touch_pointer_slot.is_none()
                        && data.state.hit_window_is_x11(tx, ty)
                    {
                        data.state.touch_pointer_slot = Some(slot);
                        let serial = SERIAL_COUNTER.next_serial();
                        let time = event.time_msec();
                        let focus = data.state.surface_at(tx as f64, ty as f64);
                        data.pointer.motion(
                            &mut data.state,
                            focus,
                            &MotionEvent { location: loc, serial, time },
                        );
                        data.pointer.button(
                            &mut data.state,
                            &ButtonEvent {
                                button: BTN_LEFT,
                                state: ButtonState::Pressed,
                                serial,
                                time,
                            },
                        );
                        data.pointer.frame(&mut data.state);
                    } else {
                        let focus = data.state.surface_at(tx as f64, ty as f64);
                        data.touch.down(
                            &mut data.state,
                            focus,
                            &DownEvent {
                                slot: event.slot(),
                                location: loc,
                                serial: SERIAL_COUNTER.next_serial(),
                                time: event.time_msec(),
                            },
                        );
                        data.touch.frame(&mut data.state);
                    }
                }
                crate::input::TouchRoute::Claim => {
                    // Second finger → gesture: revoke the first finger's
                    // touches from the client, and release any emulated
                    // pointer button so the X11 app doesn't see a stuck click.
                    data.state.loupe = None; // a multi-finger gesture, not a selection
                    data.touch.cancel(&mut data.state);
                    if data.state.touch_pointer_slot.is_some() {
                        let serial = SERIAL_COUNTER.next_serial();
                        let time = event.time_msec();
                        data.pointer.button(
                            &mut data.state,
                            &ButtonEvent {
                                button: BTN_LEFT,
                                state: ButtonState::Released,
                                serial,
                                time,
                            },
                        );
                        data.pointer.frame(&mut data.state);
                        data.state.touch_pointer_slot = None;
                    }
                }
                crate::input::TouchRoute::Gesture => {}
            }
        }

        InputEvent::TouchMotion { event } => {
            let Some((ox, oy, size, scale)) = data.primary_origin_size() else { return };
            let logical = event.position_transformed(size.to_logical(scale));
            let slot: i32 = event.slot().into();
            let (tx, ty) = ((logical.x + ox) as f32, (logical.y + oy) as f32);
            // A plugin owning a drag (text-panel/Tier-C selection, or the slot-
            // tracked OSK/overview/dock/apps-menu) continues it. See
            // `crate::plugins`.
            if crate::plugins::touch_motion(&mut data.state, tx, ty, slot) {
                for t in data.targets.iter_mut() {
                    t.needs_redraw = true;
                }
                return;
            }
            // Always track the finger for gesture recognition.
            data.state.touch_aggregator.motion(slot, tx, ty);
            // Movement past the slop cancels a pending long-press (it's a drag).
            data.state.selection_recognizer.motion(tx, ty);
            // Magnifier follows the finger during a single-finger selection drag.
            if data.state.loupe.is_some() && !data.state.touch_arbiter.is_gesture() {
                data.state.loupe = Some((tx, ty));
                for t in data.targets.iter_mut() {
                    t.needs_redraw = true;
                }
            }
            // The finger driving an emulated pointer (X11 app) → move the
            // pointer instead of delivering a client touch.
            if data.state.touch_pointer_slot == Some(slot) {
                let serial = SERIAL_COUNTER.next_serial();
                let time = event.time_msec();
                let loc = Point::<f64, smithay::utils::Logical>::from((tx as f64, ty as f64));
                let focus = data.state.surface_at(tx as f64, ty as f64);
                data.pointer.motion(
                    &mut data.state,
                    focus,
                    &MotionEvent { location: loc, serial, time },
                );
                data.pointer.frame(&mut data.state);
                return;
            }
            // Gesture sequences bypass the client (see the down arbitration).
            if data.state.touch_arbiter.is_gesture() {
                // Two fingers move the window under the centroid; 3-/4-finger
                // workspace/overview swipes are classified at touch-up instead.
                if data.state.two_finger_motion(slot, tx, ty) {
                    for t in data.targets.iter_mut() {
                        t.needs_redraw = true;
                    }
                }
            } else {
                let focus = data.state.surface_at(tx as f64, ty as f64);
                data.touch.motion(
                    &mut data.state,
                    focus,
                    &TouchMotionEvent {
                        slot: event.slot(),
                        location: Point::<f64, smithay::utils::Logical>::from((
                            tx as f64, ty as f64,
                        )),
                        time: event.time_msec(),
                    },
                );
                data.touch.frame(&mut data.state);
            }
        }

        InputEvent::TouchUp { event } => {
            let slot: i32 = event.slot().into();
            // A plugin owning the drag (selection settle, or the slot-tracked
            // OSK/overview/dock/apps-menu) finishes it + clears its slot.
            if crate::plugins::touch_up(&mut data.state, slot) {
                for t in data.targets.iter_mut() {
                    t.needs_redraw = true;
                }
                return;
            }
            // The finger driving an emulated pointer → release the button
            // (a click/drag-end) instead of a client touch-up.
            if data.state.touch_pointer_slot == Some(slot) {
                let serial = SERIAL_COUNTER.next_serial();
                let time = event.time_msec();
                data.pointer.button(
                    &mut data.state,
                    &ButtonEvent {
                        button: BTN_LEFT,
                        state: ButtonState::Released,
                        serial,
                        time,
                    },
                );
                data.pointer.frame(&mut data.state);
                data.state.touch_pointer_slot = None;
            } else if !data.state.touch_arbiter.is_gesture() {
                // Only the client-routed (single-finger) sequence gets an up;
                // gesture sequences were never delivered to the client.
                data.touch.up(
                    &mut data.state,
                    &UpEvent {
                        slot: event.slot(),
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                    },
                );
                data.touch.frame(&mut data.state);
            }
            // Clear the selection recogniser: a finger that lifts is no longer
            // a long-press candidate. Critical — without this the per-frame
            // tick would still fire a long-press 500 ms after a quick tap,
            // because the recogniser's live touch would never be cleared.
            data.state.selection_recognizer.cancel();
            data.state.single_touch_slot = None;
            let loupe_pt = data.state.loupe.take();
            if std::mem::take(&mut data.state.mouse_select) {
                // The mouse-emulated selection drag finished (button already
                // released above) → pop the Android-style Copy/Paste menu above
                // the selection's last point.
                if let Some((lx, ly)) = loupe_pt {
                    data.state.open_selection_menu(lx, ly);
                }
            }
            if loupe_pt.is_some() {
                for t in data.targets.iter_mut() {
                    t.needs_redraw = true;
                }
            }
            // Two-finger double-tap → toggle fullscreen / restore on the target.
            if data.state.two_finger_up(slot, event.time() / 1000) {
                for t in data.targets.iter_mut() {
                    t.needs_redraw = true;
                }
            }
            let gesture = data.state.touch_aggregator.up(slot, event.time() / 1000);
            // Drop the claim once the last finger lifts.
            data.state
                .touch_arbiter
                .up(data.state.touch_aggregator.is_active());
            if let Some(gesture) = gesture {
                tracing::info!(?gesture, "touch gesture recognised (udev)");
                dispatch_gesture(data, gesture);
            }
        }

        InputEvent::TouchCancel { .. } => {
            if data.state.dock_touch_slot.is_some() {
                data.state.dock_touch_cancel();
            }
            if data.state.overview_touch_slot.is_some() {
                data.state.overview_touch_slot = None;
                data.state.overview_drag = None;
            }
            if data.state.apps_menu_touch_slot.is_some() {
                data.state.apps_menu_touch_slot = None;
                data.state.apps_menu_touch_cancel();
            }
            if data.state.touch_pointer_slot.is_some() {
                let serial = SERIAL_COUNTER.next_serial();
                data.pointer.button(
                    &mut data.state,
                    &ButtonEvent {
                        button: BTN_LEFT,
                        state: ButtonState::Released,
                        serial,
                        time: 0,
                    },
                );
                data.pointer.frame(&mut data.state);
                data.state.touch_pointer_slot = None;
            }
            data.touch.cancel(&mut data.state);
            data.state.touch_aggregator.cancel();
            data.state.touch_arbiter.cancel();
            data.state.selection_recognizer.cancel();
            data.state.two_finger_cancel();
            data.state.loupe = None;
            data.state.single_touch_slot = None;
            data.state.mouse_select = false;
        }

        // Device enumerated by libinput — we silently accept everything.
        // Listening to `DeviceAdded` here is where capability-based seat
        // tuning would live (e.g. enabling tap-to-click for touchpads).
        InputEvent::PointerAxis { event } => {
            // While the Overview is open, the wheel drives the carousel and is
            // swallowed; otherwise it's left for the client-scroll path (not
            // yet wired — a separate gap).
            use smithay::backend::input::{Axis, PointerAxisEvent};
            if data.state.overview.is_some() {
                let v120 = event
                    .amount_v120(Axis::Vertical)
                    .or_else(|| event.amount_v120(Axis::Horizontal));
                let cont = event
                    .amount(Axis::Vertical)
                    .or_else(|| event.amount(Axis::Horizontal));
                let delta = crate::state::axis_notches(v120, cont);
                if delta != 0 {
                    data.state.overview_wheel(delta);
                    for t in data.targets.iter_mut() {
                        t.needs_redraw = true;
                    }
                }
            } else if data.state.apps_menu.is_some() {
                // Apps menu open → the wheel scrolls its grid.
                let v120 = event
                    .amount_v120(Axis::Vertical)
                    .or_else(|| event.amount_v120(Axis::Horizontal));
                let cont = event
                    .amount(Axis::Vertical)
                    .or_else(|| event.amount(Axis::Horizontal));
                let delta = crate::state::axis_notches(v120, cont);
                if delta != 0 {
                    data.state.apps_menu_scroll(delta);
                    for t in data.targets.iter_mut() {
                        t.needs_redraw = true;
                    }
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
                    &mut data.state,
                    &data.pointer,
                    event.time_msec(),
                    event.source(),
                    h,
                    v,
                );
            }
        }

        // 3-finger touchpad swipe → live workspace switch. The slide's
        // progress is pinned to the finger, so we redraw every frame of
        // the gesture (the 16ms tick would too, but this keeps it crisp).
        InputEvent::GestureSwipeBegin { event } => {
            data.state.ws_swipe_begin(event.fingers());
        }
        InputEvent::GestureSwipeUpdate { event } => {
            let (dx, dy) = (event.delta_x() as f32, event.delta_y() as f32);
            data.state.ws_swipe_update(dx, dy);
            for t in data.targets.iter_mut() {
                t.needs_redraw = true;
            }
        }
        InputEvent::GestureSwipeEnd { .. } => {
            data.state.ws_swipe_end();
            for t in data.targets.iter_mut() {
                t.needs_redraw = true;
            }
        }

        // Touchpad pinch → forwarded to the focused client via
        // `zwp_pointer_gestures_v1`. The compositor does NOT zoom anything
        // itself; Firefox / Chromium / etc. read these and do their own
        // (semantic) pinch-to-zoom. Goes to the surface under the pointer.
        InputEvent::GesturePinchBegin { event } => {
            let serial = SERIAL_COUNTER.next_serial();
            data.pointer.gesture_pinch_begin(
                &mut data.state,
                &smithay::input::pointer::GesturePinchBeginEvent {
                    serial,
                    time: event.time_msec(),
                    fingers: event.fingers(),
                },
            );
        }
        InputEvent::GesturePinchUpdate { event } => {
            data.pointer.gesture_pinch_update(
                &mut data.state,
                &smithay::input::pointer::GesturePinchUpdateEvent {
                    time: event.time_msec(),
                    delta: event.delta(),
                    scale: event.scale(),
                    rotation: event.rotation(),
                },
            );
        }
        InputEvent::GesturePinchEnd { event } => {
            let serial = SERIAL_COUNTER.next_serial();
            data.pointer.gesture_pinch_end(
                &mut data.state,
                &smithay::input::pointer::GesturePinchEndEvent {
                    serial,
                    time: event.time_msec(),
                    cancelled: event.cancelled(),
                },
            );
        }

        // --- Graphics tablet (stylus) --------------------------------------
        // Forward tool proximity / motion / pressure / tilt / tip / buttons to
        // clients via the seat's tablet-seat. Absolute coords map onto the
        // desktop AABB exactly like PointerMotionAbsolute. Tablet *pad* events
        // (rings/strips/body buttons) aren't forwarded yet.
        InputEvent::TabletToolProximity { event } => {
            let dh = data.display.handle();
            let tablet_seat = data.state.seat.tablet_seat();
            let desc = TabletDescriptor::from(&event.device());
            let tablet = tablet_seat.add_tablet::<BacakState>(&dh, &desc);
            let tool = tablet_seat.add_tool::<BacakState>(&mut data.state, &dh, &event.tool());

            let db = data.state.wm.desktop_bounds();
            let size = UtilsSize::<i32, smithay::utils::Logical>::from((db.w as i32, db.h as i32));
            let p = event.position_transformed(size);
            let (gx, gy) = (db.x as f64 + p.x, db.y as f64 + p.y);
            data.state.pointer_position = (gx, gy);
            let pos = Point::<f64, smithay::utils::Logical>::from((gx, gy));
            let serial = SERIAL_COUNTER.next_serial();
            let time = (event.time() / 1000) as u32;
            match event.state() {
                ProximityState::In => {
                    if let Some((surf, loc)) = data.state.surface_at(gx, gy) {
                        tool.proximity_in(pos, (surf, loc), &tablet, serial, time);
                    }
                }
                ProximityState::Out => tool.proximity_out(time),
            }
            for t in data.targets.iter_mut() {
                t.needs_redraw = true;
            }
        }

        InputEvent::TabletToolAxis { event } => {
            let tablet_seat = data.state.seat.tablet_seat();
            let desc = TabletDescriptor::from(&event.device());
            let db = data.state.wm.desktop_bounds();
            let size = UtilsSize::<i32, smithay::utils::Logical>::from((db.w as i32, db.h as i32));
            let p = event.position_transformed(size);
            let (gx, gy) = (db.x as f64 + p.x, db.y as f64 + p.y);
            data.state.pointer_position = (gx, gy);
            let pos = Point::<f64, smithay::utils::Logical>::from((gx, gy));
            let focus = data.state.surface_at(gx, gy);
            let time = (event.time() / 1000) as u32;
            if let (Some(tool), Some(tablet)) =
                (tablet_seat.get_tool(&event.tool()), tablet_seat.get_tablet(&desc))
            {
                tool.motion(pos, focus, &tablet, SERIAL_COUNTER.next_serial(), time);
                if event.pressure_has_changed() {
                    tool.pressure(event.pressure());
                }
                if event.distance_has_changed() {
                    tool.distance(event.distance());
                }
                if event.tilt_has_changed() {
                    tool.tilt(event.tilt());
                }
                if event.rotation_has_changed() {
                    tool.rotation(event.rotation());
                }
                if event.slider_has_changed() {
                    tool.slider_position(event.slider_position());
                }
            }
            for t in data.targets.iter_mut() {
                t.needs_redraw = true;
            }
        }

        InputEvent::TabletToolTip { event } => {
            let tablet_seat = data.state.seat.tablet_seat();
            let time = (event.time() / 1000) as u32;
            if let Some(tool) = tablet_seat.get_tool(&event.tool()) {
                // UFCS: the libinput event has an *inherent* `tip_state()`
                // returning its own enum, which would shadow the smithay trait
                // method — force the trait so we get smithay's `TabletToolTipState`.
                match TabletToolTipEvent::tip_state(&event) {
                    TabletToolTipState::Down => {
                        tool.tip_down(SERIAL_COUNTER.next_serial(), time)
                    }
                    TabletToolTipState::Up => tool.tip_up(time),
                }
            }
        }

        InputEvent::TabletToolButton { event } => {
            let tablet_seat = data.state.seat.tablet_seat();
            let time = (event.time() / 1000) as u32;
            if let Some(tool) = tablet_seat.get_tool(&event.tool()) {
                // UFCS for the same inherent-vs-trait reason as tip_state.
                tool.button(
                    TabletToolButtonEvent::button(&event),
                    TabletToolButtonEvent::button_state(&event),
                    SERIAL_COUNTER.next_serial(),
                    time,
                );
            }
        }

        InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. } => {}

        _ => {}
    }
}

// Silence the "unused" lint for the calloop signal: we hold it so a future
// fatal-error branch can request shutdown without restructuring this file.
#[allow(dead_code)]
fn stop_loop(data: &mut LoopData) {
    data.signal.stop();
}
