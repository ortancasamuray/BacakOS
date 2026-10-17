//! XDG Base Directory lookups shared by the modules.

use std::path::PathBuf;

/// `$XDG_DATA_HOME` (or `~/.local/share`), then `$XDG_DATA_DIRS` (or
/// `/usr/local/share:/usr/share`) — highest priority first.
pub fn data_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    match std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        Some(h) => out.push(PathBuf::from(h)),
        None => {
            if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
                out.push(PathBuf::from(home).join(".local/share"));
            }
        }
    }
    let dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    out.extend(dirs.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    out
}

/// `$XDG_CONFIG_HOME` or `~/.config`.
pub fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").filter(|v| !v.is_empty()).map(|h| PathBuf::from(h).join(".config")))
}

/// Whether `prog` is an executable file somewhere on `$PATH`.
pub fn on_path(prog: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(prog).is_file()))
}

/// The user's language code (`tr` for `tr_TR.UTF-8`) from `LC_ALL`,
/// `LC_MESSAGES` or `LANG`; `None` for `C`/`POSIX`/unset.
pub fn lang() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
        .map(|v| v.split(['_', '.', '@']).next().unwrap_or("").to_string())
        .filter(|l| !l.is_empty() && l != "C" && l != "POSIX")
}
