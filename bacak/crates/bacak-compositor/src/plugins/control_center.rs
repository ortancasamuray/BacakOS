//! Control-Center plugin — the quick-settings panel (Wi-Fi/BT/brightness/
//! volume/dark-mode/screenshot/power). Dispatches to `render_control_center`
//! and the `control_center_*` state. A modal overlay above the keyboard.
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::backend::renderer::gles::GlesRenderer;

use super::{Plugin, PluginCtx};
use crate::render::BacakElements;
use crate::state::BacakState;
use crate::wm::{OutputId, Rect};

// --- plugin-owned data types (Phase-2). Field instances + methods stay on
// `BacakState`; the definitions live here. ---

/// What a Control-Center tile does when clicked. Toggles flip a bool + fire a
/// system call; sliders map the click's x-position to a level; power buttons
/// fire and (mostly) end the session.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CcAction {
    WifiToggle,
    BtToggle,
    /// Open the Ethernet settings panel (manage + details, like Wi-Fi).
    EthernetSettings,
    DarkToggle,
    Volume,
    Brightness,
    /// Capture the whole output to ~/Pictures (same path as PrintScreen).
    Screenshot,
    PowerOff,
    Reboot,
    Logout,
    /// Open the audio output device picker.
    AudioSettings,
    /// Microphone input volume slider.
    MicVolume,
    /// Open the microphone input device picker.
    MicSettings,
    /// Open the Desktop Settings panel.
    DesktopSettings,
}

/// Visual flavour of a tile, read by the renderer. State (on/off, level) lives
/// on `ControlCenter` so a click mutates it in place without re-rasterising.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CcKind {
    /// Big rounded pill that lights up in the accent colour when on.
    Toggle,
    /// Horizontal track filled left-to-right to `level`.
    Slider,
    /// Flat action button; `danger` paints it red (power off / reboot).
    Button { danger: bool },
}

/// One Control-Center cell: its hit rect, behaviour, and the label glyphs
/// rasterised once when the panel opens.
pub struct CcTile {
    pub rect: Rect,
    pub action: CcAction,
    pub kind: CcKind,
    /// Primary label (e.g. "Wi-Fi"), pre-rasterised.
    pub label: Option<(MemoryRenderBuffer, usize, usize)>,
    /// Secondary line (e.g. the SSID, or "Açık"/"Kapalı"), pre-rasterised.
    pub sub: Option<(MemoryRenderBuffer, usize, usize)>,
}

/// macOS-style Control Center opened from the dock's gear button. Rebuilt fresh
/// on each open (so it reflects live Wi-Fi/Bluetooth/volume state); toggling a
/// tile mutates the snapshot fields in place — the renderer reads them directly.
pub struct ControlCenter {
    pub output: OutputId,
    pub panel: Rect,
    pub tiles: Vec<CcTile>,
    pub clock: Option<(MemoryRenderBuffer, usize, usize)>,
    // Live snapshot, captured at open and mutated by clicks.
    pub wifi_on: bool,
    pub bt_on: bool,
    pub volume: f32,
    pub muted: bool,
    pub mic_volume: f32,
}

/// What tapping a row in the Wi-Fi picker does.
#[derive(Clone)]
pub enum WifiAction {
    /// Connect to this network. `secured` networks prompt for a password via
    /// the on-screen keyboard.
    Connect { ssid: String, secured: bool },
    /// Just dismiss the panel (leave the radio as-is).
    ClosePanel,
    /// (Password mode) connect using the typed password.
    PwConnect,
    /// (Password mode) cancel back to the network list.
    PwCancel,
    /// Flip the Wi-Fi radio via the header switch (Android-style).
    ToggleRadio,
    /// Open the per-network details screen (tapping the connected network).
    OpenDetails,
    /// (Details) toggle auto-reconnect (Wi-Fi).
    ToggleAutoconnect,
    /// (Ethernet details) bring the link up / down.
    ToggleLink,
    /// (Details) switch to DHCP.
    SetDhcp,
    /// (Details) switch to a static IP (opens entry).
    SetStatic,
    /// (Details) back to the network list.
    Back,
    /// A read-only details row (MAC / IPv4 / IPv6) — tapping does nothing.
    Info,
    /// (Static entry) accept the current field → next field, or apply on the last.
    StaticNext,
    /// (Static entry) cancel back to the details screen.
    StaticCancel,
}

/// In-progress static-IP entry: which field is being typed and the values
/// gathered so far. The live field's text lives in `WifiPanel::pw_buf` (shared
/// with the password box).
#[derive(Clone)]
pub struct StaticEntry {
    pub conn: String,
    /// 0 = IP/prefix, 1 = gateway, 2 = DNS.
    pub step: u8,
    pub ip: String,
    pub gateway: String,
    /// Prefills for the gateway / DNS steps (the current active values).
    pub gw_prefill: String,
    pub dns_prefill: String,
}

