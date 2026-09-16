//! Hardware H.264 encode, Windows-only. See `../HARDWARE_ENCODE_PLAN.md` for
//! the overall plan — this is step 2 (v1: feed the encoder from system
//! memory, copying the BGRA buffer `capture.rs` already produces; the DXGI
//! zero-copy path is v2, not attempted before this simpler path works end
//! to end).
//!
//! **Vendor-agnostic, not Intel-only**: the deployment target's GPU vendor
//! isn't known ahead of time (a `--test-h264-encode` run against the actual
//! "Windows test machine" turned out to be a VirtualBox VM with no GPU at
//! all — see the plan's "BLOKE EDEN bulgu" section), so [`H264Encoder::new`]
//! tries a short list of FFmpeg encoders in order — NVIDIA NVENC, AMD AMF,
//! Intel QSV, then software `libx264` as a last resort — and uses whichever
//! one actually opens on this machine, rather than hard-coding QSV. The
//! vendored FFmpeg build has all four compiled in (confirmed via `strings`
//! on `avcodec-63.dll`: `--enable-ffnvcodec`/CUDA, `--enable-amf`,
//! `--enable-libvpl`, `--enable-libx264`).
//!
//! Not wired into `main.rs`'s real session/`network.rs` yet — only reachable
//! through the temporary `--test-h264-encode` CLI path. The protocol
//! (`Codec::H264` + `is_keyframe`, plan step 3) and the real caller still
//! need to be added.

use ffmpeg_next as ffmpeg;
use ffmpeg::codec::context::Context as CodecContext;
use ffmpeg::codec::encoder;
use ffmpeg::format::Pixel;
use ffmpeg::software::scaling::context::Context as ScalerContext;
use ffmpeg::software::scaling::flag::Flags as ScalerFlags;
use ffmpeg::util::error::EAGAIN;
use ffmpeg::util::frame::video::Video as VideoFrame;
use ffmpeg::{Error as FfmpegError, Packet, Rational};

use crate::capture::CapturedFrame;

/// One encoded access unit ready to go out over the wire. `is_keyframe`
/// distinguishes an IDR frame (decodable on its own) from a delta frame
/// (needs every frame back to the last IDR) — see plan step 4 for why the
/// receiving side needs this.
pub struct EncodedPacket {
    pub data: Vec<u8>,
    pub is_keyframe: bool,
}

/// Tried in order; the first one whose encoder both exists in this FFmpeg
/// build *and* successfully opens (i.e. the matching GPU/driver/runtime is
/// actually present) wins. `libx264` never fails to open (pure software),
/// so it's the guaranteed-to-work floor — every machine ends up with a
/// working encoder, hardware-accelerated when possible.
const CANDIDATE_ENCODERS: &[(&str, bool)] =
    &[("h264_nvenc", true), ("h264_amf", true), ("h264_qsv", true), ("libx264", false)];

pub struct H264Encoder {
    encoder: encoder::video::Encoder,
    scaler: ScalerContext,
    width: u32,
    height: u32,
    next_pts: i64,
    /// Which candidate from [`CANDIDATE_ENCODERS`] actually opened — for
    /// logging/diagnostics (e.g. so `--test-h264-encode` can report whether
    /// it got real hardware encode or fell back to software).
    pub backend: &'static str,
    pub is_hardware: bool,
}

// SAFETY: `encoder`/`scaler` wrap FFmpeg's `AVCodecContext`/`SwsContext` via
// raw pointers, so ffmpeg-next doesn't derive `Send` for them automatically.
// Both are safe to *move* to another thread (as `H264Encoder` does into the
// encode task spawned by `run_session`) as long as they're never accessed
// from two threads at once — true here: `H264Encoder` is constructed, then
// every `encode`/`flush` call happens serially from that one task.
unsafe impl Send for H264Encoder {}

impl H264Encoder {
    /// `keyframe_interval` is in frames (e.g. `fps * 2` for a keyframe every
    /// ~2s) — see plan step 4 on why a short interval matters here far more
    /// than it did for the old zstd-per-frame pipeline.
    pub fn new(width: u32, height: u32, fps: u32, bitrate_bits_per_sec: usize, keyframe_interval: u32) -> anyhow::Result<Self> {
        ffmpeg::init()?;

        let mut last_err: Option<anyhow::Error> = None;
        let mut opened = None;
        for &(name, is_hardware) in CANDIDATE_ENCODERS {
            match try_open(name, width, height, fps, bitrate_bits_per_sec, keyframe_interval) {
                Ok(encoder) => {
                    opened = Some((encoder, name, is_hardware));
                    break;
                }
                Err(e) => {
                    tracing::warn!("encoder '{name}' unavailable, trying next candidate: {e}");
                    last_err = Some(e);
                }
            }
        }
        let (encoder, backend, is_hardware) = opened.ok_or_else(|| {
            last_err.unwrap_or_else(|| anyhow::anyhow!("no H.264 encoder candidates configured"))
        })?;

        let scaler = ScalerContext::get(Pixel::BGRA, width, height, Pixel::NV12, width, height, ScalerFlags::BILINEAR)?;

        Ok(Self { encoder, scaler, width, height, next_pts: 0, backend, is_hardware })
    }

