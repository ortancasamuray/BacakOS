//! Publishes the daemon's current "how to pair with me" info to a small
//! JSON file, so something outside this process — the `bacak-compositor`
//! panel's QR button — can build a pairing QR code without needing any IPC
//! of its own. Rewritten every time a fresh PIN is generated (startup, and
//! after every successful pairing — see `discovery.rs`), so a reader always
//! sees the PIN that's actually live right now.
//!
//! Deliberately a plain file, not D-Bus/a socket: this daemon and the
//! compositor aren't guaranteed to start in any particular order, and a
//! file the panel just re-reads on each click needs no daemon-side server
//! at all — matching the `.cache/bacak`-style scratch-file pattern
//! `bacak-compositor`'s own `bluetooth.rs` already uses for its OBEX
//! helper scripts.

use std::io::Write;
use std::net::{IpAddr, UdpSocket};
use std::path::PathBuf;

use tracing::warn;

fn state_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home).join(".cache/uzakel/pairing.json")
}

/// The LAN-facing address a phone on the same Wi-Fi would reach this
/// machine on. `UdpSocket::connect` on a UDP socket does a route lookup
/// only — it never actually sends a packet — so this is a side-effect-free
/// way to ask the kernel "what source address would I use to reach the
/// internet," which in practice is the same NIC/address a LAN broadcast
/// goes out on.
fn local_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("1.1.1.1:80").ok()?;
    socket.local_addr().ok().map(|addr| addr.ip())
}

/// Escapes the handful of JSON-unsafe characters that could plausibly show
/// up in a daemon name (`UZAKEL_DAEMON_NAME` is user-set). Not a general
/// JSON encoder — this file only ever writes a fixed, known shape.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

/// Rewrites the pairing-state file with the currently-live PIN. Best-effort:
/// a write failure (e.g. no `HOME`, read-only filesystem) only means the
/// panel's QR button won't have anything to show — it doesn't affect
/// pairing itself, which still works via manual PIN entry.
pub fn publish(daemon_name: &str, discovery_port: u16, pin: u32) {
    let Some(ip) = local_ip() else {
        warn!("could not determine a LAN address to publish for QR pairing");
        return;
    };
    let path = state_path();
    let Some(dir) = path.parent() else { return };
    if let Err(err) = std::fs::create_dir_all(dir) {
        warn!(?err, "could not create the uzakel pairing-state cache dir");
        return;
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let json = format!(
        "{{\"daemon_name\":\"{}\",\"address\":\"{}\",\"discovery_port\":{},\"pin\":{:06},\"generated_at_unix\":{}}}\n",
        json_escape(daemon_name),
        ip,
        discovery_port,
        pin,
        now,
    );

    // Write to a temp file + rename so a concurrent reader (the panel) never
    // sees a half-written file.
    let tmp_path = path.with_extension("json.tmp");
    let result = std::fs::File::create(&tmp_path)
        .and_then(|mut f| f.write_all(json.as_bytes()))
        .and_then(|_| std::fs::rename(&tmp_path, &path));
    if let Err(err) = result {
        warn!(?err, "could not write the uzakel pairing-state file");
    }
}
