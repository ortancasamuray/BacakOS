// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Network locations (SMB/CIFS, NFS, SFTP, FTP, WebDAV).
//!
//! Altay does not implement these protocols itself. On Wayland desktops the
//! right primitive is gvfs/GIO mounts surfaced under
//! `$XDG_RUNTIME_DIR/gvfs`, which the [`security`](crate::security) sandbox
//! already permits as `Network` roots. This module models saved connections and
//! turns them into mount requests; the actual mount is delegated to the gvfs
//! backend (later phase) so credentials flow through the system keyring/portal.

use std::path::PathBuf;

/// Supported network protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Smb,
    Nfs,
    Sftp,
    Ftp,
    WebDav,
}

impl Protocol {
    /// URI scheme used when handing the location to gvfs.
    pub fn scheme(self) -> &'static str {
        match self {
            Protocol::Smb => "smb",
            Protocol::Nfs => "nfs",
            Protocol::Sftp => "sftp",
            Protocol::Ftp => "ftp",
            Protocol::WebDav => "dav",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Smb => 445,
            Protocol::Nfs => 2049,
            Protocol::Sftp => 22,
            Protocol::Ftp => 21,
            Protocol::WebDav => 443,
        }
    }

    /// Parse a URI scheme into a protocol.
    pub fn from_scheme(scheme: &str) -> Option<Protocol> {
        Some(match scheme {
            "smb" | "cifs" => Protocol::Smb,
            "nfs" => Protocol::Nfs,
            "sftp" | "ssh" => Protocol::Sftp,
            "ftp" => Protocol::Ftp,
            "dav" | "davs" | "webdav" => Protocol::WebDav,
            _ => return None,
        })
    }
}

/// A saved network connection. Passwords are never stored here — they live in
/// the system keyring, referenced by `keyring_ref`.
#[derive(Debug, Clone)]
pub struct Connection {
    pub name: String,
    pub protocol: Protocol,
    pub host: String,
    pub port: Option<u16>,
    pub share: Option<String>,
    pub username: Option<String>,
    pub keyring_ref: Option<String>,
}

impl Connection {
    /// Best-effort parse of a `scheme://[user@]host[:port]/share` URI into a
    /// connection (used when the user types a URI in the path bar).
    pub fn from_uri(uri: &str) -> Option<Connection> {
        let (scheme, rest) = uri.split_once("://")?;
        let protocol = Protocol::from_scheme(scheme)?;
        let (authority, share) = match rest.split_once('/') {
            Some((a, s)) => (a, (!s.is_empty()).then(|| s.trim_end_matches('/').to_string())),
            None => (rest, None),
        };
        let (userinfo, hostport) = match authority.split_once('@') {
            Some((u, h)) => (Some(u.to_string()), h),
            None => (None, authority),
        };
        let (host, port) = match hostport.split_once(':') {
            Some((h, p)) => (h.to_string(), p.parse().ok()),
            None => (hostport.to_string(), None),
        };
        Some(Connection {
            name: host.clone(),
            protocol,
            host,
            port,
            share,
            username: userinfo,
            keyring_ref: None,
        })
    }

    /// Build the gvfs/GIO URI for this connection.
    pub fn uri(&self) -> String {
        let port = self.port.filter(|p| *p != self.protocol.default_port());
        let auth = match (&self.username, port) {
            (Some(u), Some(p)) => format!("{u}@{}:{p}", self.host),
            (Some(u), None) => format!("{u}@{}", self.host),
            (None, Some(p)) => format!("{}:{p}", self.host),
            (None, None) => self.host.clone(),
        };
        match &self.share {
            Some(s) => format!("{}://{auth}/{s}", self.protocol.scheme()),
            None => format!("{}://{auth}/", self.protocol.scheme()),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    #[error("the `gio` mount helper is not installed")]
    BackendUnavailable,
    #[error("mount failed: {0}")]
    MountFailed(String),
    #[error("the share mounted but its local path could not be located")]
    PathNotFound,
}

/// Mount a network share via gvfs (`gio mount`). Credentials, if supplied, are
/// fed to the helper's prompts on stdin. On success returns the local path
/// under `$XDG_RUNTIME_DIR/gvfs` where the share is now browsable — which the
/// sandbox already trusts as a network root.
pub fn mount(conn: &Connection, password: Option<&str>) -> Result<PathBuf, NetworkError> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let uri = conn.uri();
    let mut child = Command::new("gio")
        .arg("mount")
        .arg(&uri)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| NetworkError::BackendUnavailable)?;

