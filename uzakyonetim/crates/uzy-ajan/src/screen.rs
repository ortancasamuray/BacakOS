// SPDX-License-Identifier: GPL-3.0-or-later
//! Screenshots of whatever is on seat0 — a user session or the Turan login
//! screen — via `grim` (bacak-compositor implements `zwlr_screencopy_v1`).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use tokio::process::Command;
use uzy_proto::Screenshot;

/// While this file exists, bacak-compositor draws the "İzleniyor" badge.
pub const INDICATOR: &str = "/run/uzakyonetim/izleniyor";

pub fn set_indicator(on: bool) {
    let p = Path::new(INDICATOR);
    if on {
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        let _ = std::fs::write(p, b"1\n");
    } else {
        let _ = std::fs::remove_file(p);
    }
}

struct Target {
    user: String,
    uid: u32,
    class: String,
}

async fn loginctl(args: &[&str]) -> Option<String> {
    let out = Command::new("loginctl").args(args).output().await.ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The session currently shown on seat0.
async fn active_target() -> Option<Target> {
    let session = loginctl(&["show-seat", "seat0", "-p", "ActiveSession", "--value"]).await?;
    if session.is_empty() {
        return None;
    }
    let props = loginctl(&["show-session", &session, "-p", "Name", "-p", "User", "-p", "Class"]).await?;
    let get = |k: &str| props.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).map(str::to_string);
    Some(Target { user: get("Name")?, uid: get("User")?.parse().ok()?, class: get("Class").unwrap_or_default() })
}

fn wayland_socket(uid: u32) -> Option<PathBuf> {
    let dir = PathBuf::from(format!("/run/user/{uid}"));
    let mut socks: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            n.starts_with("wayland-") && !n.ends_with(".lock")
        })
        .collect();
    socks.sort();
    socks.into_iter().next()
}

pub async fn capture() -> Result<Screenshot, String> {
    let t = active_target().await.ok_or("seat0'da etkin oturum yok")?;
    let sock = wayland_socket(t.uid).ok_or("Wayland soketi bulunamadı")?;
    let name = sock.file_name().unwrap().to_string_lossy().into_owned();
    // grim runs as the session's own user: root can't (and shouldn't) speak
    // to another user's compositor directly.
    let out = Command::new("runuser")
        .args(["-u", &t.user, "--", "env"])
        .arg(format!("XDG_RUNTIME_DIR=/run/user/{}", t.uid))
        .arg(format!("WAYLAND_DISPLAY={name}"))
        .args(["grim", "-t", "jpeg", "-q", "60", "-s", "0.5", "-"])
        .output()
        .await
        .map_err(|e| format!("grim çalıştırılamadı: {e}"))?;
    if !out.status.success() || out.stdout.is_empty() {
        return Err(format!("grim: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(Screenshot {
        zaman: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        oturum: (t.class != "greeter").then_some(t.user),
        jpeg_b64: base64::engine::general_purpose::STANDARD.encode(out.stdout),
    })
}
