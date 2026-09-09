//! Frame reassembly + decompression.
//!
//! Only ever tracks one in-flight frame: if a `FrameInfo` for a newer
//! `frame_id` arrives while the previous one is still incomplete, the old one
//! is dropped rather than buffered. For a live desktop stream a late frame is
//! worse than a dropped one — the same "freshest wins" choice
//! `uzakel/daemon/src/input_manager.rs` makes for input packets.

use bacak_remote_proto::{Codec, FrameInfo, PixelFormat};

pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub bgra: Vec<u8>,
}

struct InProgress {
    info: FrameInfo,
    chunks: Vec<Option<Vec<u8>>>,
    received: u16,
}

#[derive(Default)]
pub struct FrameReassembler {
    current: Option<InProgress>,
}

impl FrameReassembler {
    pub fn start_frame(&mut self, info: FrameInfo) {
        if let Some(prev) = &self.current
            && prev.received < prev.info.chunk_count
        {
            tracing::debug!(
                "dropping incomplete frame {} ({}/{} chunks) for newer frame {}",
                prev.info.frame_id,
                prev.received,
                prev.info.chunk_count,
                info.frame_id
            );
        }
        self.current = Some(InProgress { chunks: vec![None; info.chunk_count as usize], received: 0, info });
    }

    /// Feeds one chunk; returns the decoded frame once every chunk for the
    /// current frame has arrived.
    pub fn add_chunk(&mut self, frame_id: u32, chunk_index: u16, data: Vec<u8>) -> anyhow::Result<Option<DecodedFrame>> {
        let Some(progress) = &mut self.current else { return Ok(None) };
        if progress.info.frame_id != frame_id {
            return Ok(None); // chunk for a frame we already moved past, or haven't seen FrameInfo for yet
        }
        let Some(slot) = progress.chunks.get_mut(chunk_index as usize) else { return Ok(None) };
        if slot.is_none() {
            *slot = Some(data);
            progress.received += 1;
        }

        if progress.received < progress.info.chunk_count {
            return Ok(None);
        }

        let progress = self.current.take().unwrap();
        let mut payload = Vec::with_capacity(progress.info.payload_len as usize);
        for chunk in progress.chunks {
            payload.extend_from_slice(&chunk.expect("all chunks present, checked by received count"));
        }

        let bgra = match progress.info.codec {
            Codec::RawZstd => zstd::stream::decode_all(payload.as_slice())?,
        };

        Ok(Some(DecodedFrame { width: progress.info.width, height: progress.info.height, format: progress.info.format, bgra }))
    }
}
