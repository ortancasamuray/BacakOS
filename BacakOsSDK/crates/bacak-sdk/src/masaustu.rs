//! Desktop entries (`.desktop` files): discovery, lookup and launching, per
//! the freedesktop Desktop Entry spec.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::xdg;

/// An application from a `.desktop` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uygulama {
    /// Desktop file id with the extension, e.g. `bacak-belge.desktop`.
    pub id: String,
    /// `Name=`, localised for the user's language (`Name[tr]=`) when present.
    pub ad: String,
    /// `GenericName=` / `Comment=` (localised), may be empty.
    pub aciklama: String,
    /// Raw `Exec=` (with field codes); see [`Uygulama::komut`].
    pub exec: String,
    /// `Icon=` — a theme icon name or an absolute path; may be empty.
    pub simge: String,
    pub mime_turleri: Vec<String>,
    pub kategoriler: Vec<String>,
    /// `Terminal=true`: needs a terminal emulator.
    pub terminal: bool,
    /// `NoDisplay=true` or `Hidden=true`: not shown in menus.
    pub gizli: bool,
    /// `StartupWMClass=` — the window class/app id when it differs from the id.
    pub wm_sinifi: Option<String>,
    /// The file it was read from.
    pub yol: PathBuf,
}

impl Uygulama {
    /// The command line for opening `dosyalar` (none for a plain launch),
    /// with field codes expanded: `%f`/`%u` → the first file, `%F`/`%U` → all,
    /// `%c` → name, `%k` → the desktop file; `%i` and deprecated codes dropped.
    pub fn komut(&self, dosyalar: &[&Path]) -> Vec<String> {
        expand(&split_exec(&self.exec), dosyalar, &self.ad, &self.yol)
    }

    /// Starts the application (inside a terminal when `Terminal=true`),
    /// detached from our stdio.
    pub fn baslat(&self, dosyalar: &[&Path]) -> io::Result<Child> {
        let mut argv = self.komut(dosyalar);
        if argv.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{}: Exec boş", self.id)));
        }
        if self.terminal {
            if let Some(term) = terminal() {
                let mut wrapped = vec![term, "-e".to_string()];
                wrapped.append(&mut argv);
                argv = wrapped;
            }
        }
        Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    }
}

/// Parses a desktop file's text. `None` unless it's a `Type=Application`
/// with an `Exec`. Only the `[Desktop Entry]` group is read, so action groups
/// can't shadow its keys.
pub fn parse(id: &str, text: &str, yol: &Path) -> Option<Uygulama> {
    let lang = xdg::lang();
    let localized_key = |key: &str| lang.as_ref().map(|l| format!("{key}[{l}]"));
    let (name_l, generic_l, comment_l) = (localized_key("Name"), localized_key("GenericName"), localized_key("Comment"));

    let mut in_entry = false;
    let mut is_app = false;
    let mut f = Fields::default();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.starts_with('#') {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else { continue };
        let (key, val) = (key.trim(), unescape(val.trim()));
        let set = |slot: &mut Option<String>| {
            if slot.is_none() && !val.is_empty() {
                *slot = Some(val.clone());
            }
        };
        match key {
            "Type" => is_app = val == "Application",
            "Name" => set(&mut f.name),
            "GenericName" => set(&mut f.generic),
            "Comment" => set(&mut f.comment),
            "Exec" => set(&mut f.exec),
            "Icon" => set(&mut f.icon),
            "StartupWMClass" => set(&mut f.wm_class),
            "MimeType" => f.mimes = list(&val),
            "Categories" => f.categories = list(&val),
            "Terminal" => f.terminal = val.eq_ignore_ascii_case("true"),
            "NoDisplay" | "Hidden" => f.hidden |= val.eq_ignore_ascii_case("true"),
            k if Some(k) == name_l.as_deref() => set(&mut f.name_l),
            k if Some(k) == generic_l.as_deref() => set(&mut f.generic_l),
            k if Some(k) == comment_l.as_deref() => set(&mut f.comment_l),
            _ => {}
        }
    }
    if !is_app {
        return None;
    }
    let exec = f.exec?;
    let id = if id.ends_with(".desktop") { id.to_string() } else { format!("{id}.desktop") };
    Some(Uygulama {
        ad: f.name_l.or(f.name).unwrap_or_else(|| id.trim_end_matches(".desktop").to_string()),
        aciklama: f.generic_l.or(f.comment_l).or(f.generic).or(f.comment).unwrap_or_default(),
        exec,
        simge: f.icon.unwrap_or_default(),
        mime_turleri: f.mimes,
        kategoriler: f.categories,
        terminal: f.terminal,
        gizli: f.hidden,
        wm_sinifi: f.wm_class,
        yol: yol.to_path_buf(),
        id,
    })
}

