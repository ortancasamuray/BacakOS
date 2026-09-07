//! Shared wire protocol for the input (UDP) and file-transfer (TCP) channels.
//!
//! Every packet on either channel starts with the same 8-byte header
//! (`Header`); what follows is `payload_len` bytes laid out however the
//! packet's [`Opcode`] says. See `../../ARCHITECTURE.md` §2 for the full
//! design rationale — this module is only the byte-level encoding of it.
//!
//! Encoding is hand-rolled little-endian rather than `serde`-derived: the
//! layout has to match `uzakel`'s Android client byte-for-byte, and a fixed,
//! explicit `to_bytes`/`from_bytes` pair is easier to keep in lockstep across
//! two languages than a derive macro's implicit layout would be.

use std::convert::TryFrom;

pub const MAGIC: u16 = 0x557A; // "Uz"
pub const PROTOCOL_VERSION: u8 = 1;
pub const HEADER_LEN: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    // Input channel (UDP)
    MouseMove = 1,
    MouseClick = 2,
    MouseScroll = 3,
    KeyPress = 4,

    // File transfer channel (TCP)
    FileMeta = 10,
    FileAccept = 11,
    FileReject = 12,
    Chunk = 13,
    TransferCancel = 14,
    FileCorrupt = 15,

    // Discovery / pairing (UDP broadcast)
    DiscoverRequest = 20,
    DiscoverResponse = 21,
    PairRequest = 22,
    PairResponse = 23,
}

impl TryFrom<u8> for Opcode {
    type Error = ProtocolError;

    fn try_from(v: u8) -> Result<Self, Self::Error> {
        Ok(match v {
            1 => Opcode::MouseMove,
            2 => Opcode::MouseClick,
            3 => Opcode::MouseScroll,
            4 => Opcode::KeyPress,
            10 => Opcode::FileMeta,
            11 => Opcode::FileAccept,
            12 => Opcode::FileReject,
            13 => Opcode::Chunk,
            14 => Opcode::TransferCancel,
            15 => Opcode::FileCorrupt,
            20 => Opcode::DiscoverRequest,
            21 => Opcode::DiscoverResponse,
            22 => Opcode::PairRequest,
            23 => Opcode::PairResponse,
            other => return Err(ProtocolError::UnknownOpcode(other)),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("packet too short: need at least {need} bytes, got {got}")]
    TooShort { need: usize, got: usize },
    #[error("bad magic: expected {MAGIC:#06x}, got {0:#06x}")]
    BadMagic(u16),
    #[error("unsupported protocol version: {0} (daemon speaks {PROTOCOL_VERSION})")]
    BadVersion(u8),
    #[error("unknown opcode byte: {0}")]
    UnknownOpcode(u8),
    #[error("payload_len ({declared}) doesn't match actual payload ({actual})")]
    LengthMismatch { declared: u32, actual: usize },
    #[error("malformed payload for {opcode:?}: {reason}")]
    BadPayload {
        opcode: Opcode,
        reason: &'static str,
    },
}

/// The 8-byte header every packet on both channels starts with.
#[derive(Debug, Clone, Copy)]
pub struct Header {
    pub opcode: Opcode,
    pub payload_len: u32,
}

impl Header {
    pub fn encode(self, out: &mut Vec<u8>) {
        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.push(PROTOCOL_VERSION);
        out.push(self.opcode as u8);
        out.extend_from_slice(&self.payload_len.to_le_bytes());
    }

    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        if buf.len() < HEADER_LEN {
            return Err(ProtocolError::TooShort {
                need: HEADER_LEN,
                got: buf.len(),
            });
        }
        let magic = u16::from_le_bytes([buf[0], buf[1]]);
        if magic != MAGIC {
            return Err(ProtocolError::BadMagic(magic));
        }
        let version = buf[2];
        if version != PROTOCOL_VERSION {
            return Err(ProtocolError::BadVersion(version));
        }
        let opcode = Opcode::try_from(buf[3])?;
        let payload_len = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        Ok(Header {
            opcode,
            payload_len,
        })
    }
}

/// Wraps a header + payload into one buffer ready to send.
fn frame(opcode: Opcode, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    Header {
        opcode,
        payload_len: payload.len() as u32,
    }
    .encode(&mut out);
    out.extend_from_slice(payload);
    out
}

// ── Input channel payloads (UDP) ───────────────────────────────────────────

/// Every input packet is prefixed with a monotonic sequence number so
/// [`crate::input_manager`] can drop a UDP packet that arrived out of order
/// instead of momentarily moving the pointer backwards.
#[derive(Debug, Clone, Copy)]
pub enum InputPacket {
    MouseMove {
        seq: u32,
        dx: i16,
        dy: i16,
    },
    MouseClick {
        seq: u32,
        button: MouseButton,
        pressed: bool,
    },
    MouseScroll {
        seq: u32,
        dx: i16,
        dy: i16,
    },
    KeyPress {
        seq: u32,
        keycode: u16,
        modifiers: Modifiers,
        pressed: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            0 => MouseButton::Left,
            1 => MouseButton::Right,
            2 => MouseButton::Middle,
            _ => return None,
        })
    }

    fn to_byte(self) -> u8 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Right => 1,
            MouseButton::Middle => 2,
        }
    }
}

