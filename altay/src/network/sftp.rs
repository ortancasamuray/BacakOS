// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! SFTP client for [`netfs`](super::netfs): an SSH connection (russh) with
//! the SFTP subsystem on top (russh-sftp), driven from the FUSE threads.
//!
//! Host keys follow OpenSSH's `accept-new`: a host seen for the first time is
//! recorded in `~/.ssh/known_hosts`, a host whose key changed is refused.
//! Authentication tries the ssh-agent, then the usual key files without a
//! passphrase, then the password.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use russh::client;
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::RawSftpSession;
use russh_sftp::protocol::{FileAttributes, OpenFlags, Packet, StatusCode};

use super::netfs::{Change, Kind, OpenMode, Remote, Space, Stat};
use super::{Connection, NetworkError};

/// Gives up on an unreachable host / a silent server after this long.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A request the server does not answer in time fails (instead of hanging
/// the folder that asked).
const REQUEST_TIMEOUT_SECS: u64 = 15;
/// A write is sent in pieces of this size, all at once (servers take up to
/// 256 KiB; OpenSSH's own client uses 32 KiB).
const WRITE_CHUNK: usize = 32 * 1024;
const POSIX_RENAME: &str = "posix-rename@openssh.com";

/// The runtime the SSH connections live on; FUSE threads block on it.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("net-ssh")
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

struct HostCheck {
    host: String,
    port: u16,
    /// Set when the refusal was a changed host key, not a network error.
    key_changed: Arc<AtomicBool>,
}

impl client::Handler for HostCheck {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let key = key.public_key();
        match russh::keys::check_known_hosts(&self.host, self.port, &key) {
            Ok(true) => Ok(true),
            Ok(false) => {
                if let Err(e) = russh::keys::known_hosts::learn_known_hosts(&self.host, self.port, &key) {
                    log::warn!("could not record {}'s host key: {e}", self.host);
                }
                Ok(true)
            }
            Err(russh::keys::Error::KeyChanged { line }) => {
                log::warn!("{}: host key differs from known_hosts line {line}", self.host);
                self.key_changed.store(true, Ordering::Relaxed);
                Ok(false)
            }
            // An unreadable known_hosts: no record to contradict this key.
            Err(e) => {
                log::warn!("known_hosts: {e}");
                Ok(true)
            }
        }
    }
}

pub struct Sftp {
    sftp: RawSftpSession,
    /// The server can rename over an existing file in one step.
    posix_rename: bool,
    /// Keeps the SSH connection open while the share is mounted.
    _ssh: client::Handle<HostCheck>,
}

/// Connect, log in and start SFTP. Returns the client and the remote folder
/// to show: the share's path, or the user's home on the server.
pub fn connect(conn: &Connection, password: Option<&str>) -> Result<(Sftp, String), NetworkError> {
    runtime().block_on(connect_async(conn, password))
}

