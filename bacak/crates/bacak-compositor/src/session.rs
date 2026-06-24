//! Workspace / window-layout persistence.
//!
//! A JSON snapshot at `$XDG_CONFIG_HOME/bacak/session.json` (falling
//! back to `~/.config/bacak/session.json`). Pure serde/std — no
//! Smithay — so it's testable in isolation and reusable by tooling.
//!
//! **Scope (Phase 1).** What is *restored* at boot is the primary
//! output's workspace topology — how many workspaces existed and which
//! one was active — so the desktop comes back the way the user left
//! it. The full window list (app/title/geometry) is *captured* in the
//! snapshot too, but re-placing live clients onto it requires matching
//! reconnecting Wayland surfaces back to saved entries (no stable
//! cross-session window id); that heuristic is Phase 2. The data is
//! persisted now so Phase 2 / external tools have it.
//!
//! Any failure (missing file, unreadable, malformed) degrades to an
//! empty [`SessionSnapshot`] — a bad session file never blocks boot.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The primary output's workspace setup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OutputSession {
    /// How many workspaces existed on the primary output.
    pub workspace_count: usize,
    /// Zero-based index (in id order) of the active workspace.
    pub active_index: usize,
}

/// One window's placement. `workspace_index` is the zero-based slot
/// (id order) of the window's workspace on its output, so a restored
/// window returns to the right workspace, not just the right
/// coordinates. `#[serde(default)]` keeps pre-Phase-3 files loadable
/// (they restore onto workspace 0).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowRec {
    pub app: String,
    pub title: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    #[serde(default)]
    pub workspace_index: usize,
}

/// Everything persisted between sessions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SessionSnapshot {
    pub primary: OutputSession,
    pub windows: Vec<WindowRec>,
}

impl SessionSnapshot {
    fn path() -> Option<PathBuf> {
        if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
            if !dir.is_empty() {
                return Some(PathBuf::from(dir).join("bacak/session.json"));
            }
        }
        let home = std::env::var("HOME").ok()?;
        Some(PathBuf::from(home).join(".config/bacak/session.json"))
    }

    /// The resolved session-file path (public for tooling/tests).
    pub fn session_path() -> Option<PathBuf> {
        Self::path()
    }

    /// Load the snapshot, or an empty default on any failure.
    pub fn load() -> Self {
        let Some(p) = Self::path() else { return Self::default() };
        let Ok(text) = std::fs::read_to_string(&p) else {
            return Self::default(); // absent first-run is normal
        };
        match serde_json::from_str(&text) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(?p, ?e, "malformed session.json; ignoring");
                Self::default()
            }
        }
    }

    /// Atomically persist: write a sibling temp file then rename over
    /// the target (same-dir rename is atomic on POSIX). Returns the
    /// error rather than panicking — the caller logs and carries on.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no session path")
        })?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self)
            .expect("session always serialises");
        std::fs::write(&tmp, json + "\n")?;
        std::fs::rename(&tmp, &path)
    }

    /// Save to a specific path (tests / explicit export).
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self)
            .expect("session always serialises");
        std::fs::write(path, json + "\n")
    }
}

/// Consume the best saved placement for a `(app, title)` window from
/// `pool`. Prefers an exact `app && title` match (so a browser's
/// "GitHub" window returns to the GitHub slot, not the Mail one);
/// falls back to the first record with the same `app`. Consuming
/// (not peeking) means each saved window is restored exactly once and
/// extra same-app windows fall back to the default cascade.
pub fn take_placement(
    pool: &mut Vec<WindowRec>,
    app: &str,
    title: &str,
) -> Option<WindowRec> {
    let idx = pool
        .iter()
        .position(|r| r.app == app && r.title == title)
        .or_else(|| pool.iter().position(|r| r.app == app))?;
    Some(pool.remove(idx))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_empty() {
        let d = SessionSnapshot::default();
        assert_eq!(d.primary.workspace_count, 0);
        assert!(d.windows.is_empty());
    }

    #[test]
    fn roundtrips_through_json() {
        let s = SessionSnapshot {
            primary: OutputSession { workspace_count: 4, active_index: 2 },
            windows: vec![WindowRec {
                app: "firefox".into(),
                title: "Mozilla".into(),
                x: 10.0,
                y: 20.0,
                w: 800.0,
                h: 600.0,
                workspace_index: 3,
            }],
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: SessionSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn partial_and_malformed_degrade() {
        // Missing fields fill from defaults (container `#[serde(default)]`).
        let s: SessionSnapshot = serde_json::from_str("{}").unwrap();
        assert_eq!(s, SessionSnapshot::default());
        assert!(serde_json::from_str::<SessionSnapshot>("nope").is_err());
    }

    fn rec(app: &str, title: &str, x: f32) -> WindowRec {
        WindowRec {
            app: app.into(),
            title: title.into(),
            x,
            y: 0.0,
            w: 100.0,
            h: 100.0,
            workspace_index: 0,
        }
    }

    #[test]
    fn take_placement_falls_back_to_app_when_no_title_match() {
        let mut pool = vec![rec("firefox", "A", 1.0), rec("term", "", 2.0), rec("firefox", "B", 3.0)];
        // No title match → first by app.
        let a = take_placement(&mut pool, "firefox", "Z").unwrap();
        assert_eq!(a.x, 1.0);
        let b = take_placement(&mut pool, "firefox", "Z").unwrap();
        assert_eq!(b.x, 3.0);
        assert!(take_placement(&mut pool, "firefox", "Z").is_none()); // exhausted
        assert!(take_placement(&mut pool, "nope", "").is_none());
        assert_eq!(pool.len(), 1); // term untouched
    }

    #[test]
    fn take_placement_prefers_exact_title() {
        let mut pool = vec![
            rec("firefox", "GitHub", 1.0),
            rec("firefox", "Mail", 2.0),
        ];
        // Exact (app, title) wins over the earlier same-app record.
        let m = take_placement(&mut pool, "firefox", "Mail").unwrap();
        assert_eq!(m.x, 2.0);
        // The GitHub one is still there.
        let g = take_placement(&mut pool, "firefox", "GitHub").unwrap();
        assert_eq!(g.x, 1.0);
    }

    #[test]
    fn window_rec_workspace_index_defaults_for_old_files() {
        // Pre-Phase-3 records lack `workspace_index` → defaults to 0.
        let r: WindowRec = serde_json::from_str(
            r#"{"app":"x","title":"","x":0,"y":0,"w":1,"h":1}"#,
        )
        .unwrap();
        assert_eq!(r.workspace_index, 0);
    }

    #[test]
    fn save_to_then_load_via_path_roundtrips() {
        let dir = std::env::temp_dir().join("bacak-session-test");
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("session.json");
        let s = SessionSnapshot {
            primary: OutputSession { workspace_count: 3, active_index: 1 },
            windows: vec![],
        };
        s.save_to(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        let back: SessionSnapshot = serde_json::from_str(&text).unwrap();
        assert_eq!(s, back);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
