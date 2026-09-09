//! Shared wire protocol for bacak-remote (PC screen/input bridge for Bacak OS).
//!
//! One UDP socket per side carries every [`Message`] variant. Video frames are
//! larger than a safe UDP payload, so they are split into [`FrameChunk`]s by
//! the server and reassembled by the client (see `bacak-remote-client::network`).

use serde::{Deserialize, Serialize};

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

/// v1 ships a lossless zstd-compressed raw frame so the pipeline is real and
/// dependency-light on every target OS. Hardware H.264/AV1 is the designed
/// upgrade path (see workspace README) — add a variant here and a matching
/// encoder/decoder backend without touching the transport or input path.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    RawZstd,
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
    /// Client -> server: request to start streaming.
    Hello { client_name: String },
    /// Server -> client: accepted, here is the source screen size.
    HelloAck { screen_width: u32, screen_height: u32 },
    /// Server -> client: header for a frame, sent once before its chunks.
    FrameInfo(FrameInfo),
    /// Server -> client: one piece of a frame's encoded payload.
    FrameChunk(FrameChunk),
    /// Client -> server: one input event to inject.
    Input(InputEvent),
    /// Either direction: liveness + RTT probe.
    Heartbeat { timestamp_ms: u64 },
    /// Either direction: clean session teardown.
    Bye,
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
    fn roundtrip_hello() {
        let msg = Message::Hello { client_name: "bacak-remote-client".into() };
        let bytes = encode(&msg).unwrap();
        match decode(&bytes).unwrap() {
            Message::Hello { client_name } => assert_eq!(client_name, "bacak-remote-client"),
            other => panic!("unexpected: {other:?}"),
        }
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
