//! "Uzak Masaüstü" (Remote Desktop) panel — pairs with a `bacak-remote-server`
//! (see `../../../uzakel/uzakel-pc`) and displays its PC screen inside the
//! real Bacak OS desktop, forwarding local pointer input back to it. This is
//! the plugin `bacak-remote-client`'s own `render.rs` module doc names as
//! its intended home, instead of a standalone `winit` window — see that
//! crate's README for the two real bugs the standalone version's Wayland
//! testing found before this integration existed.
//!
//! ## Why a cross-workspace dependency instead of reimplementing the protocol
//!
//! `bacak-remote-proto` (PIN + ephemeral X25519 ECDH + HKDF-SHA256 +
//! ChaCha20-Poly1305, verified on real Windows/Linux hardware — see that
//! workspace's README) is pulled in as-is via a path dependency
//! (`../../../uzakel/uzakel-pc/bacak-remote-proto`). Reimplementing the same
//! handshake a second time here would be a second place to get the crypto
//! subtly wrong; a path dependency costs nothing at runtime and keeps the
//! two projects' wire formats identical by construction.
//!
//! ## What's duplicated anyway, and why
//!
//! `bacak-remote-client`'s own frame-reassembly (`decode.rs`) is small,
//! pure, and tangled up with that crate's `winit`/`wgpu` types — copying its
//! ~40 lines here (below) is simpler and lower-risk than extracting a shared
//! library crate just for this one struct. The pairing/receive network loop
//! (`run`, below) mirrors that crate's `network.rs` for the same reason:
//! this compositor is Smithay/calloop-driven, not `winit`, so the two loops
//! can't literally share code without a much larger refactor than this
//! feature warrants.
//!
//! ## v1 limitation: config-file pairing, not a text-entry dialog
//!
//! Typing an IP + PIN on the real desktop needs an on-screen-keyboard-driven
//! text field — real UI machinery this panel doesn't build yet. For now the
//! target server's address and PIN are read from
//! `~/.config/bacak-remote/pair.json` (`{"ip": "...", "pin": 123456}`),
//! written by hand for now; the panel just shows a clear status message if
//! it's missing or invalid rather than failing silently.

use std::net::{IpAddr, SocketAddr, UdpSocket as StdUdpSocket};
use std::sync::mpsc;
use std::sync::{Arc, Mutex as StdMutex};

use bacak_remote_proto::crypto::{Cipher, EphemeralKeypair, Opener};
use bacak_remote_proto::{
    decode, encode, open_message, seal_message, Codec, FrameInfo, InputEvent, Message, PointerButton as ProtoButton,
    DEFAULT_INPUT_PORT, DEFAULT_VIDEO_PORT,
};
use serde::Deserialize;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::Transform;

use crate::state::{BacakState, LABEL_SUPERSAMPLE};
use crate::text::TextRenderer;
use crate::wm::{OutputId, Rect};

fn rasterize(text: Option<&TextRenderer>, s: &str, px: f32, color: [u8; 3], max_w: usize) -> Option<(MemoryRenderBuffer, usize, usize)> {
    let ss = LABEL_SUPERSAMPLE;
    let (rgba, w, h) = text?.rasterize_line(s, px * ss as f32, color, max_w * ss as usize)?;
    let buf = MemoryRenderBuffer::from_slice(&rgba, Fourcc::Abgr8888, (w as i32, h as i32), ss, Transform::Normal, None);
    Some((buf, w / ss as usize, h / ss as usize))
}

#[derive(Deserialize)]
struct PairConfig {
    ip: IpAddr,
    pin: u32,
    #[serde(default = "default_video_port")]
    video_port: u16,
    #[serde(default = "default_input_port")]
    input_port: u16,
}
fn default_video_port() -> u16 {
    DEFAULT_VIDEO_PORT
}
fn default_input_port() -> u16 {
    DEFAULT_INPUT_PORT
}

