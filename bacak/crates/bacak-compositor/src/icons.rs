//! App-icon resolution: `app_id` → `.desktop` entry → PNG → RGBA8888.
//!
//! This is a deliberately small slice of the freedesktop Desktop Entry
//! and Icon Theme specs — enough to put a recognisable glyph on a
//! task-switcher tile, not a general icon-theme engine.
//!
//! Resolution steps:
//!
//! * `.desktop` lookup: `$XDG_DATA_HOME` then each `$XDG_DATA_DIRS`
//!   entry (with the spec defaults), `applications/<app_id>.desktop`,
//!   plus a lowercase fallback.
//! * `Icon=` is read from the first `[Desktop Entry]` group.
//! * Icon resolution: an absolute PNG/SVG path is used directly; a bare
//!   name goes through the `freedesktop-icons` resolver, which walks the
//!   **active icon theme** (`gtk-icon-theme-name`), its `Inherits=` chain,
//!   then `hicolor`, then `/usr/share/pixmaps`, size/format aware. Both PNG
//!   (`image`) and SVG (`resvg`) are decoded — SVG is fully supported.
//!
//! Output is `Abgr8888` byte order (`[R, G, B, A]`), matching what the
//! `GlesRenderer` `ImportMem` path maps `Fourcc::Abgr8888` to — same
//! convention as [`crate::text`].
//!
//! Compiled only with the `runtime` feature (pulls in `image`).

#![cfg(feature = "runtime")]

use std::path::{Path, PathBuf};

/// Icon-theme PNG sizes we probe, largest first — a 64px source
/// downsamples to the ~18px tile glyph more cleanly than a 16px one
/// upscales. Used only by the hand-rolled fallback now; the primary path
/// is the `freedesktop-icons` resolver.
const ICON_SIZES: &[&str] = &["64x64", "48x48", "128x128", "32x32", "256x256"];

/// Target size handed to the `freedesktop-icons` resolver. 64px biases it
/// toward a crisp source (or the scalable SVG) that downsamples cleanly to
/// the small tile glyph.
const ICON_LOOKUP_SIZE: u16 = 64;

/// Resolve `app_id` to a decoded RGBA8888 buffer plus its dimensions,
/// or `None` if no usable PNG icon could be found. Callers must treat
/// the icon as optional.
pub fn resolve_icon_rgba(app_id: &str) -> Option<(Vec<u8>, u32, u32)> {
    let app_id = app_id.trim();
    if app_id.is_empty() {
        return None;
    }
    // 1. The app's declared icon, via its `.desktop` `Icon=` key. Most accurate
    //    — this is what the apps menu (whose ids are `.desktop` basenames) uses.
    if let Some(rgba) = find_desktop_file(app_id)
        .and_then(|desktop| parse_icon_key(&desktop))
        .and_then(|icon| resolve_icon_path(&icon))
        .and_then(|path| decode_icon(&path))
    {
        return Some(rgba);
    }
    // 2. Many apps set their Wayland `app_id` / X11 WM class to their icon name
    //    (firefox-esr, chromium, kate, …), which matches no `.desktop` basename.
    //    Try the id itself as an icon name — this fills in most running-window
    //    dock/switcher tiles whose `.desktop` lookup in step 1 missed.
    if let Some(rgba) = resolve_icon_path(app_id)
        .or_else(|| resolve_icon_path(&app_id.to_lowercase()))
        .and_then(|path| decode_icon(&path))
    {
        return Some(rgba);
    }
    // 3. Known app-id aliases: server/client app pairs name their windows after
    //    the *client* (e.g. `foot --server` → windows have app_id `footclient`,
    //    which has no `.desktop`/icon of its own). Strip a trailing `client` and
    //    retry, but only adopt the result if the base actually resolves.
    app_id_alias(app_id).and_then(|base| resolve_icon_rgba(&base))
}

