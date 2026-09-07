//! LAN discovery and PIN pairing (UDP broadcast).
//!
//! This is the fallback path from ARCHITECTURE.md §2.1 — a UDP broadcast
//! responder, not real mDNS/DNS-SD. Real mDNS (multicast on 5353, service
//! records) is listed as an open question there; a plain broadcast on a
//! fixed port gets zero-config LAN discovery working without pulling in an
//! mDNS crate, at the cost of not working across routed subnets (fine for
//! "same Wi-Fi network," which is the only case this needs to cover today).
//!
//! **Security note:** a successful PIN match here marks the client's IP
//! trusted in the shared [`crate::trust::TrustStore`], which
//! `input_manager.rs` and `file_server.rs` check before acting on anything.
//! That's IP-based, not cryptographic — see `trust.rs`'s doc comment for
//! exactly what that does and doesn't guarantee. Picking a real TLS/PSK
//! approach is still open, see ARCHITECTURE.md §5.

use std::net::SocketAddr;
use std::process::Command;

use anyhow::{Context, Result};
use rand::Rng;
use tokio::net::UdpSocket;
use tracing::{info, warn};

use crate::protocol::{DiscoverResponse, Header, Opcode, PairRequest, HEADER_LEN};
use crate::trust::TrustStore;

const DAEMON_NAME_ENV: &str = "UZAKEL_DAEMON_NAME";

/// Generates a fresh 6-digit PIN and shows it via a desktop notification —
/// the human-in-the-loop step that turns "any phone on the LAN" into "a
/// phone the user actually approved this once for."
fn new_pin() -> u32 {
    let pin = rand::thread_rng().gen_range(0..1_000_000);
    let msg = format!("Eşleştirme PIN'i: {pin:06}");
    if let Err(err) = Command::new("notify-send").arg("Uzakel").arg(&msg).status() {
        warn!(
            ?err,
            "could not show the pairing PIN as a desktop notification"
        );
    }
    info!(
        pin = format!("{pin:06}"),
        "pairing PIN generated (also shown as a desktop notification)"
    );
    pin
}

pub async fn run(socket: UdpSocket, trust: TrustStore) -> Result<()> {
    let daemon_name = std::env::var(DAEMON_NAME_ENV)
        .unwrap_or_else(|_| hostname().unwrap_or_else(|| "BacakOS".to_string()));
    let mut current_pin = new_pin();
    let mut buf = [0u8; 512];

    loop {
        let (len, from) = match socket.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(err) => {
                warn!(?err, "UDP recv error on discovery channel, continuing");
                continue;
            }
        };

        if let Err(err) = handle(&socket, &buf[..len], from, &daemon_name, &mut current_pin, &trust).await {
            warn!(?err, %from, "error handling a discovery/pairing packet");
        }
    }
}

async fn handle(
    socket: &UdpSocket,
    buf: &[u8],
    from: SocketAddr,
    daemon_name: &str,
    current_pin: &mut u32,
    trust: &TrustStore,
) -> Result<()> {
    if buf.len() < HEADER_LEN {
        return Ok(()); // too short to even be a header — ignore, not an error
    }
    let header = Header::decode(buf)?;

    match header.opcode {
        Opcode::DiscoverRequest => {
            let response = DiscoverResponse {
                daemon_name: daemon_name.to_string(),
                daemon_version: parse_version(env!("CARGO_PKG_VERSION")),
                accepting_new_pairs: true,
            };
            socket
                .send_to(&response.encode(), from)
                .await
                .context("sending DISCOVER_RESPONSE")?;
        }
        Opcode::PairRequest => {
            let payload = &buf[HEADER_LEN..];
            let req = PairRequest::decode_payload(payload)?;
            let accepted = req.pin == *current_pin;
            if accepted {
                trust.trust(from.ip());
                info!(%from, "client paired successfully");
                // A fresh PIN for the *next* pairing attempt, so a captured
                // PIN can't be replayed once it's been used.
                *current_pin = new_pin();
            } else {
                warn!(%from, "pairing attempt with a wrong PIN");
            }
            socket
                .send_to(&crate::protocol::encode_pair_response(accepted), from)
                .await
                .context("sending PAIR_RESPONSE")?;
        }
        other => {
            warn!(?other, %from, "unexpected opcode on the discovery channel");
        }
    }
    Ok(())
}

fn hostname() -> Option<String> {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
}

fn parse_version(v: &str) -> (u8, u8, u8) {
    let mut parts = v.split('.').map(|p| p.parse::<u8>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}
