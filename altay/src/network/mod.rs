// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Network locations (SMB/CIFS, NFS, SFTP, FTP, WebDAV).
//!
//! Altay speaks SFTP, FTP, WebDAV and SMB itself and serves the share as a
//! FUSE mount under `$XDG_RUNTIME_DIR/altay-net` (see [`netfs`]). NFS still
//! goes through gvfs (`gio mount`, under `$XDG_RUNTIME_DIR/gvfs`) when it is
//! installed. Both are `Network` roots of the [`security`](crate::security)
//! sandbox. Passwords live in the system keyring, never in the config.

mod ftp;
pub mod netfs;
mod sftp;
mod smb;
pub mod tls;
mod webdav;

use std::path::PathBuf;
use std::sync::OnceLock;

/// The runtime the async clients (SSH, SMB) live on; FUSE threads block on it.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("net-io")
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

/// Supported network protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Smb,
    Nfs,
    Sftp,
    Ftp,
    /// FTP over TLS (`ftps://`): explicit, or implicit on port 990.
    Ftps,
    /// WebDAV over HTTP (`dav://`).
    WebDav,
    /// WebDAV over HTTPS (`davs://`).
    WebDavs,
}

impl Protocol {
    /// URI scheme used when handing the location to gvfs.
    pub fn scheme(self) -> &'static str {
        match self {
            Protocol::Smb => "smb",
            Protocol::Nfs => "nfs",
            Protocol::Sftp => "sftp",
            Protocol::Ftp => "ftp",
            Protocol::Ftps => "ftps",
            Protocol::WebDav => "dav",
            Protocol::WebDavs => "davs",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Smb => 445,
            Protocol::Nfs => 2049,
            Protocol::Sftp => 22,
            Protocol::Ftp | Protocol::Ftps => 21,
            Protocol::WebDav => 80,
            Protocol::WebDavs => 443,
        }
    }

    /// Parse a URI scheme into a protocol.
    pub fn from_scheme(scheme: &str) -> Option<Protocol> {
        Some(match scheme {
            "smb" | "cifs" => Protocol::Smb,
            "nfs" => Protocol::Nfs,
            "sftp" | "ssh" => Protocol::Sftp,
            "ftp" => Protocol::Ftp,
            "ftps" => Protocol::Ftps,
            "dav" | "webdav" => Protocol::WebDav,
            "davs" | "webdavs" => Protocol::WebDavs,
            _ => return None,
        })
    }
}

/// A saved network connection. Passwords are never stored here — they live in
/// the system keyring, keyed by the connection's URI.
#[derive(Debug, Clone)]
pub struct Connection {
    pub name: String,
    pub protocol: Protocol,
    pub host: String,
    pub port: Option<u16>,
    pub share: Option<String>,
    pub username: Option<String>,
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
        })
    }

    /// The name of its mount folder under [`netfs::base_dir`]: readable, and
    /// distinct per server, user and share.
    fn dir_name(&self) -> String {
        let mut name = format!("{}-", self.protocol.scheme());
        if let Some(u) = &self.username {
            name += &format!("{u}@");
        }
        name += &self.host;
        if let Some(p) = self.port.filter(|p| *p != self.protocol.default_port()) {
            name += &format!("-{p}");
        }
        if let Some(s) = &self.share {
            name += &format!("-{s}");
        }
        name.chars().map(|c| if c.is_alphanumeric() || "@.-_".contains(c) { c } else { '_' }).collect()
    }

    /// How the sidebar names the mounted share, e.g. `user@host / data (SFTP)`.
    fn label(&self) -> String {
        let mut detail = match &self.username {
            Some(u) => format!("{u}@{}", self.host),
            None => self.host.clone(),
        };
        if let Some(s) = &self.share {
            detail += &format!(" / {}", s.trim_matches('/'));
        }
        format!("{detail} ({})", self.protocol.scheme().to_uppercase())
    }

    /// Build the URI for this connection.
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
    /// NFS goes through gvfs, which is not installed.
    #[error("the `gio` mount helper is not installed")]
    BackendUnavailable,
    #[error("cannot reach the server: {0}")]
    Connect(std::io::Error),
    #[error("the server refused the user name or password")]
    AuthFailed,
    #[error("the server asks for a password")]
    PasswordNeeded,
    #[error("the server's host key differs from the one in ~/.ssh/known_hosts")]
    HostKeyChanged,
    /// The server's certificate is not signed by a known authority and was
    /// never accepted: the user may compare and accept its fingerprint.
    #[error("the server's certificate is not trusted (SHA-256 {fingerprint})")]
    UnknownCertificate { host: String, port: u16, fingerprint: String },
    #[error("the server's certificate differs from the one accepted before")]
    CertificateChanged,
    /// An SMB server without the share asked for.
    #[error("the server has no share named {0}")]
    ShareNotFound(String),
    /// `ftps://` to a server without TLS: refused rather than sent in clear.
    #[error("the server does not offer TLS")]
    NoTls,
    #[error("mount failed: {0}")]
    MountFailed(String),
    #[error("the share mounted but its local path could not be located")]
    PathNotFound,
}