    /// Encodes one captured BGRA frame. An H.264 encoder may buffer frames
    /// internally before emitting packets, so this can return zero, one, or
    /// (rarely) more than one [`EncodedPacket`] per call — always drain
    /// fully rather than assuming a 1:1 call/packet correspondence.
    pub fn encode(&mut self, frame: &CapturedFrame) -> anyhow::Result<Vec<EncodedPacket>> {
        if frame.width != self.width || frame.height != self.height {
            anyhow::bail!(
                "frame size {}x{} doesn't match encoder size {}x{} (resolution change mid-session isn't handled yet)",
                frame.width, frame.height, self.width, self.height
            );
        }

        let mut bgra = VideoFrame::new(Pixel::BGRA, self.width, self.height);
        copy_packed_bgra_into(&mut bgra, frame);

        let mut nv12 = VideoFrame::new(Pixel::NV12, self.width, self.height);
        self.scaler.run(&bgra, &mut nv12)?;
        nv12.set_pts(Some(self.next_pts));
        self.next_pts += 1;

        self.encoder.send_frame(&nv12)?;
        self.drain_packets()
    }

    /// Flushes any frames the encoder is still holding onto — call once,
    /// at session end, to get the last packet(s) out.
    pub fn flush(&mut self) -> anyhow::Result<Vec<EncodedPacket>> {
        self.encoder.send_eof()?;
        self.drain_packets()
    }

    fn drain_packets(&mut self) -> anyhow::Result<Vec<EncodedPacket>> {
        let mut out = Vec::new();
        loop {
            let mut packet = Packet::empty();
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => {
                    let data = packet.data().unwrap_or(&[]).to_vec();
                    out.push(EncodedPacket { data, is_keyframe: packet.is_key() });
                }
                // Not an error: "no packet ready yet" / "drained everything
                // after send_eof", both expected, non-fatal outcomes.
                Err(FfmpegError::Other { errno }) if errno == EAGAIN => break,
                Err(FfmpegError::Eof) => break,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(out)
    }
}

/// Turns one encoded H.264 access unit into the same wire shape
/// `encode::encode_frame` produces for `RawZstd` — chunked at
/// `MAX_CHUNK_BYTES`, one `FrameInfo` + its `FrameChunk`s — so
/// `network::run_video_link`/`send_frame` don't need to know or care which
/// codec produced it. Every call gets its own `frame_id`: an H.264 encoder
/// can emit zero, one, or more packets per captured frame (see
/// [`H264Encoder::encode`]'s doc), so the caller assigns one `frame_id` per
/// *packet*, not per captured frame.
pub fn chunk_packet(packet: EncodedPacket, frame_id: u32, width: u32, height: u32) -> crate::encode::EncodedFrame {
    use bacak_remote_proto::{Codec, FrameChunk, FrameInfo, PixelFormat, MAX_CHUNK_BYTES};

    let chunks: Vec<FrameChunk> = packet
        .data
        .chunks(MAX_CHUNK_BYTES)
        .enumerate()
        .map(|(i, data)| FrameChunk { frame_id, chunk_index: i as u16, data: data.to_vec() })
        .collect();
    let info = FrameInfo {
        frame_id,
        width,
        height,
        // The format decoded H.264 frames end up in on the receiving side —
        // same BGRA `MemoryRenderBuffer`/`render.rs` path RawZstd already
        // feeds, once a decoder exists (plan step 5).
        format: PixelFormat::Bgra8,
        codec: Codec::H264 { is_keyframe: packet.is_keyframe },
        payload_len: packet.data.len() as u32,
        chunk_count: chunks.len() as u16,
    };
    crate::encode::EncodedFrame { info, chunks }
}

fn try_open(name: &str, width: u32, height: u32, fps: u32, bitrate_bits_per_sec: usize, keyframe_interval: u32) -> anyhow::Result<encoder::video::Encoder> {
    let codec = encoder::find_by_name(name).ok_or_else(|| anyhow::anyhow!("'{name}' not compiled into this FFmpeg build"))?;

    let mut video = CodecContext::new_with_codec(codec).encoder().video()?;
    video.set_width(width);
    video.set_height(height);
    // NV12 in, not BGRA — every candidate here (NVENC/AMF/QSV/x264) takes
    // NV12; the scaler in `new` does that conversion once, regardless of
    // which candidate ends up winning.
    video.set_format(Pixel::NV12);
    video.set_time_base(Rational::new(1, fps.max(1) as i32));
    video.set_frame_rate(Some(Rational::new(fps.max(1) as i32, 1)));
    video.set_bit_rate(bitrate_bits_per_sec);
    video.set_gop(keyframe_interval.max(1));
    // Zero B-frames: they trade latency (reorder delay) for compression,
    // which is the wrong trade for a live remote-desktop link.
    video.set_max_b_frames(0);

    Ok(video.open_as(codec)?)
}

/// `VideoFrame::new` allocates its own (32-byte-aligned) row stride, which
/// usually isn't `width * 4` — unlike `CapturedFrame::bgra`, already
/// stripped of *its* platform stride by `capture.rs`. Copy row by row
/// instead of one `copy_from_slice` to respect the destination's stride.
fn copy_packed_bgra_into(dst: &mut VideoFrame, frame: &CapturedFrame) {
    let row_bytes = frame.width as usize * 4;
    let stride = dst.stride(0);
    let dst_data = dst.data_mut(0);
    for row in 0..frame.height as usize {
        let src = row * row_bytes;
        let dst_off = row * stride;
        dst_data[dst_off..dst_off + row_bytes].copy_from_slice(&frame.bgra[src..src + row_bytes]);
    }
}
