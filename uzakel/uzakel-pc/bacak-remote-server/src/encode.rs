//! Frame encoding: compress the captured BGRA buffer and split it into
//! wire-sized chunks. See `bacak_remote_proto::Codec` for the upgrade path to
//! a real hardware video codec — this module is the only place that would
//! need a second implementation alongside `RawZstd`.

use bacak_remote_proto::{Codec, FrameChunk, FrameInfo, MAX_CHUNK_BYTES};

use crate::capture::CapturedFrame;

pub struct EncodedFrame {
    pub info: FrameInfo,
    pub chunks: Vec<FrameChunk>,
}

pub fn encode_frame(frame: &CapturedFrame, frame_id: u32, zstd_level: i32) -> anyhow::Result<EncodedFrame> {
    let compressed = zstd::stream::encode_all(frame.bgra.as_slice(), zstd_level)?;
    let chunks: Vec<FrameChunk> = compressed
        .chunks(MAX_CHUNK_BYTES)
        .enumerate()
        .map(|(i, data)| FrameChunk { frame_id, chunk_index: i as u16, data: data.to_vec() })
        .collect();

    let info = FrameInfo {
        frame_id,
        width: frame.width,
        height: frame.height,
        format: frame.format,
        codec: Codec::RawZstd,
        payload_len: compressed.len() as u32,
        chunk_count: chunks.len() as u16,
    };

    Ok(EncodedFrame { info, chunks })
}