fn pair_config_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    std::path::PathBuf::from(home).join(".config/bacak-remote/pair.json")
}

fn read_pair_config() -> Result<PairConfig, String> {
    let path = pair_config_path();
    let raw = std::fs::read_to_string(&path).map_err(|_| format!("{} bulunamadı", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("geçersiz pair.json: {e}"))
}

struct DecodedFrame {
    width: u32,
    height: u32,
    bgra: Vec<u8>,
}

/// One in-flight frame's chunks, reassembled the same "drop the incomplete
/// frame the moment a newer one starts" way as
/// `uzakel-pc/bacak-remote-client/src/decode.rs::FrameReassembler` — see
/// that file's doc comment for why (a live desktop stream prefers a dropped
/// frame over a stale one).
#[derive(Default)]
struct FrameReassembler {
    current: Option<InProgress>,
}
struct InProgress {
    info: FrameInfo,
    chunks: Vec<Option<Vec<u8>>>,
    received: u16,
}
impl FrameReassembler {
    fn start_frame(&mut self, info: FrameInfo) {
        self.current = Some(InProgress { chunks: vec![None; info.chunk_count as usize], received: 0, info });
    }

    fn add_chunk(&mut self, frame_id: u32, chunk_index: u16, data: Vec<u8>) -> Option<DecodedFrame> {
        let progress = self.current.as_mut()?;
        if progress.info.frame_id != frame_id {
            return None;
        }
        let slot = progress.chunks.get_mut(chunk_index as usize)?;
        if slot.is_none() {
            *slot = Some(data);
            progress.received += 1;
        }
        if progress.received < progress.info.chunk_count {
            return None;
        }
        let progress = self.current.take()?;
        let mut payload = Vec::with_capacity(progress.info.payload_len as usize);
        for chunk in progress.chunks {
            payload.extend_from_slice(&chunk?);
        }
        let bgra = match progress.info.codec {
            Codec::RawZstd => zstd::stream::decode_all(payload.as_slice()).ok()?,
        };
        Some(DecodedFrame { width: progress.info.width, height: progress.info.height, bgra })
    }
}

enum SessionStatus {
    Connecting,
    Failed(String),
    Connected { width: u32, height: u32 },
}

/// Owns the background pairing/receive thread and the encrypted input send
/// path. Dropping this stops the thread on its next socket timeout/error
/// (no explicit shutdown signal — matches the standalone client's
/// fire-and-forget teardown, acceptable since a session is one-per-panel and
/// short-lived).
struct RemoteDesktopSession {
    frame_rx: mpsc::Receiver<DecodedFrame>,
    status_rx: mpsc::Receiver<SessionStatus>,
    status: SessionStatus,
    input_socket: StdUdpSocket,
    input_cipher: Arc<StdMutex<Option<Cipher>>>,
}

impl RemoteDesktopSession {
    fn start(cfg: PairConfig) -> anyhow::Result<Self> {
        let (frame_tx, frame_rx) = mpsc::channel();
        let (status_tx, status_rx) = mpsc::channel();
        let input_cipher = Arc::new(StdMutex::new(None));

        let input_socket = StdUdpSocket::bind("0.0.0.0:0")?;
        input_socket.connect(SocketAddr::new(cfg.ip, cfg.input_port))?;
        input_socket.set_nonblocking(true)?;

        let thread_cipher = input_cipher.clone();
        let thread_status_tx = status_tx.clone();
        std::thread::Builder::new().name("bacak-remote-desktop-net".into()).spawn(move || {
            let runtime = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = thread_status_tx.send(SessionStatus::Failed(format!("tokio runtime: {e}")));
                    return;
                }
            };
            if let Err(e) = runtime.block_on(run(cfg, frame_tx, thread_status_tx.clone(), thread_cipher)) {
                let _ = thread_status_tx.send(SessionStatus::Failed(e.to_string()));
            }
        })?;

        Ok(Self { frame_rx, status_rx, status: SessionStatus::Connecting, input_socket, input_cipher })
    }

    /// Drains pending status updates and returns the newest decoded frame,
    /// if any arrived since the last call (older queued frames are dropped —
    /// same "freshest wins" policy as the reassembler above).
    fn poll(&mut self) -> Option<(u32, u32, Vec<u8>)> {
        while let Ok(s) = self.status_rx.try_recv() {
            self.status = s;
        }
        let mut latest = None;
        while let Ok(f) = self.frame_rx.try_recv() {
            latest = Some((f.width, f.height, f.bgra));
        }
        latest
    }

    fn send_input(&self, event: InputEvent) {
        let mut guard = self.input_cipher.lock().unwrap();
        let Some(cipher) = guard.as_mut() else { return };
        let Ok(sealed) = seal_message(&Message::Input(event), cipher) else { return };
        if let Ok(bytes) = encode(&sealed) {
            let _ = self.input_socket.send(&bytes);
        }
    }
}

