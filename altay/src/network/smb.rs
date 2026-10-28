// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! SMB2/3 client for [`netfs`](super::netfs) (the pure-Rust `smb2` crate:
//! signing, encryption, NTLM; no SMB1).
//!
//! `smb://server/` shows the server's shares as folders; `smb://server/share`
//! (or a folder inside it) opens that share. Remote paths here are
//! `/share/folder/file`. Reads are positioned; a write goes on from where the
//! last one stopped, and one that jumps reopens the file at its offset, so
//! files can be changed in place — only truncating to a size other than zero
//! is not offered. Windows has no Unix permissions: taking away the write
//! bits sets the read-only attribute. A dropped connection is made again and
//! the request repeated; a file being read is opened again on it, a write
//! cut off midway fails (what reached the server is not known).

use std::collections::HashMap;
use std::io;
use std::ops::AsyncFn;
use std::time::{Duration, SystemTime};

use smb2::pack::FileTime;
use smb2::types::status::NtStatus;
use smb2::{ClientConfig, ErrorKind, FileAttributes, FileReader, FileTimes, FileWriter, RenameOptions, SmbClient, Tree};
use tokio::sync::Mutex;

use super::netfs::{Change, Kind, OpenMode, Remote, Space, Stat};
use super::{runtime, Connection, NetworkError};

/// A disk share (MS-SRVS); printers, IPC and the special `C$`-style ones
/// carry other values or flags.
const STYPE_DISKTREE: u32 = 0;

fn errno(n: i32) -> io::Error {
    io::Error::from_raw_os_error(n)
}

fn io_error(e: smb2::Error) -> io::Error {
    log::debug!("smb: {e}");
    if e.status() == Some(NtStatus::DIRECTORY_NOT_EMPTY) {
        return errno(libc::ENOTEMPTY);
    }
    errno(match e.kind() {
        ErrorKind::NotFound => libc::ENOENT,
        ErrorKind::AccessDenied | ErrorKind::AuthRequired => libc::EACCES,
        ErrorKind::AlreadyExists => libc::EEXIST,
        ErrorKind::SharingViolation => libc::EBUSY,
        ErrorKind::IsADirectory => libc::EISDIR,
        ErrorKind::NotADirectory => libc::ENOTDIR,
        ErrorKind::DiskFull => libc::ENOSPC,
        ErrorKind::TimedOut => libc::ETIMEDOUT,
        ErrorKind::ConnectionLost | ErrorKind::SessionExpired => libc::ECONNABORTED,
        ErrorKind::InvalidName => libc::EINVAL,
        ErrorKind::Unsupported => libc::ENOTSUP,
        _ => libc::EIO,
    })
}

/// The request failed because the connection did, not the file.
fn lost(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::ECONNABORTED)
}

fn time(t: FileTime) -> SystemTime {
    t.to_system_time().unwrap_or(SystemTime::UNIX_EPOCH)
}

fn dir_stat(mtime: SystemTime) -> Stat {
    Stat { kind: Kind::Dir, size: 0, perm: 0o755, mtime, atime: mtime }
}

fn to_stat(is_dir: bool, size: u64, attrs: FileAttributes, modified: FileTime, accessed: FileTime) -> Stat {
    let perm = match (is_dir, attrs.is_readonly()) {
        (true, _) => 0o755,
        (false, true) => 0o444,
        (false, false) => 0o644,
    };
    let kind = if is_dir { Kind::Dir } else { Kind::File };
    Stat { kind, size, perm, mtime: time(modified), atime: time(accessed) }
}

/// `/share/a/b` → (`share`, `a/b`); the server root is `None`.
fn split(path: &str) -> Option<(&str, &str)> {
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        return None;
    }
    Some(path.split_once('/').unwrap_or((path, "")))
}

struct Inner {
    client: SmbClient,
    /// Shares connected so far.
    trees: HashMap<String, Tree>,
}

impl Inner {
    /// Connect again: the shares connected on the old session are gone too.
    async fn reconnect(&mut self) -> io::Result<()> {
        log::info!("smb: connection lost, connecting again");
        self.trees.clear();
        self.client.reconnect().await.map_err(io_error)
    }

    async fn tree(&mut self, share: &str) -> io::Result<Tree> {
        if let Some(t) = self.trees.get(share) {
            return Ok(t.clone());
        }
        let tree = self.client.connect_share(share).await.map_err(io_error)?;
        self.trees.insert(share.to_string(), tree.clone());
        Ok(tree)
    }