async fn connect_async(conn: &Connection, password: Option<&str>) -> Result<(Sftp, String), NetworkError> {
    let port = conn.port.unwrap_or(22);
    let user = conn.username.clone().or_else(|| std::env::var("USER").ok()).unwrap_or_default();
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect((conn.host.as_str(), port)))
        .await
        .map_err(|_| NetworkError::Connect(io::ErrorKind::TimedOut.into()))?
        .map_err(NetworkError::Connect)?;
    let config = client::Config {
        keepalive_interval: Some(Duration::from_secs(15)),
        ..Default::default()
    };
    let key_changed = Arc::new(AtomicBool::new(false));
    let handler = HostCheck { host: conn.host.clone(), port, key_changed: key_changed.clone() };
    let mut ssh = match tokio::time::timeout(CONNECT_TIMEOUT, client::connect_stream(Arc::new(config), stream, handler)).await {
        Ok(Ok(h)) => h,
        Ok(Err(_)) if key_changed.load(Ordering::Relaxed) => return Err(NetworkError::HostKeyChanged),
        Ok(Err(e)) => return Err(NetworkError::MountFailed(e.to_string())),
        Err(_) => return Err(NetworkError::Connect(io::ErrorKind::TimedOut.into())),
    };
    if !authenticate(&mut ssh, &user, password).await {
        return Err(if password.is_some() { NetworkError::AuthFailed } else { NetworkError::PasswordNeeded });
    }
    let failed = |e: russh::Error| NetworkError::MountFailed(e.to_string());
    let channel = ssh.channel_open_session().await.map_err(failed)?;
    channel.request_subsystem(true, "sftp").await.map_err(failed)?;
    let sftp = RawSftpSession::new(channel.into_stream());
    sftp.set_timeout(REQUEST_TIMEOUT_SECS);
    let sftp_failed = |e: SftpError| NetworkError::MountFailed(e.to_string());
    let version = sftp.init().await.map_err(sftp_failed)?;
    let posix_rename = version.extensions.get(POSIX_RENAME).is_some_and(|v| v == "1");
    let root = match &conn.share {
        Some(share) => format!("/{}", share.trim_matches('/')),
        None => sftp.realpath(".").await.map_err(sftp_failed)?.files.first().map(|f| f.filename.clone()).unwrap_or_else(|| "/".into()),
    };
    let attrs = sftp.stat(root.as_str()).await.map_err(sftp_failed)?.attrs;
    if !attrs.is_dir() {
        return Err(NetworkError::MountFailed(format!("{root}: not a folder")));
    }
    Ok((Sftp { sftp, posix_rename, _ssh: ssh }, root))
}

/// Try the ssh-agent's keys, the default key files, then the password.
async fn authenticate(ssh: &mut client::Handle<HostCheck>, user: &str, password: Option<&str>) -> bool {
    let rsa_hash = ssh.best_supported_rsa_hash().await.ok().flatten().flatten();
    if let Ok(mut agent) = russh::keys::agent::client::AgentClient::connect_env().await {
        for id in agent.request_identities().await.unwrap_or_default() {
            let key = id.public_key().into_owned();
            if let Ok(r) = ssh.authenticate_publickey_with(user, key, rsa_hash, &mut agent).await {
                if r.success() {
                    return true;
                }
            }
        }
    }
    if let Some(home) = dirs::home_dir() {
        for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
            // A key with a passphrase is skipped: there is no prompt for it.
            let Ok(key) = russh::keys::load_secret_key(home.join(".ssh").join(name), None) else { continue };
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), rsa_hash);
            if ssh.authenticate_publickey(user, key).await.is_ok_and(|r| r.success()) {
                return true;
            }
        }
    }
    match password {
        Some(pw) => ssh.authenticate_password(user, pw).await.is_ok_and(|r| r.success()),
        None => false,
    }
}

fn io_error(e: SftpError) -> io::Error {
    let errno = match &e {
        SftpError::Status(s) => match s.status_code {
            StatusCode::NoSuchFile => libc::ENOENT,
            StatusCode::PermissionDenied => libc::EACCES,
            StatusCode::OpUnsupported => libc::ENOTSUP,
            _ => libc::EIO,
        },
        SftpError::Timeout => libc::ETIMEDOUT,
        _ => libc::EIO,
    };
    log::debug!("sftp: {e}");
    io::Error::from_raw_os_error(errno)
}

fn is_eof(e: &SftpError) -> bool {
    matches!(e, SftpError::Status(s) if s.status_code == StatusCode::Eof)
}

fn time(secs: Option<u32>) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs.unwrap_or(0).into())
}

fn to_stat(a: &FileAttributes) -> Stat {
    Stat {
        kind: if a.is_dir() { Kind::Dir } else { Kind::File },
        size: a.size.unwrap_or(0),
        perm: (a.permissions.unwrap_or(0o644) & 0o7777) as u16,
        mtime: time(a.mtime),
        atime: time(a.atime),
    }
}

impl Sftp {
    async fn exists(&self, path: &str) -> bool {
        self.sftp.lstat(path).await.is_ok()
    }