async fn run(
    cfg: PairConfig,
    frame_tx: mpsc::Sender<DecodedFrame>,
    status_tx: mpsc::Sender<SessionStatus>,
    input_cipher: Arc<StdMutex<Option<Cipher>>>,
) -> anyhow::Result<()> {
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    socket.connect(SocketAddr::new(cfg.ip, cfg.video_port)).await?;

    let client_keypair = EphemeralKeypair::generate();
    let client_pubkey = client_keypair.public_bytes;
    let pair_request = encode(&Message::PairRequest { client_name: "bacak-os".into(), pin: cfg.pin, client_pubkey })?;

    let (server_pubkey, confirm_tag, (width, height)) = loop {
        socket.send(&pair_request).await?;
        let mut buf = [0u8; 512];
        if let Ok(Ok(len)) = tokio::time::timeout(std::time::Duration::from_millis(500), socket.recv(&mut buf)).await {
            match decode(&buf[..len]) {
                Ok(Message::PairResponse { accepted: true, server_pubkey, confirm_tag, screen_width, screen_height }) => {
                    break (server_pubkey, confirm_tag, (screen_width, screen_height));
                }
                Ok(Message::PairResponse { accepted: false, .. }) => anyhow::bail!("sunucu PIN'i reddetti"),
                _ => {}
            }
        }
    };

    let material = client_keypair.derive(server_pubkey, cfg.pin, client_pubkey, server_pubkey);
    if material.confirm_tag != confirm_tag {
        anyhow::bail!("eşleşme doğrulaması başarısız — yanlış PIN ya da güvenilmeyen bağlantı");
    }
    let video_keys = material.channel_keys("video");
    let input_keys = material.channel_keys("input");
    let mut video_opener = Opener::new(video_keys.s2c_key);
    *input_cipher.lock().unwrap() = Some(Cipher::new(input_keys.c2s_key));

    let _ = status_tx.send(SessionStatus::Connected { width, height });

    let mut reassembler = FrameReassembler::default();
    let mut buf = vec![0u8; 2048];
    loop {
        let len = socket.recv(&mut buf).await?;
        let Ok(msg) = decode(&buf[..len]) else { continue };
        let Ok(inner) = open_message(&msg, &mut video_opener) else { continue };
        match inner {
            Message::FrameInfo(info) => reassembler.start_frame(info),
            Message::FrameChunk(chunk) => {
                if let Some(frame) = reassembler.add_chunk(chunk.frame_id, chunk.chunk_index, chunk.data) {
                    if frame_tx.send(frame).is_err() {
                        return Ok(()); // panel closed
                    }
                }
            }
            _ => {}
        }
    }
}

