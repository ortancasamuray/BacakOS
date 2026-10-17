//! Icon lookup across installed icon themes.
//!
//! The order matches the compositor's dock: the user's configured theme
//! (Desktop Settings / GTK `gtk-icon-theme-name`), then BacakOS's own theme,
//! then common full themes (Breeze, Adwaita, Papirus, …), any other installed
//! theme, `hicolor`, and finally `/usr/share/pixmaps`. Results are cached for
//! the process lifetime, misses included.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::{masaustu::Uygulama, xdg};

/// Path of the icon `ad` (a theme name like `folder` or an absolute path) at
/// about `boyut` pixels — PNG or SVG.
pub fn bul(ad: &str, boyut: u16) -> Option<PathBuf> {
    let ad = ad.trim();
    if ad.is_empty() {
        return None;
    }
    let p = Path::new(ad);
    if p.is_absolute() {
        return p.is_file().then(|| p.to_path_buf());
    }
    if ad.contains('/') {
        return None;
    }
    let key = format!("{ad}@{boyut}");
    if let Some(hit) = cache().lock().unwrap().get(&key) {
        return hit.clone();
    }
    let found = lookup(ad, boyut);
    cache().lock().unwrap().insert(key, found.clone());
    found
}

/// The application's icon (its `Icon=`), falling back to the generic
/// application icon.
pub fn uygulama_simgesi(app: &Uygulama, boyut: u16) -> Option<PathBuf> {
    bul(&app.simge, boyut).or_else(|| bul("application-x-executable", boyut))
}

/// The icon for a MIME type (`text/plain` → `text-plain`, then the
/// `text-x-generic` family icon).
pub fn mime_simgesi(mime: &str, boyut: u16) -> Option<PathBuf> {
    let specific = mime.replace('/', "-");
    let family = mime.split('/').next().unwrap_or("");
    bul(&specific, boyut)
        .or_else(|| bul(&format!("{family}-x-generic"), boyut))
        .or_else(|| bul("text-x-generic", boyut))
}

/// Names of installed icon themes (directories with an `index.theme`).
pub fn temalar() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for root in xdg::data_dirs() {
        let Ok(rd) = std::fs::read_dir(root.join("icons")) else { continue };
        for ent in rd.flatten() {
            if ent.path().join("index.theme").is_file() {
                if let Some(name) = ent.file_name().to_str() {
                    if !out.iter().any(|x| x == name) {
                        out.push(name.to_string());
                    }
                }
            }
        }
    }
    out
}

fn cache() -> &'static Mutex<HashMap<String, Option<PathBuf>>> {
    static C: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn lookup(name: &str, size: u16) -> Option<PathBuf> {
    for theme in candidates() {
        if let Some(p) = freedesktop_icons::lookup(name).with_size(size).with_theme(&theme).with_cache().find() {
            return Some(p);
        }
    }
    ["png", "svg", "xpm"]
        .iter()
        .map(|ext| PathBuf::from(format!("/usr/share/pixmaps/{name}.{ext}")))
        .find(|p| p.is_file())
}

/// Themes to try, best first.
fn candidates() -> Vec<String> {
    let mut out: Vec<String> = configured_theme().into_iter().collect();
    let installed = temalar();
    for pref in ["BacakOS", "breeze", "Adwaita", "Papirus", "Papirus-Dark", "Yaru", "elementary", "Tela"] {
        if let Some(real) = installed.iter().find(|t| t.eq_ignore_ascii_case(pref)) {
            if !out.iter().any(|x| x.eq_ignore_ascii_case(real)) {
                out.push(real.clone());
            }
        }
    }
    for t in &installed {
        if !t.eq_ignore_ascii_case("hicolor") && !out.contains(t) {
            out.push(t.clone());
        }
    }
    out.push("hicolor".into());
    out
}

/// The Desktop Settings choice (`compositor.json` `icon_theme`), else GTK's
/// `gtk-icon-theme-name`.
fn configured_theme() -> Option<String> {
    let cfg = xdg::config_home()?;
    if let Ok(text) = std::fs::read(cfg.join("bacak/compositor.json")) {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&text) {
            if let Some(t) = v.get("icon_theme").and_then(|t| t.as_str()).filter(|t| !t.is_empty()) {
                return Some(t.to_string());
            }
        }
    }
    for ver in ["gtk-4.0", "gtk-3.0"] {
        let Ok(text) = std::fs::read_to_string(cfg.join(ver).join("settings.ini")) else { continue };
        for line in text.lines() {
            if let Some(rest) = line.trim().strip_prefix("gtk-icon-theme-name") {
                if let Some(val) = rest.trim_start().strip_prefix('=') {
                    let name = val.trim().trim_matches('"').trim();
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_paths_and_bad_names() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x.svg");
        std::fs::write(&f, "<svg/>").unwrap();
        assert_eq!(bul(f.to_str().unwrap(), 48), Some(f));
        assert_eq!(bul("", 48), None);
        assert_eq!(bul("../../etc/passwd", 48), None);
        assert_eq!(bul("bacak-sdk-olmayan-simge-xyz", 48), None);
    }
}