    /// SFTP v3 has one code for most failures ("Failure"): when the path
    /// turns out to exist, that was the reason.
    async fn exists_error(&self, e: SftpError, path: &str) -> io::Error {
        if matches!(&e, SftpError::Status(s) if s.status_code == StatusCode::Failure) && self.exists(path).await {
            return io::Error::from_raw_os_error(libc::EEXIST);
        }
        io_error(e)
    }

    async fn posix_rename(&self, from: &str, to: &str) -> Result<(), SftpError> {
        let mut data = Vec::with_capacity(8 + from.len() + to.len());
        for s in [from, to] {
            data.extend_from_slice(&(s.len() as u32).to_be_bytes());
            data.extend_from_slice(s.as_bytes());
        }
        match self.sftp.extended(POSIX_RENAME, data).await? {
            Packet::Status(s) if s.status_code == StatusCode::Ok => Ok(()),
            Packet::Status(s) => Err(SftpError::Status(s)),
            _ => Err(SftpError::UnexpectedPacket),
        }
    }

    /// `lstat`'s answer, with a link replaced by what it points at.
    async fn follow(&self, path: &str, attrs: FileAttributes) -> Stat {
        if !attrs.is_symlink() {
            return to_stat(&attrs);
        }
        match self.sftp.stat(path).await {
            Ok(target) => to_stat(&target.attrs),
            Err(_) => Stat { kind: Kind::BrokenLink, ..to_stat(&attrs) },
        }
    }
}

impl Remote for Sftp {
    type Handle = String;

    fn stat(&self, path: &str) -> io::Result<Stat> {
        runtime().block_on(async {
            let attrs = self.sftp.lstat(path).await.map_err(io_error)?.attrs;
            Ok(self.follow(path, attrs).await)
        })
    }

    fn list(&self, path: &str) -> io::Result<Vec<(String, Stat)>> {
        runtime().block_on(async {
            let handle = self.sftp.opendir(path).await.map_err(io_error)?.handle;
            let mut raw = Vec::new();
            let result = loop {
                match self.sftp.readdir(handle.as_str()).await {
                    Ok(name) => raw.extend(name.files),
                    Err(e) if is_eof(&e) => break Ok(()),
                    Err(e) => break Err(io_error(e)),
                }
            };
            let _ = self.sftp.close(handle).await;
            result?;
            let mut out = Vec::with_capacity(raw.len());
            for f in raw {
                if f.filename == "." || f.filename == ".." || f.filename.contains('/') {
                    continue;
                }
                let full = super::netfs::join(path, &f.filename);
                let st = self.follow(&full, f.attrs).await;
                out.push((f.filename, st));
            }
            Ok(out)
        })
    }

    fn open(&self, path: &str, mode: OpenMode) -> io::Result<String> {
        let mut flags = OpenFlags::empty();
        flags.set(OpenFlags::READ, mode.read);
        flags.set(OpenFlags::WRITE, mode.write);
        flags.set(OpenFlags::APPEND, mode.append);
        flags.set(OpenFlags::TRUNCATE, mode.truncate && mode.write);
        flags.set(OpenFlags::CREATE, mode.create.is_some());
        flags.set(OpenFlags::EXCLUDE, mode.exclusive && mode.create.is_some());
        let attrs = FileAttributes { permissions: mode.create.map(u32::from), ..Default::default() };
        runtime().block_on(async {
            match self.sftp.open(path, flags, attrs).await {
                Ok(h) => Ok(h.handle),
                Err(e) if flags.contains(OpenFlags::EXCLUDE) => Err(self.exists_error(e, path).await),
                Err(e) => Err(io_error(e)),
            }
        })
    }

