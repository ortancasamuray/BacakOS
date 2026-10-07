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
        // Debian's grim is built without libjpeg ("jpeg support disabled"),
        // so take raw PPM and encode the JPEG here.
        .args(["grim", "-t", "ppm", "-s", "0.5", "-"])
        .output()
        .await
        .map_err(|e| format!("grim çalıştırılamadı: {e}"))?;
    if !out.status.success() || out.stdout.is_empty() {
        return Err(format!("grim: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let jpeg = tokio::task::spawn_blocking(move || ppm_to_jpeg(&out.stdout))
        .await
        .map_err(|e| format!("JPEG kodlama: {e}"))??;
    Ok(Screenshot {
        zaman: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        oturum: (t.class != "greeter").then_some(t.user),
        jpeg_b64: base64::engine::general_purpose::STANDARD.encode(jpeg),
    })
}

/// Parses a binary PPM (P6, maxval 255) as written by grim.
fn parse_ppm(data: &[u8]) -> Result<(u16, u16, &[u8]), String> {
    let mut pos = 0;
    let mut field = || -> Result<&[u8], String> {
        loop {
            match data.get(pos) {
                Some(b'#') => while data.get(pos).is_some_and(|&b| b != b'\n') { pos += 1 },
                Some(b) if b.is_ascii_whitespace() => pos += 1,
                Some(_) => break,
                None => return Err("PPM başlığı eksik".into()),
            }
        }
        let start = pos;
        while data.get(pos).is_some_and(|b| !b.is_ascii_whitespace()) {
            pos += 1;
        }
        Ok(&data[start..pos])
    };
    if field()? != b"P6" {
        return Err("PPM değil (P6 bekleniyordu)".into());
    }
    let mut num = || -> Result<u16, String> {
        std::str::from_utf8(field()?).ok().and_then(|s| s.parse().ok()).ok_or_else(|| "PPM başlığı bozuk".to_string())
    };
    let (w, h, max) = (num()?, num()?, num()?);
    if max != 255 || w == 0 || h == 0 {
        return Err(format!("desteklenmeyen PPM: {w}x{h}, maxval {max}"));
    }
    // Exactly one whitespace byte separates the header from the pixels.
    let pixels = data.get(pos + 1..).unwrap_or_default();
    let need = w as usize * h as usize * 3;
    if pixels.len() < need {
        return Err(format!("PPM kısa: {} / {need} bayt", pixels.len()));
    }
    Ok((w, h, &pixels[..need]))
}

fn ppm_to_jpeg(ppm: &[u8]) -> Result<Vec<u8>, String> {
    let (w, h, rgb) = parse_ppm(ppm)?;
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 60)
        .encode(rgb, w, h, jpeg_encoder::ColorType::Rgb)
        .map_err(|e| format!("JPEG kodlama: {e}"))?;
    Ok(jpeg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ppm_header_with_comment() {
        let mut ppm = b"P6\n# grim\n2 1\n255\n".to_vec();
        ppm.extend_from_slice(&[255, 0, 0, 0, 0, 255]);
        let (w, h, px) = parse_ppm(&ppm).unwrap();
        assert_eq!((w, h, px.len()), (2, 1, 6));
        // The server rejects anything that doesn't start with the JPEG SOI marker.
        assert!(ppm_to_jpeg(&ppm).unwrap().starts_with(&[0xFF, 0xD8, 0xFF]));
    }

    #[test]
    fn ppm_rejects_bad_input() {
        assert!(parse_ppm(b"P5\n1 1\n255\n\0").is_err());
        assert!(parse_ppm(b"P6\n2 2\n255\n\0\0\0").is_err());
        assert!(parse_ppm(b"P6\n1 1\n65535\n\0\0\0\0\0\0").is_err());
        assert!(parse_ppm(b"P6\n").is_err());
    }
}
