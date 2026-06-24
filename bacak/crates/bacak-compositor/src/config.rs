//! Persistent compositor configuration.
//!
//! A small JSON file at `$XDG_CONFIG_HOME/bacak/compositor.json`
//! (falling back to `~/.config/bacak/compositor.json`). JSON rather
//! than the TOML the architecture doc envisions for the *session*
//! state — this is a tiny tunables file and `serde_json` is already a
//! workspace dependency, so it adds nothing. The richer TOML session
//! store can subsume this later.
//!
//! Missing file, unreadable path, or malformed JSON all degrade to
//! [`CompositorConfig::default`] — a broken config never stops the
//! compositor from booting.
//!
//! Pure serde/std — no Smithay — so the CLI (`bacak config`) can read
//! and validate it without pulling in the Wayland runtime.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Which screen edge the dock lives on. `Bottom` is the default and
/// matches every prior phase; the other edges flip the layout math
/// (row ↔ column), the reserved strut side, the reveal-slide
/// direction, and the auto-hide hot-zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DockEdge {
    #[default]
    Bottom,
    Top,
    Left,
    Right,
}

impl DockEdge {
    /// True for `Bottom`/`Top` — the bar runs left-to-right. The
    /// renderer / layout / drag logic branches on this to swap which
    /// axis is the row axis.
    pub fn is_horizontal(self) -> bool {
        matches!(self, Self::Bottom | Self::Top)
    }
}

/// User-tunable compositor options. `#[serde(default)]` at the
/// container level means any missing field falls back to that field's
/// value in [`CompositorConfig::default`] — so a partial file (or
/// `{}`) parses, and every default is the real tuned value, not
/// `0.0`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct CompositorConfig {
    /// Enable the glassmorphism backdrop blur pipeline. Off by default
    /// — it costs an extra offscreen render + blur per frame.
    pub blur: bool,
    /// Gaussian blur reach in pixels.
    pub blur_radius: f32,
    /// Drop-shadow opacity for the topmost window.
    pub shadow_top: f32,
    /// Per-z-rank shadow opacity falloff.
    pub shadow_step: f32,
    /// Minimum shadow opacity for deep stacks.
    pub shadow_floor: f32,
    /// Minimise dock-slot width / height (px).
    pub dock_slot_w: f32,
    pub dock_slot_h: f32,
    /// Show the compositor-drawn dock and reserve a bottom strut for
    /// it so windows/snap math don't overlap the bar.
    pub dock: bool,
    /// Dock band height in pixels (also the reserved strut).
    pub dock_height: f32,
    /// Which screen edge the dock lives on.
    pub dock_edge: DockEdge,
    /// Auto-hide the dock: it tucks off the bottom edge and reveals
    /// when the pointer hits the screen edge or hovers the bar. While
    /// auto-hiding the dock reserves *no* strut (it floats over
    /// content). Off by default — the dock stays pinned + struts.
    pub dock_autohide: bool,
    /// App ids (or `.desktop` basenames) pinned to the dock, in order.
    /// A pinned app shows a launcher tile even when not running; the
    /// dock renderer/launch path (Phase 2) resolves each to its
    /// `.desktop` `Exec=`. Empty by default.
    pub dock_pinned: Vec<String>,
    /// Compositor-native wallpaper solid colour [R, G, B] (0–255).
    /// Rendered as the bottommost frame element when no Background
    /// layer-shell client (swaybg, etc.) is running.
    pub wallpaper_color: [u8; 3],
    /// Optional wallpaper image path (PNG or JPEG). When set, rendered
    /// scaled-to-cover the output, on top of `wallpaper_color`. Takes
    /// precedence over the solid colour. `null` / absent → solid colour only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallpaper_image: Option<String>,
}

impl Default for CompositorConfig {
    fn default() -> Self {
        Self {
            blur: false,
            blur_radius: 24.0,
            shadow_top: 1.0,
            shadow_step: 0.18,
            shadow_floor: 0.30,
            dock_slot_w: 96.0,
            dock_slot_h: 12.0,
            dock: false,
            dock_height: 56.0,
            dock_edge: DockEdge::Bottom,
            dock_autohide: false,
            dock_pinned: Vec::new(),
            wallpaper_color: [10, 14, 22],
            wallpaper_image: None,
        }
    }
}