/// Map a window `app_id` to a base app id for known launcher/window mismatches.
/// Currently: strip a trailing `client`/`-client` (foot's server mode names its
/// windows `footclient`; the same X/Xclient shape covers other terminals).
/// Returns `None` when there's nothing to alias.
fn app_id_alias(app_id: &str) -> Option<String> {
    let lower = app_id.to_ascii_lowercase();
    for suffix in ["-client", "client"] {
        if let Some(base) = lower.strip_suffix(suffix) {
            if !base.is_empty() {
                return Some(base.to_string());
            }
        }
    }
    None
}

/// Absolute path of a named icon resolved from a *specific* theme, or `None`.
/// Used for chrome icons that must contrast with a dark panel — e.g. the Wi-Fi
/// password reveal eye, which needs `breeze-dark`'s light glyph (plain `breeze`
/// ships a dark one that vanishes on the dark panel). The path can be fed
/// straight to [`resolve_icon_rgba`] (its step-2 absolute-path branch).
pub fn themed_icon_path(theme: &str, name: &str) -> Option<String> {
    freedesktop_icons::lookup(name)
        .with_size(ICON_LOOKUP_SIZE)
        .with_theme(theme)
        .with_cache()
        .find()
        .map(|p| p.to_string_lossy().into_owned())
}

/// A generic application icon for entries whose own icon can't be resolved (no
/// `.desktop`, no `Icon=`, or a name absent from every installed theme). Kept
/// separate from [`resolve_icon_rgba`] so that stays a pure resolver; the render
/// layer applies this fallback so a tile is never blank. Tries the standard
/// generic names in order; resolved through the same theme candidates.
pub fn generic_fallback_icon() -> Option<(Vec<u8>, u32, u32)> {
    for name in [
        "application-x-executable",
        "applications-other",
        "application-default-icon",
    ] {
        if let Some(rgba) = resolve_icon_path(name).and_then(|p| decode_icon(&p)) {
            return Some(rgba);
        }
    }
    None
}

/// Native size we rasterise SVGs at. The tile only shows the icon at
/// ~18px, but rendering large and letting the GPU downscale keeps
/// edges crisp; a square box preserves aspect via centred padding.
const SVG_RASTER_PX: u32 = 128;

/// Standard data roots, in priority order, per the Base Directory
/// spec: `$XDG_DATA_HOME` (or `~/.local/share`), then `$XDG_DATA_DIRS`
/// (or the `/usr/local/share:/usr/share` default).
fn data_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(home) = std::env::var("XDG_DATA_HOME") {
        if !home.is_empty() {
            out.push(PathBuf::from(home));
        }
    } else if let Ok(home) = std::env::var("HOME") {
        out.push(PathBuf::from(home).join(".local/share"));
    }
    let dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    for d in dirs.split(':').filter(|s| !s.is_empty()) {
        out.push(PathBuf::from(d));
    }
    out
}

/// Enumerate launchable desktop applications across the XDG data dirs.
/// Returns `(app_id, display_name)` de-duplicated by app id (the
/// highest-priority dir wins) and sorted case-insensitively by name.
/// Category bitmask bits: freedesktop main categories folded into tabs.
/// An app can belong to several; an app matching none still appears under
/// "Tümü" (the all-tab, which ignores the mask).
pub const CAT_SYSTEM: u8      = 1 << 0;
pub const CAT_DEVELOPMENT: u8 = 1 << 1;
pub const CAT_MEDIA: u8       = 1 << 2;
pub const CAT_INTERNET: u8    = 1 << 3;
pub const CAT_OFFICE: u8      = 1 << 4;
pub const CAT_UTILITY: u8     = 1 << 5;
pub const CAT_EDUCATION: u8   = 1 << 6;
pub const CAT_GAME: u8        = 1 << 7;

