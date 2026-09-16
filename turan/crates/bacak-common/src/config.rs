//! Configuration model for `/etc/bacak-display-manager.conf`.
//!
//! The on-disk format is TOML. Every field has a sane default via `serde`'s
//! `#[serde(default)]`, so a partial or empty config file is always valid and
//! an absent file falls back to [`Config::default`]. This keeps first-boot and
//! distro-packaging robust: the daemon never refuses to start because an
//! optional key is missing.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Canonical location read by the daemon at startup.
pub const SYSTEM_CONFIG_PATH: &str = "/etc/bacak-display-manager.conf";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub daemon: Daemon,
    pub autologin: Autologin,
    pub greeter: Greeter,
    pub theme: Theme,
    pub wallpaper: Wallpaper,
    pub keyboard: VirtualKeyboard,
    pub accessibility: Accessibility,
    pub power: Power,
    pub sessions: Sessions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Daemon {
    /// Unprivileged system user the greeter drops to (must exist).
    pub greeter_user: String,
    /// Seat to manage. `seat0` is the default local seat under logind.
    pub seat: String,
    /// Path to the compositor binary the greeter renders on.
    pub compositor: PathBuf,
    /// Where the daemon listens for greeter IPC (a UNIX socket).
    pub ipc_socket: PathBuf,
    /// Register a logind session (via PAM `bacak-greeter`) for the greeter so a
    /// real compositor (weston/bacak-compositor) can take DRM master on the
    /// seat. Requires a `system-pam` build; ignored otherwise. Disable only for
    /// nested/dev setups that don't drive a real display.
    pub register_greeter_session: bool,
}

