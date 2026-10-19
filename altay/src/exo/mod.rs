//! exo-utils — application-association and file-opener infrastructure.
//!
//! Inspired by XFCE's `exo`/`exo-open`, this module turns a file into the set
//! of applications that can open it and launches them, so the UI can offer an
//! "Open" / "Open With…" experience instead of a bare `xdg-open`:
//!
//!   * [`mime_type`] — detect a file's MIME type (via `xdg-mime`, extension
//!     fallback),
//!   * [`apps_for`] — every installed application, MIME-matching ones and the
//!     current default sorted first,
//!   * [`open`] / [`open_with`] — launch the default or a chosen application,
//!   * [`set_default`] — make an application the default for a type.
//!
//! Desktop-entry parsing, `Exec` expansion, MIME detection/defaults and
//! launching come from BacakOsSDK (`bacak_sdk::masaustu` / `bacak_sdk::mime`),
//! shared with every BacakOS app; this module keeps Altay's own [`AppEntry`]
//! view, the icon lookups and the sandbox check. The file path is always
//! sandbox-validated before launch.

use std::path::{Path, PathBuf};

use crate::security::{AccessDenied, Sandbox};

#[derive(Debug, thiserror::Error)]
pub enum ExoError {
    #[error(transparent)]
    Denied(#[from] AccessDenied),
    #[error("no application available to open this file")]
    NoHandler,
    #[error("could not launch application: {0}")]
    Launch(String),
}

/// A desktop application that can open files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    /// Desktop file id, e.g. `org.gnome.gedit.desktop`.
    pub id: String,
    /// Display name (`Name=`).
    pub name: String,
    /// Raw `Exec=` line (with field codes).
    pub exec: String,
    /// MIME types this app declares (`MimeType=`).
    pub mime_types: Vec<String>,
    /// Raw `Icon=` value (icon name or absolute path); empty if none.
    pub icon: String,
    /// `NoDisplay=true` / `Hidden=true` apps are excluded from menus.
    pub no_display: bool,
    /// `Terminal=true` — must be launched inside a terminal emulator.
    pub terminal: bool,
}

// ---- Discovery --------------------------------------------------------------

impl From<bacak_sdk::Uygulama> for AppEntry {
    fn from(a: bacak_sdk::Uygulama) -> Self {
        AppEntry {
            id: a.id,
            name: a.ad,
            exec: a.exec,
            mime_types: a.mime_turleri,
            icon: a.simge,
            no_display: a.gizli,
            terminal: a.terminal,
        }
    }
}

/// Parse a desktop-entry file's text into an [`AppEntry`]. Pure; testable.
/// Only the `[Desktop Entry]` group is read; `Name` is localised.
pub fn parse_entry(id: &str, text: &str) -> Option<AppEntry> {
    bacak_sdk::masaustu::parse(id, text, Path::new(id)).map(AppEntry::from)
}

/// Resolve a desktop `Icon=` value to an image file: an absolute path is used
/// as-is; otherwise the name is looked up the same way the dock does (the
/// configured theme, BacakOS, other themes, hicolor, `pixmaps`). Cached by the
/// SDK, misses included.
pub fn icon_path(app: &AppEntry) -> Option<PathBuf> {
    bacak_sdk::simge::bul(&app.icon, 48)
}

/// Look up a desktop entry by id (or window class) across the application
/// directories.
pub fn app_by_id(id: &str) -> Option<AppEntry> {
    bacak_sdk::masaustu::uygulama(id).map(AppEntry::from)
}

/// All visible applications, de-duplicated by id (user entries win).
pub fn all_apps() -> Vec<AppEntry> {
    bacak_sdk::masaustu::uygulamalar().into_iter().map(AppEntry::from).collect()
}

// ---- MIME / defaults --------------------------------------------------------

/// Detect a file's MIME type: `xdg-mime` first, then an extension fallback.
pub fn mime_type(path: &Path) -> Option<String> {
    bacak_sdk::mime::tur(path)
}

/// The default application id for a MIME type (`xdg-mime`, then BacakOS's
/// built-in defaults such as bacak-belge for PDFs and images).
pub fn default_app(mime: &str) -> Option<String> {
    bacak_sdk::mime::varsayilan(mime)
}

/// Every installed application, for the "Birlikte Aç" (Open With) picker —
/// not just the ones declaring this MIME type in their `.desktop` file, since
/// many installed apps have incomplete `MimeType=` entries and would
/// otherwise never show up even though they can open the file fine when
/// launched with it. Apps that *do* declare the MIME type are sorted first
/// (still alphabetically among themselves), followed by the rest
/// alphabetically; the current default (if any) is pinned to the very top.
pub fn apps_for(sandbox: &Sandbox, path: &Path) -> Result<Vec<AppEntry>, ExoError> {
    let safe = sandbox.resolve(path)?;
    let apps = match mime_type(safe.as_path()) {
        Some(mime) => bacak_sdk::mime::uygulamalar_icin(&mime),
        None => bacak_sdk::masaustu::uygulamalar(),
    };
    Ok(apps.into_iter().map(AppEntry::from).collect())
}

