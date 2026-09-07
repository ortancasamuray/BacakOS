//! The file-transfer channel (TCP): receives files sent from the Android
//! client, verifies them, and writes them into the configured download
//! directory. See `../../ARCHITECTURE.md` §2.2 for the three-phase protocol
//! this implements (handshake → streaming → verification).
//!
//! Only the receive direction (Android → BacakOS) is implemented so far —
//! the architecture doc notes the state machine is meant to be symmetric,
//! but daemon-initiated sends (BacakOS → Android) aren't wired up yet; see
//! the "open questions" section there.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};

use crate::protocol::{
    encode_file_reject, encode_simple, ChunkHeader, FileMeta, Header, Opcode, HEADER_LEN,
};
use crate::trust::TrustStore;

pub async fn run(listener: TcpListener, download_dir: PathBuf, trust: TrustStore) -> Result<()> {
    tokio::fs::create_dir_all(&download_dir)
        .await
        .with_context(|| format!("creating download dir {}", download_dir.display()))?;

    loop {
        let (mut stream, peer) = listener
            .accept()
            .await
            .context("accepting file-transfer connection")?;

        if !trust.is_trusted(peer.ip()) {
            // Reject immediately, before the handshake even starts — unlike
            // the input (UDP) channel, TCP gives us a connection to write a
            // real reason back on, so the sender doesn't have to guess why
            // its file went nowhere.
            warn!(%peer, "rejecting file-transfer connection from an unpaired address");
            stream
                .write_all(&encode_file_reject("cihaz eşleştirilmemiş"))
                .await
                .ok();
            continue;
        }

        let download_dir = download_dir.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_receive(stream, &download_dir).await {
                warn!(?err, %peer, "file transfer failed");
            }
        });
    }
}

async fn read_frame(stream: &mut TcpStream) -> Result<(Opcode, Vec<u8>)> {
    let mut header_buf = [0u8; HEADER_LEN];
    stream
        .read_exact(&mut header_buf)
        .await
        .context("reading frame header")?;
    let header = Header::decode(&header_buf)?;

    let mut payload = vec![0u8; header.payload_len as usize];
    stream
        .read_exact(&mut payload)
        .await
        .context("reading frame payload")?;
    Ok((header.opcode, payload))
}

async fn handle_receive(mut stream: TcpStream, download_dir: &Path) -> Result<()> {
    // Phase 1: handshake.
    let (opcode, payload) = read_frame(&mut stream).await?;
    if opcode != Opcode::FileMeta {
        stream
            .write_all(&encode_file_reject("expected FILE_META first"))
            .await
            .ok();
        anyhow::bail!("first frame on a file-transfer connection was {opcode:?}, not FileMeta");
    }
    let meta = FileMeta::decode_payload(&payload)?;

    let dest = unique_destination(download_dir, &meta.name);
    let Some(dest) = dest else {
        stream
            .write_all(&encode_file_reject(
                "could not choose a destination filename",
            ))
            .await
            .ok();
        anyhow::bail!("destination filename resolution failed for {:?}", meta.name);
    };

    stream
        .write_all(&encode_simple(Opcode::FileAccept))
        .await
        .context("sending FILE_ACCEPT")?;
    info!(name = %meta.name, size = meta.size, dest = %dest.display(), "receiving file");

    // Phase 2: streaming.
    let mut file = File::create(&dest)
        .await
        .with_context(|| format!("creating {}", dest.display()))?;
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    let mut reader = BufReader::new(stream);

    loop {
        if received >= meta.size {
            break;
        }

        let mut header_buf = [0u8; HEADER_LEN];
        if let Err(err) = reader.read_exact(&mut header_buf).await {
            if err.kind() == io::ErrorKind::UnexpectedEof {
                anyhow::bail!(
                    "connection closed mid-transfer at {received}/{} bytes",
                    meta.size
                );
            }
            return Err(err).context("reading chunk frame header");
        }
        let header = Header::decode(&header_buf)?;

        match header.opcode {
            Opcode::TransferCancel => {
                warn!(name = %meta.name, "transfer cancelled by sender");
                drop(file);
                tokio::fs::remove_file(&dest).await.ok();
                return Ok(());
            }
            Opcode::Chunk => {
                let mut chunk_payload = vec![0u8; header.payload_len as usize];
                reader
                    .read_exact(&mut chunk_payload)
                    .await
                    .context("reading chunk payload")?;
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

                file.write_all(data)
                    .await
                    .context("writing chunk to disk")?;
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
        let mut stream = reader.into_inner();
        stream
            .write_all(&encode_simple(Opcode::FileCorrupt))
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