/// Fold a `Categories=` value (`;`-separated freedesktop categories) into
/// the bacak category bitmask.
pub fn categorize(categories: &str) -> u8 {
    let mut mask = 0u8;
    for c in categories.split(';') {
        match c.trim() {
            "System" | "Settings" | "HardwareSettings" | "TerminalEmulator" => mask |= CAT_SYSTEM,
            "Development" | "IDE" | "Debugger" | "RevisionControl" => mask |= CAT_DEVELOPMENT,
            "AudioVideo" | "Audio" | "Video" | "Graphics" | "2DGraphics" | "3DGraphics"
            | "Photography" => mask |= CAT_MEDIA,
            "Network" | "WebBrowser" | "Email" | "InstantMessaging" | "Chat" => mask |= CAT_INTERNET,
            "Office" | "WordProcessor" | "Spreadsheet" | "Presentation" | "Database"
            | "FlowChart" | "Math" => mask |= CAT_OFFICE,
            "Utility" | "TextEditor" | "Archiving" | "Compression" | "FileManager"
            | "Calculator" | "Clock" | "Accessibility" => mask |= CAT_UTILITY,
            "Education" | "Science" | "Languages" | "Literature" => mask |= CAT_EDUCATION,
            "Game" | "ActionGame" | "ArcadeGame" | "BoardGame" | "BlocksGame"
            | "CardGame" | "LogicGame" | "RolePlaying" | "Simulation"
            | "SportsGame" | "StrategyGame" => mask |= CAT_GAME,
            _ => {}
        }
    }
    mask
}

/// Skips entries that aren't `Type=Application`, are `NoDisplay=true`
/// or `Hidden=true`, or have no `Exec`. Used to populate the dock's
/// applications grid menu. Each entry is `(app_id, name, category_mask)`.
pub fn list_desktop_apps() -> Vec<(String, String, u8)> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut apps: Vec<(String, String, u8)> = Vec::new();
    for root in data_dirs() {
        let dir = root.join("applications");
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for ent in rd.flatten() {
            let path = ent.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            // First (highest-priority) dir to define an app id wins; a
            // later copy never overrides it, even if filtered out here.
            if seen.contains(id) {
                continue;
            }
            if desktop_entry_value(&path, "Type").as_deref() != Some("Application") {
                continue;
            }
            seen.insert(id.to_string());
            if desktop_entry_value(&path, "NoDisplay").as_deref() == Some("true")
                || desktop_entry_value(&path, "Hidden").as_deref() == Some("true")
                || desktop_entry_value(&path, "Exec").is_none()
            {
                continue;
            }
            let name = desktop_entry_value(&path, "Name").unwrap_or_else(|| id.to_string());
            let mask = desktop_entry_value(&path, "Categories")
                .map(|c| categorize(&c))
                .unwrap_or(0);
            apps.push((id.to_string(), name, mask));
        }
    }
    apps.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
    apps
}

fn find_desktop_file(app_id: &str) -> Option<PathBuf> {
    // Try the app id as given and a lowercased variant — clients are
    // inconsistent about casing (`Firefox` vs `firefox`).
    let names = [
        format!("{app_id}.desktop"),
        format!("{}.desktop", app_id.to_lowercase()),
    ];
    for root in data_dirs() {
        let apps = root.join("applications");
        // 1. Direct filename match — by far the common case.
        for n in &names {
            let p = apps.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
        // 2. `StartupWMClass` fallback. Some apps report a WM class
        // that doesn't match their .desktop basename (Steam games,
        // Electron apps, JetBrains IDEs, …); the spec lets the entry
        // declare the class explicitly so a compositor can map back.
        if let Some(p) = scan_for_wmclass(&apps, app_id) {
            return Some(p);
        }
    }
    None
}

/// Walk `apps_dir` for a `.desktop` whose `StartupWMClass` equals
/// `app_id` (exact first, then case-insensitive — X11 WM_CLASS is
/// technically case-sensitive but real-world clients are sloppy).
/// Non-recursive: the spec's nested layout is rare and the flat dir
/// covers the apps users actually run.
fn scan_for_wmclass(apps_dir: &Path, app_id: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(apps_dir).ok()?;
    let mut ci_hit: Option<PathBuf> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e != "desktop").unwrap_or(true) {
            continue;
        }
        let Some(class) = desktop_entry_value(&path, "StartupWMClass") else {
            continue;
        };
        if class == app_id {
            return Some(path); // exact wins immediately
        }
        if ci_hit.is_none() && class.eq_ignore_ascii_case(app_id) {
            ci_hit = Some(path); // remember, keep looking for an exact
        }
    }
    ci_hit
}

