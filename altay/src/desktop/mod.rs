// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Desktop integration: launching files in their default application and
//! revealing the active D-Bus portal environment.
//!
//! Files are opened with `xdg-open`, which routes through
//! `xdg-desktop-portal` on Wayland — so Altay never needs to know MIME
//! handlers itself, and sandboxed/Flatpak deployments work unchanged. Every
//! path is sandbox-validated before launch.

use std::path::Path;

use crate::security::{AccessDenied, Sandbox};

#[derive(Debug, thiserror::Error)]
pub enum DesktopError {
    #[error(transparent)]
    Denied(#[from] AccessDenied),
    #[error("could not launch handler: {0}")]
    Launch(String),
}

/// Open a path in the user's default application via `xdg-open`. Returns once
/// the helper has been spawned (it detaches and runs independently).
pub fn open(sandbox: &Sandbox, path: impl AsRef<Path>) -> Result<(), DesktopError> {
    let safe = sandbox.resolve(path)?;
    spawn("xdg-open", safe.as_path())
}

/// Open the folder containing a path (used for "reveal" actions).
pub fn open_folder(sandbox: &Sandbox, path: impl AsRef<Path>) -> Result<(), DesktopError> {
    let safe = sandbox.resolve(path)?;
    let dir = safe.as_path().parent().unwrap_or(safe.as_path());
    spawn("xdg-open", dir)
}

fn spawn(program: &str, arg: &Path) -> Result<(), DesktopError> {
    use std::process::{Command, Stdio};
    Command::new(program)
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| DesktopError::Launch(format!("{program}: {e}")))
}

// ---- Cross-application file exchange via the system clipboard ---------------
//
// Slint/winit do not expose OS-level drag-and-drop, so the portable way to move
// files to/from other apps (Nautilus, Dolphin, Files, Thunar, …) is the
// freedesktop clipboard convention: the `x-special/gnome-copied-files` target,
// whose payload is `copy`/`cut` followed by `file://` URIs. We drive it through
// `wl-copy`/`wl-paste` (Wayland), falling back to `xclip` (X11).

/// Publish the given paths to the system clipboard so other apps can paste them.
/// `cut` marks a move rather than a copy.
pub fn clipboard_export(paths: &[std::path::PathBuf], cut: bool) -> Result<(), DesktopError> {
    let payload = build_gnome_payload(paths, cut);
    if run_with_stdin("wl-copy", &["--type", "x-special/gnome-copied-files"], &payload) {
        return Ok(());
    }
    if run_with_stdin(
        "xclip",
        &["-selection", "clipboard", "-t", "x-special/gnome-copied-files"],
        &payload,
    ) {
        return Ok(());
    }
    Err(DesktopError::Launch(
        "no clipboard helper found (install wl-clipboard or xclip)".into(),
    ))
}

/// Read file paths another app placed on the system clipboard (copy or cut).
pub fn clipboard_import() -> Option<(Vec<std::path::PathBuf>, bool)> {
    let raw = read_clipboard("x-special/gnome-copied-files")
        .or_else(|| read_clipboard("text/uri-list"))?;
    let (paths, cut) = parse_gnome_payload(&raw);
    if paths.is_empty() {
        None
    } else {
        Some((paths, cut))
    }
}

fn build_gnome_payload(paths: &[std::path::PathBuf], cut: bool) -> String {
    let mut out = String::from(if cut { "cut" } else { "copy" });
    for p in paths {
        out.push('\n');
        out.push_str(&to_file_uri(p));
    }
    out
}

/// Parse a `gnome-copied-files` (or plain `uri-list`) payload into paths + cut.
fn parse_gnome_payload(text: &str) -> (Vec<std::path::PathBuf>, bool) {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let mut cut = false;
    let mut first = lines.clone();
    // A leading "copy"/"cut" header is gnome-copied-files; uri-list has none.
    let has_header = matches!(first.next(), Some("copy") | Some("cut"));
    if has_header {
        cut = text.lines().next() == Some("cut");
        lines.next(); // consume the header
    }
    let paths = lines.filter_map(from_file_uri).collect();
    (paths, cut)
}

/// `/home/u/a b.txt` → `file:///home/u/a%20b.txt`.
fn to_file_uri(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    let mut out = String::from("file://");
    for b in s.as_bytes() {
        let c = *b;
        if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

/// `file:///home/u/a%20b.txt` → `/home/u/a b.txt` (only `file://` URIs).
fn from_file_uri(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // Drop an optional host component before the path.
    let path_part = rest.strip_prefix('/').map(|_| rest).unwrap_or(rest);
    let bytes = path_part.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16)?;
            let lo = (bytes[i + 2] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    use std::os::unix::ffi::OsStringExt;
    Some(std::path::PathBuf::from(std::ffi::OsString::from_vec(out)))
}

fn run_with_stdin(program: &str, args: &[&str], input: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let Ok(mut child) = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
        // stdin dropped here → EOF, helper can finish.
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

fn read_clipboard(mime: &str) -> Option<String> {
    use std::process::Command;
    for (prog, args) in [
        ("wl-paste", vec!["--no-newline", "--type", mime]),
        ("xclip", vec!["-selection", "clipboard", "-o", "-t", mime]),
    ] {
        if let Ok(out) = Command::new(prog).args(&args).output() {
            if out.status.success() && !out.stdout.is_empty() {
                return Some(String::from_utf8_lossy(&out.stdout).into_owned());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AllowedRoot;

    #[test]
    fn file_uri_roundtrip_with_spaces() {
        let p = std::path::PathBuf::from("/home/u/My Photos/öü #1.jpg");
        let uri = to_file_uri(&p);
        assert!(uri.starts_with("file:///home/u/My%20Photos/"));
        assert_eq!(from_file_uri(&uri), Some(p));
    }

    #[test]
    fn parses_gnome_copied_files_cut() {
        let payload = "cut\nfile:///home/u/a.txt\nfile:///home/u/b%20c.txt";
        let (paths, cut) = parse_gnome_payload(payload);
        assert!(cut);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[1], std::path::PathBuf::from("/home/u/b c.txt"));
    }

    #[test]
    fn parses_plain_uri_list_as_copy() {
        let (paths, cut) = parse_gnome_payload("file:///tmp/x\nfile:///tmp/y");
        assert!(!cut);
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn open_outside_sandbox_is_denied() {
        // A home-only sandbox must refuse to launch /etc/passwd.
        let home = std::env::temp_dir().join(format!("altay-desk-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let sb = Sandbox::with_roots(vec![AllowedRoot::home(std::fs::canonicalize(&home).unwrap())]);
        assert!(matches!(open(&sb, "/etc/passwd"), Err(DesktopError::Denied(_))));
        let _ = std::fs::remove_dir_all(&home);
    }
}
