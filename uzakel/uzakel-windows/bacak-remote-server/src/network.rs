//! UDP transport: one socket for pairing/`FrameInfo`/`FrameChunk`/`Heartbeat`
//! (video port), one dedicated to inbound `Input` events (input port) so a
//! burst of video chunks never queues behind — or after — an input packet.
//!
//! Pairing (`PairRequest`/`PairResponse`) travels in the clear; everything
//! else is wrapped in `Message::Encrypted` once a session exists (see
//! `bacak_remote_proto::crypto` module doc for why, and its doc on
//! [`bacak_remote_proto::crypto::SessionMaterial::channel_keys`] for why the
//! video and input channels each get their own independently-keyed
//! `Cipher`/`Opener` rather than sharing one pair.

use std::net::SocketAddr;
use std::sync::Arc;

use bacak_remote_proto::crypto::{Cipher, EphemeralKeypair, Opener};
use bacak_remote_proto::{decode, encode, open_message, seal_message, Message};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

use crate::encode::EncodedFrame;
use crate::input_inject::Injector;
use crate::{SessionStatus, StatusChannel};

/// A completed pairing: independently-keyed encrypt/decrypt state for each
/// channel, plus the address it belongs to (so a stale session for a
/// previous client can't be confused with the current one).
pub struct PairedSession {
    addr: SocketAddr,
    video_opener: Opener,  // decrypts client -> server traffic on the video socket (Heartbeat/Bye)
    video_cipher: Cipher,  // encrypts server -> client traffic on the video socket (FrameInfo/FrameChunk/Heartbeat/Bye)
    input_opener: Opener,  // decrypts client -> server traffic on the input socket (Input)
}

pub type SharedSession = Arc<Mutex<Option<PairedSession>>>;

pub fn new_shared_session() -> SharedSession {
    Arc::new(Mutex::new(None))
}

/// How encoded frames flow from the encode task to this module's send loop.
///
/// `RawZstd` frames are independent — the *n*th frame decodes correctly no
/// matter what happened to frame *n-1*, so it's fine (and, at any real
/// resolution/frame rate, necessary — see `run_session`'s doc on why this
/// used a `watch` channel in the first place) for a network task that falls
/// behind to skip straight to the newest one and never send the
/// superseded ones at all.
///
/// H.264 delta frames are NOT independent: each one is only decodable
/// relative to the reference picture the *previous* frame actually left in
/// the decoder. A "freshest wins" channel silently skipping delta frames
/// whenever the network task is still busy sending an earlier (often
/// larger, e.g. a keyframe) one breaks that reference chain — the decoder
/// on the other end (`FrameReassembler` in `bacak-compositor`) has no way
/// to tell an intentional skip from actual packet loss, so it does the only
/// safe thing and stalls until the next keyframe, same as it would for a
/// real dropped packet. Real-hardware testing (2026-09-15) found this is
/// what actually produced multi-second freezes on ordinary scene changes
/// (closing a window), not network loss — so H.264 frames instead go
/// through `Ordered`, a bounded `mpsc` that blocks the encode task
/// (backpressure, not silent drops) when the network can't keep up.
pub enum FrameSource {
    Latest(tokio::sync::watch::Receiver<Option<EncodedFrame>>),
    Ordered(tokio::sync::mpsc::Receiver<EncodedFrame>),
}

impl FrameSource {
    async fn recv(&mut self) -> Option<EncodedFrame> {
        match self {
            FrameSource::Latest(rx) => loop {
                if rx.changed().await.is_err() {
                    return None;
                }
                if let Some(v) = rx.borrow_and_update().clone() {
                    return Some(v);
                }
                // Only reachable before the encode task's first `send` —
                // keep waiting for the real first frame instead of treating
                // that initial `None` as "channel closed".
            },
            FrameSource::Ordered(rx) => rx.recv().await,
        }
    }
}