/// Mount a network share and return the local folder where it is now
/// browsable — a network root of the sandbox. A share already mounted by
/// altay is not mounted again.
pub fn mount(conn: &Connection, password: Option<&str>) -> Result<PathBuf, NetworkError> {
    let key = conn.uri();
    if let Some(path) = netfs::find(&key) {
        return Ok(path);
    }
    match conn.protocol {
        Protocol::Sftp => serve(conn, sftp::connect(conn, password)?),
        Protocol::Ftp | Protocol::Ftps => serve(conn, ftp::connect(conn, password)?),
        Protocol::WebDav | Protocol::WebDavs => serve(conn, webdav::connect(conn, password)?),
        Protocol::Smb => serve(conn, smb::connect(conn, password)?),
        Protocol::Nfs => mount_gio(conn, password),
    }
}

/// Serve a connected client's `root` folder through altay's FUSE server.
fn serve<R: netfs::Remote>(conn: &Connection, (client, root): (R, String)) -> Result<PathBuf, NetworkError> {
    netfs::mount(client, &root, &conn.uri(), &conn.dir_name(), &conn.label()).map_err(|e| NetworkError::MountFailed(e.to_string()))
}

/// Mount through gvfs (`gio mount`). Credentials, if supplied, are fed to the
/// helper's prompts on stdin. The share appears under `$XDG_RUNTIME_DIR/gvfs`.
fn mount_gio(conn: &Connection, password: Option<&str>) -> Result<PathBuf, NetworkError> {
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
        return Err(NetworkError::MountFailed(gio_reason(err.trim()).to_string()));
    }

    locate_mount(conn).ok_or(NetworkError::PathNotFound)
}

/// The reason in gio's "gio: <uri>: <reason>" — the dialog already shows the
/// address, and gio rewrites it (e.g. adds a trailing slash).
fn gio_reason(stderr: &str) -> &str {
    stderr
        .strip_prefix("gio: ")
        .and_then(|rest| rest.split_once(": "))
        .filter(|(uri, _)| uri.contains("://"))
        .map_or(stderr, |(_, reason)| reason)
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
    fn gio_reason_drops_the_address() {
        assert_eq!(gio_reason("gio: smb://10.0.0.1/pay/: Failed to mount Windows share: Connection timed out"), "Failed to mount Windows share: Connection timed out");
        assert_eq!(gio_reason("gio: sftp://127.0.0.1:2/: Connection refused by server"), "Connection refused by server");
        assert_eq!(gio_reason("something else"), "something else");
    }

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
    fn mount_folder_name_and_label() {
        let c = Connection::from_uri("sftp://os2@192.168.1.16:2222/srv/my data").unwrap();
        assert_eq!(c.dir_name(), "sftp-os2@192.168.1.16-2222-srv_my_data");
        assert_eq!(c.label(), "os2@192.168.1.16 / srv/my data (SFTP)");
        let c = Connection::from_uri("sftp://host/").unwrap();
        assert_eq!(c.dir_name(), "sftp-host");
        assert_eq!(c.label(), "host (SFTP)");
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
        };
        assert_eq!(c.uri(), "smb://guest@192.168.1.10/media");
    }
}