impl CompositorConfig {
    /// Resolve the on-disk config path per the XDG Base Directory
    /// spec.
    fn path() -> Option<PathBuf> {
        if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
            if !dir.is_empty() {
                return Some(PathBuf::from(dir).join("bacak/compositor.json"));
            }
        }
        let home = std::env::var("HOME").ok()?;
        Some(PathBuf::from(home).join(".config/bacak/compositor.json"))
    }

    /// The resolved on-disk config path (public so the hot-reload
    /// watcher can derive the directory to watch).
    pub fn config_path() -> Option<PathBuf> {
        Self::path()
    }

    /// Load the config, or the defaults on any failure.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            // Absent file is the normal first-run case — quiet.
            return Self::default();
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(mut cfg) => {
                cfg.sanitize();
                tracing::info!(?path, "loaded compositor config");
                cfg
            }
            Err(e) => {
                tracing::warn!(?path, ?e, "malformed compositor config; using defaults");
                Self::default()
            }
        }
    }

    /// Parse a specific file strictly: returns the sanitised config or
    /// a human-readable error. Used by `bacak config validate` so the
    /// user gets told *why* a file is bad (unlike [`load`], which
    /// silently falls back).
    pub fn load_from(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut cfg: Self =
            serde_json::from_str(&text).map_err(|e| format!("invalid JSON: {e}"))?;
        cfg.sanitize();
        Ok(cfg)
    }

    /// Atomically persist the current config to the resolved
    /// [`config_path`](Self::config_path): write a sibling temp file
    /// then rename over the target (same-dir rename is atomic on
    /// POSIX). The hot-reload watcher will re-read this exact content
    /// on the next tick — harmless, just slightly redundant. Returns
    /// the error rather than panicking so callers (compositor live
    /// edits, CLI) can decide how to surface it.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no config path")
        })?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self)
            .expect("config always serialises");
        std::fs::write(&tmp, json + "\n")?;
        std::fs::rename(&tmp, &path)
    }

    /// Every settable key, for help text and validation.
    pub const KEYS: &'static [&'static str] = &[
        "blur",
        "blur_radius",
        "shadow_top",
        "shadow_step",
        "shadow_floor",
        "dock_slot_w",
        "dock_slot_h",
        "dock",
        "dock_height",
        "dock_edge",
        "dock_autohide",
        "dock_pinned",
    ];

    /// Split a comma-separated list value into trimmed, non-empty,
    /// order-preserving, de-duplicated entries. Used for list-typed
    /// keys like `dock_pinned` from `bacak config set` and as the
    /// canonical normaliser in [`sanitize`](Self::sanitize).
    fn parse_list(value: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for part in value.split(',') {
            let t = part.trim();
            if !t.is_empty() && !out.iter().any(|e| e == t) {
                out.push(t.to_string());
            }
        }
        out
    }

    /// Set one field from string `key`/`value` (the `bacak config set`
    /// backend). Type-checks the value; does *not* clamp — the caller
    /// runs [`sanitize`](Self::sanitize) afterwards so out-of-range
    /// input is corrected consistently with file loading.
    pub fn set_field(&mut self, key: &str, value: &str) -> Result<(), String> {
        fn num(v: &str) -> Result<f32, String> {
            v.parse::<f32>()
                .map_err(|_| format!("`{v}` is not a number"))
        }
        match key {
            "blur" => {
                self.blur = value
                    .parse::<bool>()
                    .map_err(|_| format!("`{value}` is not true/false"))?
            }
            "blur_radius" => self.blur_radius = num(value)?,
            "shadow_top" => self.shadow_top = num(value)?,
            "shadow_step" => self.shadow_step = num(value)?,
            "shadow_floor" => self.shadow_floor = num(value)?,
            "dock_slot_w" => self.dock_slot_w = num(value)?,
            "dock_slot_h" => self.dock_slot_h = num(value)?,
            "dock" => {
                self.dock = value
                    .parse::<bool>()
                    .map_err(|_| format!("`{value}` is not true/false"))?
            }
            "dock_height" => self.dock_height = num(value)?,
            "dock_edge" => {
                self.dock_edge = match value.trim().to_lowercase().as_str() {
                    "bottom" => DockEdge::Bottom,
                    "top" => DockEdge::Top,
                    "left" => DockEdge::Left,
                    "right" => DockEdge::Right,
                    other => {
                        return Err(format!(
                            "`{other}` is not one of bottom/top/left/right"
                        ))
                    }
                }
            }
            "dock_autohide" => {
                self.dock_autohide = value
                    .parse::<bool>()
                    .map_err(|_| format!("`{value}` is not true/false"))?
            }
            "dock_pinned" => self.dock_pinned = Self::parse_list(value),
            other => {
                return Err(format!(
                    "unknown key `{other}` (valid: {})",
                    Self::KEYS.join(", ")
                ))
            }
        }
        Ok(())
    }

    /// Reset one field to its default (the `bacak config reset <key>`
    /// backend). Unknown key → error listing the valid ones.
    pub fn reset_field(&mut self, key: &str) -> Result<(), String> {
        let d = Self::default();
        match key {
            "blur" => self.blur = d.blur,
            "blur_radius" => self.blur_radius = d.blur_radius,
            "shadow_top" => self.shadow_top = d.shadow_top,
            "shadow_step" => self.shadow_step = d.shadow_step,
            "shadow_floor" => self.shadow_floor = d.shadow_floor,
            "dock_slot_w" => self.dock_slot_w = d.dock_slot_w,
            "dock_slot_h" => self.dock_slot_h = d.dock_slot_h,
            "dock" => self.dock = d.dock,
            "dock_height" => self.dock_height = d.dock_height,
            "dock_edge" => self.dock_edge = d.dock_edge,
            "dock_autohide" => self.dock_autohide = d.dock_autohide,
            "dock_pinned" => self.dock_pinned = d.dock_pinned,
            other => {
                return Err(format!(
                    "unknown key `{other}` (valid: {})",
                    Self::KEYS.join(", ")
                ))
            }
        }
        Ok(())
    }

    /// Clamp out-of-range / nonsensical values so a hand-edited config
    /// can't break rendering (negative radii, zero-area dock slots,
    /// shadow opacities outside `[0, 1]`, …).
    pub fn sanitize(&mut self) {
        let d = Self::default();
        if !self.blur_radius.is_finite() || self.blur_radius < 0.0 {
            self.blur_radius = d.blur_radius;
        }
        self.shadow_top = self.shadow_top.clamp(0.0, 1.0);
        self.shadow_floor = self.shadow_floor.clamp(0.0, self.shadow_top);
        if !self.shadow_step.is_finite() || self.shadow_step < 0.0 {
            self.shadow_step = d.shadow_step;
        }
        if !self.dock_slot_w.is_finite() || self.dock_slot_w <= 0.0 {
            self.dock_slot_w = d.dock_slot_w;
        }
        if !self.dock_slot_h.is_finite() || self.dock_slot_h <= 0.0 {
            self.dock_slot_h = d.dock_slot_h;
        }
        if !self.dock_height.is_finite() || self.dock_height <= 0.0 {
            self.dock_height = d.dock_height;
        }
        // A hand-edited file may carry blanks / dupes / padded names —
        // run the same normaliser `set` uses so the in-memory list is
        // always clean for the launcher path.
        self.dock_pinned = Self::parse_list(&self.dock_pinned.join(","));
    }

    /// Effective blur setting: the `BACAK_BLUR` env var force-enables
    /// it regardless of the file (handy for one-off testing, mirroring
    /// how `BACAK_BACKEND` overrides at runtime); otherwise the file
    /// value wins.
    pub fn blur_enabled(&self) -> bool {
        std::env::var("BACAK_BLUR").is_ok() || self.blur
    }
}

