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
    BsdFileFlags, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, LockOwner,
    OpenFlags, RenameFlags, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen,
    ReplyStatfs, ReplyWrite, Request, TimeOrNow, WriteFlags,
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

/// How to open a remote file.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenMode {
    pub read: bool,
    pub write: bool,
    pub append: bool,
    pub truncate: bool,
    /// Create it if missing, with these permission bits.
    pub create: Option<u16>,
    /// Fail if it exists (with `create`).
    pub exclusive: bool,
}

impl OpenMode {
    fn from_flags(flags: i32) -> Self {
        let acc = flags & libc::O_ACCMODE;
        Self {
            read: acc != libc::O_WRONLY,
            write: acc != libc::O_RDONLY,
            append: flags & libc::O_APPEND != 0,
            truncate: flags & libc::O_TRUNC != 0,
            create: None,
            exclusive: flags & libc::O_EXCL != 0,
        }
    }
}

/// Attributes to change; `None` leaves one as it is.
#[derive(Debug, Clone, Copy, Default)]
pub struct Change {
    pub perm: Option<u16>,
    pub size: Option<u64>,
    /// Access and modification time, always set together.
    pub times: Option<(SystemTime, SystemTime)>,
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
    fn open(&self, path: &str, mode: OpenMode) -> io::Result<Self::Handle>;
    /// Up to `size` bytes at `offset`; fewer only at the end of the file.
    fn read(&self, handle: &Self::Handle, offset: u64, size: u32) -> io::Result<Vec<u8>>;
    fn write(&self, handle: &Self::Handle, offset: u64, data: &[u8]) -> io::Result<()>;
    /// Closing reports what the server failed to store (a full disk).
    fn close(&self, handle: Self::Handle) -> io::Result<()>;
    fn space(&self, path: &str) -> io::Result<Space>;
    fn mkdir(&self, path: &str, perm: u16) -> io::Result<()>;
    fn remove_file(&self, path: &str) -> io::Result<()>;
    fn remove_dir(&self, path: &str) -> io::Result<()>;
    /// Move `from` to `to`; an existing `to` is replaced only if `replace`.
    fn rename(&self, from: &str, to: &str, replace: bool) -> io::Result<()>;
    fn change(&self, path: &str, change: Change) -> io::Result<()>;
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

    /// `path` and everything below it, as the inode table knows them.
    fn under(&self, path: &str) -> Vec<(String, u64)> {
        let prefix = format!("{}/", path.trim_end_matches('/'));
        self.by_path.iter().filter(|(p, _)| *p == path || p.starts_with(&prefix)).map(|(p, &i)| (p.clone(), i)).collect()
    }

    /// The entry is gone from the server. Its inode stays valid (a file
    /// still open keeps working) but no longer answers to the path.
    fn forget_path(&mut self, path: &str) {
        for (p, _) in self.under(path) {
            self.by_path.remove(&p);
        }
    }

    /// The entry (and, for a folder, its contents) moved from `from` to `to`.
    fn moved(&mut self, from: &str, to: &str) {
        self.forget_path(to);
        for (p, ino) in self.under(from) {
            self.by_path.remove(&p);
            let new = format!("{to}{}", &p[from.len()..]);
            if let Some(n) = self.nodes.get_mut(&ino) {
                n.path = new.clone();
                n.stat = None;
            }
            self.by_path.insert(new, ino);
        }
    }