#[derive(Default)]
struct Fields {
    name: Option<String>,
    name_l: Option<String>,
    generic: Option<String>,
    generic_l: Option<String>,
    comment: Option<String>,
    comment_l: Option<String>,
    exec: Option<String>,
    icon: Option<String>,
    wm_class: Option<String>,
    mimes: Vec<String>,
    categories: Vec<String>,
    terminal: bool,
    hidden: bool,
}

fn list(v: &str) -> Vec<String> {
    v.split(';').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
}

/// Value escapes from the spec: `\s \n \t \r \\`.
fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// `<data dir>/applications` directories, highest priority first.
fn application_dirs() -> Vec<PathBuf> {
    xdg::data_dirs().into_iter().map(|d| d.join("applications")).collect()
}

/// Every application shown in menus (not `NoDisplay`/`Hidden`), one per id —
/// a user's `~/.local/share/applications` copy wins over the system one —
/// sorted by name.
pub fn uygulamalar() -> Vec<Uygulama> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for dir in application_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for ent in rd.flatten() {
            let id = ent.file_name().to_string_lossy().into_owned();
            if !id.ends_with(".desktop") || !seen.insert(id.clone()) {
                continue;
            }
            let path = ent.path();
            if let Some(app) = std::fs::read_to_string(&path).ok().and_then(|t| parse(&id, &t, &path)) {
                if !app.gizli {
                    out.push(app);
                }
            }
        }
    }
    out.sort_by_key(|a| a.ad.to_lowercase());
    out
}

/// Finds an application by desktop id (`altay` or `altay.desktop`, any case)
/// or by window class (`StartupWMClass`), the way a compositor maps a
/// window's app id back to its entry. Includes hidden entries.
pub fn uygulama(id_or_class: &str) -> Option<Uygulama> {
    let base = id_or_class.trim().trim_end_matches(".desktop");
    if base.is_empty() || base.contains('/') {
        return None;
    }
    let names = [format!("{base}.desktop"), format!("{}.desktop", base.to_lowercase())];
    let dirs = application_dirs();
    for dir in &dirs {
        for n in &names {
            let p = dir.join(n);
            if let Some(app) = std::fs::read_to_string(&p).ok().and_then(|t| parse(n, &t, &p)) {
                return Some(app);
            }
        }
    }
    // StartupWMClass: exact match first, then case-insensitive.
    let mut loose = None;
    for dir in &dirs {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for ent in rd.flatten() {
            let id = ent.file_name().to_string_lossy().into_owned();
            if !id.ends_with(".desktop") {
                continue;
            }
            let path = ent.path();
            let Some(app) = std::fs::read_to_string(&path).ok().and_then(|t| parse(&id, &t, &path)) else { continue };
            match app.wm_sinifi.as_deref() {
                Some(c) if c == base => return Some(app),
                Some(c) if loose.is_none() && c.eq_ignore_ascii_case(base) => loose = Some(app),
                _ => {}
            }
        }
    }
    loose
}

/// The command line for a raw `Exec=` value, for callers that keep their own
/// entry type; see [`Uygulama::komut`] (`%c`/`%k` expand to nothing here).
pub fn exec_argv(exec: &str, dosyalar: &[&Path]) -> Vec<String> {
    expand(&split_exec(exec), dosyalar, "", Path::new(""))
}

/// The first installed terminal emulator, Wayland-native first.
pub fn terminal() -> Option<String> {
    const ADAYLAR: &[&str] =
        &["foot", "alacritty", "kitty", "wezterm", "konsole", "gnome-terminal", "x-terminal-emulator", "xterm"];
    ADAYLAR.iter().find(|t| xdg::on_path(t)).map(|t| t.to_string())
}

/// Splits an `Exec=` value into arguments: double quotes group (with
/// `\" \\ \` \$` escapes inside), whitespace separates.
fn split_exec(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_tok = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_tok = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => match chars.peek() {
                            Some('"' | '\\' | '`' | '$') => cur.push(chars.next().unwrap()),
                            _ => cur.push('\\'),
                        },
                        _ => cur.push(q),
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
}