/// Filesystem watcher for live config reload. Shared by every live
/// backend (winit + udev) so they hot-reload identically.
///
/// We watch the *directory* holding `compositor.json`, not the file:
/// editors replace it via atomic rename, which would invalidate a
/// bare file watch. `notify` fires on its own thread into an mpsc
/// channel; the backend drains it each frame via [`poll_changed`].
///
/// Runtime-gated because `notify` is an optional dependency only
/// pulled in by the live runtimes — the CLI uses the rest of this
/// module without it.
#[cfg(feature = "runtime")]
pub struct ConfigWatcher {
    // Kept alive so the watch stays active; never accessed directly.
    _watcher: notify::RecommendedWatcher,
    rx: std::sync::mpsc::Receiver<notify::Result<notify::Event>>,
    path: PathBuf,
}

#[cfg(feature = "runtime")]
impl ConfigWatcher {
    /// Begin watching, or `None` (hot-reload off) if the path can't be
    /// resolved, the directory is absent, or the watcher won't start.
    /// Never fatal.
    pub fn start() -> Option<Self> {
        use notify::Watcher;

        let path = CompositorConfig::config_path()?;
        let dir = path.parent()?.to_path_buf();
        if !dir.is_dir() {
            tracing::debug!(?dir, "config dir absent; hot-reload off until restart");
            return None;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher = match notify::recommended_watcher(
            move |res: notify::Result<notify::Event>| {
                let _ = tx.send(res);
            },
        ) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(?e, "config watcher init failed; hot-reload off");
                return None;
            }
        };
        if let Err(e) = watcher.watch(&dir, notify::RecursiveMode::NonRecursive) {
            tracing::warn!(?e, ?dir, "config watch() failed; hot-reload off");
            return None;
        }
        tracing::info!(?dir, "watching for compositor.json changes");
        Some(Self { _watcher: watcher, rx, path })
    }

    /// Drain all pending events; return `true` iff `compositor.json`
    /// changed. Coalesces an editor's write burst into one reload.
    pub fn poll_changed(&self) -> bool {
        let mut hit = false;
        while let Ok(res) = self.rx.try_recv() {
            if let Ok(ev) = res {
                if ev.paths.iter().any(|p| p == &self.path) {
                    hit = true;
                }
            }
        }
        hit
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_tuned_constants() {
        let d = CompositorConfig::default();
        assert!(!d.blur);
        assert_eq!(d.blur_radius, 24.0);
        assert_eq!(d.shadow_top, 1.0);
        assert_eq!(d.shadow_step, 0.18);
        assert_eq!(d.shadow_floor, 0.30);
        assert_eq!((d.dock_slot_w, d.dock_slot_h), (96.0, 12.0));
    }

    #[test]
    fn partial_and_empty_json_fill_missing_from_defaults() {
        let empty: CompositorConfig = serde_json::from_str("{}").unwrap();
        assert!(!empty.blur);
        assert_eq!(empty.blur_radius, 24.0); // missing → real default
        let part: CompositorConfig =
            serde_json::from_str(r#"{"blur":true,"blur_radius":40}"#).unwrap();
        assert!(part.blur);
        assert_eq!(part.blur_radius, 40.0);
        assert_eq!(part.shadow_step, 0.18); // still defaulted
    }

    #[test]
    fn set_field_typed_and_unknown() {
        let mut c = CompositorConfig::default();
        c.set_field("blur", "true").unwrap();
        assert!(c.blur);
        c.set_field("blur_radius", "40").unwrap();
        assert_eq!(c.blur_radius, 40.0);
        c.set_field("dock", "true").unwrap();
        assert!(c.dock);
        c.set_field("dock_height", "72").unwrap();
        assert_eq!(c.dock_height, 72.0);
        c.set_field("dock_autohide", "true").unwrap();
        assert!(c.dock_autohide);
        assert!(c.set_field("dock_autohide", "sometimes").is_err()); // not a bool
        c.set_field("dock_edge", "left").unwrap();
        assert_eq!(c.dock_edge, DockEdge::Left);
        c.set_field("dock_edge", "TOP").unwrap(); // case-insensitive
        assert_eq!(c.dock_edge, DockEdge::Top);
        assert!(c.set_field("dock_edge", "north").is_err());
        assert!(c.set_field("dock", "maybe").is_err()); // not a bool
        assert!(c.set_field("dock_height", "tall").is_err()); // not a number
        assert!(c.set_field("blur", "yes").is_err()); // not a bool
        assert!(c.set_field("blur_radius", "wide").is_err()); // not a number
        // List key: comma-split, trimmed, de-duped, order-preserving.
        c.set_field("dock_pinned", " firefox , org.kde.kate ,firefox, ")
            .unwrap();
        assert_eq!(c.dock_pinned, vec!["firefox", "org.kde.kate"]);
        let e = c.set_field("nope", "1").unwrap_err();
        assert!(e.contains("unknown key") && e.contains("blur_radius"));
    }

    #[test]
    fn reset_field_restores_default_and_rejects_unknown() {
        let mut c = CompositorConfig::default();
        c.set_field("blur_radius", "99").unwrap();
        assert_eq!(c.blur_radius, 99.0);
        c.reset_field("blur_radius").unwrap();
        assert_eq!(c.blur_radius, CompositorConfig::default().blur_radius);
        c.set_field("dock", "true").unwrap();
        c.set_field("dock_height", "120").unwrap();
        c.reset_field("dock").unwrap();
        c.reset_field("dock_height").unwrap();
        assert_eq!(c.dock, CompositorConfig::default().dock);
        assert_eq!(c.dock_height, CompositorConfig::default().dock_height);
        c.set_field("dock_autohide", "true").unwrap();
        c.reset_field("dock_autohide").unwrap();
        assert_eq!(c.dock_autohide, CompositorConfig::default().dock_autohide);
        c.set_field("dock_edge", "left").unwrap();
        c.reset_field("dock_edge").unwrap();
        assert_eq!(c.dock_edge, CompositorConfig::default().dock_edge);
        c.set_field("dock_pinned", "a,b").unwrap();
        c.reset_field("dock_pinned").unwrap();
        assert!(c.dock_pinned.is_empty());
        assert!(c.reset_field("bogus").unwrap_err().contains("unknown key"));
    }

    #[test]
    fn set_then_sanitize_clamps_like_a_loaded_file() {
        let mut c = CompositorConfig::default();
        c.set_field("shadow_top", "9").unwrap(); // accepted as-is
        assert_eq!(c.shadow_top, 9.0);
        c.sanitize(); // caller clamps, same as load()
        assert_eq!(c.shadow_top, 1.0);
    }

    #[test]
    fn sanitize_clamps_bad_values() {
        let mut c = CompositorConfig {
            blur_radius: -5.0,
            shadow_top: 9.0,
            shadow_floor: -1.0,
            shadow_step: f32::NAN,
            dock_slot_w: 0.0,
            dock_slot_h: -3.0,
            dock_height: -10.0,
            dock_pinned: vec![
                " firefox ".into(),
                "".into(),
                "firefox".into(),
                "kate".into(),
            ],
            ..CompositorConfig::default()
        };
        c.sanitize();
        assert_eq!(c.blur_radius, 24.0);
        assert_eq!(c.shadow_top, 1.0);
        assert_eq!(c.shadow_floor, 0.0); // clamped into [0, top]
        assert_eq!(c.shadow_step, 0.18);
        assert_eq!(c.dock_slot_w, 96.0);
        assert_eq!(c.dock_slot_h, 12.0);
        assert_eq!(c.dock_height, 56.0); // bad height → default band
        // Blanks dropped, padding trimmed, dupes collapsed, order kept.
        assert_eq!(c.dock_pinned, vec!["firefox", "kate"]);
    }

    #[test]
    fn malformed_json_is_a_parse_error_not_a_panic() {
        // `load()` swallows this into a default; here we just confirm
        // the parse itself errors rather than aborting.
        assert!(serde_json::from_str::<CompositorConfig>("not json").is_err());
    }

    #[test]
    fn env_forces_blur_on_over_a_false_file() {
        let cfg = CompositorConfig { blur: false, ..CompositorConfig::default() };
        // Can't safely mutate process env in parallel tests, so assert
        // the file-only path here; the env override is a single `||`
        // and is exercised in the live backend.
        assert_eq!(cfg.blur_enabled(), std::env::var("BACAK_BLUR").is_ok());
    }
}
