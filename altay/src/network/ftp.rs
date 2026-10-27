// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! FTP client for [`netfs`](super::netfs) (suppaftp, plain FTP).
//!
//! One control connection answers listings and changes; every open file
//! gets a connection of its own, since FTP moves a file's data over a
//! transfer that blocks its connection until it ends. Reads follow the file
//! with one `RETR` (restarted with `REST` when a read jumps); writes are one
//! `STOR` from start to end, so a file can be written anew or appended to,
//! not changed in the middle.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use suppaftp::list::{File, ListParser, PosixPexQuery};
use suppaftp::types::FileType;
use suppaftp::{FtpError, FtpStream, ImplFtpStream, Status, TlsStream, TransferStream};

use super::netfs::{Change, Kind, OpenMode, Remote, Space, Stat};
use super::{Connection, NetworkError};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A server that stops answering fails the request instead of hanging it.
const IO_TIMEOUT: Duration = Duration::from_secs(20);
/// Idle connections kept for the next open file.
const KEEP_IDLE: usize = 4;

/// suppaftp does not export its plain stream type; name it through the
/// connection type that uses it.
trait HasTls {
    type T: TlsStream;
}
impl<T: TlsStream> HasTls for ImplFtpStream<T> {
    type T = T;
}
type Transfer = TransferStream<<FtpStream as HasTls>::T>;

#[derive(Clone)]
struct Login {
    addr: SocketAddr,
    user: String,
    password: String,
}

fn timed(stream: TcpStream) -> io::Result<TcpStream> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    Ok(stream)
}

fn open_connection(login: &Login) -> Result<FtpStream, FtpError> {
    let stream = TcpStream::connect_timeout(&login.addr, CONNECT_TIMEOUT).and_then(timed).map_err(FtpError::ConnectionError)?;
    let mut ftp = FtpStream::connect_with_stream(stream)?.passive_stream_builder(|addr| {
        TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).and_then(timed).map_err(FtpError::ConnectionError)
    });
    ftp.login(login.user.as_str(), login.password.as_str())?;
    ftp.transfer_type(FileType::Binary)?;
    // Most servers send UTF-8 names anyway; some need asking.
    let _ = ftp.opts("UTF8", Some("ON"));
    Ok(ftp)
}

fn code(e: &FtpError) -> Option<u32> {
    match e {
        FtpError::UnexpectedResponse(r) => Some(r.status.code()),
        _ => None,
    }
}

fn io_error(e: FtpError) -> io::Error {
    let errno = match &e {
        FtpError::ConnectionError(io) if matches!(io.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) => libc::ETIMEDOUT,
        FtpError::UnexpectedResponse(r) => match r.status.code() {
            550 => libc::ENOENT,
            530 | 532 | 553 => libc::EACCES,
            452 | 552 => libc::ENOSPC,
            502 | 504 => libc::ENOTSUP,
            _ => libc::EIO,
        },
        _ => libc::EIO,
    };
    log::debug!("ftp: {e}");
    io::Error::from_raw_os_error(errno)
}

fn lost(e: &FtpError) -> bool {
    matches!(e, FtpError::ConnectionError(_) | FtpError::BadResponse)
        // 421: the server closed the idle connection.
        || code(e) == Some(421)
}

pub struct Ftp {
    login: Login,
    /// The connection for listings and changes (`None` after it broke).
    main: Mutex<Option<FtpStream>>,
    /// Connections a closed file left, ready for the next one.
    idle: Mutex<Vec<FtpStream>>,
    /// The server lists in the machine-readable form (MLSD/MLST).
    mlsx: bool,
}

