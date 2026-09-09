//! Desktop screen capture.
//!
//! v1 backend is [`scrap`] (X11 on Linux/XWayland, DXGI on Windows, CoreGraphics
//! on macOS) — one dependency, no system codec/portal setup required, covering
//! all three target OSes from the spec. It runs on its own OS thread because
//! `scrap::Capturer::frame()` is a blocking, poll-and-retry API, not async.
//!
//! Known gap: native Wayland compositors (not running XWayland) aren't
//! reachable by `scrap`. A `PipeWireCapturer` behind the same [`run_capture_thread`]
//! contract (xdg-desktop-portal ScreenCast + pipewire-rs) is the designed
//! upgrade slot for that case — not implemented here, since it needs a running
//! portal backend to test against.

use std::thread::JoinHandle;
use std::time::Duration;

use bacak_remote_proto::PixelFormat;
use scrap::{Capturer, Display};
use tokio::sync::mpsc;

pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Tightly packed, no row padding — see the stride-strip step below.
    pub bgra: Vec<u8>,
}

/// Spawns the blocking capture loop and returns its handle plus a receiver
/// for frames. `scrap::Capturer` holds non-`Send` platform handles (e.g. an
/// `Rc`-based XCB connection on X11), so it must be constructed *inside* the
/// new thread rather than moved into it. Drop the sender side (stop the
/// thread) by dropping the returned `JoinHandle`'s channel is not possible
/// directly; the thread exits when the receiver is dropped and a subsequent
/// send fails.
pub fn run_capture_thread(target_fps: u32) -> anyhow::Result<(JoinHandle<()>, mpsc::Receiver<CapturedFrame>)> {
    let (tx, rx) = mpsc::channel(2); // small: a stale queued frame is worse than backpressure
    let frame_interval = Duration::from_secs_f64(1.0 / target_fps.max(1) as f64);
    let handle = std::thread::Builder::new()
        .name("bacak-remote-capture".into())
        .spawn(move || {
            let display = match Display::primary() {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!("no primary display: {e}");
                    return;
                }
            };
            let capturer = match Capturer::new(display) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("capturer init failed: {e}");
                    return;
                }
            };
            capture_loop(capturer, frame_interval, tx);
        })?;

    Ok((handle, rx))
}

fn capture_loop(mut capturer: Capturer, frame_interval: Duration, tx: mpsc::Sender<CapturedFrame>) {
    let width = capturer.width() as u32;
    let height = capturer.height() as u32;

    loop {
        let tick_start = std::time::Instant::now();
        match capturer.frame() {
            Ok(frame) => {
                let packed = strip_stride_padding(&frame, width, height);
                let captured = CapturedFrame { width, height, format: PixelFormat::Bgra8, bgra: packed };
                if tx.blocking_send(captured).is_err() {
                    tracing::info!("capture: receiver gone, stopping");
                    return;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No new frame yet; scrap wants a short retry, not a busy spin.
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            Err(e) => {
                tracing::error!("capture: fatal error, stopping: {e}");
                return;
            }
        }
        let elapsed = tick_start.elapsed();
        if elapsed < frame_interval {
            std::thread::sleep(frame_interval - elapsed);
        }
    }
}

/// `scrap` frame buffers are row-padded to the platform's stride; the wire
/// protocol assumes tightly packed BGRA so the client can `width * 4` index it.
fn strip_stride_padding(frame: &[u8], width: u32, height: u32) -> Vec<u8> {
    let row_bytes = width as usize * PixelFormat::Bgra8.bytes_per_pixel();
    let stride = frame.len() / height.max(1) as usize;
    if stride == row_bytes {
        return frame.to_vec();
    }
    let mut out = Vec::with_capacity(row_bytes * height as usize);
    for row in 0..height as usize {
        let start = row * stride;
        out.extend_from_slice(&frame[start..start + row_bytes]);
    }
    out
}