/// Value of `key=` in the first `[Desktop Entry]` group. Stops at the
/// next group header so an action group's value can't shadow the
/// main one.
fn desktop_entry_value(path: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let prefix = format!("{key}=");
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if in_entry {
            if let Some(val) = line.strip_prefix(&prefix) {
                let val = val.trim();
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

fn parse_icon_key(path: &Path) -> Option<String> {
    desktop_entry_value(path, "Icon")
}

/// The human display name for an app id, read from its `.desktop` `Name=` key,
/// preferring the Turkish localisation `Name[tr]=` when present. `None` when
/// there's no matching `.desktop` entry — callers fall back to the raw app id.
pub fn resolve_app_name(app_id: &str) -> Option<String> {
    let desktop = find_desktop_file(app_id)?;
    desktop_entry_value(&desktop, "Name[tr]").or_else(|| desktop_entry_value(&desktop, "Name"))
}

/// Resolve an app id (or `.desktop` basename) to its launch argv via
/// the Desktop Entry `Exec=` key. Returns the tokenised command with
/// freedesktop field codes stripped, or `None` when there's no entry,
/// no `Exec`, or it parses empty. This is pure lookup + parse — the
/// actual spawn is the backend's job ([`crate::launcher`]), so the
/// security-sensitive process launch stays isolated and the parser
/// stays unit-testable.
pub fn resolve_exec(app_id: &str) -> Option<Vec<String>> {
    if app_id.trim().is_empty() {
        return None;
    }
    let desktop = find_desktop_file(app_id)?;
    let raw = desktop_entry_value(&desktop, "Exec")?;
    let argv = parse_exec(&raw);
    (!argv.is_empty()).then_some(argv)
}

/// Like [`resolve_exec`] but also honours `Terminal=true`: console
/// programs (vim, htop, …) are wrapped in a terminal emulator so they
/// get a real TTY instead of being run bare against `/dev/null` (which
/// is meaningless for a curses UI). Prefers the Wayland-native `foot`,
/// falling back through common emulators; if none is installed the bare
/// command is returned rather than failing outright.
pub fn resolve_launch(app_id: &str) -> Option<Vec<String>> {
    let desktop = find_desktop_file(app_id)?;
    let raw = desktop_entry_value(&desktop, "Exec")?;
    let argv = parse_exec(&raw);
    if argv.is_empty() {
        return None;
    }
    if desktop_entry_value(&desktop, "Terminal").as_deref() != Some("true") {
        return Some(argv);
    }
    match terminal_emulator() {
        Some(term) => {
            let mut wrapped = vec![term, "-e".to_string()];
            wrapped.extend(argv);
            Some(wrapped)
        }
        None => Some(argv),
    }
}

/// First terminal emulator found on `$PATH`, Wayland-native preferred.
fn terminal_emulator() -> Option<String> {
    const CANDIDATES: &[&str] = &[
        "foot",
        "alacritty",
        "kitty",
        "wezterm",
        "konsole",
        "gnome-terminal",
        "x-terminal-emulator",
        "xterm",
    ];
    CANDIDATES.iter().find(|t| on_path(t)).map(|t| t.to_string())
}

fn on_path(prog: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| dir.join(prog).is_file())
    })
}