/// Connect and log in. Returns the client and the remote folder to show: the
/// share's path, or the folder the server starts in.
pub fn connect(conn: &Connection, password: Option<&str>) -> Result<(Ftp, String), NetworkError> {
    let port = conn.port.unwrap_or(21);
    let addr = (conn.host.as_str(), port)
        .to_socket_addrs()
        .map_err(NetworkError::Connect)?
        .next()
        .ok_or_else(|| NetworkError::Connect(io::ErrorKind::NotFound.into()))?;
    let login = Login {
        addr,
        user: conn.username.clone().unwrap_or_else(|| "anonymous".into()),
        password: password.unwrap_or_default().to_string(),
    };
    let mut ftp = open_connection(&login).map_err(|e| match e {
        FtpError::ConnectionError(io) => NetworkError::Connect(io),
        e if code(&e) == Some(530) && password.is_none() => NetworkError::PasswordNeeded,
        e if code(&e) == Some(530) => NetworkError::AuthFailed,
        e => NetworkError::MountFailed(e.to_string()),
    })?;
    let failed = |e: FtpError| NetworkError::MountFailed(e.to_string());
    let mlsx = ftp.feat().map(|f| f.contains_key("MLST")).unwrap_or(false);
    let root = match &conn.share {
        Some(share) => format!("/{}", share.trim_matches('/')),
        None => ftp.pwd().map_err(failed)?,
    };
    ftp.cwd(&root).map_err(|e| NetworkError::MountFailed(format!("{root}: {e}")))?;
    let client = Ftp { login, main: Mutex::new(Some(ftp)), idle: Mutex::new(Vec::new()), mlsx };
    Ok((client, root))
}

fn time_or_epoch(t: SystemTime) -> SystemTime {
    t.max(SystemTime::UNIX_EPOCH)
}

/// The permission bits a listing gave; servers that show none get the usual.
fn perm(f: &File, dir: bool) -> u16 {
    let mut perm = 0;
    for (who, shift) in [(PosixPexQuery::Owner, 6), (PosixPexQuery::Group, 3), (PosixPexQuery::Others, 0)] {
        let bits = (f.can_read(who) as u16) << 2 | (f.can_write(who) as u16) << 1 | f.can_execute(who) as u16;
        perm |= bits << shift;
    }
    match perm {
        0 if dir => 0o755,
        0 => 0o644,
        p => p,
    }
}

impl Ftp {
    /// Run `f` on the control connection; a connection the server dropped
    /// (idle too long) is made again and `f` tried once more.
    fn with_main<T>(&self, f: impl Fn(&mut FtpStream) -> Result<T, FtpError>) -> io::Result<T> {
        let mut main = self.main.lock().unwrap();
        for attempt in 0..2 {
            if main.is_none() {
                *main = Some(open_connection(&self.login).map_err(io_error)?);
            }
            match f(main.as_mut().unwrap()) {
                Err(e) if attempt == 0 && lost(&e) => *main = None,
                result => return result.map_err(io_error),
            }
        }
        unreachable!()
    }

    fn take_connection(&self) -> io::Result<FtpStream> {
        while let Some(mut c) = self.idle.lock().unwrap().pop() {
            if c.noop().is_ok() {
                return Ok(c);
            }
        }
        open_connection(&self.login).map_err(io_error)
    }

    fn give_back(&self, mut c: FtpStream) {
        let mut idle = self.idle.lock().unwrap();
        if idle.len() < KEEP_IDLE {
            idle.push(c);
        } else {
            let _ = c.quit();
        }
    }

    fn to_stat(&self, f: &File, path: &str) -> Stat {
        let modified = time_or_epoch(f.modified());
        let mut kind = if f.is_directory() { Kind::Dir } else { Kind::File };
        let mut size = f.size() as u64;
        // A link: what it points at is a file if the server can size it.
        if f.is_symlink() {
            match self.with_main(|c| c.size(path)) {
                Ok(n) => size = n as u64,
                Err(_) => kind = Kind::Dir,
            }
        }
        Stat { kind, size, perm: perm(f, kind == Kind::Dir), mtime: modified, atime: modified }
    }

    fn entries(&self, path: &str) -> io::Result<Vec<File>> {
        let lines = self.with_main(|c| if self.mlsx { c.mlsd(Some(path)) } else { c.list(Some(path)) })?;
        Ok(lines
            .iter()
            .filter_map(|l| {
                if self.mlsx {
                    ListParser::parse_mlsd(l)
                } else {
                    ListParser::parse_posix(l).or_else(|_| ListParser::parse_dos(l))
                }
                .ok()
            })
            .filter(|f| f.name() != "." && f.name() != ".." && !f.name().contains('/'))
            .collect())
    }

