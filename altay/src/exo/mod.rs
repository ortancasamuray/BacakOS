//! exo-utils — application-association and file-opener infrastructure.
//!
//! Inspired by XFCE's `exo`/`exo-open`, this module turns a file into the set
//! of applications that can open it and launches them, so the UI can offer an
//! "Open" / "Open With…" experience instead of a bare `xdg-open`:
//!
//!   * [`mime_type`] — detect a file's MIME type (via `xdg-mime`, extension
//!     fallback),
//!   * [`apps_for`] — the applications that declare support for that type,
//!     default first,
//!   * [`open`] / [`open_with`] — launch the default or a chosen application,
//!   * [`set_default`] — make an application the default for a type.
//!
//! Desktop-entry parsing and `Exec` field-code expansion are pure and tested;
//! discovery and launching shell out to the standard freedesktop tools and
//! degrade gracefully when they are missing. The file path is always
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

/// freedesktop application directories, most-specific (user) first.
fn application_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(data_home) = dirs::data_dir() {
        dirs.push(data_home.join("applications"));
    }
    let system = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    for base in system.split(':').filter(|s| !s.is_empty()) {
        dirs.push(Path::new(base).join("applications"));
    }
    dirs
}

/// Parse a desktop-entry file's text into an [`AppEntry`]. Pure; testable.
/// Only the `[Desktop Entry]` group is read.
pub fn parse_entry(id: &str, text: &str) -> Option<AppEntry> {
    let mut in_group = false;
    let (mut name, mut exec, mut mimes, mut no_display, mut is_app, mut icon, mut terminal) =
        (String::new(), String::new(), Vec::new(), false, false, String::new(), false);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group || line.starts_with('#') {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else { continue };
        match key.trim() {
            "Type" => is_app = val.trim() == "Application",
            "Name" if name.is_empty() => name = val.trim().to_string(),
            "Exec" => exec = val.trim().to_string(),
            "Icon" if icon.is_empty() => icon = val.trim().to_string(),
            "MimeType" => {
                mimes = val.split(';').filter(|s| !s.is_empty()).map(|s| s.trim().to_string()).collect()
            }
            "NoDisplay" | "Hidden" => {
                if val.trim().eq_ignore_ascii_case("true") {
                    no_display = true;
                }
            }
            "Terminal" => {
                if val.trim().eq_ignore_ascii_case("true") {
                    terminal = true;
                }
            }
            _ => {}
        }
    }
    if !is_app || exec.is_empty() {
        return None;
    }
    if name.is_empty() {
        name = id.trim_end_matches(".desktop").to_string();
    }
    Some(AppEntry { id: id.to_string(), name, exec, mime_types: mimes, icon, no_display, terminal })
}

/// Memoized icon name → resolved file. Theme lookups touch many directories, so
/// results (including misses) are cached for the process lifetime.
fn icon_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, Option<PathBuf>>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Option<PathBuf>>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Resolve a desktop `Icon=` value to an actual image file: an absolute path is
/// used as-is; otherwise the name is looked up across icon themes (hicolor
/// first) and `pixmaps`. Returns the first `.svg`/`.png` found. Cached.
pub fn icon_path(app: &AppEntry) -> Option<PathBuf> {
    let name = app.icon.trim();
    if name.is_empty() {
        return None;
    }
    if let Some(hit) = icon_cache().lock().ok().and_then(|c| c.get(name).cloned()) {
        return hit;
    }
    let resolved = resolve_icon_uncached(name);
    if let Ok(mut c) = icon_cache().lock() {
        c.insert(name.to_string(), resolved.clone());
    }
    resolved
}

/// Uncached icon resolution; see [`icon_path`].
fn resolve_icon_uncached(name: &str) -> Option<PathBuf> {
    let direct = Path::new(name);
    if direct.is_absolute() && direct.exists() {
        return Some(direct.to_path_buf());
    }
    const SUBS: &[&str] = &[
        "scalable/apps", "512x512/apps", "256x256/apps", "128x128/apps",
        "96x96/apps", "64x64/apps", "48x48/apps", "32x32/apps",
    ];
    const EXTS: &[&str] = &["svg", "png"];

    let mut bases: Vec<PathBuf> = Vec::new();
    if let Some(d) = dirs::data_dir() {
        bases.push(d.join("icons"));
    }
    bases.push(PathBuf::from("/usr/local/share/icons"));
    bases.push(PathBuf::from("/usr/share/icons"));

    for base in &bases {
        // hicolor (the guaranteed fallback theme) first, then any other theme.
        let mut themes = vec![base.join("hicolor")];
        if let Ok(rd) = std::fs::read_dir(base) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() && p.file_name().map(|n| n != "hicolor").unwrap_or(false) {
                    themes.push(p);
                }
            }
        }
        for theme in &themes {
            for sub in SUBS {
                for ext in EXTS {
                    let c = theme.join(sub).join(format!("{name}.{ext}"));
                    if c.exists() {
                        return Some(c);
                    }
                }
            }
        }
    }
    for px in ["/usr/share/pixmaps", "/usr/local/share/pixmaps"] {
        for ext in EXTS {
            let c = Path::new(px).join(format!("{name}.{ext}"));
            if c.exists() {
                return Some(c);
            }
        }
    }
    None
}

