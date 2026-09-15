//! H.264 decode for the "Uzak Masaüstü" panel's hardware-encode path — see
//! `../../../uzakel/uzakel-pc/HARDWARE_ENCODE_PLAN.md` step 5. Used by
//! `remote_desktop::FrameReassembler` once `Codec::H264` frames arrive.
//!
//! Software decode only (FFmpeg's default `h264` decoder, no VAAPI/hardware
//! path) — decode is far cheaper than encode, and *a* working decode path
//! matters more right now than a fast one (same "get v1 working before
//! optimizing" reasoning the encode side's module doc gives for not
//! attempting DXGI zero-copy before the system-memory path worked).

use ffmpeg_next as ffmpeg;
use ffmpeg::codec::context::Context as CodecContext;
use ffmpeg::codec::{decoder, Id};
use ffmpeg::format::Pixel;
use ffmpeg::software::scaling::context::Context as ScalerContext;
use ffmpeg::software::scaling::flag::Flags as ScalerFlags;
use ffmpeg::util::error::EAGAIN;
use ffmpeg::util::frame::video::Video as VideoFrame;
use ffmpeg::{Error as FfmpegError, Packet};

/// One decoded frame, already converted to packed (no row padding) BGRA —
/// the same shape `remote_desktop::DecodedFrame` (and the `RawZstd` path)
/// already expects, so the caller doesn't need to know which codec produced
/// it.
pub struct DecodedBgra {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

pub struct H264Decoder {
    decoder: decoder::video::Video,
    // Rebuilt lazily, and again whenever the decoded frame's format/size
    // changes (a mid-stream resolution or pixel-format change is legal
    // H.264, even though nothing on the encode side triggers one yet).
    scaler: Option<ScalerContext>,
    scaler_source: Option<(Pixel, u32, u32)>,
}

// SAFETY: same reasoning as bacak-remote-server's `H264Encoder` — wraps raw
// FFmpeg pointers (`AVCodecContext`/`SwsContext`) that ffmpeg-next doesn't
// mark `Send`, but this decoder is only ever driven from the one task that
// owns it (`remote_desktop::run`'s receive loop), never concurrently.
unsafe impl Send for H264Decoder {}

impl H264Decoder {
    pub fn new() -> anyhow::Result<Self> {
        ffmpeg::init()?;
        let codec = decoder::find(Id::H264).ok_or_else(|| anyhow::anyhow!("no H.264 decoder in this FFmpeg build"))?;
        let decoder = CodecContext::new_with_codec(codec).decoder().video()?;
        Ok(Self { decoder, scaler: None, scaler_source: None })
    }

    /// Feeds one already-reassembled H.264 access unit. A decoder may
    /// buffer internally before it has enough to produce a frame, so this
    /// can return zero, one, or (after a delayed start) more than one
    /// [`DecodedBgra`] — always drain fully rather than assuming 1:1 with
    /// calls, mirroring the encoder side's `EncodedPacket` contract.
    pub fn decode(&mut self, data: &[u8]) -> anyhow::Result<Vec<DecodedBgra>> {
        let packet = Packet::copy(data);
        self.decoder.send_packet(&packet)?;
        self.drain()
    }

    fn drain(&mut self) -> anyhow::Result<Vec<DecodedBgra>> {
        let mut out = Vec::new();
        loop {
            let mut frame = VideoFrame::empty();
            match self.decoder.receive_frame(&mut frame) {
                Ok(()) => out.push(self.to_bgra(&frame)?),
                // Not an error: "not enough data buffered yet for a whole
                // frame" — expected, non-fatal, happens on ~every call.
                Err(FfmpegError::Other { errno }) if errno == EAGAIN => break,
                Err(FfmpegError::Eof) => break,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(out)
    }

    fn to_bgra(&mut self, frame: &VideoFrame) -> anyhow::Result<DecodedBgra> {
        let (width, height) = (frame.width(), frame.height());
        let source = (frame.format(), width, height);
        if self.scaler_source != Some(source) {
            self.scaler = Some(ScalerContext::get(frame.format(), width, height, Pixel::BGRA, width, height, ScalerFlags::BILINEAR)?);
            self.scaler_source = Some(source);
        }

        let mut bgra = VideoFrame::new(Pixel::BGRA, width, height);
        self.scaler.as_mut().expect("just set above").run(frame, &mut bgra)?;

        // `VideoFrame::new` pads each row to its own (32-byte-aligned)
        // stride; the wire/`MemoryRenderBuffer` path both assume tightly
        // packed BGRA, same stride-stripping `bacak-remote-server::capture`
        // already does for the raw-BGRA path.
        let row_bytes = width as usize * 4;
        let stride = bgra.stride(0);
        let src = bgra.data(0);
        let mut packed = Vec::with_capacity(row_bytes * height as usize);
        for row in 0..height as usize {
            let start = row * stride;
            packed.extend_from_slice(&src[start..start + row_bytes]);
        }

        Ok(DecodedBgra { width, height, bgra: packed })
    }
}
