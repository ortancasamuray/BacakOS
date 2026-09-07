//! Virtual mouse + keyboard, driven by [`crate::protocol::InputPacket`]s
//! arriving on the UDP input channel.
//!
//! Injection goes through `/dev/uinput` — the same mechanism a Bluetooth
//! mouse or keyboard driver uses, so from the compositor's point of view an
//! Uzakel client is indistinguishable from real hardware. Everything here
//! sends *relative* motion (`REL_X`/`REL_Y`), so clamping the pointer to the
//! screen is the compositor's job, not ours; we never need to know the
//! current screen geometry.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::net::SocketAddr;

use anyhow::{Context, Result};
use input_linux::{
    EventKind, EventTime, InputId, Key, KeyEvent, KeyState, RelativeAxis, RelativeEvent,
    SynchronizeEvent, UInputHandle,
};
use tokio::net::UdpSocket;
use tracing::{debug, trace, warn};

use crate::protocol::{decode_encrypted_frame_payload, Header, InputPacket, Modifiers, MouseButton, Opcode, HEADER_LEN};
use crate::trust::TrustStore;

/// Every keyboard `KEY_PRESS` opcode carries a Linux evdev keycode directly
/// (the client is expected to translate its own platform's keycodes to
/// these), so the daemon never needs its own keymap — it just replays what
/// it's told.
const MOUSE_BUTTON_KEYS: [Key; 3] = [Key::ButtonLeft, Key::ButtonRight, Key::ButtonMiddle];

/// A software acceleration curve applied on top of whatever the client
/// already applied — mild by design (see ARCHITECTURE.md §3): the client is
/// expected to do the bulk of the feel-tuning, this is just a safety net so
/// a client that sends raw deltas isn't unusably slow.
fn accelerate(delta: i16) -> i32 {
    let d = delta as f32;
    let scaled = d.signum() * d.abs().powf(1.15);
    scaled.round().clamp(i32::MIN as f32, i32::MAX as f32) as i32
}

pub struct VirtualInput {
    handle: UInputHandle<std::fs::File>,
}

impl VirtualInput {
    /// Opens `/dev/uinput` and registers a combined virtual mouse + keyboard
    /// device. Requires the process to be in the `uinput` group (or root);
    /// BacakOS's session setup grants the former to the logged-in user, the
    /// same way it grants access to other user-scope desktop services (see
    /// `bacak/packaging/bacak-session`).
    pub fn open() -> Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .context("opening /dev/uinput — is the current user in the `uinput` group?")?;
        let handle = UInputHandle::new(file);

        handle.set_evbit(EventKind::Key).context("EV_KEY")?;
        for key in MOUSE_BUTTON_KEYS {
            handle.set_keybit(key).context("mouse button keybit")?;
        }
        // The full keyboard keycode range (KEY_ESC..KEY_MICMUTE-ish); we
        // register every code below Key::COUNT rather than a hand-picked
        // subset so the daemon never has to be updated when the client
        // starts sending a keycode it didn't before.
        for code in 1u16..Key::COUNT as u16 {
            if let Ok(key) = Key::from_code(code) {
                if key.is_key() {
                    let _ = handle.set_keybit(key); // best-effort: some codes are reserved/invalid
                }
            }
        }

        handle.set_evbit(EventKind::Relative).context("EV_REL")?;
        for axis in [
            RelativeAxis::X,
            RelativeAxis::Y,
            RelativeAxis::Wheel,
            RelativeAxis::HorizontalWheel,
        ] {
            handle.set_relbit(axis).context("relbit")?;
        }

        let id = InputId {
            bustype: input_linux::sys::BUS_VIRTUAL,
            vendor: 0x0,
            product: 0x0,
            version: 1,
        };
        handle
            .create(&id, b"Uzakel Virtual Input", 0, &[])
            .context("UI_DEV_CREATE")?;

        Ok(Self { handle })
    }

    fn sync(&self) -> Result<()> {
        let time = EventTime::new(0, 0);
        self.handle
            .write(&[SynchronizeEvent::report(time).into_event().into_raw()])
            .context("writing EV_SYN")?;
        Ok(())
    }

    pub fn mouse_move(&self, dx: i16, dy: i16) -> Result<()> {
        let time = EventTime::new(0, 0);
        let events = [
            RelativeEvent::new(time, RelativeAxis::X, accelerate(dx))
                .into_event()
                .into_raw(),
            RelativeEvent::new(time, RelativeAxis::Y, accelerate(dy))
                .into_event()
                .into_raw(),
        ];
        self.handle.write(&events).context("writing REL_X/REL_Y")?;
        self.sync()
    }

    pub fn mouse_scroll(&self, dx: i16, dy: i16) -> Result<()> {
        let time = EventTime::new(0, 0);
        let events = [
            RelativeEvent::new(time, RelativeAxis::HorizontalWheel, dx as i32)
                .into_event()
                .into_raw(),
            RelativeEvent::new(time, RelativeAxis::Wheel, -(dy as i32))
                .into_event()
                .into_raw(),
        ];
        self.handle.write(&events).context("writing REL_WHEEL")?;
        self.sync()
    }

    pub fn mouse_click(&self, button: MouseButton, pressed: bool) -> Result<()> {
        let key = match button {
            MouseButton::Left => Key::ButtonLeft,
            MouseButton::Right => Key::ButtonRight,
            MouseButton::Middle => Key::ButtonMiddle,
        };
        self.key(key, pressed)
    }

    pub fn key_press(&self, keycode: u16, modifiers: Modifiers, pressed: bool) -> Result<()> {
        // Modifier keys are sent as their own KEY_PRESS packets by a
        // well-behaved client; `modifiers` here is informational (lets a
        // future client send "Ctrl+C" as one packet if it wants to), so we
        // only synthesize modifier key events for bits set on a *press* to
        // avoid a stuck-key situation if release packets are lost.
        if pressed {
            if modifiers.contains(Modifiers::CTRL) {
                self.key(Key::LeftCtrl, true)?;
            }
            if modifiers.contains(Modifiers::SHIFT) {
                self.key(Key::LeftShift, true)?;
            }
            if modifiers.contains(Modifiers::ALT) {
                self.key(Key::LeftAlt, true)?;
            }
            if modifiers.contains(Modifiers::SUPER) {
                self.key(Key::LeftMeta, true)?;
            }
        }
        let Ok(key) = Key::from_code(keycode) else {
            warn!(keycode, "ignoring KEY_PRESS with an out-of-range keycode");
            return Ok(());
        };
        self.key(key, pressed)
    }

    fn key(&self, key: Key, pressed: bool) -> Result<()> {
        let time = EventTime::new(0, 0);
        let state = if pressed {
            KeyState::PRESSED
        } else {
            KeyState::RELEASED
        };
        self.handle
            .write(&[KeyEvent::new(time, key, state).into_event().into_raw()])
            .context("writing EV_KEY")?;
        self.sync()
    }
}

