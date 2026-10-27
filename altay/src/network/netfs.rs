// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! A network share served as a local folder: altay answers the kernel's FUSE
//! requests itself (through `fusermount3`, no root) by asking a [`Remote`]
//! protocol client. Everything else in altay — listing, copying, preview,
//! archives — then works on the share through plain paths, and the sandbox
//! only needs to know the mount point (see [`active`]).
//!
//! The share lives as long as altay: [`unmount_all`] runs on exit, and a
//! mount point left behind by a crash is cleared before it is reused.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, LockOwner, OpenFlags,
    ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, Request,
};

/// What a protocol client tells the FUSE layer about one remote entry.
#[derive(Debug, Clone, Copy)]
pub struct Stat {
    pub kind: Kind,
    pub size: u64,
    /// Permission bits (`0o7777`), as the server reports them.
    pub perm: u16,
    pub mtime: SystemTime,
    pub atime: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    /// A link the server could not follow (dangling, or a loop).
    BrokenLink,
}

/// Space on the remote filesystem, in bytes.
#[derive(Debug, Clone, Copy)]
pub struct Space {
    pub total: u64,
    pub free: u64,
    pub avail: u64,
}

/// A connected protocol client. Calls block; they run on the FUSE worker
/// threads and must give up (with an error) on a dead connection rather than
/// hang. Paths are absolute remote paths joined with `/`; links are followed.
pub trait Remote: Send + Sync + 'static {
    type Handle: Send + Sync;
    fn stat(&self, path: &str) -> io::Result<Stat>;
    fn list(&self, path: &str) -> io::Result<Vec<(String, Stat)>>;
    fn open(&self, path: &str) -> io::Result<Self::Handle>;
    /// Up to `size` bytes at `offset`; fewer only at the end of the file.
    fn read(&self, handle: &Self::Handle, offset: u64, size: u32) -> io::Result<Vec<u8>>;
    fn close(&self, handle: Self::Handle);
    fn space(&self, path: &str) -> io::Result<Space>;
}

/// How long the kernel (and our own lookup cache) may trust an answer.
/// Short: other clients change the share behind our back.
const TTL: Duration = Duration::from_secs(2);

struct Node {
    path: String,
    /// The attributes a recent listing or lookup gave, with when.
    stat: Option<(Stat, Instant)>,
}

struct Inodes {
    nodes: HashMap<u64, Node>,
    by_path: HashMap<String, u64>,
    next: u64,
}

impl Inodes {
    fn ino_for(&mut self, path: &str) -> u64 {
        if let Some(&ino) = self.by_path.get(path) {
            return ino;
        }
        let ino = self.next;
        self.next += 1;
        self.nodes.insert(ino, Node { path: path.to_string(), stat: None });
        self.by_path.insert(path.to_string(), ino);
        ino
    }
}

struct NetFs<R: Remote> {
    remote: R,
    inodes: Mutex<Inodes>,
    /// Open files: our handle number → the client's handle.
    files: Mutex<HashMap<u64, Arc<R::Handle>>>,
    /// Open directories: the listing read at `opendir`, handed out in pages.
    dirs: Mutex<HashMap<u64, Vec<(u64, FileType, String)>>>,
    next_fh: AtomicU64,
    uid: u32,
    gid: u32,
}

fn errno(e: &io::Error) -> Errno {
    Errno::from_i32(e.raw_os_error().unwrap_or(libc::EIO))
}

pub(super) fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