bitflags::bitflags! {
    /// Bitmask over Shift/Ctrl/Alt/Super, matching `KEY_PRESS`'s `modifiers` byte.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Modifiers: u8 {
        const SHIFT = 0b0001;
        const CTRL  = 0b0010;
        const ALT   = 0b0100;
        const SUPER = 0b1000;
    }
}

impl InputPacket {
    pub fn opcode(&self) -> Opcode {
        match self {
            InputPacket::MouseMove { .. } => Opcode::MouseMove,
            InputPacket::MouseClick { .. } => Opcode::MouseClick,
            InputPacket::MouseScroll { .. } => Opcode::MouseScroll,
            InputPacket::KeyPress { .. } => Opcode::KeyPress,
        }
    }

    pub fn encode(self) -> Vec<u8> {
        let mut payload = Vec::new();
        match self {
            InputPacket::MouseMove { seq, dx, dy } | InputPacket::MouseScroll { seq, dx, dy } => {
                payload.extend_from_slice(&seq.to_le_bytes());
                payload.extend_from_slice(&dx.to_le_bytes());
                payload.extend_from_slice(&dy.to_le_bytes());
            }
            InputPacket::MouseClick {
                seq,
                button,
                pressed,
            } => {
                payload.extend_from_slice(&seq.to_le_bytes());
                payload.push(button.to_byte());
                payload.push(pressed as u8);
            }
            InputPacket::KeyPress {
                seq,
                keycode,
                modifiers,
                pressed,
            } => {
                payload.extend_from_slice(&seq.to_le_bytes());
                payload.extend_from_slice(&keycode.to_le_bytes());
                payload.push(modifiers.bits());
                payload.push(pressed as u8);
            }
        }
        frame(self.opcode(), &payload)
    }

    /// Parses a single UDP datagram (header + payload already concatenated).
    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        let header = Header::decode(buf)?;
        let payload = &buf[HEADER_LEN..];
        if payload.len() != header.payload_len as usize {
            return Err(ProtocolError::LengthMismatch {
                declared: header.payload_len,
                actual: payload.len(),
            });
        }

        let need = |n: usize| -> Result<(), ProtocolError> {
            if payload.len() < n {
                Err(ProtocolError::BadPayload {
                    opcode: header.opcode,
                    reason: "payload too short",
                })
            } else {
                Ok(())
            }
        };
        let seq = |p: &[u8]| u32::from_le_bytes([p[0], p[1], p[2], p[3]]);