/// Resolve a MIME/file-type icon name (e.g. `"folder"`, `"image-x-generic"`)
/// to a file path, searching `places/` then `mimetypes/` in Adwaita then hicolor.
pub fn resolve_mime_icon(name: &str) -> Option<PathBuf> {
    let cache_key = format!("__mime__{name}");
    if let Some(hit) = icon_cache().lock().ok().and_then(|c| c.get(&cache_key).cloned()) {
        return hit;
    }
    let resolved = resolve_mime_icon_uncached(name);
    if let Ok(mut c) = icon_cache().lock() {
        c.insert(cache_key, resolved.clone());
    }
    resolved
}

fn resolve_mime_icon_uncached(name: &str) -> Option<PathBuf> {
    const SUBS: &[&str] = &[
        "scalable/places",
        "scalable/mimetypes",
        "256x256/places",
        "256x256/mimetypes",
        "128x128/places",
        "128x128/mimetypes",
        "48x48/places",
        "48x48/mimetypes",
    ];
    const EXTS: &[&str] = &["svg", "png"];
    const THEMES: &[&str] = &["Adwaita", "hicolor"];

    let mut bases: Vec<PathBuf> = Vec::new();
    if let Some(d) = dirs::data_dir() {
        bases.push(d.join("icons"));
    }
    bases.push(PathBuf::from("/usr/local/share/icons"));
    bases.push(PathBuf::from("/usr/share/icons"));

    for base in &bases {
        for theme in THEMES {
            let theme_dir = base.join(theme);
            for sub in SUBS {
                for ext in EXTS {
                    let c = theme_dir.join(sub).join(format!("{name}.{ext}"));
                    if c.exists() {
                        return Some(c);
                    }
                }
            }
        }
    }
    None
}

/// Look up a desktop entry by id across the application directories.
pub fn app_by_id(id: &str) -> Option<AppEntry> {
    for dir in application_dirs() {
        let path = dir.join(id);
        if let Ok(text) = std::fs::read_to_string(&path) {
            return parse_entry(id, &text);
        }
    }
    None
}

/// All visible applications, de-duplicated by id (user entries win).
pub fn all_apps() -> Vec<AppEntry> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for dir in application_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let id = entry.file_name().to_string_lossy().into_owned();
            if !id.ends_with(".desktop") || !seen.insert(id.clone()) {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(entry.path()) {
                if let Some(app) = parse_entry(&id, &text) {
                    if !app.no_display {
                        out.push(app);
                    }
                }
            }
        }
    }
    out
}

// ---- MIME / defaults --------------------------------------------------------

/// Detect a file's MIME type: `xdg-mime` first, then an extension fallback.
pub fn mime_type(path: &Path) -> Option<String> {
    use std::process::Command;
    if let Ok(out) = Command::new("xdg-mime").args(["query", "filetype"]).arg(path).output() {
        if out.status.success() {
            let m = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !m.is_empty() {
                return Some(m);
            }
        }
    }
    mime_from_extension(path)
}

/// The default application id for a MIME type, via `xdg-mime query default`.
pub fn default_app(mime: &str) -> Option<String> {
    use std::process::Command;
    let out = Command::new("xdg-mime").args(["query", "default", mime]).output().ok()?;
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !id.is_empty()).then_some(id)
}

/// Applications that can open `path`, with the default (if any) listed first.
pub fn apps_for(sandbox: &Sandbox, path: &Path) -> Result<Vec<AppEntry>, ExoError> {
    let safe = sandbox.resolve(path)?;
    let Some(mime) = mime_type(safe.as_path()) else {
        return Ok(Vec::new());
    };
    let mut apps: Vec<AppEntry> =
        all_apps().into_iter().filter(|a| a.mime_types.iter().any(|m| m == &mime)).collect();
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    if let Some(def) = default_app(&mime) {
        if let Some(pos) = apps.iter().position(|a| a.id == def) {
            let d = apps.remove(pos);
            apps.insert(0, d);
        }
    }
    Ok(apps)
}

