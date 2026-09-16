//! The file-transfer channel (TCP): receives files sent from the Android
//! client, verifies them, and writes them into the configured download
//! directory. See `../../ARCHITECTURE.md` §2.2 for the three-phase protocol
//! this implements (handshake → streaming → verification).
//!
//! Only the receive direction (Android → BacakOS) is implemented so far —
//! the architecture doc notes the state machine is meant to be symmetric,
//! but daemon-initiated sends (BacakOS → Android) aren't wired up yet; see
//! the "open questions" section there.
//!
//! Every frame after the initial trust check is wrapped in an
//! `EncryptedFrame` (§2.3.1) using the paired session's keys — there is no
//! plaintext fallback once a connection is accepted. The one deliberate
//! exception is the immediate `FILE_REJECT` sent to an *unpaired* address:
//! there's no session key to encrypt it with yet, and a rejection reason
//! carries nothing sensitive.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};

use crate::protocol::{
    decode_encrypted_frame_payload, encode_encrypted_frame, encode_file_reject, encode_simple,
    ChunkHeader, FileMeta, Header, Opcode, HEADER_LEN,
};
use crate::trust::{Session, TrustStore};

pub async fn run(listener: TcpListener, download_dir: PathBuf, trust: TrustStore) -> Result<()> {
    tokio::fs::create_dir_all(&download_dir)
        .await
        .with_context(|| format!("creating download dir {}", download_dir.display()))?;

    loop {
        let (mut stream, peer) = listener
            .accept()
            .await
            .context("accepting file-transfer connection")?;

        let Some(session) = trust.session(peer.ip()) else {
            // Reject immediately, before the handshake even starts — unlike
            // the input (UDP) channel, TCP gives us a connection to write a
            // real reason back on, so the sender doesn't have to guess why
            // its file went nowhere. Plaintext is correct here: there's no
            // session key yet, and a rejection reason isn't sensitive.
            warn!(%peer, "rejecting file-transfer connection from an unpaired address");
            stream
                .write_all(&encode_file_reject("cihaz eşleştirilmemiş"))
                .await
                .ok();
            continue;
        };

        let download_dir = download_dir.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_receive(stream, &download_dir, session).await {
                warn!(?err, %peer, "file transfer failed");
            }
        });
    }
}

/// Reads one `EncryptedFrame` off `stream`, decrypts it under `session`,
/// and returns the inner frame's opcode + payload — the same shape
/// `read_frame` used to return directly, before every frame on this
/// channel was encrypted.
async fn read_encrypted_frame(
    stream: &mut (impl AsyncRead + Unpin),
    session: &Session,
) -> Result<(Opcode, Vec<u8>)> {
    let mut header_buf = [0u8; HEADER_LEN];
    stream
        .read_exact(&mut header_buf)
        .await
        .context("reading encrypted frame header")?;
    let outer_header = Header::decode(&header_buf)?;
    if outer_header.opcode != Opcode::EncryptedFrame {
        anyhow::bail!("expected EncryptedFrame, got {:?} — no plaintext fallback on this channel", outer_header.opcode);
    }

    let mut outer_payload = vec![0u8; outer_header.payload_len as usize];
    stream
        .read_exact(&mut outer_payload)
        .await
        .context("reading encrypted frame payload")?;
    let (nonce, ciphertext) = decode_encrypted_frame_payload(&outer_payload)?;

    let inner_bytes = session
        .opener
        .lock()
        .unwrap()
        .open(nonce, ciphertext)
        .ok_or_else(|| anyhow::anyhow!("failed to decrypt/authenticate frame (bad key, corrupted data, or replay)"))?;

    let inner_header = Header::decode(&inner_bytes)?;
    let inner_payload = inner_bytes[HEADER_LEN..].to_vec();
    if inner_payload.len() != inner_header.payload_len as usize {
        anyhow::bail!("inner frame's declared length doesn't match what decrypted");
    }
    Ok((inner_header.opcode, inner_payload))
}

/// Encrypts `inner_frame` (a complete frame's bytes — header + payload, as
/// produced by e.g. `encode_simple`/`encode_file_reject`) under `session`
/// and writes the resulting `EncryptedFrame` to `stream`.
async fn write_encrypted_frame(
    stream: &mut (impl AsyncWrite + Unpin),
    session: &Session,
    inner_frame: &[u8],
) -> Result<()> {
    let (nonce, ciphertext) = session.cipher.lock().unwrap().seal(inner_frame);
    let outer = encode_encrypted_frame(nonce, &ciphertext);
    stream.write_all(&outer).await.context("writing encrypted frame")?;
    Ok(())
}

