//! Desktop-wide appearance (`~/.config/bacak/gorunum.json`).
//!
//! The compositor is the writer: its dark-mode toggles (Control Center,
//! Desktop Settings, IPC) save this file and announce the change as an
//! [`crate::Olay::Gorunum`] event. Applications read it at start-up — so they
//! get the right theme even with no compositor running — and follow changes
//! through the event or by polling the file.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gorunum {
    /// Dark mode. BacakOS ships dark, so a missing file or key means `true`.
    #[serde(default = "yes")]
    pub koyu: bool,
}

fn yes() -> bool {
    true
}

impl Default for Gorunum {
    fn default() -> Self {
        Gorunum { koyu: true }
    }
}

/// `~/.config/bacak/gorunum.json`.
pub fn path() -> Option<PathBuf> {
    crate::config_dir().map(|d| d.join("gorunum.json"))
}

impl Gorunum {
    /// The saved appearance, or the default when the file is missing or
    /// unreadable (a broken file must never stop an application starting).
    pub fn load() -> Gorunum {
        path().map(|p| Self::load_from(&p)).unwrap_or_default()
    }

    pub fn load_from(path: &std::path::Path) -> Gorunum {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// Writes atomically (temp file + rename) so a reader never sees half a file.
    pub fn save(&self) -> std::io::Result<()> {
        let p = path().ok_or_else(|| std::io::Error::other("HOME/XDG_CONFIG_HOME tanımsız"))?;
        self.save_to(&p)
    }

    pub fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_broken_file_is_dark_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("gorunum.json");
        assert_eq!(Gorunum::load_from(&p), Gorunum { koyu: true });
        std::fs::write(&p, b"{bozuk").unwrap();
        assert_eq!(Gorunum::load_from(&p), Gorunum { koyu: true });
        std::fs::write(&p, b"{}").unwrap();
        assert_eq!(Gorunum::load_from(&p), Gorunum { koyu: true });
    }

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("alt/gorunum.json");
        Gorunum { koyu: false }.save_to(&p).unwrap();
        assert_eq!(Gorunum::load_from(&p), Gorunum { koyu: false });
    }
}