    /// Drop the remembered attributes: the entry changed.
    fn stale(&mut self, ino: u64) {
        if let Some(n) = self.nodes.get_mut(&ino) {
            n.stat = None;
        }
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

    /// The remote path of `name` in folder `parent`.
    fn child(&self, parent: INodeNo, name: &OsStr) -> Option<String> {
        Some(join(&self.path_of(parent)?, name.to_str()?))
    }

    /// Note a new or changed `path` (and that its folder changed): its
    /// inode, with the server's fresh attributes.
    fn refreshed(&self, parent: INodeNo, path: &str) -> io::Result<FileAttr> {
        let ino = {
            let mut inodes = self.inodes.lock().unwrap();
            inodes.stale(parent.into());
            let ino = inodes.ino_for(path);
            inodes.stale(ino);
            ino
        };
        Ok(self.attr(ino, &self.stat_of(ino, path)?))
    }

    fn keep_file(&self, h: R::Handle) -> u64 {
        let fh = self.new_fh();
        self.files.lock().unwrap().insert(fh, Arc::new(h));
        fh
    }

    fn file(&self, fh: FileHandle) -> Option<Arc<R::Handle>> {
        self.files.lock().unwrap().get(&u64::from(fh)).cloned()
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
        let Some(path) = self.path_of(ino) else { return reply.error(Errno::ENOENT) };
        let mode = OpenMode::from_flags(flags.0);
        if mode.truncate {
            self.inodes.lock().unwrap().stale(ino.into());
        }
        match self.remote.open(&path, mode) {
            Ok(h) => reply.opened(FileHandle(self.keep_file(h)), FopenFlags::empty()),
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn create(&self, _req: &Request, parent: INodeNo, name: &OsStr, mode: u32, umask: u32, flags: i32, reply: ReplyCreate) {
        let Some(path) = self.child(parent, name) else { return reply.error(Errno::ENOENT) };
        let open = OpenMode { create: Some((mode & !umask & 0o7777) as u16), ..OpenMode::from_flags(flags) };
        let h = match self.remote.open(&path, open) {
            Ok(h) => h,
            Err(e) => return reply.error(errno(&e)),
        };
        match self.refreshed(parent, &path) {
            Ok(attr) => reply.created(&TTL, &attr, Generation(0), FileHandle(self.keep_file(h)), FopenFlags::empty()),
            Err(e) => {
                let _ = self.remote.close(h);
                reply.error(errno(&e));
            }
        }
    }

    fn write(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        let Some(h) = self.file(fh) else { return reply.error(Errno::EBADF) };
        self.inodes.lock().unwrap().stale(ino.into());
        match self.remote.write(&h, offset, data) {
            Ok(()) => reply.written(data.len() as u32),
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        // Every file shows as the user's own (see `attr`): giving it to
        // them is a no-op, giving it to anyone else is not possible here.
        if uid.is_some_and(|u| u != self.uid) || gid.is_some_and(|g| g != self.gid) {
            return reply.error(Errno::EPERM);
        }
        let Some(path) = self.path_of(ino) else { return reply.error(Errno::ENOENT) };
        let mut change = Change { perm: mode.map(|m| (m & 0o7777) as u16), size, times: None };
        if atime.is_some() || mtime.is_some() {
            // SFTP sets both times at once: keep the one not asked for.
            let cur = match self.stat_of(ino.into(), &path) {
                Ok(st) => st,
                Err(e) => return reply.error(errno(&e)),
            };
            let pick = |t: Option<TimeOrNow>, keep: SystemTime| match t {
                Some(TimeOrNow::SpecificTime(t)) => t,
                Some(TimeOrNow::Now) => SystemTime::now(),
                None => keep,
            };
            change.times = Some((pick(atime, cur.atime), pick(mtime, cur.mtime)));
        }
        self.inodes.lock().unwrap().stale(ino.into());
        if let Err(e) = self.remote.change(&path, change) {
            return reply.error(errno(&e));
        }
        match self.stat_of(ino.into(), &path) {
            Ok(st) => reply.attr(&TTL, &self.attr(ino.into(), &st)),
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn mkdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, mode: u32, umask: u32, reply: ReplyEntry) {
        let Some(path) = self.child(parent, name) else { return reply.error(Errno::ENOENT) };
        if let Err(e) = self.remote.mkdir(&path, (mode & !umask & 0o7777) as u16) {
            return reply.error(errno(&e));
        }
        match self.refreshed(parent, &path) {
            Ok(attr) => reply.entry(&TTL, &attr, Generation(0)),
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let Some(path) = self.child(parent, name) else { return reply.error(Errno::ENOENT) };
        match self.remote.remove_file(&path) {
            Ok(()) => {
                let mut inodes = self.inodes.lock().unwrap();
                inodes.forget_path(&path);
                inodes.stale(parent.into());
                reply.ok();
            }
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let Some(path) = self.child(parent, name) else { return reply.error(Errno::ENOENT) };
        match self.remote.remove_dir(&path) {
            Ok(()) => {
                let mut inodes = self.inodes.lock().unwrap();
                inodes.forget_path(&path);
                inodes.stale(parent.into());
                reply.ok();
            }
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        // Swapping two entries has no SFTP equivalent.
        if flags.contains(RenameFlags::RENAME_EXCHANGE) {
            return reply.error(Errno::EINVAL);
        }
        let (Some(from), Some(to)) = (self.child(parent, name), self.child(newparent, newname)) else {
            return reply.error(Errno::ENOENT);
        };
        let replace = !flags.contains(RenameFlags::RENAME_NOREPLACE);
        match self.remote.rename(&from, &to, replace) {
            Ok(()) => {
                let mut inodes = self.inodes.lock().unwrap();
                inodes.moved(&from, &to);
                inodes.stale(parent.into());
                inodes.stale(newparent.into());
                reply.ok();
            }
            Err(e) => reply.error(errno(&e)),
        }
    }

    fn flush(&self, _req: &Request, _ino: INodeNo, _fh: FileHandle, _lock_owner: LockOwner, reply: ReplyEmpty) {
        // Every write already went to the server.
        reply.ok();
    }

    fn fsync(&self, _req: &Request, _ino: INodeNo, _fh: FileHandle, _datasync: bool, reply: ReplyEmpty) {
        reply.ok();
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
        let Some(h) = self.file(fh) else { return reply.error(Errno::EBADF) };
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
        // Its error comes too late for `close()` to see; it is logged.
        if let Some(h) = self.files.lock().unwrap().remove(&u64::from(fh)).and_then(Arc::into_inner) {
            if let Err(e) = self.remote.close(h) {
                log::warn!("closing a remote file: {e}");
            }
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