/// One row in the Wi-Fi network list: its hit rect, behaviour, and labels
/// rasterised once when the panel opens.
pub struct WifiRow {
    pub rect: Rect,
    pub action: WifiAction,
    /// Network name / button caption, pre-rasterised.
    pub label: Option<(MemoryRenderBuffer, usize, usize)>,
    /// Right-aligned meta ("%92 · Kilitli"), pre-rasterised. `None` for buttons.
    pub meta: Option<(MemoryRenderBuffer, usize, usize)>,
    /// `true` for the currently-connected network (drawn with an accent tick).
    pub active: bool,
}

/// Native Wi-Fi picker, opened by tapping the Control-Center Wi-Fi tile. Lists
/// scanned networks (strongest first); a tap connects on a background thread so
/// the slow `nmcli` call never blocks the compositor.
pub struct WifiPanel {
    pub output: OutputId,
    pub panel: Rect,
    /// "Wi-Fi Ağları" header, pre-rasterised.
    pub title: Option<(MemoryRenderBuffer, usize, usize)>,
    /// Live status line ("Aranıyor…", "Bağlanıyor: X", …), re-rasterised on change.
    pub status: Option<(MemoryRenderBuffer, usize, usize)>,
    pub rows: Vec<WifiRow>,
    /// Whether the Wi-Fi radio is on — drives the header switch + whether the
    /// network list is shown (Android-style: off → just the switch).
    pub wifi_on: bool,
    /// Hit rect of the header on/off switch (list mode only).
    pub switch_rect: Rect,
    /// The last scan result, kept so we can rebuild the list view (e.g. after a
    /// connect, or when leaving password mode) without re-scanning.
    pub nets: Vec<crate::controls::WifiNet>,
    /// SSID a background thread is currently connecting to — keeps the render
    /// loop ticking (so the status updates) until the result lands. `None` idle.
    pub connecting: Option<String>,
    /// `true` while a background scan is running (spinner + keep redrawing).
    pub scanning: bool,
    /// When `Some(ssid)`, the panel is in password-entry mode for that network
    /// (the on-screen keyboard feeds `pw_buf`); the list rows are replaced by a
    /// masked field + Connect/Cancel buttons.
    pub pw_for: Option<String>,
    /// The password typed so far (password mode only).
    pub pw_buf: String,
    /// Whether the typed password is shown in clear text (vs masked dots).
    pub pw_show: bool,
    /// Password input box rect (password mode only) — the masked/clear text sits
    /// here with a reveal (eye) icon at its right end.
    pub pw_field: Option<Rect>,
    /// Pre-rasterised password text (placeholder / masked dots / clear).
    pub pw_text: Option<(MemoryRenderBuffer, usize, usize)>,
    /// When `Some`, the panel shows the per-network details screen (the
    /// connected network's settings: auto-reconnect, IP method, MAC, IPv4/IPv6).
    pub details: Option<crate::controls::WifiDetails>,
    /// Auto-reconnect switch hit rect (details mode only).
    pub ac_switch_rect: Rect,
    /// When `Some`, the panel is collecting a static IP config (IP → gateway →
    /// DNS) via the keyboard; the live field's text is in `pw_buf`.
    pub static_entry: Option<StaticEntry>,
    /// `true` when this details/static screen is for the Ethernet device (no
    /// network list → Back closes the panel; title says "Ethernet").
    pub eth: bool,
}

/// Background-worker → render-loop hand-off, drained in `tick_animations`.
pub enum WifiMsg {
    /// Instant cached scan (first paint); the receiver stays open for the fresh
    /// pass that follows.
    ScannedPartial(Vec<crate::controls::WifiNet>),
    /// The final fresh scan (real signals); rebuilds the list and ends scanning.
    Scanned(Vec<crate::controls::WifiNet>),
    /// A finished connect attempt, with nmcli's message (the failure reason on
    /// `!ok`).
    Connected { ssid: String, ok: bool, msg: String },
    /// Background-fetched details for the connected network (settings screen).
    Details(crate::controls::WifiDetails),
}

// ----- Bluetooth panel -------------------------------------------------------

/// One discovered/known Bluetooth device.
#[derive(Clone)]
pub struct BtDevice {
    pub mac: String,
    pub name: String,
    pub paired: bool,
    pub connected: bool,
}

/// What tapping a Bluetooth row does (resolved from the device's state).
#[derive(Clone)]
pub enum BtAction {
    /// Pair (+ trust + connect) a new device.
    Pair(String),
    /// Connect an already-paired device.
    Connect(String),
    /// Disconnect a connected device.
    Disconnect(String),
    /// Forget (unpair/remove) a paired device.
    Forget(String),
    /// A non-tappable section header row.
    Header,
    /// Start a fresh device discovery ("Cihazları Tara").
    Scan,
    /// Toggle the adapter via the header switch.
    TogglePower,
    /// Dismiss the panel.
    Close,
}

/// One row in the Bluetooth device list.
pub struct BtRow {
    pub rect: Rect,
    pub action: BtAction,
    pub label: Option<(MemoryRenderBuffer, usize, usize)>,
    pub meta: Option<(MemoryRenderBuffer, usize, usize)>,
    /// `true` for the connected device (accent highlight).
    pub connected: bool,
    /// For a paired row: the right-edge "Unut" (forget) hit rect + label.
    pub forget: Option<(Rect, crate::state::Label)>,
}