/// Make `app_id` the default application for `mime` (`xdg-mime default`).
pub fn set_default(mime: &str, app_id: &str) -> Result<(), ExoError> {
    bacak_sdk::mime::varsayilan_yap(mime, app_id).map_err(|e| ExoError::Launch(e.to_string()))
}

// ---- Launching --------------------------------------------------------------

/// Open `path` in its default application. Tries the desktop default app first
/// (so it behaves like a real file manager), falling back to `xdg-open`.
pub fn open(sandbox: &Sandbox, path: impl AsRef<Path>) -> Result<(), ExoError> {
    let safe = sandbox.resolve(path)?;
    bacak_sdk::mime::ac(safe.as_path()).map_err(|_| ExoError::NoHandler)
}

/// Open `path` with a specific application id.
pub fn open_with(sandbox: &Sandbox, path: impl AsRef<Path>, app_id: &str) -> Result<(), ExoError> {
    let safe = sandbox.resolve(path)?;
    bacak_sdk::mime::birlikte_ac(safe.as_path(), app_id)
        .map_err(|e| ExoError::Launch(format!("could not start {app_id}: {e}")))
}

/// Expand a desktop `Exec=` line for a single file, returning (program, args).
/// Quoted arguments stay whole; `%f %F %u %U` become the file, other field
/// codes are dropped. Pure; testable.
pub fn expand_exec(exec: &str, file: &Path) -> Option<(String, Vec<String>)> {
    let mut argv = bacak_sdk::masaustu::exec_argv(exec, &[file]).into_iter();
    let prog = argv.next()?;
    Some((prog, argv.collect()))
}

/// Minimal extension→MIME fallback for when `xdg-mime` is unavailable.
#[cfg(test)]
fn mime_from_extension(path: &Path) -> Option<String> {
    bacak_sdk::mime::uzantidan(path).map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GEDIT: &str = "\
[Desktop Entry]
Type=Application
Name=Text Editor
Name[tr]=Metin Düzenleyici
Exec=gedit %U
MimeType=text/plain;text/markdown;
Icon=org.gnome.gedit
[Desktop Action new-window]
Name=New Window
Exec=gedit --new-window";

    #[test]
    fn parses_desktop_entry_first_group_only() {
        std::env::set_var("LANG", "tr_TR.UTF-8");
        let app = parse_entry("org.gnome.gedit.desktop", GEDIT).unwrap();
        assert_eq!(app.name, "Metin Düzenleyici", "Name[tr] tercih edilmeli");
        assert_eq!(app.exec, "gedit %U");
        assert_eq!(app.icon, "org.gnome.gedit");
        assert!(app.mime_types.contains(&"text/plain".to_string()));
        assert!(!app.no_display);
    }

    #[test]
    fn skips_non_application_and_nodisplay() {
        assert!(parse_entry("x.desktop", "[Desktop Entry]\nType=Link\nURL=http://x").is_none());
        let hidden = parse_entry(
            "h.desktop",
            "[Desktop Entry]\nType=Application\nExec=foo %f\nNoDisplay=true",
        )
        .unwrap();
        assert!(hidden.no_display);
    }

    #[test]
    fn expands_exec_field_codes() {
        let (prog, args) = expand_exec("gedit %U", Path::new("/home/u/a b.txt")).unwrap();
        assert_eq!(prog, "gedit");
        assert_eq!(args, vec!["/home/u/a b.txt"]);

        let (prog, args) = expand_exec("myapp --flag %f --x", Path::new("/t/f")).unwrap();
        assert_eq!(prog, "myapp");
        assert_eq!(args, vec!["--flag", "/t/f", "--x"]);

        // Quoted program paths with spaces stay one argument.
        let (prog, args) = expand_exec(r#""/opt/My App/run" %f"#, Path::new("/t/f")).unwrap();
        assert_eq!(prog, "/opt/My App/run");
        assert_eq!(args, vec!["/t/f"]);
    }

    #[test]
    fn icon_path_resolves_absolute_and_caches() {
        let dir = std::env::temp_dir().join(format!("altay-icon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let iconf = dir.join("myapp.png");
        std::fs::write(&iconf, b"x").unwrap();
        let app = AppEntry {
            id: "a.desktop".into(),
            name: "A".into(),
            exec: "a %f".into(),
            mime_types: vec![],
            icon: iconf.to_string_lossy().into_owned(),
            no_display: false,
            terminal: false,
        };
        let r1 = icon_path(&app);
        let r2 = icon_path(&app); // second call served from cache
        assert_eq!(r1.as_deref(), Some(iconf.as_path()));
        assert_eq!(r1, r2);
        // A miss is cached as None and returns None.
        let miss = AppEntry { icon: "altay-no-such-icon-xyz".into(), terminal: false, ..app.clone() };
        assert_eq!(icon_path(&miss), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mime_extension_fallback() {
        assert_eq!(mime_from_extension(Path::new("a.pdf")).as_deref(), Some("application/pdf"));
        assert_eq!(mime_from_extension(Path::new("a.unknownext")), None);
    }
}