/// Runs the UDP input listener forever, replaying every well-formed,
/// in-order packet through `input`. Per-source sequence tracking means a
/// packet from a phone that's still finishing its old connection while a new
/// one starts doesn't get interleaved with (or overtake) the current one.
///
/// Every datagram accepted here must be an `EncryptedFrame` (ARCHITECTURE.md
/// §2.3.1) from an address `trust` has a session for — there is no
/// plaintext fallback. A client that hasn't paired, or a plaintext
/// `InputPacket` sent to an IP that *has* paired (a downgrade attempt, or
/// just a bug), is dropped identically to a garbled packet: silently, at
/// trace level.
pub async fn run(socket: UdpSocket, input: VirtualInput, trust: TrustStore) -> Result<()> {
    let mut last_seq: HashMap<SocketAddr, u32> = HashMap::new();
    let mut buf = [0u8; 2048];

    loop {
        let (len, from) = match socket.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(err) => {
                warn!(?err, "UDP recv error on input channel, continuing");
                continue;
            }
        };

        let Some(session) = trust.session(from.ip()) else {
            // Not a "malformed packet" — a real client that just hasn't
            // paired (or paired before the last daemon restart, see
            // trust.rs). trace, not warn: this is the expected steady-state
            // response to an unpaired sender probing the port, not
            // something the operator needs to see by default.
            trace!(%from, "dropping input packet from an unpaired address");
            continue;
        };

        let decrypted = match decrypt_frame(&session, &buf[..len]) {
            Some(bytes) => bytes,
            None => {
                trace!(%from, "dropping input datagram that wasn't a valid encrypted frame");
                continue;
            }
        };

        let packet = match InputPacket::decode(&decrypted) {
            Ok(p) => p,
            Err(err) => {
                trace!(?err, %from, "dropping malformed (decrypted) input packet");
                continue;
            }
        };

        let seq = packet_seq(&packet);
        let entry = last_seq.entry(from).or_insert(0);
        if seq != 0 && *entry != 0 && seq <= *entry && entry.wrapping_sub(seq) < u32::MAX / 2 {
            // Older than (or a duplicate of) the last packet we applied from
            // this source — drop it rather than move the pointer backwards.
            trace!(seq, last = *entry, %from, "dropping out-of-order input packet");
            continue;
        }
        *entry = seq;

        if let Err(err) = apply(&input, packet) {
            warn!(?err, "failed to replay input packet via /dev/uinput");
        }
    }
}

/// Unwraps one `EncryptedFrame` datagram into the inner frame's bytes
/// (header + payload), or `None` if it isn't a well-formed `EncryptedFrame`
/// or fails to decrypt/authenticate under `session`'s key (wrong key,
/// corrupted/forged ciphertext, or a replayed/stale nonce — see
/// `crypto::Opener`). Shared by `input_manager.rs` and `file_server.rs`
/// isn't worth a common module for two call sites this small; both just
/// mirror this pattern.
fn decrypt_frame(session: &crate::trust::Session, datagram: &[u8]) -> Option<Vec<u8>> {
    let header = Header::decode(datagram).ok()?;
    if header.opcode != Opcode::EncryptedFrame {
        return None;
    }
    let payload = datagram.get(HEADER_LEN..)?;
    if payload.len() != header.payload_len as usize {
        return None;
    }
    let (nonce, ciphertext) = decode_encrypted_frame_payload(payload).ok()?;
    session.opener.lock().unwrap().open(nonce, ciphertext)
}

fn packet_seq(packet: &InputPacket) -> u32 {
    match *packet {
        InputPacket::MouseMove { seq, .. }
        | InputPacket::MouseClick { seq, .. }
        | InputPacket::MouseScroll { seq, .. }
        | InputPacket::KeyPress { seq, .. } => seq,
    }
}

fn apply(input: &VirtualInput, packet: InputPacket) -> Result<()> {
    match packet {
        InputPacket::MouseMove { dx, dy, .. } => input.mouse_move(dx, dy),
        InputPacket::MouseScroll { dx, dy, .. } => input.mouse_scroll(dx, dy),
        InputPacket::MouseClick {
            button, pressed, ..
        } => input.mouse_click(button, pressed),
        InputPacket::KeyPress {
            keycode,
            modifiers,
            pressed,
            ..
        } => {
            debug!(keycode, pressed, "replaying key press");
            input.key_press(keycode, modifiers, pressed)
        }
    }
}