/// Make `app_id` the default application for `mime` (`xdg-mime default`).
pub fn set_default(mime: &str, app_id: &str) -> Result<(), ExoError> {
    let status = std::process::Command::new("xdg-mime")
        .args(["default", app_id, mime])
        .status()
        .map_err(|e| ExoError::Launch(e.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(ExoError::Launch("xdg-mime default returned non-zero".into()))
    }
}

// ---- Launching --------------------------------------------------------------

/// Open `path` in its default application. Tries the desktop default app first
/// (so it behaves like a real file manager), falling back to `xdg-open`.
pub fn open(sandbox: &Sandbox, path: impl AsRef<Path>) -> Result<(), ExoError> {
    let safe = sandbox.resolve(path)?;
    let p = safe.as_path();
    if let Some(mime) = mime_type(p) {
        if let Some(id) = default_app(&mime) {
            if launch_app(&id, p) {
                return Ok(());
            }
        }
    }
    if spawn("xdg-open", &[p]) {
        Ok(())
    } else {
        Err(ExoError::NoHandler)
    }
}

/// Open `path` with a specific application id.
pub fn open_with(sandbox: &Sandbox, path: impl AsRef<Path>, app_id: &str) -> Result<(), ExoError> {
    let safe = sandbox.resolve(path)?;
    if launch_app(app_id, safe.as_path()) {
        Ok(())
    } else {
        Err(ExoError::Launch(format!("could not start {app_id}")))
    }
}

/// Terminal emulators to try when launching a Terminal=true app, in order.
const TERM_EMULATORS: &[(&str, &str)] = &[
    ("alacritty", "-e"),
    ("xterm", "-e"),
    ("gnome-terminal", "--"),
    ("xfce4-terminal", "-e"),
    ("konsole", "-e"),
];

/// Launch an application id with one file.
/// Prefers parsing the Exec line directly (reliable) over gio/gtk-launch
/// (which require specific installation paths or portal access). Terminal=true
/// apps are wrapped in a terminal emulator.
fn launch_app(app_id: &str, file: &Path) -> bool {
    let Some(app) = app_by_id(app_id) else { return false };
    let Some((prog, args)) = expand_exec(&app.exec, file) else { return false };

    if app.terminal {
        // Wrap in terminal emulator: `alacritty -e <prog> [args...]`
        for (term, flag) in TERM_EMULATORS {
            let mut full_args: Vec<&Path> = vec![Path::new(flag), Path::new(&prog)];
            full_args.extend(args.iter().map(|s| Path::new(s.as_str())));
            if spawn(term, &full_args) {
                return true;
            }
        }
        return false;
    }

    spawn(&prog, &args.iter().map(|s| Path::new(s.as_str())).collect::<Vec<_>>())
}

/// Expand a desktop `Exec=` line for a single file, returning (program, args).
/// Field codes `%f %F %u %U` are replaced with the file; `%i %c %k %%` and other
/// codes are dropped/handled. Pure; testable.
pub fn expand_exec(exec: &str, file: &Path) -> Option<(String, Vec<String>)> {
    let file = file.to_string_lossy().into_owned();
    let mut tokens = exec.split_whitespace();
    let prog = tokens.next()?.to_string();
    let mut args = Vec::new();
    for tok in tokens {
        match tok {
            "%f" | "%F" | "%u" | "%U" => args.push(file.clone()),
            "%i" | "%c" | "%k" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => {}
            "%%" => args.push("%".to_string()),
            other => args.push(other.to_string()),
        }
    }
    Some((prog, args))
}

fn spawn(program: &str, args: &[&Path]) -> bool {
    use std::process::{Command, Stdio};
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

/// Minimal extension→MIME fallback for when `xdg-mime` is unavailable.
fn mime_from_extension(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    let m = match ext.as_str() {
        "txt" | "log" | "md" => "text/plain",
        "html" | "htm" => "text/html",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "ogg" => "audio/ogg",
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "zip" => "application/zip",
        "json" => "application/json",
        _ => return None,
    };
    Some(m.to_string())
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
        let app = parse_entry("org.gnome.gedit.desktop", GEDIT).unwrap();
        assert_eq!(app.name, "Text Editor");
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