async fn handle_receive(mut stream: TcpStream, download_dir: &Path, session: Arc<Session>) -> Result<()> {
    // Phase 1: handshake.
    let (opcode, payload) = read_encrypted_frame(&mut stream, &session).await?;
    if opcode != Opcode::FileMeta {
        write_encrypted_frame(&mut stream, &session, &encode_file_reject("expected FILE_META first"))
            .await
            .ok();
        anyhow::bail!("first frame on a file-transfer connection was {opcode:?}, not FileMeta");
    }
    let meta = FileMeta::decode_payload(&payload)?;

    let dest = unique_destination(download_dir, &meta.name);
    let Some(dest) = dest else {
        write_encrypted_frame(
            &mut stream,
            &session,
            &encode_file_reject("could not choose a destination filename"),
        )
        .await
        .ok();
        anyhow::bail!("destination filename resolution failed for {:?}", meta.name);
    };

    write_encrypted_frame(&mut stream, &session, &encode_simple(Opcode::FileAccept))
        .await
        .context("sending FILE_ACCEPT")?;
    info!(name = %meta.name, size = meta.size, dest = %dest.display(), "receiving file");

    // Phase 2: streaming.
    let mut file = File::create(&dest)
        .await
        .with_context(|| format!("creating {}", dest.display()))?;
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;

    loop {
        if received >= meta.size {
            break;
        }

        let (opcode, chunk_payload) = match read_encrypted_frame(&mut stream, &session).await {
            Ok(v) => v,
            Err(err) => {
                // read_encrypted_frame wraps io errors via `.context(...)`,
                // so check the source chain for a clean EOF rather than
                // matching on `io::ErrorKind` directly here.
                if err
                    .chain()
                    .any(|cause| matches!(cause.downcast_ref::<io::Error>(), Some(e) if e.kind() == io::ErrorKind::UnexpectedEof))
                {
                    anyhow::bail!("connection closed mid-transfer at {received}/{} bytes", meta.size);
                }
                return Err(err);
            }
        };

        match opcode {
            Opcode::TransferCancel => {
                warn!(name = %meta.name, "transfer cancelled by sender");
                drop(file);
                tokio::fs::remove_file(&dest).await.ok();
                return Ok(());
            }
            Opcode::Chunk => {
                let chunk_header = ChunkHeader::decode(&chunk_payload)?;
                let data = &chunk_payload[ChunkHeader::ENCODED_LEN..];
                if data.len() != chunk_header.len as usize {
                    anyhow::bail!(
                        "chunk {} declared {} bytes but carried {}",
                        chunk_header.index,
                        chunk_header.len,
                        data.len()
                    );
                }

                file.write_all(data).await.context("writing chunk to disk")?;
                hasher.update(data);
                received += data.len() as u64;
            }
            other => anyhow::bail!("unexpected opcode {other:?} during streaming phase"),
        }
    }

    file.flush().await.ok();
    drop(file);

    // Phase 3: verification.
    let digest: [u8; 32] = hasher.finalize().into();
    if digest != meta.sha256 {
        warn!(name = %meta.name, "checksum mismatch, deleting partial file");
        tokio::fs::remove_file(&dest).await.ok();
        write_encrypted_frame(&mut stream, &session, &encode_simple(Opcode::FileCorrupt))
            .await
            .ok();
        anyhow::bail!("SHA-256 mismatch for {:?}", meta.name);
    }

    info!(name = %meta.name, dest = %dest.display(), "file received and verified");
    notify_received(&dest);
    Ok(())
}

/// Picks `dest_dir/name`, or `dest_dir/name (2)`, `(3)`, … if it already
/// exists — mirrors the "never silently overwrite" behaviour of desktop
/// file managers (see `altay`'s transfer module for the same convention on
/// the drag-and-drop side).
fn unique_destination(dest_dir: &Path, name: &str) -> Option<PathBuf> {
    let name = Path::new(name).file_name()?.to_str()?.to_string();
    let candidate = dest_dir.join(&name);
    if !candidate.exists() {
        return Some(candidate);
    }

    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.clone(), String::new()),
    };
    for n in 2..10_000 {
        let candidate = dest_dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Fires a desktop notification for the completed transfer via the
/// freedesktop notification bus. Shelling out to `notify-send` rather than
/// speaking `org.freedesktop.Notifications` over D-Bus directly is a
/// deliberate scope cut for this first pass — see ARCHITECTURE.md's open
/// questions for the native-D-Bus follow-up.
fn notify_received(path: &Path) {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("dosya");
    let result = Command::new("notify-send")
        .arg("Uzakel")
        .arg(format!("Dosya alındı: {name}"))
        .status();
    if let Err(err) = result {
        warn!(
            ?err,
            "could not show a desktop notification (is notify-send installed?)"
        );
    }
}