    fn read(&self, handle: &String, offset: u64, size: u32) -> io::Result<Vec<u8>> {
        runtime().block_on(async {
            let mut out = Vec::with_capacity(size as usize);
            // Servers cap one answer (OpenSSH: 256 KiB, some 32 KiB).
            while out.len() < size as usize {
                let want = size - out.len() as u32;
                match self.sftp.read(handle.as_str(), offset + out.len() as u64, want).await {
                    Ok(d) if d.data.is_empty() => break,
                    Ok(d) => out.extend_from_slice(&d.data),
                    Err(e) if is_eof(&e) => break,
                    Err(e) => return Err(io_error(e)),
                }
            }
            Ok(out)
        })
    }

    fn write(&self, handle: &String, offset: u64, data: &[u8]) -> io::Result<()> {
        runtime().block_on(async {
            let writes = data.chunks(WRITE_CHUNK).enumerate().map(|(i, chunk)| {
                self.sftp.write(handle.as_str(), offset + (i * WRITE_CHUNK) as u64, chunk.to_vec())
            });
            futures_util::future::try_join_all(writes).await.map_err(io_error)?;
            Ok(())
        })
    }

    fn close(&self, handle: String) -> io::Result<()> {
        runtime().block_on(async { self.sftp.close(handle).await.map(drop).map_err(io_error) })
    }

    fn mkdir(&self, path: &str, perm: u16) -> io::Result<()> {
        let attrs = FileAttributes { permissions: Some(perm.into()), ..Default::default() };
        runtime().block_on(async {
            match self.sftp.mkdir(path, attrs).await {
                Ok(_) => Ok(()),
                Err(e) => Err(self.exists_error(e, path).await),
            }
        })
    }

    fn remove_file(&self, path: &str) -> io::Result<()> {
        runtime().block_on(async { self.sftp.remove(path).await.map(drop).map_err(io_error) })
    }

    fn remove_dir(&self, path: &str) -> io::Result<()> {
        runtime().block_on(async {
            match self.sftp.rmdir(path).await {
                Ok(_) => Ok(()),
                // "Failure" on a folder that is still there: it is not empty.
                Err(SftpError::Status(s)) if s.status_code == StatusCode::Failure && self.exists(path).await => {
                    Err(io::Error::from_raw_os_error(libc::ENOTEMPTY))
                }
                Err(e) => Err(io_error(e)),
            }
        })
    }

    fn rename(&self, from: &str, to: &str, replace: bool) -> io::Result<()> {
        runtime().block_on(async {
            // A plain SFTP rename never replaces: it fails on an existing target.
            if !replace {
                return match self.sftp.rename(from, to).await {
                    Ok(_) => Ok(()),
                    Err(e) => Err(self.exists_error(e, to).await),
                };
            }
            if self.posix_rename {
                return self.posix_rename(from, to).await.map_err(io_error);
            }
            match self.sftp.rename(from, to).await {
                Ok(_) => Ok(()),
                // Without the extension: remove the file in the way, then
                // move (not atomic, as close as SFTP v3 gets).
                Err(e) => {
                    if !self.exists(to).await {
                        return Err(io_error(e));
                    }
                    self.sftp.remove(to).await.map_err(io_error)?;
                    self.sftp.rename(from, to).await.map(drop).map_err(io_error)
                }
            }
        })
    }

    fn change(&self, path: &str, change: Change) -> io::Result<()> {
        let secs = |t: SystemTime| t.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() as u32);
        let attrs = FileAttributes {
            permissions: change.perm.map(u32::from),
            size: change.size,
            atime: change.times.map(|(a, _)| secs(a)),
            mtime: change.times.map(|(_, m)| secs(m)),
            ..Default::default()
        };
        runtime().block_on(async { self.sftp.setstat(path, attrs).await.map(drop).map_err(io_error) })
    }

    fn space(&self, path: &str) -> io::Result<Space> {
        runtime().block_on(async {
            let s = self.sftp.statvfs(path).await.map_err(io_error)?;
            Ok(Space {
                total: s.blocks * s.fragment_size,
                free: s.blocks_free * s.fragment_size,
                avail: s.blocks_avail * s.fragment_size,
            })
        })
    }
}