        Ok(match header.opcode {
            Opcode::MouseMove => {
                need(8)?;
                InputPacket::MouseMove {
                    seq: seq(payload),
                    dx: i16::from_le_bytes([payload[4], payload[5]]),
                    dy: i16::from_le_bytes([payload[6], payload[7]]),
                }
            }
            Opcode::MouseScroll => {
                need(8)?;
                InputPacket::MouseScroll {
                    seq: seq(payload),
                    dx: i16::from_le_bytes([payload[4], payload[5]]),
                    dy: i16::from_le_bytes([payload[6], payload[7]]),
                }
            }
            Opcode::MouseClick => {
                need(6)?;
                let button =
                    MouseButton::from_byte(payload[4]).ok_or(ProtocolError::BadPayload {
                        opcode: header.opcode,
                        reason: "unknown mouse button",
                    })?;
                InputPacket::MouseClick {
                    seq: seq(payload),
                    button,
                    pressed: payload[5] != 0,
                }
            }
            Opcode::KeyPress => {
                need(8)?;
                InputPacket::KeyPress {
                    seq: seq(payload),
                    keycode: u16::from_le_bytes([payload[4], payload[5]]),
                    modifiers: Modifiers::from_bits_truncate(payload[6]),
                    pressed: payload[7] != 0,
                }
            }
            other => {
                return Err(ProtocolError::BadPayload {
                    opcode: other,
                    reason: "not an input-channel opcode",
                })
            }
        })
    }
}

// ── File transfer channel payloads (TCP) ───────────────────────────────────

pub const CHUNK_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct FileMeta {
    pub name: String,
    pub size: u64,
    pub sha256: [u8; 32],
}

impl FileMeta {
    pub fn encode(&self) -> Vec<u8> {
        let name_bytes = self.name.as_bytes();
        let mut payload = Vec::with_capacity(2 + name_bytes.len() + 8 + 32);
        payload.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        payload.extend_from_slice(name_bytes);
        payload.extend_from_slice(&self.size.to_le_bytes());
        payload.extend_from_slice(&self.sha256);
        frame(Opcode::FileMeta, &payload)
    }

    pub fn decode_payload(payload: &[u8]) -> Result<Self, ProtocolError> {
        let bad = || ProtocolError::BadPayload {
            opcode: Opcode::FileMeta,
            reason: "truncated FileMeta",
        };
        if payload.len() < 2 {
            return Err(bad());
        }
        let name_len = u16::from_le_bytes([payload[0], payload[1]]) as usize;
        let rest = &payload[2..];
        if rest.len() < name_len + 8 + 32 {
            return Err(bad());
        }
        let name = String::from_utf8(rest[..name_len].to_vec()).map_err(|_| {
            ProtocolError::BadPayload {
                opcode: Opcode::FileMeta,
                reason: "name is not valid UTF-8",
            }
        })?;
        let size_off = name_len;
        let size = u64::from_le_bytes(rest[size_off..size_off + 8].try_into().unwrap());
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&rest[size_off + 8..size_off + 8 + 32]);
        Ok(FileMeta { name, size, sha256 })
    }
}

/// `FILE_REJECT`'s payload: a short human-readable reason.
pub fn encode_file_reject(reason: &str) -> Vec<u8> {
    frame(Opcode::FileReject, reason.as_bytes())
}

pub fn encode_simple(opcode: Opcode) -> Vec<u8> {
    frame(opcode, &[])
}

/// One `CHUNK` frame: `index: u32, len: u32` followed by `len` bytes of data.
pub fn encode_chunk(index: u32, data: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(8 + data.len());
    payload.extend_from_slice(&index.to_le_bytes());
    payload.extend_from_slice(&(data.len() as u32).to_le_bytes());
    payload.extend_from_slice(data);
    frame(Opcode::Chunk, &payload)
}

pub struct ChunkHeader {
    pub index: u32,
    pub len: u32,
}

impl ChunkHeader {
    pub const ENCODED_LEN: usize = 8;

    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        if buf.len() < Self::ENCODED_LEN {
            return Err(ProtocolError::BadPayload {
                opcode: Opcode::Chunk,
                reason: "truncated chunk header",
            });
        }
        Ok(ChunkHeader {
            index: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            len: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
        })
    }
}

// ── Discovery / pairing payloads (UDP broadcast) ───────────────────────────

#[derive(Debug, Clone)]
pub struct DiscoverResponse {
    pub daemon_name: String,
    pub daemon_version: (u8, u8, u8),
    pub accepting_new_pairs: bool,
}