impl<R: Remote> NetFs<R> {
    fn new(remote: R, root: &str) -> Self {
        let mut inodes = Inodes { nodes: HashMap::new(), by_path: HashMap::new(), next: 2 };
        inodes.nodes.insert(1, Node { path: root.to_string(), stat: None });
        inodes.by_path.insert(root.to_string(), 1);
        // SAFETY: getuid/getgid cannot fail.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        Self {
            remote,
            inodes: Mutex::new(inodes),
            files: Mutex::new(HashMap::new()),
            dirs: Mutex::new(HashMap::new()),
            next_fh: AtomicU64::new(1),
            uid,
            gid,
        }
    }

    fn path_of(&self, ino: INodeNo) -> Option<String> {
        self.inodes.lock().unwrap().nodes.get(&u64::from(ino)).map(|n| n.path.clone())
    }

    /// The entry's attributes: from a fresh listing if there was one, else
    /// asked from the server (and remembered).
    fn stat_of(&self, ino: u64, path: &str) -> io::Result<Stat> {
        if let Some((st, at)) = self.inodes.lock().unwrap().nodes.get(&ino).and_then(|n| n.stat) {
            if at.elapsed() < TTL {
                return Ok(st);
            }
        }
        let st = self.remote.stat(path)?;
        if let Some(n) = self.inodes.lock().unwrap().nodes.get_mut(&ino) {
            n.stat = Some((st, Instant::now()));
        }
        Ok(st)
    }

    fn attr(&self, ino: u64, st: &Stat) -> FileAttr {
        let kind = match st.kind {
            Kind::Dir => FileType::Directory,
            Kind::File => FileType::RegularFile,
            Kind::BrokenLink => FileType::Symlink,
        };
        FileAttr {
            ino: INodeNo(ino),
            size: st.size,
            blocks: st.size.div_ceil(512),
            atime: st.atime,
            mtime: st.mtime,
            ctime: st.mtime,
            crtime: st.mtime,
            kind,
            perm: st.perm,
            nlink: if kind == FileType::Directory { 2 } else { 1 },
            // The share is the logged-in user's: whoever owns a file there,
            // altay shows it as theirs (the server enforces the real rights).
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            flags: 0,
            blksize: 4096,
        }
    }

    fn new_fh(&self) -> u64 {
        self.next_fh.fetch_add(1, Ordering::Relaxed)
    }
}

