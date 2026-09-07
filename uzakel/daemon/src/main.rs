//! `uzakel-daemon` — the BacakOS-side half of Uzakel (see `../../README.md`
//! and `../../ARCHITECTURE.md` for the system this implements).
//!
//! Three independent tasks, one per channel from ARCHITECTURE.md §1:
//! discovery/pairing (UDP broadcast), input replay (UDP), file transfer
//! (TCP). None of them share state beyond what each module owns internally
//! — a crash or bug in one channel doesn't take the others down.

mod discovery;
mod file_server;
mod input_manager;

// The encode side of `protocol` (InputPacket::encode, FileMeta::encode,
// encode_chunk, …) isn't called by this binary yet — only the Android
// client and the not-yet-implemented daemon-initiated send path
// (ARCHITECTURE.md's "open questions") need it. It's covered by
// protocol.rs's own round-trip tests, so keep it rather than deleting API
// surface the wire format is defined against.
#[allow(dead_code)]
mod protocol;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use anyhow::{Context, Result};
use tokio::net::{TcpListener, UdpSocket};
use tracing::info;
use tracing_subscriber::EnvFilter;

/// UDP broadcast port for discovery + PIN pairing. Deliberately *not* 5353
/// (the standard mDNS port): `avahi-daemon` already owns that on most
/// desktop Linux systems, and this is a plain broadcast responder, not a
/// real mDNS/DNS-SD implementation — sharing the port would risk confusing
/// (or being confused by) genuine mDNS traffic. Real mDNS support is an open
/// question in ARCHITECTURE.md; this is the pragmatic stand-in until then.
const DEFAULT_DISCOVERY_PORT: u16 = 45922;
const DEFAULT_INPUT_PORT: u16 = 9876;
const DEFAULT_FILE_PORT: u16 = 9877;

fn env_port(var: &str, default: u16) -> u16 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn download_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("UZAKEL_DOWNLOAD_DIR") {
        return PathBuf::from(dir);
    }
    // Mirrors the Turkish-localised XDG user dirs altay's `userdirs` module
    // sets up on first login — "İndirilenler" is the real Downloads folder
    // on a BacakOS install, not just an English fallback name.
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    for candidate in ["İndirilenler", "Downloads"] {
        let path = PathBuf::from(&home).join(candidate);
        if path.is_dir() {
            return path;
        }
    }
    PathBuf::from(home).join("İndirilenler")
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let discovery_port = env_port("UZAKEL_DISCOVERY_PORT", DEFAULT_DISCOVERY_PORT);
    let input_port = env_port("UZAKEL_INPUT_PORT", DEFAULT_INPUT_PORT);
    let file_port = env_port("UZAKEL_FILE_PORT", DEFAULT_FILE_PORT);
    let downloads = download_dir();

    info!(
        version = env!("CARGO_PKG_VERSION"),
        "uzakel-daemon starting"
    );
    info!(discovery_port, input_port, file_port, downloads = %downloads.display(), "configuration");

    let discovery_socket = bind_broadcast_udp(discovery_port)
        .await
        .context("binding the discovery/pairing UDP socket")?;
    let input_socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, input_port)))
        .await
        .context("binding the input UDP socket")?;
    let file_listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, file_port)))
        .await
        .context("binding the file-transfer TCP listener")?;

    let virtual_input = input_manager::VirtualInput::open()
        .context("setting up the virtual mouse/keyboard — is this user in the `uinput` group?")?;

    let discovery_task = tokio::spawn(discovery::run(discovery_socket));
    let input_task = tokio::spawn(input_manager::run(input_socket, virtual_input));
    let file_task = tokio::spawn(file_server::run(file_listener, downloads));

    tokio::select! {
        res = discovery_task => res.context("discovery task panicked")??,
        res = input_task => res.context("input task panicked")??,
        res = file_task => res.context("file-transfer task panicked")??,
        _ = tokio::signal::ctrl_c() => {
            info!("received Ctrl+C, shutting down");
        }
    }

    Ok(())
}

async fn bind_broadcast_udp(port: u16) -> Result<UdpSocket> {
    let socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))).await?;
    socket.set_broadcast(true).context("SO_BROADCAST")?;
    Ok(socket)
}
