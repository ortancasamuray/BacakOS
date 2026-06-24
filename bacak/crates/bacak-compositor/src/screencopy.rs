//! `zwlr_screencopy_v1` — lets screenshot / screen-share tools (grim, OBS via
//! xdg-desktop-portal-wlr, wf-recorder) capture an output's pixels.
//!
//! smithay 0.7 has no built-in screencopy, so this is a hand-rolled
//! implementation of the wlr protocol. Scope: **SHM client buffers**, full
//! output or a sub-region; no dma-buf target, no cursor compositing, no damage
//! tracking (every `copy` does a full readback).
//!
//! Split of responsibility, mirroring the dma-buf path:
//! * The Dispatch handlers here run on [`BacakState`] (no renderer). They
//!   resolve the output, hand the client a buffer spec, and on `copy` validate
//!   the buffer and push a [`ScreencopyRequest`] onto `state.pending_screencopy`.
//! * The backend render tick calls [`crate::render::process_pending_screencopy`]
//!   (which *has* a `GlesRenderer`) to render the output offscreen, read it
//!   back, copy into the client's SHM buffer, and fire `ready` / `failed`.

use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
};
use smithay::reexports::wayland_server::protocol::{
    wl_buffer::WlBuffer, wl_output::WlOutput, wl_shm,
};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::{Physical, Rectangle};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::state::BacakState;
use crate::wm::OutputId;

/// Per-frame state: which output/region this frame captures, and a one-shot
/// guard (the protocol allows exactly one `copy` per frame object).
#[derive(Debug)]
pub struct ScreencopyFrameData {
    pub output: OutputId,
    /// Capture region in **physical** pixels, relative to the output.
    pub region: Rectangle<i32, Physical>,
    pub copied: AtomicBool,
}

/// A validated, queued capture awaiting the renderer.
#[derive(Debug)]
pub struct ScreencopyRequest {
    pub frame: ZwlrScreencopyFrameV1,
    pub buffer: WlBuffer,
    pub output: OutputId,
    pub region: Rectangle<i32, Physical>,
}

/// Resolve a client `wl_output` to our `OutputId` and physical mode size.
fn resolve_output(state: &BacakState, wl: &WlOutput) -> Option<(OutputId, (i32, i32))> {
    let o = smithay::output::Output::from_resource(wl)?;
    let (oid, out) = state.outputs.iter().find(|(_, ro)| **ro == o)?;
    let size = out.current_mode()?.size;
    Some((*oid, (size.w, size.h)))
}

/// Announce the buffer the client must allocate, then `buffer_done`. We only
/// offer an SHM XRGB8888 buffer at the capture size; stride is tightly packed.
fn advertise(frame: &ZwlrScreencopyFrameV1, w: i32, h: i32) {
    let stride = (w * 4).max(0) as u32;
    frame.buffer(wl_shm::Format::Xrgb8888, w.max(0) as u32, h.max(0) as u32, stride);
    if frame.version() >= 3 {
        frame.buffer_done();
    }
}

impl GlobalDispatch<ZwlrScreencopyManagerV1, ()> for BacakState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for BacakState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_screencopy_manager_v1::Request;
        match request {
            Request::CaptureOutput { frame, overlay_cursor: _, output } => {
                let Some((oid, (w, h))) = resolve_output(state, &output) else {
                    // Init the frame just so we can report failure cleanly.
                    let f = data_init.init(
                        frame,
                        ScreencopyFrameData {
                            output: OutputId::default(),
                            region: Rectangle::default(),
                            copied: AtomicBool::new(true),
                        },
                    );
                    f.failed();
                    return;
                };
                let region = Rectangle::from_size((w, h).into());
                let f = data_init.init(
                    frame,
                    ScreencopyFrameData { output: oid, region, copied: AtomicBool::new(false) },
                );
                advertise(&f, w, h);
            }
            Request::CaptureOutputRegion {
                frame,
                overlay_cursor: _,
                output,
                x,
                y,
                width,
                height,
            } => {
                let Some((oid, (ow, oh))) = resolve_output(state, &output) else {
                    let f = data_init.init(
                        frame,
                        ScreencopyFrameData {
                            output: OutputId::default(),
                            region: Rectangle::default(),
                            copied: AtomicBool::new(true),
                        },
                    );
                    f.failed();
                    return;
                };
                // The region arrives in output-logical coords; scale to physical
                // and clamp to the output.
                let scale = state.wm.output_scale(oid).round() as i32;
                let scale = scale.max(1);
                let mut rx = (x * scale).clamp(0, ow);
                let mut ry = (y * scale).clamp(0, oh);
                let rw = (width * scale).clamp(0, ow - rx);
                let rh = (height * scale).clamp(0, oh - ry);
                if rw == 0 || rh == 0 {
                    rx = 0;
                    ry = 0;
                }
                let region = Rectangle::new((rx, ry).into(), (rw.max(1), rh.max(1)).into());
                let f = data_init.init(
                    frame,
                    ScreencopyFrameData { output: oid, region, copied: AtomicBool::new(false) },
                );
                advertise(&f, region.size.w, region.size.h);
            }
            Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameData> for BacakState {
    fn request(
        state: &mut Self,
        _client: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &ScreencopyFrameData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_screencopy_frame_v1::Request;
        match request {
            Request::Copy { buffer } | Request::CopyWithDamage { buffer } => {
                // The protocol allows one copy per frame.
                if data.copied.swap(true, Ordering::SeqCst) {
                    frame.post_error(
                        zwlr_screencopy_frame_v1::Error::AlreadyUsed,
                        "frame already copied",
                    );
                    return;
                }
                // Buffer must be SHM with the format/size we advertised; the
                // detailed read happens in the renderer pass (it validates the
                // SHM contents there and fails the frame if they don't fit).
                state.pending_screencopy.push(ScreencopyRequest {
                    frame: frame.clone(),
                    buffer,
                    output: data.output,
                    region: data.region,
                });
            }
            Request::Destroy => {}
            _ => {}
        }
    }
}
