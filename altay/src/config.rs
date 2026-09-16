// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Persistent configuration: saved network connections and UI preferences.
//! Stored as TOML at `$XDG_CONFIG_HOME/altay/config.toml`. Passwords are never
//! written here — only a keyring reference (Phase 7).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::network::Protocol;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub connections: Vec<SavedConnection>,
    #[serde(default)]
    pub prefs: Prefs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedConnection {
    pub name: String,
    pub protocol: String, // smb | sftp | ftp | dav | nfs
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub share: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prefs {
    #[serde(default)]
    pub show_hidden: bool,
    #[serde(default = "default_view")]
    pub view_mode: i32,
    #[serde(default = "default_scale")]
    pub grid_scale: f32,
    #[serde(default = "default_dark")]
    pub dark: bool,
    /// UI language: "en", "tr", or "" to auto-detect from the locale.
    #[serde(default)]
    pub lang: String,
}

fn default_view() -> i32 {
    0
}

fn default_scale() -> f32 {
    1.0
}

fn default_dark() -> bool {
    true
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            show_hidden: false,
            view_mode: default_view(),
            grid_scale: default_scale(),
            dark: default_dark(),
            lang: String::new(),
        }
    }
}

impl SavedConnection {
    /// Parse the stored protocol string.
    pub fn protocol(&self) -> Option<Protocol> {
        Some(match self.protocol.as_str() {
            "smb" => Protocol::Smb,
            "sftp" => Protocol::Sftp,
            "ftp" => Protocol::Ftp,
            "dav" | "webdav" => Protocol::WebDav,
            "nfs" => Protocol::Nfs,
            _ => return None,
        })
    }
}

/// Path to the config file (creating the parent directory if needed).
fn config_path() -> Option<PathBuf> {
    let dir = dirs::config_dir()?.join("altay");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("config.toml"))
}

/// Load config, returning defaults if missing or malformed.
pub fn load() -> Config {
    let Some(path) = config_path() else {
        return Config::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
            log::warn!("config parse error ({}): {e}", path.display());
            Config::default()
        }),
        Err(_) => Config::default(),
    }
}

/// Persist config to disk.
pub fn save(config: &Config) -> std::io::Result<()> {
    let Some(path) = config_path() else {
        return Ok(());
    };
    let text = toml::to_string_pretty(config)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(path, text)
}
