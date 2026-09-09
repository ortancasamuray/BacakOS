//! Client-side transport: an async task owns the video socket (pairing,
//! frame reassembly, heartbeats) and hands finished frames to the render
//! thread over a plain [`std::sync::mpsc`] channel — the render loop is
//! `winit`'s synchronous event loop, not a tokio task, so the handoff has to
//! cross that boundary. Input packets are sent from the render thread
//! directly over a blocking `std::net::UdpSocket`: send-only, fire-and-
//! forget, so there is no reason to route them through the async runtime at
//! all — that would only add scheduling latency to the one path where it
//! matters most. The input channel's `Cipher` is produced by pairing (on the
//! async side) and handed to the render thread via [`SharedInputCipher`].

use std::net::{SocketAddr, UdpSocket as StdUdpSocket};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use bacak_remote_proto::crypto::{Cipher, EphemeralKeypair, Opener};
use bacak_remote_proto::{decode, encode, open_message, seal_message, InputEvent, Message};
use tokio::net::UdpSocket;
use tokio::time::interval;

use crate::decode::{DecodedFrame, FrameReassembler};

/// Shared with the winit thread: `None` until pairing completes, after
/// which `send_input` can encrypt outgoing events with it.
pub type SharedInputCipher = Arc<StdMutex<Option<Cipher>>>;

pub fn new_shared_input_cipher() -> SharedInputCipher {
    Arc::new(StdMutex::new(None))
}

/// Runs until the socket errors out; call from a dedicated tokio runtime
/// thread. Pairs with `pin` (sending `PairRequest` until `PairResponse`
/// arrives), verifies the server's `confirm_tag` before trusting anything it
/// sent, then reassembles frames. Returns an error — rather than retrying
/// forever — on a wrong PIN or a confirm-tag mismatch (possible
/// man-in-the-middle), since no amount of retrying fixes either.
pub async fn run_video_receiver(
    server_addr: SocketAddr,
    client_name: String,
    pin: u32,
    frame_tx: Sender<DecodedFrame>,
    input_cipher: SharedInputCipher,
) -> anyhow::Result<(u32, u32)> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    socket.connect(server_addr).await?;
    let socket = Arc::new(socket);

    let client_keypair = EphemeralKeypair::generate();
    let client_pubkey = client_keypair.public_bytes;
    let pair_request = encode(&Message::PairRequest { client_name, pin, client_pubkey })?;

    let (server_pubkey, confirm_tag, screen_size) = loop {
        socket.send(&pair_request).await?;
        let mut buf = [0u8; 512];
        match tokio::time::timeout(Duration::from_millis(500), socket.recv(&mut buf)).await {
            Ok(Ok(len)) => match decode(&buf[..len]) {
                Ok(Message::PairResponse { accepted: true, server_pubkey, confirm_tag, screen_width, screen_height }) => {
                    break (server_pubkey, confirm_tag, (screen_width, screen_height));
                }
                Ok(Message::PairResponse { accepted: false, .. }) => {
                    anyhow::bail!("server rejected pairing — wrong PIN?");
                }
                _ => tracing::debug!("no PairResponse yet, retrying"),
            },
            _ => tracing::debug!("no PairResponse yet, retrying"),
        }
    };

    let material = client_keypair.derive(server_pubkey, pin, client_pubkey, server_pubkey);
    if material.confirm_tag != confirm_tag {
        anyhow::bail!(
            "pairing confirm-tag mismatch — either the PIN was wrong, or this exchange was intercepted; refusing to trust the received keys"
        );
    }
    let video_keys = material.channel_keys("video");
    let input_keys = material.channel_keys("input");
    let mut video_opener = Opener::new(video_keys.s2c_key); // decrypts server -> client
    let mut video_cipher = Cipher::new(video_keys.c2s_key); // encrypts client -> server (Heartbeat/Bye)
    *input_cipher.lock().unwrap() = Some(Cipher::new(input_keys.c2s_key));

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
            let Ok(sealed) = seal_message(&Message::Heartbeat { timestamp_ms: now_ms }, &mut video_cipher) else { continue };
            if let Ok(bytes) = encode(&sealed) {
                let _ = heartbeat_socket.send(&bytes).await;
            }
        }
    });

    let mut reassembler = FrameReassembler::default();
    let mut buf = vec![0u8; 2048];
    loop {
        let len = socket.recv(&mut buf).await?;
        let msg = match decode(&buf[..len]) {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!("dropping malformed video packet: {e}");
                continue;
            }
        };
        let inner = match open_message(&msg, &mut video_opener) {
            Ok(inner) => inner,
            Err(e) => {
                tracing::debug!("dropping undecryptable video packet: {e}");
                continue;
            }
        };
        match inner {
            Message::FrameInfo(info) => reassembler.start_frame(info),
            Message::FrameChunk(chunk) => match reassembler.add_chunk(chunk.frame_id, chunk.chunk_index, chunk.data) {
                Ok(Some(frame)) => {
                    if frame_tx.send(frame).is_err() {
                        return Ok(screen_size); // render thread gone, shut down cleanly
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("frame decode failed: {e}"),
            },
            other => tracing::debug!("unexpected inner message on video socket: {other:?}"),
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

/// Encrypts and sends one input event — a no-op (with a debug log) until
/// pairing has populated `cipher`, so input generated in that brief window
/// is dropped rather than sent in the clear or queued.
pub fn send_input(socket: &StdUdpSocket, cipher: &SharedInputCipher, event: InputEvent) {
    let mut guard = cipher.lock().unwrap();
    let Some(cipher) = guard.as_mut() else {
        tracing::debug!("dropping input event sent before pairing completed: {event:?}");
        return;
    };
    let sealed = match seal_message(&Message::Input(event), cipher) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("input encrypt failed: {e}");
            return;
        }
    };
    match encode(&sealed) {
        Ok(bytes) => {
            if let Err(e) = socket.send(&bytes) {
                tracing::debug!("input send failed (dropped, non-fatal): {e}");
            }
        }
        Err(e) => tracing::warn!("input encode failed: {e}"),
    }
}
