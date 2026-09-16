//! Discovery of desktop sessions from freedesktop `.desktop` entries.
//!
//! Wayland sessions live in `/usr/share/wayland-sessions`, X11 sessions in
//! `/usr/share/xsessions`. Each `.desktop` file is a `[Desktop Entry]` group;
//! we extract the keys the launcher actually needs and ignore the rest.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionType {
    Wayland,
    X11,
}

/// A selectable session in the greeter's session menu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// Stable id = the `.desktop` basename without extension (e.g. `bacak`).
    pub id: String,
    /// `Name=` — what the user sees.
    pub name: String,
    /// `Comment=` — optional one-line description.
    pub comment: Option<String>,
    /// `Exec=` — command handed to the launcher.
    pub exec: String,
    /// `DesktopNames=` -> `XDG_CURRENT_DESKTOP` (colon-separated upstream).
    pub desktop_names: Option<String>,
    pub kind: SessionType,
    pub path: PathBuf,
}

/// Parse one `.desktop` file's `[Desktop Entry]` group.
///
/// Returns `Ok(None)` when the entry is present but explicitly hidden
/// (`Hidden=true` / `NoDisplay=true`), so callers can simply skip it.
pub fn parse_desktop_entry(path: &Path, kind: SessionType) -> crate::Result<Option<Session>> {
    let text = std::fs::read_to_string(path).map_err(|e| crate::Error::io(path, e))?;
    parse_desktop_text(&text, path, kind)
}

fn parse_desktop_text(
    text: &str,
    path: &Path,
    kind: SessionType,
) -> crate::Result<Option<Session>> {
    let mut in_group = false;
    let mut name = None;
    let mut comment = None;
    let mut exec = None;
    let mut desktop_names = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            // Only the main group matters; later groups (actions) are ignored.
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        // Localised keys like `Name[tr]` are ignored in favour of the C key.
        match key.trim() {
            "Name" => name = Some(value.trim().to_string()),
            "Comment" => comment = Some(value.trim().to_string()),
            "Exec" => exec = Some(value.trim().to_string()),
            "DesktopNames" => desktop_names = Some(value.trim().to_string()),
            "Hidden" | "NoDisplay" if value.trim().eq_ignore_ascii_case("true") => {
                return Ok(None);
            }
            _ => {}
        }
    }

    let id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| crate::Error::malformed("session", "no file stem"))?
        .to_string();

    let exec = exec
        .filter(|e| !e.is_empty())
        .ok_or_else(|| crate::Error::malformed("session", format!("{id}: missing Exec")))?;

    Ok(Some(Session {
        name: name.unwrap_or_else(|| id.clone()),
        id,
        comment: comment.filter(|c| !c.is_empty()),
        exec,
        desktop_names,
        kind,
        path: path.to_path_buf(),
    }))
}

/// Scan all configured directories and return the deduplicated session list.
///
/// If the same id appears in multiple directories, the first one wins (mirrors
/// XDG precedence: earlier directories override later ones). Malformed entries
/// are logged and skipped rather than aborting discovery.
pub fn discover(wayland_dirs: &[PathBuf], xsession_dirs: &[PathBuf]) -> Vec<Session> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();

    let scans = wayland_dirs
        .iter()
        .map(|d| (d, SessionType::Wayland))
        .chain(xsession_dirs.iter().map(|d| (d, SessionType::X11)));

    for (dir, kind) in scans {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => continue, // directory may not exist; that's fine
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            match parse_desktop_entry(&path, kind) {
                Ok(Some(session)) => {
                    if seen.insert(session.id.clone()) {
                        out.push(session);
                    }
                }
                Ok(None) => {} // hidden
                Err(e) => log::warn!("skipping {}: {e}", path.display()),
            }
        }
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_wayland_session() {
        let text = "\
[Desktop Entry]
Name=Bacak Desktop
Comment=The Bacak Wayland session
Exec=bacak-session
DesktopNames=Bacak
Type=Application
";
        let s = parse_desktop_text(text, Path::new("/x/bacak.desktop"), SessionType::Wayland)
            .unwrap()
            .unwrap();
        assert_eq!(s.id, "bacak");
        assert_eq!(s.name, "Bacak Desktop");
        assert_eq!(s.exec, "bacak-session");
        assert_eq!(s.desktop_names.as_deref(), Some("Bacak"));
        assert_eq!(s.kind, SessionType::Wayland);
    }

    #[test]
    fn hidden_entries_are_skipped() {
        let text = "[Desktop Entry]\nName=X\nExec=x\nNoDisplay=true\n";
        let r = parse_desktop_text(text, Path::new("/x/x.desktop"), SessionType::X11).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn missing_exec_is_an_error() {
        let text = "[Desktop Entry]\nName=No Exec\n";
        let r = parse_desktop_text(text, Path::new("/x/n.desktop"), SessionType::Wayland);
        assert!(r.is_err());
    }

    #[test]
    fn action_groups_are_ignored() {
        let text = "\
[Desktop Entry]
Name=Main
Exec=main
[Desktop Action new]
Exec=should-be-ignored
";
        let s = parse_desktop_text(text, Path::new("/x/m.desktop"), SessionType::Wayland)
            .unwrap()
            .unwrap();
        assert_eq!(s.exec, "main");
    }
}