impl DiscoverResponse {
    pub fn encode(&self) -> Vec<u8> {
        let name_bytes = self.daemon_name.as_bytes();
        let mut payload = Vec::with_capacity(2 + name_bytes.len() + 3 + 1);
        payload.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        payload.extend_from_slice(name_bytes);
        payload.push(self.daemon_version.0);
        payload.push(self.daemon_version.1);
        payload.push(self.daemon_version.2);
        payload.push(self.accepting_new_pairs as u8);
        frame(Opcode::DiscoverResponse, &payload)
    }
}

pub fn encode_discover_request() -> Vec<u8> {
    encode_simple(Opcode::DiscoverRequest)
}

#[derive(Debug, Clone, Copy)]
pub struct PairRequest {
    pub pin: u32,
}

impl PairRequest {
    pub fn decode_payload(payload: &[u8]) -> Result<Self, ProtocolError> {
        if payload.len() < 4 {
            return Err(ProtocolError::BadPayload {
                opcode: Opcode::PairRequest,
                reason: "truncated PairRequest",
            });
        }
        Ok(PairRequest {
            pin: u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]),
        })
    }
}

pub fn encode_pair_response(accepted: bool) -> Vec<u8> {
    frame(Opcode::PairResponse, &[accepted as u8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_move_roundtrips() {
        let pkt = InputPacket::MouseMove {
            seq: 42,
            dx: -12,
            dy: 300,
        };
        let bytes = pkt.encode();
        let decoded = InputPacket::decode(&bytes).unwrap();
        match decoded {
            InputPacket::MouseMove { seq, dx, dy } => {
                assert_eq!(seq, 42);
                assert_eq!(dx, -12);
                assert_eq!(dy, 300);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn key_press_roundtrips_modifiers() {
        let pkt = InputPacket::KeyPress {
            seq: 1,
            keycode: 30,
            modifiers: Modifiers::CTRL | Modifiers::SHIFT,
            pressed: true,
        };
        let bytes = pkt.encode();
        match InputPacket::decode(&bytes).unwrap() {
            InputPacket::KeyPress {
                modifiers,
                pressed,
                keycode,
                ..
            } => {
                assert!(modifiers.contains(Modifiers::CTRL));
                assert!(modifiers.contains(Modifiers::SHIFT));
                assert!(!modifiers.contains(Modifiers::ALT));
                assert!(pressed);
                assert_eq!(keycode, 30);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut bytes = InputPacket::MouseMove {
            seq: 0,
            dx: 0,
            dy: 0,
        }
        .encode();
        bytes[0] ^= 0xFF; // corrupt magic
        assert!(matches!(
            InputPacket::decode(&bytes),
            Err(ProtocolError::BadMagic(_))
        ));
    }

    #[test]
    fn file_meta_roundtrips() {
        let meta = FileMeta {
            name: "resim.png".into(),
            size: 123_456,
            sha256: [7u8; 32],
        };
        let bytes = meta.encode();
        let header = Header::decode(&bytes).unwrap();
        let decoded = FileMeta::decode_payload(&bytes[HEADER_LEN..]).unwrap();
        assert_eq!(header.opcode, Opcode::FileMeta);
        assert_eq!(decoded.name, "resim.png");
        assert_eq!(decoded.size, 123_456);
        assert_eq!(decoded.sha256, [7u8; 32]);
    }

    #[test]
    fn chunk_header_roundtrips() {
        let data = vec![1u8, 2, 3, 4, 5];
        let bytes = encode_chunk(9, &data);
        let header = Header::decode(&bytes).unwrap();
        assert_eq!(header.opcode, Opcode::Chunk);
        let payload = &bytes[HEADER_LEN..];
        let chunk_header = ChunkHeader::decode(payload).unwrap();
        assert_eq!(chunk_header.index, 9);
        assert_eq!(chunk_header.len, 5);
        assert_eq!(&payload[ChunkHeader::ENCODED_LEN..], &data[..]);
    }
}
