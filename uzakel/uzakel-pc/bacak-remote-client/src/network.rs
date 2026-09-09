//! Client-side transport: an async task owns the video socket (Hello, frame
//! reassembly, heartbeats) and hands finished frames to the render thread over
//! a plain [`std::sync::mpsc`] channel — the render loop is `winit`'s
//! synchronous event loop, not a tokio task, so the handoff has to cross that
//! boundary. Input packets are sent from the render thread directly over a
//! blocking `std::net::UdpSocket`: send-only, fire-and-forget, so there is no
//! reason to route them through the async runtime at all — that would only
//! add scheduling latency to the one path where it matters most.

use std::net::{SocketAddr, UdpSocket as StdUdpSocket};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use bacak_remote_proto::{decode, encode, InputEvent, Message};
use tokio::net::UdpSocket;
use tokio::time::interval;

use crate::decode::{DecodedFrame, FrameReassembler};

/// Runs until the socket errors out; call from a dedicated tokio runtime
/// thread. Sends `Hello` until `HelloAck` is seen, then reassembles frames.
pub async fn run_video_receiver(
    server_addr: SocketAddr,
    client_name: String,
    frame_tx: Sender<DecodedFrame>,
) -> anyhow::Result<(u32, u32)> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.connect(server_addr).await?;
    let socket = Arc::new(socket);

    let hello = encode(&Message::Hello { client_name })?;
    let screen_size = loop {
        socket.send(&hello).await?;
        let mut buf = [0u8; 512];
        match tokio::time::timeout(Duration::from_millis(500), socket.recv(&mut buf)).await {
            Ok(Ok(len)) => {
                if let Ok(Message::HelloAck { screen_width, screen_height }) = decode(&buf[..len]) {
                    break (screen_width, screen_height);
                }
            }
            _ => tracing::debug!("no HelloAck yet, retrying"),
        }
    };
    tracing::info!("paired with server, source screen {}x{}", screen_size.0, screen_size.1);

    let heartbeat_socket = socket.clone();
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(2));
        loop {
            ticker.tick().await;
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            if let Ok(bytes) = encode(&Message::Heartbeat { timestamp_ms: now_ms }) {
                let _ = heartbeat_socket.send(&bytes).await;
            }
        }
    });

    let mut reassembler = FrameReassembler::default();
    let mut buf = vec![0u8; 2048];
    loop {
        let len = socket.recv(&mut buf).await?;
        match decode(&buf[..len]) {
            Ok(Message::FrameInfo(info)) => reassembler.start_frame(info),
            Ok(Message::FrameChunk(chunk)) => {
                match reassembler.add_chunk(chunk.frame_id, chunk.chunk_index, chunk.data) {
                    Ok(Some(frame)) => {
                        if frame_tx.send(frame).is_err() {
                            return Ok(screen_size); // render thread gone, shut down cleanly
                        }
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!("frame decode failed: {e}"),
                }
            }
            Ok(other) => tracing::debug!("unexpected message on video socket: {other:?}"),
            Err(e) => tracing::debug!("dropping malformed video packet: {e}"),
        }
    }
}

/// A cloned, already-`connect`ed input socket for the render thread to send
/// [`InputEvent`]s over without touching the async runtime.
pub fn connect_input_socket(server_ip: std::net::IpAddr, input_port: u16) -> anyhow::Result<StdUdpSocket> {
    let socket = StdUdpSocket::bind("0.0.0.0:0")?;
    socket.connect(SocketAddr::new(server_ip, input_port))?;
    socket.set_nonblocking(true)?;
    Ok(socket)
}

pub fn send_input(socket: &StdUdpSocket, event: InputEvent) {
    match encode(&Message::Input(event)) {
        Ok(bytes) => {
            if let Err(e) = socket.send(&bytes) {
                tracing::debug!("input send failed (dropped, non-fatal): {e}");
            }
        }
        Err(e) => tracing::warn!("input encode failed: {e}"),
    }
}