    /// A path inside a share, for the operations that need one: the server
    /// root and the shares themselves are not files.
    async fn inside<'p>(&mut self, path: &'p str) -> io::Result<(Tree, &'p str)> {
        match split(path) {
            Some((share, rest)) if !rest.is_empty() => Ok((self.tree(share).await?, rest)),
            _ => Err(errno(libc::EACCES)),
        }
    }
}

pub struct Smb {
    inner: Mutex<Inner>,
}

/// Connect and log in. Returns the client and the remote folder to show: the
/// share (and folder) asked for, or the server root listing its shares.
pub fn connect(conn: &Connection, password: Option<&str>) -> Result<(Smb, String), NetworkError> {
    runtime().block_on(connect_async(conn, password))
}

async fn connect_async(conn: &Connection, password: Option<&str>) -> Result<(Smb, String), NetworkError> {
    // `smb://DOMAIN;user@server` names the user's domain.
    let (domain, user) = match conn.username.as_deref().unwrap_or_default().split_once(';') {
        Some((d, u)) => (d.to_string(), u.to_string()),
        None => (String::new(), conn.username.clone().unwrap_or_default()),
    };
    let config = ClientConfig {
        addr: format!("{}:{}", conn.host, conn.port.unwrap_or(445)),
        timeout: Duration::from_secs(10),
        username: user.clone(),
        password: password.unwrap_or_default().to_string(),
        domain,
        auto_reconnect: true,
        ..Default::default()
    };
    let client = SmbClient::connect(config).await.map_err(|e| {
        let logon = e.status() == Some(NtStatus::LOGON_FAILURE);
        match e.kind() {
            _ if logon && password.is_none() => NetworkError::PasswordNeeded,
            ErrorKind::AuthRequired if !user.is_empty() && password.is_none() => NetworkError::PasswordNeeded,
            _ if logon => NetworkError::AuthFailed,
            ErrorKind::AuthRequired | ErrorKind::GuestSessionRefused | ErrorKind::AccessDenied => NetworkError::AuthFailed,
            ErrorKind::ConnectionLost | ErrorKind::TimedOut | ErrorKind::Io => NetworkError::Connect(io_error(e)),
            _ => NetworkError::MountFailed(e.to_string()),
        }
    })?;
    let mut inner = Inner { client, trees: HashMap::new() };
    let root = match &conn.share {
        Some(share) => {
            let root = format!("/{}", share.trim_matches('/'));
            let (name, rest) = split(&root).unwrap_or_default();
            inner.tree(name).await.map_err(|e| match e.raw_os_error() {
                Some(libc::ENOENT) => NetworkError::ShareNotFound(name.to_string()),
                Some(libc::EACCES) => NetworkError::AuthFailed,
                _ => NetworkError::MountFailed(e.to_string()),
            })?;
            if !rest.is_empty() {
                let tree = inner.trees.get_mut(name).expect("tree");
                let info = inner.client.stat(tree, rest).await.map_err(|e| NetworkError::MountFailed(format!("{root}: {e}")))?;
                if !info.is_directory {
                    return Err(NetworkError::MountFailed(format!("{root}: not a folder")));
                }
            }
            root
        }
        None => "/".to_string(),
    };
    Ok((Smb { inner: Mutex::new(inner) }, root))
}

/// An open file. A writer is opened at the offset of the first write (or
/// right away, for a file created or emptied by the opening).
pub enum Handle {
    Read(Mutex<Reading>),
    Write(Mutex<Writer>),
}

pub struct Reading {
    path: String,
    reader: FileReader,
}

pub struct Writer {
    path: String,
    writer: Option<FileWriter>,
    /// Where the next write lands without reopening.
    pos: u64,
}

impl Smb {
    /// Run `op`; if the connection was lost, connect again and run it once
    /// more. (The library repeats only what is safe to repeat by itself;
    /// here a request failing on a dead connection never reached a server
    /// that would have acted on it.)
    fn run<T>(&self, op: impl AsyncFn(&mut Inner) -> io::Result<T>) -> io::Result<T> {
        runtime().block_on(async {
            let mut inner = self.inner.lock().await;
            match op(&mut inner).await {
                Err(e) if lost(&e) => {
                    inner.reconnect().await?;
                    op(&mut inner).await
                }
                r => r,
            }
        })
    }

