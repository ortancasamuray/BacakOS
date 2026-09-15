//! Shared wire protocol for bacak-remote (PC screen/input bridge for Bacak OS).
//!
//! One UDP socket per side carries every [`Message`] variant. Video frames are
//! larger than a safe UDP payload, so they are split into [`FrameChunk`]s by
//! the server and reassembled by the client (see `bacak-remote-client::network`).
//!
//! Pairing (`PairRequest`/`PairResponse`) is sent and read as plain
//! [`Message`] variants; everything else — `FrameInfo`, `FrameChunk`,
//! `Input`, `Heartbeat`, `Bye` — travels wrapped in `Message::Encrypted`
//! once a session is established (see [`seal_message`]/[`open_message`]),
//! the same "handshake in the clear, everything after it encrypted" shape
//! `uzakel`'s daemon uses.

pub mod crypto;

use serde::{Deserialize, Serialize};

use crypto::{Cipher, Opener, NONCE_LEN, PUBKEY_LEN, TAG_LEN};

/// Rejects stray/foreign UDP traffic on the socket before it reaches decode.
pub const MAGIC: u32 = 0xBACA_2026;
pub const PROTOCOL_VERSION: u8 = 1;

/// Conservative payload budget per UDP datagram (below common Wi-Fi MTU minus
/// IP/UDP/postcard overhead) so a chunk never triggers IP fragmentation.
pub const MAX_CHUNK_BYTES: usize = 1200;

/// Discovery/control port (Hello/HelloAck/Heartbeat) and the same socket also
/// carries FrameChunk (server -> client) and Input (client -> server) once
/// negotiated, matching uzakel's "one socket per direction" simplicity.
pub const DEFAULT_VIDEO_PORT: u16 = 9910;
pub const DEFAULT_INPUT_PORT: u16 = 9911;

#[derive(thiserror::Error, Debug)]
pub enum ProtoError {
    #[error("bad magic: expected {MAGIC:#x}, got {0:#x}")]
    BadMagic(u32),
    #[error("unsupported protocol version {0}, expected {PROTOCOL_VERSION}")]
    BadVersion(u8),
    #[error("packet too short for header")]
    Truncated,
    #[error("serialize failed: {0}")]
    Serialize(#[from] postcard::Error),
    #[error("message was not the expected variant")]
    UnexpectedVariant,
    #[error("decryption failed: bad key, forged/corrupted ciphertext, or replayed nonce")]
    DecryptFailed,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Bgra8,
}

impl PixelFormat {
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            PixelFormat::Bgra8 => 4,
        }
    }
}