/// The "Uzak Masaüstü" panel's live state — opened from the Control Center's
/// tile (see `plugins/control_center.rs`'s `CcAction::RemoteDesktopConnect`).
pub struct RemoteDesktopPanel {
    output: OutputId,
    /// Where the video frame is blitted (fills most of the output — this is
    /// meant to be looked at, unlike the small utility panels).
    video_rect: Rect,
    close_rect: Rect,
    close_label: Option<(MemoryRenderBuffer, usize, usize)>,
    status_label: Option<(MemoryRenderBuffer, usize, usize)>,
    session: RemoteDesktopSession,
    /// The most recently decoded frame, already wrapped as a render buffer —
    /// rebuilt (not mutated in place) each time a new frame arrives, since
    /// `MemoryRenderBuffer` has no in-place "replace the pixels" API here.
    frame: Option<(MemoryRenderBuffer, u32, u32)>,
    /// Last pointer position *inside `video_rect`*, in that rect's own
    /// 0.0..=1.0 normalized space — `None` right after the panel opens or
    /// the pointer re-enters the rect, so the first motion doesn't send a
    /// spurious huge delta from an arbitrary previous position.
    last_norm_pos: Option<(f32, f32)>,
}

impl BacakState {
    fn remote_desktop_status_text(status: &SessionStatus) -> String {
        match status {
            SessionStatus::Connecting => "Bağlanıyor…".to_string(),
            SessionStatus::Failed(e) => format!("Bağlantı hatası: {e}"),
            SessionStatus::Connected { width, height } => format!("Bağlandı — {width}x{height}"),
        }
    }

    /// Open the Remote Desktop panel: reads `~/.config/bacak-remote/pair.json`
    /// and starts pairing immediately. On a missing/invalid config, still
    /// opens the panel so the person sees *why* it can't connect instead of
    /// nothing happening when they tap the tile.
    pub fn open_remote_desktop_panel(&mut self, out: OutputId) {
        self.control_center = None;

        let bounds = self.wm.output(out).map(|o| o.bounds).unwrap_or(Rect::new(0.0, 0.0, 1920.0, 1080.0));
        let m = 40.0;
        let video_rect = Rect::new(bounds.x + m, bounds.y + m, (bounds.w - 2.0 * m).max(1.0), (bounds.h - 2.0 * m - 56.0).max(1.0));
        let close_rect = Rect::new(bounds.x + bounds.w - m - 120.0, bounds.y + bounds.h - m - 40.0, 120.0, 40.0);

        let text = self.text.as_ref();
        const LABEL: [u8; 3] = [232, 236, 244];
        let close_label = rasterize(text, "Kapat", 15.0, LABEL, 120);

        let outcome = read_pair_config().and_then(|cfg| RemoteDesktopSession::start(cfg).map_err(|e| e.to_string()));
        let (session, status_text) = match outcome {
            Ok(session) => {
                let text = Self::remote_desktop_status_text(&session.status);
                (session, text)
            }
            Err(e) => {
                // No server to talk to — a session that immediately reports
                // Failed, so the render/close path stays uniform (always a
                // session, never an Option<Option<..>>).
                let (tx, rx) = mpsc::channel();
                let (stx, srx) = mpsc::channel();
                let _ = stx.send(SessionStatus::Failed(e.clone()));
                drop(tx);
                let dummy_socket = StdUdpSocket::bind("0.0.0.0:0").expect("bind ephemeral UDP socket");
                let session = RemoteDesktopSession {
                    frame_rx: rx,
                    status_rx: srx,
                    status: SessionStatus::Failed(e.clone()),
                    input_socket: dummy_socket,
                    input_cipher: Arc::new(StdMutex::new(None)),
                };
                (session, Self::remote_desktop_status_text(&SessionStatus::Failed(e)))
            }
        };
        let status_label = rasterize(text, &status_text, 14.0, LABEL, video_rect.w as usize);

        self.remote_desktop_panel =
            Some(RemoteDesktopPanel { output: out, video_rect, close_rect, close_label, status_label, session, frame: None, last_norm_pos: None });
    }

    pub fn close_remote_desktop_panel(&mut self) {
        self.remote_desktop_panel = None;
    }