/// Tokenise an `Exec=` string per the Desktop Entry spec: arguments
/// may be double-quoted (with `\\ \" \` \$` escapes inside quotes);
/// outside quotes, whitespace separates tokens. Each resulting token
/// then has its field codes expanded ([`expand_field_codes`]); tokens
/// that were *only* a field code collapse to empty and are dropped, so
/// `firefox %u` → `["firefox"]`.
fn parse_exec(s: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_tok = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_tok = true;
                while let Some(q) = chars.next() {
                    if q == '"' {
                        break;
                    }
                    if q == '\\' {
                        // Spec: only these are escapable in quotes;
                        // anything else keeps the backslash.
                        match chars.peek() {
                            Some('"') | Some('\\') | Some('`') | Some('$') => {
                                cur.push(chars.next().unwrap());
                            }
                            _ => cur.push('\\'),
                        }
                    } else {
                        cur.push(q);
                    }
                }
            }
            c if c.is_whitespace() => {
                if in_tok {
                    tokens.push(std::mem::take(&mut cur));
                    in_tok = false;
                }
            }
            c => {
                in_tok = true;
                cur.push(c);
            }
        }
    }
    if in_tok {
        tokens.push(cur);
    }
    tokens
        .into_iter()
        .filter_map(|t| {
            let e = expand_field_codes(&t);
            (!e.is_empty()).then_some(e)
        })
        .collect()
}

/// Expand/strip Desktop Entry field codes within one token: `%%` → a
/// literal `%`; every other `%<letter>` (file/url/icon/name/etc.
/// placeholders, including the deprecated set) is removed since we
/// launch pinned apps with no document argument. A trailing lone `%`
/// is dropped.
fn expand_field_codes(tok: &str) -> String {
    let mut out = String::with_capacity(tok.len());
    let mut chars = tok.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some(_) => {} // a field code → drop it
            None => {}    // trailing lone '%' → drop
        }
    }
    out
}

/// Map an `Icon=` value to a concrete file path. Absolute paths are
/// used as-is if PNG or SVG; bare names are probed across the common
/// theme sizes (PNG), the `scalable` dir (SVG), and the legacy
/// pixmaps dir. PNG is preferred over SVG at the same precedence
/// level — it's cheaper to decode and already pixel-perfect.
fn resolve_icon_path(icon: &str) -> Option<PathBuf> {
    let p = Path::new(icon);
    if p.is_absolute() {
        let ext_ok = matches!(
            p.extension().and_then(|e| e.to_str()),
            Some("png") | Some("svg")
        );
        return (ext_ok && p.is_file()).then(|| p.to_path_buf());
    }

    // Theme-aware lookup, size/format aware, returning PNG or SVG (both decode
    // below). Try each candidate theme (configured → installed full themes →
    // hicolor); each lookup already walks that theme's `Inherits=` chain. The
    // first theme that has the name wins — without this, a system with no GTK
    // icon-theme config defaulted to `hicolor` and most menu entries (whose
    // `.desktop` names a standard icon living in Breeze/Adwaita) had no icon.
    for theme in icon_theme_candidates() {
        if let Some(path) = freedesktop_icons::lookup(icon)
            .with_size(ICON_LOOKUP_SIZE)
            .with_theme(&theme)
            .with_cache()
            .find()
        {
            return Some(path);
        }
    }

    // Fallback safety net — the original hand-rolled hicolor + pixmaps probe,
    // in case the resolver misses on an unusual data-dir layout.
    for root in data_dirs() {
        let hicolor = root.join("icons/hicolor");
        // Sized raster first.
        for size in ICON_SIZES {
            let c = hicolor.join(size).join("apps").join(format!("{icon}.png"));
            if c.is_file() {
                return Some(c);
            }
        }
        // Then the scalable vector.
        let svg = hicolor.join("scalable/apps").join(format!("{icon}.svg"));
        if svg.is_file() {
            return Some(svg);
        }
    }
    // Legacy flat pixmaps dir — no theme/size structure.
    for ext in ["png", "svg"] {
        let pixmap = PathBuf::from(format!("/usr/share/pixmaps/{icon}.{ext}"));
        if pixmap.is_file() {
            return Some(pixmap);
        }
    }
    None
}