/// Answers `PairRequest` (checking `pin`) with `PairResponse`, and forwards
/// every `EncodedFrame` produced by the capture/encode pipeline to whichever
/// client most recently paired successfully. `status_tx`, when given, is
/// told about each pairing attempt's outcome — see [`SessionStatus`].
pub async fn run_video_link(
    socket: Arc<UdpSocket>,
    pin: u32,
    screen_width: u32,
    screen_height: u32,
    mut frame_rx: FrameSource,
    session: SharedSession,
    status_tx: Option<StatusChannel>,
) -> anyhow::Result<()> {
    let recv_socket = socket.clone();
    let recv_session = session.clone();

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
                Ok(Message::PairRequest { client_name, pin: given_pin, client_pubkey }) => {
                    let server_keypair = EphemeralKeypair::generate();
                    let server_pubkey = server_keypair.public_bytes;

                    if given_pin != pin {
                        tracing::warn!("client '{client_name}' from {from} gave wrong PIN, rejecting");
                        if let Some(tx) = &status_tx {
                            let _ = tx.send(SessionStatus::Rejected { from: from.to_string() });
                        }
                        let resp = Message::PairResponse {
                            accepted: false,
                            server_pubkey: [0; 32],
                            confirm_tag: [0; 32],
                            screen_width: 0,
                            screen_height: 0,
                        };
                        if let Ok(bytes) = encode(&resp) {
                            let _ = recv_socket.send_to(&bytes, from).await;
                        }
                        continue;
                    }

                    let material = server_keypair.derive(client_pubkey, pin, client_pubkey, server_pubkey);
                    let video_keys = material.channel_keys("video");
                    let input_keys = material.channel_keys("input");

                    *recv_session.lock().await = Some(PairedSession {
                        addr: from,
                        video_opener: Opener::new(video_keys.c2s_key),
                        video_cipher: Cipher::new(video_keys.s2c_key),
                        input_opener: Opener::new(input_keys.c2s_key),
                    });

                    tracing::info!("client '{client_name}' paired successfully from {from}");
                    if let Some(tx) = &status_tx {
                        let _ = tx.send(SessionStatus::Paired { client_name: client_name.clone(), addr: from.to_string() });
                    }
                    let resp = Message::PairResponse {
                        accepted: true,
                        server_pubkey,
                        confirm_tag: material.confirm_tag,
                        screen_width,
                        screen_height,
                    };
                    if let Ok(bytes) = encode(&resp) {
                        let _ = recv_socket.send_to(&bytes, from).await;
                    }
                }
                Ok(msg @ Message::Encrypted { .. }) => {
                    let mut guard = recv_session.lock().await;
                    let Some(sess) = guard.as_mut() else {
                        tracing::debug!("dropping encrypted packet from {from}: no paired session yet");
                        continue;
                    };
                    if sess.addr != from {
                        tracing::debug!("dropping encrypted packet from {from}: paired with a different address");
                        continue;
                    }
                    match open_message(&msg, &mut sess.video_opener) {
                        Ok(Message::Heartbeat { .. }) => {} // liveness only
                        Ok(Message::Bye) => {
                            tracing::info!("client {from} disconnected");
                            *guard = None;
                        }
                        Ok(other) => tracing::debug!("unexpected inner message on video socket: {other:?}"),
                        Err(e) => tracing::debug!("dropping undecryptable packet from {from}: {e}"),
                    }
                }
                Ok(other) => tracing::debug!("unexpected message on video socket: {other:?}"),
                Err(e) => tracing::debug!("dropping malformed packet from {from}: {e}"),
            }
        }
    });

    while let Some(encoded) = frame_rx.recv().await {
        let mut guard = session.lock().await;
        let Some(sess) = guard.as_mut() else { continue };
        send_frame(&socket, sess.addr, &encoded, &mut sess.video_cipher).await;
    }
    Ok(())
}

async fn send_frame(socket: &UdpSocket, addr: SocketAddr, encoded: &EncodedFrame, cipher: &mut Cipher) {
    match seal_and_encode(&Message::FrameInfo(encoded.info), cipher) {
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
        match seal_and_encode(&Message::FrameChunk(chunk.clone()), cipher) {
            Ok(bytes) => {
                if let Err(e) = socket.send_to(&bytes, addr).await {
                    tracing::warn!("send FrameChunk {} failed: {e}", chunk.chunk_index);
                }
            }
            Err(e) => tracing::error!("encode FrameChunk failed: {e}"),
        }
    }
}

fn seal_and_encode(inner: &Message, cipher: &mut Cipher) -> anyhow::Result<Vec<u8>> {
    let sealed = seal_message(inner, cipher)?;
    Ok(encode(&sealed)?)
}

/// Tells whichever client is currently paired (if any) that the operator
/// ended the session from this side (`gui.rs`'s "Eşleşmeyi Bitir") — a
/// silent goodbye instead of just going dark, so the BacakOS panel
/// (`remote_desktop.rs`'s `run` loop) knows to close itself rather than
/// sitting on the last frame forever (there's nothing else that would ever
/// make its `socket.recv` return for a peer that simply stopped sending).
/// Best-effort: called right before `run_session` aborts the video/input
/// tasks, so a send failure here changes nothing about the shutdown itself.
pub async fn send_bye(socket: &UdpSocket, session: &SharedSession) {
    let mut guard = session.lock().await;
    let Some(sess) = guard.as_mut() else { return };
    if let Ok(bytes) = seal_and_encode(&Message::Bye, &mut sess.video_cipher) {
        let _ = socket.send_to(&bytes, sess.addr).await;
    }
}

/// Receives `Input` events from the client and injects them immediately —
/// this loop is deliberately separate from video traffic (see module doc).
/// Every packet must decrypt under the *current* paired session's input key
/// and match its address; anything else (no session yet, wrong address,
/// bad/replayed ciphertext) is silently dropped, same as an unpaired sender
/// always was before encryption existed.
pub async fn run_input_listener(socket: UdpSocket, mut injector: Injector, session: SharedSession) -> anyhow::Result<()> {
    let mut buf = vec![0u8; 512];
    loop {
        let (len, from) = socket.recv_from(&mut buf).await?;
        let msg = match decode(&buf[..len]) {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!("dropping malformed input packet from {from}: {e}");
                continue;
            }
        };
        let mut guard = session.lock().await;
        let Some(sess) = guard.as_mut() else {
            tracing::debug!("dropping input packet from {from}: no paired session yet");
            continue;
        };
        // Only the IP has to match, not the port: the input packet arrives on
        // a *different* UDP socket than the one `sess.addr` was captured
        // from (the video socket's `PairRequest`), so the client's ephemeral
        // source port here is legitimately different — comparing full
        // `SocketAddr`s would reject every real input packet.
        if sess.addr.ip() != from.ip() {
            tracing::debug!("dropping input packet from {from}: paired with a different address");
            continue;
        }
        match open_message(&msg, &mut sess.input_opener) {
            Ok(Message::Input(event)) => {
                tracing::info!("received input from {from}: {event:?}");
                if let Err(e) = injector.inject(event) {
                    tracing::warn!("input injection failed: {e}");
                }
            }
            Ok(other) => tracing::debug!("unexpected inner message on input socket: {other:?}"),
            Err(e) => tracing::debug!("dropping undecryptable input packet from {from}: {e}"),
        }
    }
}