    // gio prompts for User / Domain / Password on stdin when needed. Feed what
    // we have; blank lines accept defaults / anonymous.
    if let Some(mut stdin) = child.stdin.take() {
        if let Some(user) = &conn.username {
            let _ = writeln!(stdin, "{user}");
        }
        let _ = writeln!(stdin); // domain: default
        if let Some(pw) = password {
            let _ = writeln!(stdin, "{pw}");
        }
    }

    let output = child.wait_with_output().map_err(|e| NetworkError::MountFailed(e.to_string()))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(NetworkError::MountFailed(err.trim().to_string()));
    }

    locate_mount(conn).ok_or(NetworkError::PathNotFound)
}

/// Unmount a previously-mounted gvfs location.
pub fn unmount(local_path: &std::path::Path) -> Result<(), NetworkError> {
    use std::process::Command;
    let status = Command::new("gio")
        .arg("mount")
        .arg("-u")
        .arg(local_path)
        .status()
        .map_err(|_| NetworkError::BackendUnavailable)?;
    if status.success() {
        Ok(())
    } else {
        Err(NetworkError::MountFailed("gio mount -u returned non-zero".into()))
    }
}

// ---- System keyring (Secret Service) ---------------------------------------
//
// Passwords are never written to the config file; they live in the desktop
// keyring (gnome-keyring / kwallet via the Secret Service D-Bus API), keyed by
// the connection's URI. If no keyring backend is running, these degrade
// gracefully (save is a no-op, load returns None).

const KEYRING_SERVICE: &str = "altay-network";

/// Store a password for a connection in the system keyring.
pub fn store_secret(key: &str, password: &str) -> bool {
    match keyring::Entry::new(KEYRING_SERVICE, key) {
        Ok(entry) => entry.set_password(password).is_ok(),
        Err(_) => false,
    }
}

/// Load a previously-saved password for a connection, if present.
pub fn load_secret(key: &str) -> Option<String> {
    keyring::Entry::new(KEYRING_SERVICE, key).ok()?.get_password().ok()
}

/// Forget a saved password.
pub fn forget_secret(key: &str) {
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, key) {
        let _ = entry.delete_credential();
    }
}

/// Find the local gvfs directory for a freshly-mounted connection by matching
/// the host (and share) against the gvfs mount directory names.
fn locate_mount(conn: &Connection) -> Option<PathBuf> {
    let gvfs = dirs::runtime_dir()?.join("gvfs");
    let host = conn.host.to_lowercase();
    let mut best: Option<PathBuf> = None;
    for entry in std::fs::read_dir(&gvfs).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if name.contains(&format!("host={host}")) || name.contains(&format!("server={host}")) {
            // Prefer an entry that also matches the share, if any.
            if let Some(share) = &conn.share {
                if name.contains(&share.to_lowercase()) {
                    return Some(entry.path());
                }
            }
            best = Some(entry.path());
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sftp_uri() {
        let c = Connection::from_uri("sftp://alice@example.com:2222/srv/data").unwrap();
        assert_eq!(c.protocol, Protocol::Sftp);
        assert_eq!(c.host, "example.com");
        assert_eq!(c.port, Some(2222));
        assert_eq!(c.username.as_deref(), Some("alice"));
        assert_eq!(c.share.as_deref(), Some("srv/data"));
    }

    #[test]
    fn parse_smb_uri_no_user() {
        let c = Connection::from_uri("smb://192.168.1.10/media").unwrap();
        assert_eq!(c.protocol, Protocol::Smb);
        assert_eq!(c.host, "192.168.1.10");
        assert_eq!(c.username, None);
        assert_eq!(c.share.as_deref(), Some("media"));
    }

    #[test]
    fn rejects_non_network_scheme() {
        assert!(Connection::from_uri("file:///etc/passwd").is_none());
    }

    #[test]
    fn smb_uri() {
        let c = Connection {
            name: "nas".into(),
            protocol: Protocol::Smb,
            host: "192.168.1.10".into(),
            port: None,
            share: Some("media".into()),
            username: Some("guest".into()),
            keyring_ref: None,
        };
        assert_eq!(c.uri(), "smb://guest@192.168.1.10/media");
    }
}