    fn exists(&self, path: &str) -> bool {
        self.stat(path).is_ok()
    }
}

/// An open file: its own connection, and the transfer under way on it.
pub struct Handle(Mutex<Open>);

struct Open {
    path: String,
    conn: Option<FtpStream>,
    transfer: Option<Transfer>,
    /// Where the transfer is in the file.
    pos: u64,
    write: bool,
    /// The upload ended (with this outcome); later writes fail.
    finished: Option<Result<(), i32>>,
}

impl Open {
    /// Stop a download midway (the server would otherwise send the rest).
    fn stop_reading(&mut self) {
        if let (Some(t), Some(c)) = (self.transfer.take(), self.conn.as_mut()) {
            if c.abort(t).is_err() {
                self.conn = None;
            }
        }
    }

    fn finish_upload(&mut self) -> io::Result<()> {
        if let Some(done) = self.finished {
            return done.map_err(io::Error::from_raw_os_error);
        }
        let result = match self.transfer.take() {
            Some(t) => t.finish().map_err(io_error),
            None => Ok(()),
        };
        self.finished = Some(result.as_ref().map(drop).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO)));
        result
    }
}

impl Remote for Ftp {
    type Handle = Handle;

    fn stat(&self, path: &str) -> io::Result<Stat> {
        if self.mlsx {
            let line = self.with_main(|c| c.mlst(Some(path)))?;
            let f = ListParser::parse_mlst(&line).map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
            return Ok(self.to_stat(&f, path));
        }
        // Without MLST: find the entry in its folder's listing.
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        if name.is_empty() {
            let now = SystemTime::now();
            return Ok(Stat { kind: Kind::Dir, size: 0, perm: 0o755, mtime: now, atime: now });
        }
        let dir = if dir.is_empty() { "/" } else { dir };
        let f = self.entries(dir)?.into_iter().find(|f| f.name() == name).ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
        Ok(self.to_stat(&f, path))
    }

    fn list(&self, path: &str) -> io::Result<Vec<(String, Stat)>> {
        Ok(self
            .entries(path)?
            .iter()
            .map(|f| (f.name().to_string(), self.to_stat(f, &super::netfs::join(path, f.name()))))
            .collect())
    }

    fn open(&self, path: &str, mode: OpenMode) -> io::Result<Handle> {
        if mode.read && mode.write {
            return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
        }
        if mode.exclusive && mode.create.is_some() && self.exists(path) {
            return Err(io::Error::from_raw_os_error(libc::EEXIST));
        }
        let mut open = Open { path: path.to_string(), conn: None, transfer: None, pos: 0, write: mode.write, finished: None };
        if mode.write {
            let mut size = 0;
            if !mode.truncate && mode.create.is_none() {
                size = self.stat(path)?.size;
                // STOR replaces the whole file: changing one in the middle
                // would lose its end.
                if size > 0 && !mode.append {
                    return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
                }
            }
            let mut c = self.take_connection()?;
            let t = if mode.append { c.append_with_stream(path) } else { c.put_with_stream(path) };
            open.transfer = Some(t.map_err(io_error)?);
            open.conn = Some(c);
            open.pos = if mode.append { size } else { 0 };
        }
        Ok(Handle(Mutex::new(open)))
    }

