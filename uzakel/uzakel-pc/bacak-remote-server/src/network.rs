//! UDP transport: one socket for Hello/HelloAck/FrameInfo/FrameChunk/Heartbeat
//! (video port), one dedicated to inbound `Input` events (input port) so a
//! burst of video chunks never queues behind — or after — an input packet.

use std::net::SocketAddr;
use std::sync::Arc;

use bacak_remote_proto::{decode, encode, Message};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, RwLock};

use crate::encode::EncodedFrame;
use crate::input_inject::Injector;

pub type SharedClientAddr = Arc<RwLock<Option<SocketAddr>>>;

/// Answers `Hello` with `HelloAck` and remembers the client's address; forwards
/// every `EncodedFrame` produced by the capture/encode pipeline to it.
pub async fn run_video_link(
    socket: UdpSocket,
    screen_width: u32,
    screen_height: u32,
    mut frame_rx: mpsc::Receiver<EncodedFrame>,
    client_addr: SharedClientAddr,
) -> anyhow::Result<()> {
    let socket = Arc::new(socket);
    let recv_socket = socket.clone();
    let recv_client_addr = client_addr.clone();

    tokio::spawn(async move {
        let mut buf = vec![0u8; 2048];
        loop {
            let (len, from) = match recv_socket.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("video socket recv error: {e}");
                    continue;
                }
            };
            match decode(&buf[..len]) {
                Ok(Message::Hello { client_name }) => {
                    tracing::info!("client '{client_name}' connected from {from}");
                    *recv_client_addr.write().await = Some(from);
                    let ack = Message::HelloAck { screen_width, screen_height };
                    if let Ok(bytes) = encode(&ack) {
                        let _ = recv_socket.send_to(&bytes, from).await;
                    }
                }
                Ok(Message::Heartbeat { .. }) => {
                    // Presence confirmation only; nothing to do beyond the recv above.
                }
                Ok(Message::Bye) => {
                    tracing::info!("client {from} disconnected");
                    let mut guard = recv_client_addr.write().await;
                    if *guard == Some(from) {
                        *guard = None;
                    }
                }
                Ok(other) => tracing::debug!("unexpected message on video socket: {other:?}"),
                Err(e) => tracing::debug!("dropping malformed packet from {from}: {e}"),
            }
        }
    });

    while let Some(encoded) = frame_rx.recv().await {
        let Some(addr) = *client_addr.read().await else { continue };
        send_frame(&socket, addr, &encoded).await;
    }
    Ok(())
}

async fn send_frame(socket: &UdpSocket, addr: SocketAddr, encoded: &EncodedFrame) {
    match encode(&Message::FrameInfo(encoded.info)) {
        Ok(bytes) => {
            if let Err(e) = socket.send_to(&bytes, addr).await {
                tracing::warn!("send FrameInfo failed: {e}");
                return;
            }
        }
        Err(e) => {
            tracing::error!("encode FrameInfo failed: {e}");
            return;
        }
    }
    for chunk in &encoded.chunks {
        match encode(&Message::FrameChunk(chunk.clone())) {
            Ok(bytes) => {
                if let Err(e) = socket.send_to(&bytes, addr).await {
                    tracing::warn!("send FrameChunk {} failed: {e}", chunk.chunk_index);
                }
            }
            Err(e) => tracing::error!("encode FrameChunk failed: {e}"),
        }
    }
}

/// Receives `Input` events from the client and injects them immediately —
/// this loop is deliberately separate from video traffic (see module doc).
pub async fn run_input_listener(socket: UdpSocket, mut injector: Injector) -> anyhow::Result<()> {
    let mut buf = vec![0u8; 512];
    loop {
        let (len, from) = socket.recv_from(&mut buf).await?;
        match decode(&buf[..len]) {
            Ok(Message::Input(event)) => {
                tracing::info!("received input from {from}: {event:?}");
                if let Err(e) = injector.inject(event) {
                    tracing::warn!("input injection failed: {e}");
                }
            }
            Ok(other) => tracing::debug!("unexpected message on input socket: {other:?}"),
            Err(e) => tracing::debug!("dropping malformed input packet from {from}: {e}"),
        }
    }
}
