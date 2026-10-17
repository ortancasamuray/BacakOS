use std::path::{Path, PathBuf};

/// The compositor's IPC socket: `$BACAK_IPC` when set, otherwise
/// `$XDG_RUNTIME_DIR/bacak-ipc-<WAYLAND_DISPLAY>.sock`. `None` outside a
/// BacakOS session (no runtime dir or no Wayland display).
pub fn socket_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(crate::SOCKET_ENV).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let display = std::env::var("WAYLAND_DISPLAY").ok()?;
    socket_path_for(&display)
}

/// The socket path for a given Wayland display name (the compositor knows its
/// own name before exporting it). `WAYLAND_DISPLAY` may be an absolute path;
/// only its file name is used, so two displays never share a socket.
pub fn socket_path_for(wayland_display: &str) -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|p| !p.is_empty())?;
    let name = Path::new(wayland_display).file_name()?.to_str()?;
    if name.is_empty() {
        return None;
    }
    Some(PathBuf::from(runtime).join(format!("bacak-ipc-{name}.sock")))
}

/// BacakOS's per-user configuration directory: `$XDG_CONFIG_HOME/bacak`, or
/// `~/.config/bacak`.
pub fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").filter(|p| !p.is_empty()).map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("bacak"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_name_uses_display_file_name_only() {
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        assert_eq!(
            socket_path_for("wayland-bacak-0").unwrap(),
            PathBuf::from("/run/user/1000/bacak-ipc-wayland-bacak-0.sock")
        );
        assert_eq!(
            socket_path_for("/tmp/x/wayland-9").unwrap(),
            PathBuf::from("/run/user/1000/bacak-ipc-wayland-9.sock")
        );
        assert!(socket_path_for("").is_none());
    }
}