    fn read(&self, handle: &Handle, offset: u64, size: u32) -> io::Result<Vec<u8>> {
        let mut open = handle.0.lock().unwrap();
        if open.transfer.is_none() || open.pos != offset {
            open.stop_reading();
            let mut c = match open.conn.take() {
                Some(c) => c,
                None => self.take_connection()?,
            };
            c.resume_transfer(offset as usize).map_err(io_error)?;
            let t = c.retr_as_stream(open.path.as_str()).map_err(io_error)?;
            open.conn = Some(c);
            open.transfer = Some(t);
            open.pos = offset;
        }
        let mut out = vec![0; size as usize];
        let mut got = 0;
        while got < out.len() {
            match open.transfer.as_mut().unwrap().read(&mut out[got..]) {
                Ok(0) => {
                    // The end of the file: the transfer is complete.
                    if let Some(t) = open.transfer.take() {
                        t.finish().map_err(io_error)?;
                    }
                    break;
                }
                Ok(n) => got += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    open.transfer = None;
                    open.conn = None;
                    return Err(e);
                }
            }
        }
        out.truncate(got);
        open.pos += got as u64;
        Ok(out)
    }

    fn write(&self, handle: &Handle, offset: u64, data: &[u8]) -> io::Result<()> {
        let mut open = handle.0.lock().unwrap();
        if open.finished.is_some() || open.transfer.is_none() {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        // One stream from start to end: no going back, no gaps.
        if offset != open.pos {
            return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
        }
        open.transfer.as_mut().unwrap().write_all(data)?;
        open.pos += data.len() as u64;
        Ok(())
    }

    fn flush(&self, handle: &Handle) -> io::Result<()> {
        let mut open = handle.0.lock().unwrap();
        if open.write {
            open.finish_upload()?;
        }
        Ok(())
    }

    fn close(&self, handle: Handle) -> io::Result<()> {
        let mut open = handle.0.into_inner().unwrap();
        let result = if open.write { open.finish_upload() } else { Ok(open.stop_reading()) };
        if let Some(c) = open.conn.take() {
            self.give_back(c);
        }
        result
    }

    fn space(&self, _path: &str) -> io::Result<Space> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn mkdir(&self, path: &str, _perm: u16) -> io::Result<()> {
        match self.with_main(|c| c.mkdir(path)) {
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) && self.exists(path) => Err(io::Error::from_raw_os_error(libc::EEXIST)),
            r => r,
        }
    }

    fn remove_file(&self, path: &str) -> io::Result<()> {
        self.with_main(|c| c.rm(path))
    }

    fn remove_dir(&self, path: &str) -> io::Result<()> {
        match self.with_main(|c| c.rmdir(path)) {
            // 550 on a folder that is there: it is not empty.
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) && self.entries(path).is_ok_and(|l| !l.is_empty()) => {
                Err(io::Error::from_raw_os_error(libc::ENOTEMPTY))
            }
            r => r,
        }
    }

    fn rename(&self, from: &str, to: &str, replace: bool) -> io::Result<()> {
        let target = self.exists(to);
        if target && !replace {
            return Err(io::Error::from_raw_os_error(libc::EEXIST));
        }
        match self.with_main(|c| c.rename(from, to)) {
            // Some servers refuse to rename over a file: remove it first.
            Err(_) if target => {
                self.remove_file(to)?;
                self.with_main(|c| c.rename(from, to))
            }
            r => r,
        }
    }

    fn change(&self, path: &str, change: Change) -> io::Result<()> {
        if let Some(size) = change.size {
            if size == 0 {
                self.with_main(|c| c.put_file(path, &mut io::empty()).map(drop))?;
            } else if self.stat(path)?.size != size {
                return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
            }
        }
        // Permissions and times are a courtesy of the server (SITE CHMOD,
        // MFMT): one that does not offer them leaves them as they are.
        if let Some(perm) = change.perm {
            if let Err(e) = self.with_main(|c| c.site(format!("CHMOD {perm:o} {path}")).map(drop)) {
                log::debug!("ftp chmod {path}: {e}");
            }
        }
        if let Some((_, mtime)) = change.times {
            let stamp = chrono::DateTime::<chrono::Utc>::from(mtime).format("%Y%m%d%H%M%S");
            if let Err(e) = self.with_main(|c| c.custom_command(format!("MFMT {stamp} {path}"), &[Status::File]).map(drop)) {
                log::debug!("ftp mfmt {path}: {e}");
            }
        }
        Ok(())
    }
}