impl<R: Remote> Filesystem for NetFs<R> {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let (Some(dir), Some(name)) = (self.path_of(parent), name.to_str()) else {
            return reply.error(Errno::ENOENT);
        };
        let path = join(&dir, name);
        let ino = self.inodes.lock().unwrap().ino_for(&path);
        match self.stat_of(ino, &path) {
            Ok(st) => reply.entry(&TTL, &self.attr(ino, &st), Generation(0)),
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let Some(path) = self.path_of(ino) else { return reply.error(Errno::ENOENT) };
        match self.stat_of(ino.into(), &path) {
            Ok(st) => reply.attr(&TTL, &self.attr(ino.into(), &st)),
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn readlink(&self, _req: &Request, _ino: INodeNo, reply: ReplyData) {
        // Only dangling links are shown as links; their target is on the
        // server, and resolving it here would point into the local disk.
        reply.error(Errno::ENOENT);
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        if flags.0 & libc::O_ACCMODE != libc::O_RDONLY {
            return reply.error(Errno::EROFS);
        }
        let Some(path) = self.path_of(ino) else { return reply.error(Errno::ENOENT) };
        match self.remote.open(&path) {
            Ok(h) => {
                let fh = self.new_fh();
                self.files.lock().unwrap().insert(fh, Arc::new(h));
                reply.opened(FileHandle(fh), FopenFlags::empty());
            }
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn read(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        // The read itself must not hold the lock: other files wait on it.
        let Some(h) = self.files.lock().unwrap().get(&u64::from(fh)).cloned() else {
            return reply.error(Errno::EBADF);
        };
        match self.remote.read(&h, offset, size) {
            Ok(data) => reply.data(&data),
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        // `release` comes after the file's last read, so nothing else holds it.
        if let Some(h) = self.files.lock().unwrap().remove(&u64::from(fh)).and_then(|h| Arc::into_inner(h)) {
            self.remote.close(h);
        }
        reply.ok();
    }

    fn opendir(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let Some(path) = self.path_of(ino) else { return reply.error(Errno::ENOENT) };
        let listing = match self.remote.list(&path) {
            Ok(l) => l,
            Err(e) => return reply.error(errno(&e)),
        };
        let now = Instant::now();
        let mut entries = vec![(u64::from(ino), FileType::Directory, ".".to_string()), (1, FileType::Directory, "..".to_string())];
        {
            let mut inodes = self.inodes.lock().unwrap();
            for (name, st) in listing {
                let child = inodes.ino_for(&join(&path, &name));
                if let Some(n) = inodes.nodes.get_mut(&child) {
                    n.stat = Some((st, now));
                }
                let kind = match st.kind {
                    Kind::Dir => FileType::Directory,
                    Kind::File => FileType::RegularFile,
                    Kind::BrokenLink => FileType::Symlink,
                };
                entries.push((child, kind, name));
            }
        }
        let fh = self.new_fh();
        self.dirs.lock().unwrap().insert(fh, entries);
        reply.opened(FileHandle(fh), FopenFlags::empty());
    }

    fn readdir(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
        let dirs = self.dirs.lock().unwrap();
        let Some(entries) = dirs.get(&u64::from(fh)) else { return reply.error(Errno::EBADF) };
        for (i, (ino, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
            if reply.add(INodeNo(*ino), (i + 1) as u64, *kind, name) {
                break;
            }
        }
        reply.ok();
    }

    fn releasedir(&self, _req: &Request, _ino: INodeNo, fh: FileHandle, _flags: OpenFlags, reply: ReplyEmpty) {
        self.dirs.lock().unwrap().remove(&u64::from(fh));
        reply.ok();
    }

    fn statfs(&self, _req: &Request, ino: INodeNo, reply: ReplyStatfs) {
        const BSIZE: u64 = 4096;
        let space = self.path_of(ino).and_then(|p| self.remote.space(&p).ok());
        match space {
            Some(s) => reply.statfs(s.total / BSIZE, s.free / BSIZE, s.avail / BSIZE, 0, 0, BSIZE as u32, 255, BSIZE as u32),
            // Unknown: report no size rather than fail (`df`, the sidebar).
            None => reply.statfs(0, 0, 0, 0, 0, BSIZE as u32, 255, BSIZE as u32),
        }
    }
}

// ---- Mount registry -----------------------------------------------------------

struct Mounted {
    key: String,
    path: PathBuf,
    label: String,
    session: fuser::BackgroundSession,
}

static MOUNTS: Mutex<Vec<Mounted>> = Mutex::new(Vec::new());

/// Where altay mounts its shares: one folder per connection under here.
pub fn base_dir() -> Option<PathBuf> {
    Some(dirs::runtime_dir()?.join("altay-net"))
}

/// The local folder of the share mounted under `key`, if it is mounted.
pub fn find(key: &str) -> Option<PathBuf> {
    MOUNTS.lock().unwrap().iter().find(|m| m.key == key).map(|m| m.path.clone())
}

/// The shares mounted now: (local folder, sidebar label).
pub fn active() -> Vec<(PathBuf, String)> {
    MOUNTS.lock().unwrap().iter().map(|m| (m.path.clone(), m.label.clone())).collect()
}

/// Serve `remote`'s `root` folder at `<base_dir>/<dir_name>`. `key` identifies
/// the connection (see [`find`]); `label` names it in the sidebar.
pub fn mount<R: Remote>(remote: R, root: &str, key: &str, dir_name: &str, label: &str) -> io::Result<PathBuf> {
    let base = base_dir().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no XDG_RUNTIME_DIR"))?;
    let path = base.join(dir_name);
    clear_if_stale(&path);
    std::fs::create_dir_all(&path)?;
    let mut config = fuser::Config::default();
    config.mount_options = vec![
        fuser::MountOption::RO,
        fuser::MountOption::NoDev,
        fuser::MountOption::NoSuid,
        fuser::MountOption::NoExec,
        fuser::MountOption::FSName(format!("altay:{key}")),
        fuser::MountOption::Subtype("altay".into()),
    ];
    // A slow read must not hold up listing another folder.
    config.n_threads = Some(4);
    let session = fuser::spawn_mount(NetFs::new(remote, root), &path, &config)?;
    MOUNTS.lock().unwrap().push(Mounted { key: key.to_string(), path: path.clone(), label: label.to_string(), session });
    Ok(path)
}

/// A killed altay (it had no chance to run [`unmount_all`]) leaves its mounts
/// behind: unreachable ("transport endpoint is not connected") but in the way.
fn clear_if_stale(path: &std::path::Path) {
    if std::fs::metadata(path).is_err_and(|e| e.raw_os_error() == Some(libc::ENOTCONN)) {
        let _ = std::process::Command::new("fusermount3").arg("-uz").arg(path).status();
        let _ = std::fs::remove_dir(path);
    }
}

/// Clear the mounts a killed altay left (at start-up). A live mount of
/// another altay window answers and is left alone.
pub fn clear_stale() {
    let Some(base) = base_dir() else { return };
    let Ok(entries) = std::fs::read_dir(&base) else { return };
    for e in entries.flatten() {
        clear_if_stale(&e.path());
    }
}

/// Unmount every share (on exit). A share still in use (a terminal open in
/// it) is detached lazily.
pub fn unmount_all() {
    let mounts = std::mem::take(&mut *MOUNTS.lock().unwrap());
    for m in mounts {
        if let Err(e) = m.session.umount_and_join() {
            log::warn!("unmount {}: {e}", m.path.display());
            let _ = std::process::Command::new("fusermount3").arg("-uz").arg(&m.path).status();
        }
        let _ = std::fs::remove_dir(&m.path);
    }
}