    /// Finish the open writer: the server has every byte.
    async fn finish(w: &mut Writer) -> io::Result<()> {
        match w.writer.take() {
            Some(writer) => writer.finish().await.map(drop).map_err(io_error),
            None => Ok(()),
        }
    }
}

impl Remote for Smb {
    type Handle = Handle;

    fn stat(&self, path: &str) -> io::Result<Stat> {
        self.run(async |inner| {
            match split(path) {
                None => Ok(dir_stat(SystemTime::now())),
                Some((share, "")) => inner.tree(share).await.map(|_| dir_stat(SystemTime::now())),
                Some((share, rest)) => {
                    let mut tree = inner.tree(share).await?;
                    let i = inner.client.stat(&mut tree, rest).await.map_err(io_error)?;
                    Ok(to_stat(i.is_directory, i.size, i.attributes, i.modified, i.accessed))
                }
            }
        })
    }

    fn list(&self, path: &str) -> io::Result<Vec<(String, Stat)>> {
        self.run(async |inner| {
            let Some((share, rest)) = split(path) else {
                // The server root: its disk shares, without the hidden ones.
                let now = SystemTime::now();
                let shares = inner.client.list_shares().await.map_err(io_error)?;
                return Ok(shares
                    .into_iter()
                    .filter(|s| s.share_type == STYPE_DISKTREE && !s.name.ends_with('$'))
                    .map(|s| (s.name, dir_stat(now)))
                    .collect());
            };
            let mut tree = inner.tree(share).await?;
            let entries = inner.client.list_directory(&mut tree, rest).await.map_err(io_error)?;
            Ok(entries
                .into_iter()
                .filter(|e| e.name != "." && e.name != ".." && !e.name.contains('/'))
                .map(|e| {
                    let st = to_stat(e.is_directory, e.size, e.attributes, e.modified, e.modified);
                    (e.name, st)
                })
                .collect())
        })
    }

    fn open(&self, path: &str, mode: OpenMode) -> io::Result<Handle> {
        if mode.read && mode.write {
            return Err(errno(libc::ENOTSUP));
        }
        self.run(async |inner| {
            let (tree, rest) = inner.inside(path).await?;
            if !mode.write {
                let reader = inner.client.open_file_reader(&tree, rest).await.map_err(io_error)?;
                return Ok(Handle::Read(Mutex::new(Reading { path: path.to_string(), reader })));
            }
            let (writer, pos) = if mode.create.is_some() && mode.exclusive {
                (Some(inner.client.create_file_writer_exclusive(&tree, rest).await.map_err(io_error)?), 0)
            } else if mode.create.is_some() || mode.truncate {
                (Some(inner.client.create_file_writer(&tree, rest).await.map_err(io_error)?), 0)
            } else if mode.append {
                let mut t = tree.clone();
                (None, inner.client.stat(&mut t, rest).await.map_err(io_error)?.size)
            } else {
                // Opened at the offset of the first write.
                (None, u64::MAX)
            };
            Ok(Handle::Write(Mutex::new(Writer { path: path.to_string(), writer, pos })))
        })
    }

    fn read(&self, handle: &Handle, offset: u64, size: u32) -> io::Result<Vec<u8>> {
        let Handle::Read(r) = handle else { return Err(errno(libc::EBADF)) };
        runtime().block_on(async {
            let mut r = r.lock().await;
            match r.reader.read_at(offset, size.into()).await.map_err(io_error) {
                // The file was open on the lost connection: open it again.
                Err(e) if lost(&e) => {
                    let mut inner = self.inner.lock().await;
                    inner.reconnect().await?;
                    let (tree, rest) = inner.inside(&r.path).await?;
                    r.reader = inner.client.open_file_reader(&tree, rest).await.map_err(io_error)?;
                    r.reader.read_at(offset, size.into()).await.map_err(io_error)
                }
                res => res,
            }
        })
    }

    fn write(&self, handle: &Handle, offset: u64, data: &[u8]) -> io::Result<()> {
        let Handle::Write(w) = handle else { return Err(errno(libc::EBADF)) };
        runtime().block_on(async {
            let mut w = w.lock().await;
            if w.writer.is_none() || w.pos != offset {
                Self::finish(&mut w).await?;
                let mut inner = self.inner.lock().await;
                let (tree, rest) = inner.inside(&w.path).await?;
                let writer = inner.client.create_file_writer_at(&tree, rest, offset).await.map_err(io_error)?;
                w.writer = Some(writer);
                w.pos = offset;
            }
            w.writer.as_mut().unwrap().write_chunk(data).await.map_err(io_error)?;
            w.pos += data.len() as u64;
            Ok(())
        })
    }