    /// Poll the active session for a new frame / status change, updating the
    /// panel's texture and status label. Returns `true` if a redraw is
    /// warranted (called from the plugin's `tick`).
    pub fn remote_desktop_tick(&mut self) -> bool {
        let Some(panel) = self.remote_desktop_panel.as_mut() else { return false };
        let mut dirty = false;
        if let Some((w, h, bgra)) = panel.session.poll() {
            let buf = MemoryRenderBuffer::from_slice(&bgra, Fourcc::Argb8888, (w as i32, h as i32), 1, Transform::Normal, None);
            panel.frame = Some((buf, w, h));
            dirty = true;
        }
        let status_text = Self::remote_desktop_status_text(&panel.session.status);
        let text = self.text.as_ref();
        panel.status_label = rasterize(text, &status_text, 14.0, [232, 236, 244], panel.video_rect.w as usize);
        dirty
    }

    /// A press while the panel is open: the close button dismisses it,
    /// anywhere else in `video_rect` starts a left-button press forwarded to
    /// the remote (a real click needs down+up, so this alone doesn't send a
    /// click — see [`Self::remote_desktop_pointer_release`]).
    pub fn remote_desktop_press(&mut self, px: f32, py: f32) -> bool {
        let Some(panel) = self.remote_desktop_panel.as_mut() else { return false };
        tracing::debug!("remote_desktop_press at ({px}, {py}), video_rect={:?}, close_rect={:?}", panel.video_rect, panel.close_rect);
        if panel.close_rect.contains(px, py) {
            self.remote_desktop_panel = None;
            return true;
        }
        if panel.video_rect.contains(px, py) {
            panel.last_norm_pos = Some(norm_in_rect(panel.video_rect, px, py));
            panel.session.send_input(InputEvent::PointerButton { button: ProtoButton::Left, pressed: true });
            return true;
        }
        // Any other press outside the panel closes it, matching the Uzakel
        // QR panel's "any tap dismisses" convention for a modal-ish overlay.
        self.remote_desktop_panel = None;
        true
    }

    /// While the pointer moves with the button down inside `video_rect`,
    /// forward a relative delta computed from the last normalized position —
    /// mirrors the standalone client's `CursorMoved`-based approach (see its
    /// README's "what real BacakOS desktop testing found" for why that, and
    /// not a raw device-motion event, is the reliable cross-backend source).
    pub fn remote_desktop_pointer_motion(&mut self, px: f32, py: f32) -> bool {
        let Some(panel) = self.remote_desktop_panel.as_mut() else { return false };
        if !panel.video_rect.contains(px, py) {
            panel.last_norm_pos = None;
            return false;
        }
        let (nx, ny) = norm_in_rect(panel.video_rect, px, py);
        if let Some((lx, ly)) = panel.last_norm_pos {
            // Scale the normalized delta back up to the *source* screen's
            // pixels (not this panel's on-screen size) so a full sweep of
            // the panel maps to a full sweep of the actual remote desktop.
            let (sw, sh) = match panel.frame {
                Some((_, w, h)) => (w as f32, h as f32),
                None => (panel.video_rect.w, panel.video_rect.h),
            };
            let (dx, dy) = ((nx - lx) * sw, (ny - ly) * sh);
            if dx != 0.0 || dy != 0.0 {
                panel.session.send_input(InputEvent::PointerMotion { dx, dy });
            }
        }
        panel.last_norm_pos = Some((nx, ny));
        true
    }

    pub fn remote_desktop_pointer_release(&mut self, px: f32, py: f32) -> bool {
        let Some(panel) = self.remote_desktop_panel.as_mut() else { return false };
        if !panel.video_rect.contains(px, py) && panel.last_norm_pos.is_none() {
            return false;
        }
        panel.session.send_input(InputEvent::PointerButton { button: ProtoButton::Left, pressed: false });
        true
    }
}