/// The active icon theme name, read from GTK settings
/// (`<config>/gtk-4.0|gtk-3.0/settings.ini`, key `gtk-icon-theme-name`).
/// Falls back to `hicolor` — which the resolver always includes anyway, so an
/// unconfigured desktop simply gets the hicolor + pixmaps behaviour. Read live
/// per resolution; that's rare since decoded textures are cached by app id on
/// [`crate::state::BacakState`]. (A theme change therefore only takes effect
/// for icons resolved after it — live invalidation is a follow-up.)
/// The user's configured GTK icon theme (`gtk-icon-theme-name` in the GTK 4/3
/// `settings.ini`), or `None` when unset — there's no reliable system default
/// to assume, so callers fall back to probing what's actually installed.
fn configured_icon_theme() -> Option<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let cfg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")))?;
    for ver in ["gtk-4.0", "gtk-3.0"] {
        let Ok(text) = std::fs::read_to_string(cfg.join(ver).join("settings.ini")) else {
            continue;
        };
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

/// Icon-theme directories (those with an `index.theme`) installed across the
/// XDG data dirs, e.g. `Adwaita`, `breeze`, `hicolor`.
fn installed_icon_themes() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for root in data_dirs() {
        let Ok(rd) = std::fs::read_dir(root.join("icons")) else {
            continue;
        };
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

/// Icon themes to try, best first: the configured GTK theme, then the most
/// complete installed themes (so standard names like `utilities-terminal` or
/// `preferences-system` resolve even with no GTK config — they live in a full
/// theme such as Breeze/Adwaita, not in `hicolor`), then any other installed
/// theme, with `hicolor` always last as the spec fallback.
fn icon_theme_candidates() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(cfg) = configured_icon_theme() {
        out.push(cfg);
    }
    let installed = installed_icon_themes();
    for pref in [
        "breeze",
        "Adwaita",
        "Papirus",
        "Papirus-Dark",
        "Yaru",
        "elementary",
        "Tela",
    ] {
        if let Some(real) = installed.iter().find(|t| t.eq_ignore_ascii_case(pref)) {
            if !out.iter().any(|x| x.eq_ignore_ascii_case(real)) {
                out.push(real.clone());
            }
        }
    }
    for t in &installed {
        if !t.eq_ignore_ascii_case("hicolor") && !out.iter().any(|x| x == t) {
            out.push(t.clone());
        }
    }
    if !out.iter().any(|x| x.eq_ignore_ascii_case("hicolor")) {
        out.push("hicolor".into());
    }
    out
}

/// Decode an icon file to tightly-packed straight-alpha RGBA8888,
/// dispatching on extension: SVG → rasterise via `resvg`, anything
/// else → PNG via the `image` crate.
fn decode_icon(path: &Path) -> Option<(Vec<u8>, u32, u32)> {
    if path.extension().and_then(|e| e.to_str()) == Some("svg") {
        rasterize_svg(path)
    } else {
        decode_png_rgba(path)
    }
}

/// Decode a PNG into tightly-packed RGBA8888. Delegates colour-type
/// handling (palette, grayscale, 16-bit, …) to the `image` crate so we
/// don't reimplement it.
fn decode_png_rgba(path: &Path) -> Option<(Vec<u8>, u32, u32)> {
    let img = image::ImageReader::open(path).ok()?.decode().ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    Some((rgba.into_raw(), w, h))
}

/// Rasterise an SVG into a square [`SVG_RASTER_PX`] buffer, scaled to
/// fit while preserving aspect (transparent padding), then convert
/// `tiny_skia`'s premultiplied output to the straight-alpha RGBA the
/// rest of the icon pipeline expects.
fn rasterize_svg(path: &Path) -> Option<(Vec<u8>, u32, u32)> {
    use resvg::{tiny_skia, usvg};

    let data = std::fs::read(path).ok()?;
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_data(&data, &opt).ok()?;

    let size = tree.size();
    let (sw, sh) = (size.width(), size.height());
    if sw <= 0.0 || sh <= 0.0 {
        return None;
    }

    let box_px = SVG_RASTER_PX;
    let mut pixmap = tiny_skia::Pixmap::new(box_px, box_px)?;
    // Uniform scale to fit the longer side, centred in the square.
    let scale = (box_px as f32 / sw).min(box_px as f32 / sh);
    let tx = (box_px as f32 - sw * scale) / 2.0;
    let ty = (box_px as f32 - sh * scale) / 2.0;
    let transform = tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, tx, ty);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // `tiny_skia` stores premultiplied RGBA; the GLES import path (and
    // our PNG / text buffers) use straight alpha. Un-premultiply.
    let mut buf = pixmap.take();
    for px in buf.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
        } else if a < 255 {
            // straight = premul * 255 / a, rounded, clamped.
            px[0] = (((px[0] as u32 * 255) + a / 2) / a).min(255) as u8;
            px[1] = (((px[1] as u32 * 255) + a / 2) / a).min(255) as u8;
            px[2] = (((px[2] as u32 * 255) + a / 2) / a).min(255) as u8;
        }
    }
    Some((buf, box_px, box_px))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_app_id_is_none() {
        assert!(resolve_icon_rgba("").is_none());
        assert!(resolve_icon_rgba("   ").is_none());
    }

    #[test]
    fn unknown_app_id_is_none_not_panic() {
        // A name that can't have a .desktop file must fail gracefully.
        assert!(resolve_icon_rgba("definitely-not-an-installed-app-zzz").is_none());
    }

    #[test]
    fn parse_exec_strips_field_codes_and_keeps_quotes() {
        assert_eq!(parse_exec("firefox %u"), vec!["firefox"]);
        assert_eq!(parse_exec("env FOO=bar app %F --flag"), vec![
            "env", "FOO=bar", "app", "--flag"
        ]);
        // Quoted arg with spaces stays one token; escapes unwrapped.
        assert_eq!(
            parse_exec(r#"prog "a b" "c\"d" %f"#),
            vec!["prog", "a b", r#"c"d"#]
        );
        // %% → literal %, glued field code stripped from the token.
        assert_eq!(parse_exec("p 100%% file%f"), vec!["p", "100%", "file"]);
        // Deprecated codes also dropped; empty result → empty vec.
        assert_eq!(parse_exec("%d %n").len(), 0);
        assert!(parse_exec("   ").is_empty());
    }

    #[test]
    fn resolve_exec_reads_exec_key_from_first_group() {
        let dir = std::env::temp_dir();
        let f = dir.join("bacak-exec-test.desktop");
        std::fs::write(
            &f,
            "[Desktop Entry]\nName=Foo\nExec=foo --bar %U\n\n[Desktop Action New]\nExec=other\n",
        )
        .unwrap();
        assert_eq!(parse_exec(&desktop_entry_value(&f, "Exec").unwrap()), vec![
            "foo", "--bar"
        ]);
        let _ = std::fs::remove_file(&f);
        assert!(resolve_exec("").is_none());
        assert!(resolve_exec("definitely-not-installed-zzz").is_none());
    }

    #[test]
    fn categorize_folds_freedesktop_categories_into_tabs() {
        // System and Settings both map to the System tab.
        assert_eq!(categorize("Settings;System;"), CAT_SYSTEM);
        // Audio/Video/Graphics all fold into Media; combined masks OR together.
        assert_eq!(
            categorize("GTK;Development;AudioVideo;"),
            CAT_DEVELOPMENT | CAT_MEDIA
        );
        assert_eq!(categorize("Network;WebBrowser;"), CAT_INTERNET);
        // Unknown-only categories yield no mask (app shows only under "Tümü").
        assert_eq!(categorize("Utility;Office;"), 0);
        assert_eq!(categorize(""), 0);
    }

    #[test]
    fn parse_icon_key_reads_first_entry_group_only() {
        let dir = std::env::temp_dir();
        let f = dir.join("bacak-icons-test.desktop");
        std::fs::write(
            &f,
            "[Desktop Entry]\nName=Foo\nIcon=foo-icon\n\n[Desktop Action New]\nIcon=other\n",
        )
        .unwrap();
        assert_eq!(parse_icon_key(&f).as_deref(), Some("foo-icon"));
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn scan_for_wmclass_matches_exact_then_case_insensitive() {
        let base = std::env::temp_dir().join("bacak-wmclass-test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        std::fs::write(
            base.join("plain.desktop"),
            "[Desktop Entry]\nName=Plain\nIcon=plain\n",
        )
        .unwrap();
        std::fs::write(
            base.join("game.desktop"),
            "[Desktop Entry]\nName=Game\nIcon=game\nStartupWMClass=steam_app_42\n",
        )
        .unwrap();
        std::fs::write(
            base.join("ide.desktop"),
            "[Desktop Entry]\nName=IDE\nStartupWMClass=jetbrains-idea\n",
        )
        .unwrap();

        // Exact match.
        let hit = scan_for_wmclass(&base, "steam_app_42").unwrap();
        assert!(hit.ends_with("game.desktop"));

        // Case-insensitive fallback when no exact match exists.
        let hit = scan_for_wmclass(&base, "JetBrains-Idea").unwrap();
        assert!(hit.ends_with("ide.desktop"));

        // No StartupWMClass anywhere for this id.
        assert!(scan_for_wmclass(&base, "nonexistent").is_none());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn rasterize_svg_produces_square_straight_alpha_buffer() {
        let dir = std::env::temp_dir();
        let f = dir.join("bacak-icon-test.svg");
        // A half-opaque red square — exercises the un-premultiply path.
        std::fs::write(
            &f,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40">
                 <rect width="40" height="40" fill="#ff0000" fill-opacity="0.5"/>
               </svg>"##,
        )
        .unwrap();

        let (buf, w, h) = rasterize_svg(&f).expect("valid svg should rasterize");
        assert_eq!((w, h), (SVG_RASTER_PX, SVG_RASTER_PX));
        assert_eq!(buf.len() as u32, w * h * 4);

        // Find a covered pixel: alpha ~128, and after un-premultiply the
        // red channel should be near full (≈255), not the premultiplied
        // ≈128 it would be straight out of tiny_skia.
        let covered = buf
            .chunks_exact(4)
            .find(|px| px[3] > 100 && px[3] < 200)
            .expect("expected a half-opaque pixel");
        assert!(
            covered[0] > 200,
            "red channel {} should be un-premultiplied toward 255",
            covered[0]
        );
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn data_dirs_has_spec_defaults_without_env() {
        // We can't reliably mutate process env in parallel tests, so
        // just assert the default fallback dirs are always present.
        let dirs = data_dirs();
        assert!(
            dirs.iter().any(|d| d.ends_with("share")),
            "expected a /usr[/local]/share entry, got {dirs:?}"
        );
    }





    #[test]
    fn probe_icon_pixels() {
        for id in ["foot", "footclient", "libreoffice-writer", "libreoffice-calc", "soffice", "libreoffice-startcenter"] {
            match super::resolve_icon_rgba(id) {
                Some((buf, w, h)) => {
                    let mut opaque = 0u32; let (mut r,mut g,mut b)=(0u64,0u64,0u64);
                    let mut uniq = std::collections::HashSet::new();
                    for px in buf.chunks_exact(4) {
                        if px[3] > 10 { opaque+=1; r+=px[0] as u64; g+=px[1] as u64; b+=px[2] as u64; uniq.insert((px[0],px[1],px[2])); }
                    }
                    let n=opaque.max(1) as u64;
                    eprintln!("{id:?}: {w}x{h} opaque={opaque} uniqueColors={} avgRGB=({},{},{})", uniq.len(), r/n, g/n, b/n);
                }
                None => eprintln!("{id:?}: None (would fall back to generic/tile_color)"),
            }
        }
    }


    #[test]
    fn probe_dump_icons() {
        for id in ["foot", "libreoffice-writer"] {
            if let Some((buf, w, h)) = super::resolve_icon_rgba(id) {
                let img = image::RgbaImage::from_raw(w, h, buf).unwrap();
                let p = format!("/tmp/icon-{id}.png");
                img.save(&p).unwrap();
                eprintln!("wrote {p} ({w}x{h})");
            }
        }
    }



}
