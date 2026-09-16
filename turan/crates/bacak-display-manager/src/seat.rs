//! Seat / runtime-directory preparation.
//!
//! BDM relies on `systemd-logind` for actual seat and VT allocation (the
//! `.service` file uses `Type=notify` and runs on a dedicated VT). Here we only
//! ensure the runtime directory the greeter socket lives in exists with tight
//! permissions, owned such that the unprivileged greeter can connect but not
//! tamper with the directory.

use bacak_common::config::Config;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Create `/run/bacak-display-manager` (parent of the IPC socket) as root-owned,
/// mode 0755, and the socket's parent reachable by the greeter user.
pub fn prepare_runtime_dir(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let dir = config
        .daemon
        .ipc_socket
        .parent()
        .ok_or("ipc_socket has no parent directory")?;

    if !dir.exists() {
        fs::create_dir_all(dir)?;
    }
    // Root owns the directory; world can traverse but not write. The socket
    // itself is created 0660 and chowned to the greeter group (see ipc.rs).
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755))?;
    log::debug!("runtime dir ready: {}", dir.display());

    // Stale socket from a previous run must go or bind() fails with EADDRINUSE.
    let sock = &config.daemon.ipc_socket;
    if sock.exists() {
        let _ = fs::remove_file(sock);
    }
    Ok(())
}

/// Resolve the configured greeter user to (uid, gid). Used to chown the socket
/// and to drop privileges for the greeter process.
pub fn greeter_ids(config: &Config) -> Result<(u32, u32), Box<dyn std::error::Error>> {
    let name = &config.daemon.greeter_user;
    let user = nix::unistd::User::from_name(name)?
        .ok_or_else(|| format!("greeter user '{name}' does not exist"))?;
    Ok((user.uid.as_raw(), user.gid.as_raw()))
}

/// Resolve a target login user to the fields the launcher needs.
pub struct TargetUser {
    pub uid: u32,
    pub gid: u32,
    pub name: String,
    pub home: std::path::PathBuf,
    pub shell: std::path::PathBuf,
}

pub fn lookup_user(name: &str) -> Result<TargetUser, Box<dyn std::error::Error>> {
    let u = nix::unistd::User::from_name(name)?
        .ok_or_else(|| format!("user '{name}' does not exist"))?;
    Ok(TargetUser {
        uid: u.uid.as_raw(),
        gid: u.gid.as_raw(),
        name: u.name,
        home: u.dir,
        shell: u.shell,
    })
}

/// The per-user `XDG_RUNTIME_DIR` logind would create: `/run/user/<uid>`.
pub fn xdg_runtime_dir(uid: u32) -> String {
    format!("/run/user/{uid}")
}

pub(crate) fn _assert_path(_p: &Path) {}
