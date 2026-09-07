// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! First-run localisation of the standard XDG user directories to Turkish
//! names.
//!
//! Runs once (guarded by a marker in the altay config dir) the first time
//! Altay launches after install. For each XDG category it:
//!   * renames an existing English (or variant) directory to the Turkish
//!     name if one is present, otherwise
//!   * creates the Turkish directory, and finally
//!   * rewrites `~/.config/user-dirs.dirs` so the sidebar categories
//!     (which resolve through the `dirs` crate) point at the Turkish paths.
//!
//! Existing Turkish directories are never overwritten — if the target
//! already exists we leave it and just record the mapping.

use std::path::{Path, PathBuf};

/// `(user-dirs.dirs key, Turkish directory name, aliases to migrate from)`.
/// Aliases are checked in order; the first existing one is renamed.
const DIRS: &[(&str, &str, &[&str])] = &[
    ("XDG_DESKTOP_DIR", "Masaüstü", &["Desktop", "Masaustu"]),
    ("XDG_DOCUMENTS_DIR", "Belgeler", &["Documents"]),
    (
        "XDG_DOWNLOAD_DIR",
        "İndirilenler",
        &["Downloads", "Download", "Indirilenler", "İndirmeler"],
    ),
    ("XDG_MUSIC_DIR", "Müzik", &["Music", "Müzikler", "Muzik"]),
    ("XDG_PICTURES_DIR", "Resimler", &["Pictures", "Görseller", "Gorseller"]),
    ("XDG_VIDEOS_DIR", "Videolar", &["Videos", "Video"]),
];

/// Ensure the Turkish user directories exist and `user-dirs.dirs` points at
/// them. Idempotent: does nothing once the marker file is present. Every
/// step is best-effort — a failure is logged and never aborts startup.
pub fn ensure_localized(home: &Path) {
    let Some(marker) = marker_path() else { return };
    if marker.exists() {
        return;
    }

    let mut mapping: Vec<(&str, PathBuf)> = Vec::new();
    for (key, name, aliases) in DIRS {
        let target = home.join(name);
        if !target.exists() {
            if !migrate_alias(home, aliases, &target, name) {
                match std::fs::create_dir_all(&target) {
                    Ok(()) => log::info!("userdirs: created {name}"),
                    Err(e) => {
                        log::warn!("userdirs: create {name} failed: {e}");
                        continue;
                    }
                }
            }
        }
        mapping.push((key, target));
    }

    match write_user_dirs(home, &mapping) {
        Ok(()) => {
            if let Some(parent) = marker.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&marker, b"1\n");
        }
        Err(e) => log::warn!("userdirs: user-dirs.dirs update failed: {e}"),
    }
}

/// Try to rename the first existing alias directory to `target`.
/// Returns true if a rename succeeded.
fn migrate_alias(home: &Path, aliases: &[&str], target: &Path, name: &str) -> bool {
    for alias in aliases {
        let src = home.join(alias);
        if src != target && src.is_dir() {
            match std::fs::rename(&src, target) {
                Ok(()) => {
                    log::info!("userdirs: renamed {alias} -> {name}");
                    return true;
                }
                Err(e) => log::warn!("userdirs: rename {alias} -> {name} failed: {e}"),
            }
        }
    }
    false
}

/// One-time marker: `~/.config/altay/.userdirs-localized`.
fn marker_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("altay").join(".userdirs-localized"))
}

/// Rewrite `~/.config/user-dirs.dirs`, updating the six localised keys in
/// place and preserving every other line/comment. Values are stored
/// relative to `$HOME` (the canonical xdg-user-dirs form). Atomic via a
/// sibling temp file + rename.
fn write_user_dirs(home: &Path, mapping: &[(&str, PathBuf)]) -> std::io::Result<()> {
    let cfg_dir = dirs::config_dir()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no config dir"))?;
    std::fs::create_dir_all(&cfg_dir)?;
    let path = cfg_dir.join("user-dirs.dirs");

    // Desired `KEY="value"` pairs, one per localised category.
    let desired: Vec<(&str, String)> = mapping
        .iter()
        .map(|(key, p)| {
            let value = match p.strip_prefix(home) {
                Ok(rel) => format!("\"$HOME/{}\"", rel.display()),
                Err(_) => format!("\"{}\"", p.display()),
            };
            (*key, value)
        })
        .collect();

    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut out = String::new();
    if existing.trim().is_empty() {
        out.push_str("# Localised by altay — do not remove.\n");
    }

    // Replace existing keys in place; keep unrelated lines verbatim.
    for line in existing.lines() {
        let key = line.trim_start().split('=').next().unwrap_or("").trim();
        if let Some((k, v)) = desired.iter().find(|(k, _)| *k == key) {
            out.push_str(k);
            out.push('=');
            out.push_str(v);
            out.push('\n');
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }

    // Append any keys not already present.
    for (k, v) in &desired {
        let present = existing
            .lines()
            .any(|l| l.trim_start().split('=').next().map(str::trim) == Some(*k));
        if !present {
            out.push_str(k);
            out.push('=');
            out.push_str(v);
            out.push('\n');
        }
    }

    let tmp = path.with_extension("dirs.tmp");
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh home with an English `Documents` and an existing Turkish
    /// `Resimler`: the former is renamed, the latter is kept, the rest are
    /// created, and user-dirs.dirs lists all six localised keys.
    #[test]
    fn migrate_create_and_keep() {
        let base = std::env::temp_dir().join(format!("altay-ud-{}", std::process::id()));
        let home = base.join("home");
        let cfg = base.join("config");
        std::fs::create_dir_all(home.join("Documents")).unwrap();
        std::fs::write(home.join("Documents/report.txt"), b"x").unwrap();
        std::fs::create_dir_all(home.join("Resimler")).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &cfg);

        ensure_localized(&home);

        // English → Turkish rename carried the file across.
        assert!(!home.join("Documents").exists());
        assert!(home.join("Belgeler/report.txt").exists());
        // Pre-existing Turkish dir untouched (not duplicated/renamed).
        assert!(home.join("Resimler").is_dir());
        // Missing ones created.
        for d in ["Masaüstü", "İndirilenler", "Müzik", "Videolar"] {
            assert!(home.join(d).is_dir(), "{d} not created");
        }
        // user-dirs.dirs points documents at the Turkish path, $HOME-relative.
        let ud = std::fs::read_to_string(cfg.join("user-dirs.dirs")).unwrap();
        assert!(ud.contains("XDG_DOCUMENTS_DIR=\"$HOME/Belgeler\""), "got:\n{ud}");
        assert!(ud.contains("XDG_PICTURES_DIR=\"$HOME/Resimler\""), "got:\n{ud}");

        // Second run is a no-op (marker present): re-creating Documents must
        // NOT trigger another migration.
        std::fs::create_dir_all(home.join("Documents")).unwrap();
        ensure_localized(&home);
        assert!(home.join("Documents").is_dir(), "marker should have skipped run");

        std::fs::remove_dir_all(&base).ok();
        std::env::remove_var("XDG_CONFIG_HOME");
    }
}