/// v1 shipped a lossless zstd-compressed raw frame so the pipeline was real
/// and dependency-light on every target OS. `H264` is the hardware upgrade
/// path (see `uzakel-pc/HARDWARE_ENCODE_PLAN.md`) — the server doesn't send
/// it yet (encode is written and vendor-agnostic-tested but not wired into
/// the real session, see that plan's "nerede duruyoruz"), and neither
/// decoder (`bacak-compositor::remote_desktop`, `bacak-remote-client`) can
/// decode it yet (plan step 5) — both already match it exhaustively and
/// drop/reject the frame in the meantime.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    RawZstd,
    /// `is_keyframe` is per-frame (an H.264 access unit is either an IDR
    /// frame, decodable standalone, or a delta frame that needs every frame
    /// back to the last IDR) — unlike `RawZstd` where every frame already
    /// stands alone, so the receiving side needs this to know whether it's
    /// safe to start decoding from a given frame after joining mid-stream or
    /// after a dropped packet (see the plan's keyframe-loss-tolerance step).
    H264 { is_keyframe: bool },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct FrameInfo {
    pub frame_id: u32,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub codec: Codec,
    /// Length of the encoded (e.g. zstd-compressed) payload, pre-chunking.
    pub payload_len: u32,
    pub chunk_count: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FrameChunk {
    pub frame_id: u32,
    pub chunk_index: u16,
    pub data: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerButton {
    Left,
    Right,
    Middle,
}

/// Touch/pointer coordinates are normalized 0.0..=1.0 against the *server's*
/// captured screen so the client's window/panel size never needs to match it.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum InputEvent {
    PointerMotion { dx: f32, dy: f32 },
    PointerButton { button: PointerButton, pressed: bool },
    PointerScroll { dx: f32, dy: f32 },
    TouchDown { finger_id: u32, x: f32, y: f32 },
    TouchMotion { finger_id: u32, x: f32, y: f32 },
    TouchUp { finger_id: u32 },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Message {
    /// Client -> server: starts pairing. `pin` is what the person at the
    /// server typed in (shown there, e.g. on its console/tray icon);
    /// `client_pubkey` is a fresh, single-use X25519 public key for this
    /// attempt only.
    PairRequest { client_name: String, pin: u32, client_pubkey: [u8; PUBKEY_LEN] },
    /// Server -> client: `accepted` is false on a wrong PIN (no keys are
    /// derived/stored in that case). On success, `confirm_tag` lets the
    /// client verify it's really the server that knows the PIN — not a
    /// man-in-the-middle who intercepted the exchange — before it trusts
    /// `server_pubkey`/derives session keys from it (see `crypto` module doc).
    PairResponse {
        accepted: bool,
        server_pubkey: [u8; PUBKEY_LEN],
        confirm_tag: [u8; TAG_LEN],
        screen_width: u32,
        screen_height: u32,
    },
    /// Either direction, post-pairing: `ciphertext` decrypts (via the
    /// session's [`Cipher`]/[`Opener`]) to a postcard-encoded inner
    /// `Message` — `FrameInfo`, `FrameChunk`, `Input`, `Heartbeat`, or `Bye`.
    Encrypted { nonce: [u8; NONCE_LEN], ciphertext: Vec<u8> },
    /// Server -> client: header for a frame, sent once before its chunks.
    /// Always carried inside `Encrypted` once paired — never sent bare.
    FrameInfo(FrameInfo),
    /// Server -> client: one piece of a frame's encoded payload. Always
    /// carried inside `Encrypted` once paired — never sent bare.
    FrameChunk(FrameChunk),
    /// Client -> server: one input event to inject. Always carried inside
    /// `Encrypted` once paired — never sent bare.
    Input(InputEvent),
    /// Either direction: liveness + RTT probe. Always carried inside
    /// `Encrypted` once paired — never sent bare.
    Heartbeat { timestamp_ms: u64 },
    /// Either direction: clean session teardown. Always carried inside
    /// `Encrypted` once paired — never sent bare.
    Bye,
}

/// Encrypts `inner` (one of `FrameInfo`/`FrameChunk`/`Input`/`Heartbeat`/`Bye`)
/// with the session's outbound [`Cipher`], wrapping the result as
/// `Message::Encrypted` ready to pass to [`encode`].
pub fn seal_message(inner: &Message, cipher: &mut Cipher) -> Result<Message, ProtoError> {
    let plaintext = postcard::to_allocvec(inner)?;
    let (nonce, ciphertext) = cipher.seal(&plaintext);
    Ok(Message::Encrypted { nonce, ciphertext })
}

/// Inverse of [`seal_message`]: given an already-decoded `Message::Encrypted`,
/// decrypts and deserializes the inner `Message` using the session's
/// inbound [`Opener`]. Returns `None` on any failure (see [`Opener::open`]).
pub fn open_message(msg: &Message, opener: &mut Opener) -> Result<Message, ProtoError> {
    let Message::Encrypted { nonce, ciphertext } = msg else {
        return Err(ProtoError::UnexpectedVariant);
    };
    let plaintext = opener.open(*nonce, ciphertext).ok_or(ProtoError::DecryptFailed)?;
    Ok(postcard::from_bytes(&plaintext)?)
}

/// Encodes `msg` as `[MAGIC:4][VERSION:1][postcard bytes]`.
pub fn encode(msg: &Message) -> Result<Vec<u8>, ProtoError> {
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(&MAGIC.to_le_bytes());
    buf.push(PROTOCOL_VERSION);
    let body = postcard::to_allocvec(msg)?;
    buf.extend_from_slice(&body);
    Ok(buf)
}

/// Inverse of [`encode`]; validates magic/version before touching the body.
pub fn decode(bytes: &[u8]) -> Result<Message, ProtoError> {
    if bytes.len() < 5 {
        return Err(ProtoError::Truncated);
    }
    let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    if magic != MAGIC {
        return Err(ProtoError::BadMagic(magic));
    }
    let version = bytes[4];
    if version != PROTOCOL_VERSION {
        return Err(ProtoError::BadVersion(version));
    }
    Ok(postcard::from_bytes(&bytes[5..])?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_pair_request() {
        let msg = Message::PairRequest { client_name: "bacak-remote-client".into(), pin: 123456, client_pubkey: [1u8; PUBKEY_LEN] };
        let bytes = encode(&msg).unwrap();
        match decode(&bytes).unwrap() {
            Message::PairRequest { client_name, pin, .. } => {
                assert_eq!(client_name, "bacak-remote-client");
                assert_eq!(pin, 123456);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn seal_and_open_message_roundtrip() {
        let mut cipher = crypto::Cipher::new([5u8; 32]);
        let mut opener = crypto::Opener::new([5u8; 32]);

        let inner = Message::Input(InputEvent::TouchMotion { finger_id: 1, x: 0.5, y: 0.25 });
        let sealed = seal_message(&inner, &mut cipher).unwrap();
        assert!(matches!(sealed, Message::Encrypted { .. }));

        let opened = open_message(&sealed, &mut opener).unwrap();
        assert_eq!(format!("{opened:?}"), format!("{inner:?}"));
    }

    #[test]
    fn open_message_rejects_wrong_key() {
        let mut cipher = crypto::Cipher::new([5u8; 32]);
        let mut wrong_opener = crypto::Opener::new([6u8; 32]);
        let sealed = seal_message(&Message::Bye, &mut cipher).unwrap();
        assert!(matches!(open_message(&sealed, &mut wrong_opener), Err(ProtoError::DecryptFailed)));
    }

    #[test]
    fn roundtrip_input_event() {
        let msg = Message::Input(InputEvent::TouchMotion { finger_id: 1, x: 0.5, y: 0.25 });
        let bytes = encode(&msg).unwrap();
        assert_eq!(bytes[..4], MAGIC.to_le_bytes());
        let decoded = decode(&bytes).unwrap();
        assert_eq!(format!("{decoded:?}"), format!("{msg:?}"));
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = encode(&Message::Bye).unwrap();
        bytes[0] ^= 0xFF;
        assert!(matches!(decode(&bytes), Err(ProtoError::BadMagic(_))));
    }

    #[test]
    fn chunk_budget_leaves_header_room() {
        // A FrameChunk's own postcard overhead + our 5-byte header must still
        // fit under a real UDP payload once MAX_CHUNK_BYTES of frame data is attached.
        let chunk = FrameChunk { frame_id: 1, chunk_index: 0, data: vec![0u8; MAX_CHUNK_BYTES] };
        let bytes = encode(&Message::FrameChunk(chunk)).unwrap();
        assert!(bytes.len() < 1400, "chunk message {} bytes exceeds typical MTU", bytes.len());
    }
}