/// `(x, y)` global coords → 0.0..=1.0 normalized position within `rect`,
/// clamped so a pointer at the very edge still maps inside 0.0..=1.0.
fn norm_in_rect(rect: Rect, x: f32, y: f32) -> (f32, f32) {
    let nx = ((x - rect.x) / rect.w.max(1.0)).clamp(0.0, 1.0);
    let ny = ((y - rect.y) / rect.h.max(1.0)).clamp(0.0, 1.0);
    (nx, ny)
}

pub(crate) fn render_remote_desktop_panel(
    state: &BacakState,
    renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
    output: OutputId,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<crate::render::BacakElements>,
) {
    use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
    use smithay::backend::renderer::element::Kind;
    use smithay::utils::{Physical, Point};

    let Some(p) = state.remote_desktop_panel.as_ref() else { return };
    if p.output != output {
        return;
    }
    let scale = output_scale as f32;
    let to_phys = |x: f32, y: f32| {
        Point::<f64, Physical>::from((((x - off_x as f32) * scale) as f64, ((y - off_y as f32) * scale) as f64))
    };

    if let Some((buf, w, h)) = &p.frame {
        let phys = to_phys(p.video_rect.x, p.video_rect.y);
        // Scale the source frame to fill `video_rect` exactly, regardless of
        // its native resolution — `dst` overrides the element's logical
        // size the same way `arrow_cursor_element` overrides the cursor's.
        let dst = smithay::utils::Size::<i32, smithay::utils::Logical>::from((p.video_rect.w as i32, p.video_rect.h as i32));
        let _ = (w, h); // native size only needed for the input-delta scaling in state.rs
        if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(renderer, phys, buf, Some(1.0), None, Some(dst), Kind::Unspecified) {
            out.push(crate::render::BacakElements::Memory(el));
        }
    } else {
        // Nothing decoded yet (still pairing, or the connection failed) —
        // the status label below is the only content.
    }

    let status_x = p.video_rect.x + (p.video_rect.w - p.status_label.as_ref().map(|(_, w, _)| *w as f32).unwrap_or(0.0)) / 2.0;
    let status_y = p.video_rect.y + p.video_rect.h + 10.0;
    if let Some((buf, _, _)) = &p.status_label {
        let phys = to_phys(status_x, status_y);
        if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(renderer, phys, buf, Some(1.0), None, None, Kind::Unspecified) {
            out.push(crate::render::BacakElements::Memory(el));
        }
    }

    let cx = p.close_rect.x + (p.close_rect.w - p.close_label.as_ref().map(|(_, w, _)| *w as f32).unwrap_or(0.0)) / 2.0;
    let cy = p.close_rect.y + (p.close_rect.h - p.close_label.as_ref().map(|(_, _, h)| *h as f32).unwrap_or(0.0)) / 2.0;
    if let Some((buf, _, _)) = &p.close_label {
        let phys = to_phys(cx, cy);
        if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(renderer, phys, buf, Some(1.0), None, None, Kind::Unspecified) {
            out.push(crate::render::BacakElements::Memory(el));
        }
    }
    crate::render::cc_card(out, renderer, p.close_rect, smithay::backend::renderer::Color32F::new(1.0, 1.0, 1.0, 0.12), 12.0, output_scale, off_x, off_y);

    // Backdrop behind everything — pushed last (this element list is
    // top-first: earliest push = frontmost), so it never covers the video
    // frame, status text, or close button pushed above. Doubles as visible
    // content while `p.frame` is still `None` (pairing in progress, or a
    // failed connection), so the panel never looks like nothing happened.
    let backdrop = Rect::new(p.video_rect.x - 12.0, p.video_rect.y - 12.0, p.video_rect.w + 24.0, p.video_rect.h + 24.0 + 56.0);
    crate::render::cc_card(out, renderer, backdrop, smithay::backend::renderer::Color32F::new(0.03, 0.05, 0.08, 0.97), 18.0, output_scale, off_x, off_y);
}