/// An in-progress pairing agent prompt overlaid on the panel.
#[derive(Clone)]
pub enum BtDialogKind {
    /// Confirm the displayed passkey matches the peer (yes/no).
    ConfirmPasskey(String),
    /// Show a passkey/PIN for the user to enter on the peer (acknowledge).
    DisplayPasskey(String),
    /// Type a PIN code on this side (via the keyboard).
    EnterPin,
    /// Type a numeric passkey on this side.
    EnterPasskey,
}

/// Native Bluetooth picker, opened from the Control-Center Bluetooth tile.
pub struct BtPanel {
    pub output: OutputId,
    pub panel: Rect,
    pub title: Option<(MemoryRenderBuffer, usize, usize)>,
    pub status: Option<(MemoryRenderBuffer, usize, usize)>,
    pub rows: Vec<BtRow>,
    pub powered: bool,
    pub switch_rect: Rect,
    /// Latest known devices (kept so a re-render needs no re-scan).
    pub devices: Vec<BtDevice>,
    pub scanning: bool,
    /// When scanning auto-stops (so the list settles and "Kapat" stops moving
    /// under the finger). `None` when not scanning.
    pub scan_deadline: Option<std::time::Instant>,
    /// MAC currently being paired/connected (status spinner). While `Some`, the
    /// list is frozen (no relayout) so pairing isn't disturbed.
    pub busy: Option<String>,
    /// Active pairing dialog, if any.
    pub dialog: Option<BtDialogKind>,
    /// Title/body of the dialog, pre-rasterised.
    pub dialog_title: Option<(MemoryRenderBuffer, usize, usize)>,
    pub dialog_body: Option<(MemoryRenderBuffer, usize, usize)>,
    /// PIN/passkey typed so far (EnterPin/EnterPasskey dialogs).
    pub pin_buf: String,
    /// Dialog box + button rects (computed together so render and hit-test agree).
    pub dlg_rect: Rect,
    pub dlg_ok_rect: Rect,
    pub dlg_cancel_rect: Rect,
    pub dlg_ok_label: Option<(MemoryRenderBuffer, usize, usize)>,
    pub dlg_cancel_label: Option<(MemoryRenderBuffer, usize, usize)>,
}

// ----- Audio device panel -----------------------------------------------

/// What tapping an audio device row does.
#[derive(Clone)]
pub enum AudioAction {
    /// Set this sink as the WirePlumber default output.
    SelectSink(String),
    /// Set this source as the WirePlumber default input.
    SelectSource(String),
    /// Dismiss the panel without changing anything.
    Close,
}

/// One row in the audio device list.
pub struct AudioRow {
    pub rect: Rect,
    pub action: AudioAction,
    /// Device name, pre-rasterised.
    pub label: Option<(MemoryRenderBuffer, usize, usize)>,
    /// True when this is the current default sink (accent checkmark).
    pub is_default: bool,
}

/// Audio output device picker — lists every PipeWire sink and lets the user
/// set a new default with one tap. Opened from the Control-Center volume tile.
pub struct AudioPanel {
    pub output: OutputId,
    pub panel: Rect,
    /// "Ses Çıkış Cihazı" header, pre-rasterised.
    pub title: Option<(MemoryRenderBuffer, usize, usize)>,
    pub rows: Vec<AudioRow>,
    /// Status line ("Cihazlar aranıyor…" or "Cihaz bulunamadı"), pre-rasterised.
    pub status: Option<(MemoryRenderBuffer, usize, usize)>,
}

/// Background-fetched Control-Center state (each field is a blocking subprocess),
/// handed to the render loop and applied in `tick_animations` so opening the
/// panel never blocks.
pub struct CcSnapshot {
    pub wifi_on: bool,
    pub bt_on: bool,
    pub volume: f32,
    pub muted: bool,
    pub mic_volume: f32,
    pub ssid: Option<String>,
    pub clock: String,
}

pub struct ControlCenterPlugin;

impl Plugin for ControlCenterPlugin {
    fn id(&self) -> &'static str {
        "control_center"
    }

    fn z(&self) -> i32 {
        60
    }

    fn input_z(&self) -> i32 {
        72
    }

    fn enabled(&self, state: &BacakState) -> bool {
        state.config.dock
    }

    fn on_pointer_press(&self, ctx: &mut PluginCtx, gx: f64, gy: f64) -> bool {
        ctx.state().control_center_left_press(gx as f32, gy as f32)
    }

    fn on_touch_press(&self, ctx: &mut PluginCtx, tx: f32, ty: f32, _slot: i32) -> bool {
        ctx.state().control_center_left_press(tx, ty)
    }

    fn render(
        &self,
        state: &BacakState,
        renderer: &mut GlesRenderer,
        output: OutputId,
        scale: i32,
        off_x: i32,
        off_y: i32,
        out: &mut Vec<BacakElements>,
    ) {
        crate::render::render_control_center(state, renderer, output, scale, off_x, off_y, out);
    }
}