fn expand(tokens: &[String], files: &[&Path], name: &str, desktop: &Path) -> Vec<String> {
    let file = |p: &Path| p.to_string_lossy().into_owned();
    let mut out = Vec::new();
    for tok in tokens {
        // A token that is exactly a list code expands to several arguments.
        match tok.as_str() {
            "%F" | "%U" => {
                out.extend(files.iter().map(|p| file(p)));
                continue;
            }
            "%i" => continue,
            _ => {}
        }
        let mut s = String::new();
        let mut chars = tok.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                s.push(c);
                continue;
            }
            match chars.next() {
                Some('%') => s.push('%'),
                Some('f' | 'u' | 'F' | 'U') => {
                    if let Some(p) = files.first() {
                        s.push_str(&file(p));
                    }
                }
                Some('c') => s.push_str(name),
                Some('k') => s.push_str(&desktop.to_string_lossy()),
                _ => {} // %i inside a token, deprecated %d %D %n %N %v %m
            }
        }
        if !s.is_empty() {
            out.push(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GEDIT: &str = "\
[Desktop Entry]
Type=Application
Name=Text Editor
Name[tr]=Metin Düzenleyici
Comment=Edit text files
Exec=gedit %U
MimeType=text/plain;text/markdown;
Icon=org.gnome.gedit
StartupWMClass=Gedit
[Desktop Action new-window]
Name=New Window
Exec=gedit --new-window";

    #[test]
    fn parses_first_group_and_localises() {
        std::env::set_var("LANG", "tr_TR.UTF-8");
        let a = parse("org.gnome.gedit", GEDIT, Path::new("/x/org.gnome.gedit.desktop")).unwrap();
        assert_eq!(a.id, "org.gnome.gedit.desktop");
        assert_eq!(a.ad, "Metin Düzenleyici");
        assert_eq!(a.aciklama, "Edit text files");
        assert_eq!(a.exec, "gedit %U");
        assert_eq!(a.mime_turleri, vec!["text/plain", "text/markdown"]);
        assert_eq!(a.wm_sinifi.as_deref(), Some("Gedit"));
        assert!(!a.gizli && !a.terminal);
    }

    #[test]
    fn rejects_non_applications() {
        assert!(parse("l", "[Desktop Entry]\nType=Link\nURL=x", Path::new("l")).is_none());
        assert!(parse("n", "[Desktop Entry]\nType=Application\nName=N", Path::new("n")).is_none());
        let h = parse("h", "[Desktop Entry]\nType=Application\nExec=h\nNoDisplay=true", Path::new("h")).unwrap();
        assert!(h.gizli);
    }

    #[test]
    fn exec_quotes_and_field_codes() {
        let app = |exec: &str| Uygulama {
            exec: exec.into(),
            ..parse("t", "[Desktop Entry]\nType=Application\nName=T\nExec=t", Path::new("/a/t.desktop")).unwrap()
        };
        let a = Path::new("/ev/a b.pdf");
        let b = Path::new("/ev/c.pdf");
        assert_eq!(app("gedit %U").komut(&[a, b]), vec!["gedit", "/ev/a b.pdf", "/ev/c.pdf"]);
        assert_eq!(app("gedit %U").komut(&[]), vec!["gedit"]);
        assert_eq!(app("v --file=%f --x").komut(&[a]), vec!["v", "--file=/ev/a b.pdf", "--x"]);
        assert_eq!(
            app(r#""/opt/My App/run" --title "%c" %i 100%%"#).komut(&[]),
            vec!["/opt/My App/run", "--title", "T", "100%"]
        );
        assert_eq!(app(r#"sh -c "echo \"hi\"""#).komut(&[]), vec!["sh", "-c", "echo \"hi\""]);
        assert_eq!(app("t %k").komut(&[]), vec!["t", "/a/t.desktop"]);
    }

    #[test]
    fn finds_by_id_and_wm_class() {
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(apps.join("org.gnome.gedit.desktop"), GEDIT).unwrap();
        std::env::set_var("XDG_DATA_HOME", dir.path());
        std::env::set_var("XDG_DATA_DIRS", dir.path().join("yok"));
        assert!(uygulama("org.gnome.gedit").is_some());
        assert!(uygulama("org.gnome.gedit.desktop").is_some());
        assert_eq!(uygulama("Gedit").unwrap().id, "org.gnome.gedit.desktop");
        assert_eq!(uygulama("gedit").unwrap().id, "org.gnome.gedit.desktop", "küçük harf sınıf");
        assert!(uygulama("../etc/passwd").is_none());
        assert_eq!(uygulamalar().len(), 1);
    }
}