    fn flush(&self, handle: &Handle) -> io::Result<()> {
        match handle {
            Handle::Write(w) => runtime().block_on(async { Self::finish(&mut *w.lock().await).await }),
            Handle::Read(_) => Ok(()),
        }
    }

    fn close(&self, handle: Handle) -> io::Result<()> {
        runtime().block_on(async {
            match handle {
                Handle::Write(w) => Self::finish(&mut w.into_inner()).await,
                Handle::Read(r) => r.into_inner().reader.close().await.map_err(io_error),
            }
        })
    }

    fn space(&self, path: &str) -> io::Result<Space> {
        self.run(async |inner| {
            let Some((share, _)) = split(path) else { return Err(errno(libc::ENOSYS)) };
            let mut tree = inner.tree(share).await?;
            let i = inner.client.fs_info(&mut tree).await.map_err(io_error)?;
            Ok(Space { total: i.total_bytes, free: i.total_free_bytes, avail: i.free_bytes })
        })
    }

    fn mkdir(&self, path: &str, _perm: u16) -> io::Result<()> {
        self.run(async |inner| {
            let (tree, rest) = inner.inside(path).await?;
            inner.client.create_directory(&tree, rest).await.map_err(io_error)
        })
    }

    fn remove_file(&self, path: &str) -> io::Result<()> {
        self.run(async |inner| {
            let (tree, rest) = inner.inside(path).await?;
            inner.client.delete_file(&tree, rest).await.map_err(io_error)
        })
    }

    fn remove_dir(&self, path: &str) -> io::Result<()> {
        self.run(async |inner| {
            let (tree, rest) = inner.inside(path).await?;
            inner.client.delete_directory(&tree, rest).await.map_err(io_error)
        })
    }

    fn rename(&self, from: &str, to: &str, replace: bool) -> io::Result<()> {
        // SMB renames within a share only.
        if split(from).map(|(s, _)| s) != split(to).map(|(s, _)| s) {
            return Err(errno(libc::EXDEV));
        }
        self.run(async |inner| {
            let (tree, from) = inner.inside(from).await?;
            let (_, to) = inner.inside(to).await?;
            let options = RenameOptions::new().replace_if_exists(replace);
            inner.client.rename_with(&tree, from, to, options).await.map_err(io_error)
        })
    }

    fn change(&self, path: &str, change: Change) -> io::Result<()> {
        self.run(async |inner| {
            let (tree, rest) = inner.inside(path).await?;
            match change.size {
                Some(0) => {
                    let w = inner.client.create_file_writer(&tree, rest).await.map_err(io_error)?;
                    w.finish().await.map_err(io_error)?;
                }
                Some(n) => {
                    let mut t = tree.clone();
                    if inner.client.stat(&mut t, rest).await.map_err(io_error)?.size != n {
                        return Err(errno(libc::ENOTSUP));
                    }
                }
                None => {}
            }
            let mut times = FileTimes::new();
            if let Some((a, m)) = change.times {
                times = times.set_accessed(FileTime::from_system_time(a)).set_modified(FileTime::from_system_time(m));
            }
            if let Some(perm) = change.perm {
                // No owner bits on Windows: no write bit for anyone is
                // "read-only", anything else is not.
                let mut t = tree.clone();
                let attrs = inner.client.stat(&mut t, rest).await.map_err(io_error)?.attributes;
                let readonly = perm & 0o222 == 0;
                if attrs.is_readonly() != readonly {
                    let bits = if readonly { attrs.bits() | FileAttributes::READONLY } else { attrs.bits() & !FileAttributes::READONLY };
                    times = times.set_attributes(FileAttributes::new(bits));
                }
            }
            if times != FileTimes::new() {
                inner.client.set_times(&tree, rest, times).await.map_err(io_error)?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_share_and_path() {
        assert_eq!(split("/"), None);
        assert_eq!(split("/media"), Some(("media", "")));
        assert_eq!(split("/media/a/b.txt"), Some(("media", "a/b.txt")));
    }
}