impl Default for Daemon {
    fn default() -> Self {
        Self {
            greeter_user: "bacak-greeter".into(),
            seat: "seat0".into(),
            compositor: PathBuf::from("/usr/bin/bacak-compositor"),
            ipc_socket: PathBuf::from("/run/bacak-display-manager/greeter.sock"),
            register_greeter_session: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Autologin {
    pub enabled: bool,
    /// User to log in automatically when `enabled`.
    pub user: Option<String>,
    /// Session id (`.desktop` basename) to start; falls back to user default.
    pub session: Option<String>,
    /// Seconds to wait before autologin, giving a chance to cancel. 0 = instant.
    pub delay_seconds: u32,
}

/// UI language for the greeter and the daemon's user-facing messages. `tr` is
/// the project default. See [`crate::i18n`] for the string table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    Tr,
    En,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Greeter {
    /// Show the list of local users (vs. a blank username field only).
    pub show_user_list: bool,
    /// Pre-select the last user that successfully logged in.
    pub remember_last_user: bool,
    /// Pre-select the last session a user chose.
    pub remember_last_session: bool,
    /// Offer an ephemeral guest session.
    pub allow_guest: bool,
    /// Allow PIN entry as an alternative PAM conversation.
    pub allow_pin: bool,
    /// Allow typing an arbitrary username not in the list.
    pub allow_manual_login: bool,
    /// UI language (`tr` or `en`).
    pub language: Language,
}

impl Default for Greeter {
    fn default() -> Self {
        Self {
            show_user_list: true,
            remember_last_user: true,
            remember_last_session: true,
            allow_guest: false,
            allow_pin: false,
            allow_manual_login: true,
            language: Language::Tr,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub mode: ColorMode,
    /// Named theme installed under `/usr/share/bacak-display-manager/themes`.
    pub name: String,
    /// Accent color as `#rrggbb`.
    pub accent: String,
    /// Path to the logo shown above the user list.
    pub logo: PathBuf,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            mode: ColorMode::Dark,
            name: "bacak".into(),
            accent: "#3b82f6".into(),
            logo: PathBuf::from("/usr/share/bacak-display-manager/logo.svg"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorMode {
    Dark,
    Light,
    /// Follow the system `org.freedesktop.appearance` preference if available.
    Auto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Wallpaper {
    pub mode: WallpaperMode,
    /// Single image, or first frame for `slideshow`.
    pub image: PathBuf,
    /// Directory of images used when `mode = "slideshow"`.
    pub slideshow_dir: Option<PathBuf>,
    /// Seconds between slideshow images.
    pub slideshow_interval: u32,
    /// Gaussian blur sigma applied to the wallpaper (0 = off).
    pub blur_sigma: f32,
}

impl Default for Wallpaper {
    fn default() -> Self {
        Self {
            mode: WallpaperMode::Image,
            image: PathBuf::from("/usr/share/bacak-display-manager/wallpaper.jpg"),
            slideshow_dir: None,
            slideshow_interval: 30,
            blur_sigma: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WallpaperMode {
    /// Solid color from `theme.accent`-derived palette.
    Solid,
    Image,
    Slideshow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VirtualKeyboard {
    /// `auto` shows it only when a touchscreen is detected; `always`/`never`
    /// force the behaviour.
    pub mode: KeyboardMode,
    /// Pop up automatically when a text/password field gains focus.
    pub show_on_focus: bool,
    /// On-screen keyboard layout: `trf` (Turkish F, the default), `trq`
    /// (Turkish Q), or `us` (US QWERTY). `tr` is an alias for `trf`.
    pub layout: String,
}

impl Default for VirtualKeyboard {
    fn default() -> Self {
        Self {
            mode: KeyboardMode::Auto,
            show_on_focus: true,
            layout: "trf".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyboardMode {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Accessibility {
    /// Global UI scale factor (1.0 = 100%). Also drives touch target sizing.
    pub scale: f32,
    /// Large font preset.
    pub large_fonts: bool,
    /// High-contrast palette override.
    pub high_contrast: bool,
    /// Enable on-screen reader hooks (Orca/AT-SPI bridge).
    pub screen_reader: bool,
}

impl Default for Accessibility {
    fn default() -> Self {
        Self {
            scale: 1.0,
            large_fonts: false,
            high_contrast: false,
            screen_reader: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Power {
    pub allow_shutdown: bool,
    pub allow_restart: bool,
    pub allow_suspend: bool,
    pub allow_hibernate: bool,
}

impl Default for Power {
    fn default() -> Self {
        Self {
            allow_shutdown: true,
            allow_restart: true,
            allow_suspend: true,
            allow_hibernate: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sessions {
    /// Directories scanned for Wayland session `.desktop` files.
    pub wayland_dirs: Vec<PathBuf>,
    /// Directories scanned for X11 session `.desktop` files.
    pub xsession_dirs: Vec<PathBuf>,
    /// Default session id when the user has no recorded preference.
    pub default_session: String,
}

impl Default for Sessions {
    fn default() -> Self {
        Self {
            wayland_dirs: vec![PathBuf::from("/usr/share/wayland-sessions")],
            xsession_dirs: vec![PathBuf::from("/usr/share/xsessions")],
            default_session: "bacak".into(),
        }
    }
}

impl Config {
    /// Load the config from `path`. A missing file is **not** an error — it
    /// yields defaults, so the daemon always has a usable configuration.
    pub fn load(path: impl AsRef<Path>) -> crate::Result<Self> {
        let path = path.as_ref();
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                log::warn!("{} not found; using built-in defaults", path.display());
                Ok(Self::default())
            }
            Err(e) => Err(crate::Error::io(path, e)),
        }
    }

    /// Load from the canonical system path.
    pub fn load_system() -> crate::Result<Self> {
        Self::load(SYSTEM_CONFIG_PATH)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_coherent() {
        let c = Config::default();
        assert_eq!(c.daemon.seat, "seat0");
        assert!(c.greeter.show_user_list);
        assert_eq!(c.theme.mode, ColorMode::Dark);
        assert_eq!(c.sessions.wayland_dirs.len(), 1);
    }

    #[test]
    fn partial_config_merges_with_defaults() {
        let toml = r##"
            [theme]
            mode = "light"
            accent = "#ff0000"

            [autologin]
            enabled = true
            user = "ayse"
        "##;
        let c: Config = toml::from_str(toml).unwrap();
        assert_eq!(c.theme.mode, ColorMode::Light);
        assert_eq!(c.theme.accent, "#ff0000");
        assert!(c.autologin.enabled);
        assert_eq!(c.autologin.user.as_deref(), Some("ayse"));
        // untouched sections still get defaults
        assert_eq!(c.daemon.seat, "seat0");
        assert!(c.power.allow_shutdown);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let toml = r#"
            [theme]
            bogus_key = 1
        "#;
        assert!(toml::from_str::<Config>(toml).is_err());
    }
}
