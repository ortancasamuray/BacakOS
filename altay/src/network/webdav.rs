// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! WebDAV client for [`netfs`](super::netfs) (ureq; `dav://` is HTTP,
//! `davs://` HTTPS).
//!
//! Listings are `PROPFIND`s; reads fetch 1 MiB blocks with `Range`; a write
//! is one streamed `PUT` from start to end, so a file can be written anew,
//! not changed in the middle. Permissions and times are the server's: a
//! request to change them is accepted and has no effect.

use std::io::{self, Read};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use base64::Engine;
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use ureq::http::{Method, Request, Response, StatusCode};
use ureq::{Agent, Body, SendBody};

use super::netfs::{Change, Kind, OpenMode, Remote, Space, Stat};
use super::{Connection, NetworkError, Protocol};

/// How much one read fetches; later reads inside it need no request.
const BLOCK: u64 = 1024 * 1024;
/// A listing larger than this is refused (a folder of ~100 000 entries).
const MAX_LISTING: u64 = 64 * 1024 * 1024;
/// Upload pieces queued ahead of the network.
const QUEUE: usize = 8;

/// What may stand unescaped in a URL path segment (RFC 3986 "unreserved").
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/></d:prop></d:propfind>"#;

const QUOTA_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:quota-available-bytes/><d:quota-used-bytes/></d:prop></d:propfind>"#;

pub struct WebDav {
    agent: Agent,
    /// `scheme://host:port`, without a path.
    origin: String,
    auth: Option<String>,
}

fn errno(n: i32) -> io::Error {
    io::Error::from_raw_os_error(n)
}

fn status_error(status: StatusCode) -> io::Error {
    errno(match status.as_u16() {
        404 | 409 => libc::ENOENT,
        401 | 403 => libc::EACCES,
        405 | 412 => libc::EEXIST,
        423 => libc::EBUSY,
        507 => libc::ENOSPC,
        _ => libc::EIO,
    })
}

fn http_error(e: ureq::Error) -> io::Error {
    log::debug!("webdav: {e}");
    match e {
        ureq::Error::Io(e) => e,
        ureq::Error::Timeout(_) => errno(libc::ETIMEDOUT),
        ureq::Error::HostNotFound => io::ErrorKind::NotFound.into(),
        ureq::Error::ConnectionFailed => io::ErrorKind::ConnectionRefused.into(),
        _ => errno(libc::EIO),
    }
}

/// `/a b/c` → `/a%20b/c`.
fn encode_path(path: &str) -> String {
    path.split('/').map(|s| utf8_percent_encode(s, SEGMENT).to_string()).collect::<Vec<_>>().join("/")
}

/// An `href` from a listing as a plain remote path: the server may give a
/// whole URL, and escapes as it likes.
fn href_path(href: &str) -> String {
    let path = match href.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("/", |i| &rest[i..]),
        None => href,
    };
    percent_decode_str(path).decode_utf8_lossy().into_owned()
}

/// An HTTP date (`Tue, 27 Oct 2026 20:21:00 GMT`), the form WebDAV uses.
fn http_date(s: &str) -> Option<SystemTime> {
    let mut it = s.split_whitespace().skip(1);
    let day: u32 = it.next()?.parse().ok()?;
    let name = it.next()?;
    let month = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"].iter().position(|m| *m == name)? as u32 + 1;
    let year: i32 = it.next()?.parse().ok()?;
    let mut hms = it.next()?.split(':').map(|p| p.parse::<u32>().ok());
    let (h, m, sec) = (hms.next()??, hms.next()??, hms.next()??);
    let t = chrono::NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(h, m, sec)?.and_utc();
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(t.timestamp().max(0) as u64))
}

/// The entries of a `207 Multi-Status` answer: (path, attributes).
fn parse_multistatus(xml: &str) -> io::Result<Vec<(String, Stat)>> {
    let doc = roxmltree::Document::parse(xml).map_err(|_| errno(libc::EIO))?;
    let dav = |n: &roxmltree::Node, name: &str| n.tag_name().namespace() == Some("DAV:") && n.tag_name().name() == name;
    let mut out = Vec::new();
    for resp in doc.descendants().filter(|n| dav(n, "response")) {
        let Some(href) = resp.children().find(|n| dav(n, "href")).and_then(|n| n.text()) else { continue };
        let now = SystemTime::now();
        let mut st = Stat { kind: Kind::File, size: 0, perm: 0o644, mtime: now, atime: now };
        let mut found = false;
        for propstat in resp.children().filter(|n| dav(n, "propstat")) {
            let ok = propstat.children().find(|n| dav(n, "status")).and_then(|n| n.text()).is_some_and(|s| s.contains(" 200 "));
            let Some(prop) = propstat.children().find(|n| dav(n, "prop")).filter(|_| ok) else { continue };
            found = true;
            for p in prop.children().filter(|n| n.is_element()) {
                if dav(&p, "resourcetype") && p.children().any(|c| dav(&c, "collection")) {
                    st.kind = Kind::Dir;
                    st.perm = 0o755;
                } else if dav(&p, "getcontentlength") {
                    st.size = p.text().and_then(|t| t.trim().parse().ok()).unwrap_or(0);
                } else if dav(&p, "getlastmodified") {
                    if let Some(t) = p.text().and_then(http_date) {
                        (st.mtime, st.atime) = (t, t);
                    }
                }
            }
        }
        if found {
            out.push((href_path(href), st));
        }
    }
    Ok(out)
}

/// The space a server reports for a folder (RFC 4331), if it does.
fn parse_quota(xml: &str) -> Option<Space> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let prop = |name: &str| -> Option<u64> {
        doc.descendants()
            .find(|n| n.tag_name().namespace() == Some("DAV:") && n.tag_name().name() == name)?
            .text()?
            .trim()
            .parse()
            .ok()
    };
    let (avail, used) = (prop("quota-available-bytes")?, prop("quota-used-bytes")?);
    Some(Space { total: avail + used, free: avail, avail })
}

/// Turns the chunks a writer sends into the body of a `PUT`.
struct ChannelReader {
    rx: Receiver<Vec<u8>>,
    chunk: Vec<u8>,
    at: usize,
}

impl Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.at == self.chunk.len() {
            match self.rx.recv() {
                Ok(c) => (self.chunk, self.at) = (c, 0),
                // The writer is done: the body ends.
                Err(_) => return Ok(0),
            }
        }
        let n = buf.len().min(self.chunk.len() - self.at);
        buf[..n].copy_from_slice(&self.chunk[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

pub enum Handle {
    Read {
        path: String,
        /// The last block fetched: (where it starts, its bytes).
        block: Mutex<Option<(u64, Vec<u8>)>>,
    },
    Write(Mutex<Upload>),
}

pub struct Upload {
    tx: Option<SyncSender<Vec<u8>>>,
    sender: Option<JoinHandle<io::Result<()>>>,
    pos: u64,
    /// The upload ended (with this outcome); later writes fail.
    finished: Option<Result<(), i32>>,
}

impl Upload {
    fn finish(&mut self) -> io::Result<()> {
        if let Some(done) = self.finished {
            return done.map_err(errno);
        }
        self.tx = None;
        let result = match self.sender.take().map(|t| t.join()) {
            Some(Ok(r)) => r,
            Some(Err(_)) => Err(errno(libc::EIO)),
            None => Ok(()),
        };
        self.finished = Some(result.as_ref().map(drop).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO)));
        result
    }
}

/// Connect and check the share answers (and the password). Returns the
/// client and the remote folder to show.
pub fn connect(conn: &Connection, password: Option<&str>) -> Result<(WebDav, String), NetworkError> {
    let scheme = if conn.protocol == Protocol::WebDavs { "https" } else { "http" };
    let origin = match conn.port.filter(|p| *p != conn.protocol.default_port()) {
        Some(p) => format!("{scheme}://{}:{p}", conn.host),
        None => format!("{scheme}://{}", conn.host),
    };
    let config = Agent::config_builder()
        .http_status_as_error(false)
        .allow_non_standard_methods(true)
        // A redirect would turn PROPFIND into GET: answered by hand instead.
        .max_redirects(0)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_secs(30)))
        // Apache drops an idle connection after 5 s: reusing one older than
        // that would fail (and a streamed PUT cannot be sent twice).
        .max_idle_age(Duration::from_secs(4))
        .build();
    let auth = conn.username.as_ref().map(|u| {
        let pair = format!("{u}:{}", password.unwrap_or_default());
        format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(pair))
    });
    let client = WebDav { agent: config.into(), origin, auth };
    let root = match &conn.share {
        Some(share) => format!("/{}/", share.trim_matches('/')),
        None => "/".to_string(),
    };
    match client.propfind(&root, 0) {
        Ok(entries) if entries.first().is_some_and(|(_, st)| st.kind == Kind::Dir) => {
            let root = match root.trim_end_matches('/') {
                "" => "/".to_string(),
                r => r.to_string(),
            };
            Ok((client, root))
        }
        Ok(_) => Err(NetworkError::MountFailed(format!("{root}: not a folder"))),
        Err(e) if e.raw_os_error() == Some(libc::EACCES) && password.is_none() => Err(NetworkError::PasswordNeeded),
        Err(e) if e.raw_os_error() == Some(libc::EACCES) => Err(NetworkError::AuthFailed),
        Err(e) if e.raw_os_error().is_none() => Err(NetworkError::Connect(e)),
        Err(e) => Err(NetworkError::MountFailed(e.to_string())),
    }
}

impl WebDav {
    fn url(&self, path: &str) -> String {
        format!("{}{}", self.origin, encode_path(path))
    }

    /// One request. A connection that dropped before answering (the server
    /// closed it while it idled in the pool, or the network blinked) is
    /// given a second try on a new one.
    fn send(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> io::Result<Response<Body>> {
        let request = || {
            let mut req = Request::builder().method(Method::from_bytes(method.as_bytes()).expect("method")).uri(self.url(path));
            for (k, v) in headers {
                req = req.header(*k, *v);
            }
            if let Some(a) = &self.auth {
                req = req.header("Authorization", a.as_str());
            }
            req.body(body).map_err(|_| errno(libc::EINVAL))
        };
        match self.agent.run(request()?) {
            Err(ureq::Error::Io(e)) => {
                log::debug!("webdav {method}: {e}; trying again");
                self.agent.run(request()?).map_err(http_error)
            }
            r => r.map_err(http_error),
        }
    }

    fn propfind_xml(&self, path: &str, depth: u8, body: &str) -> io::Result<String> {
        let depth = depth.to_string();
        let headers = [("Depth", depth.as_str()), ("Content-Type", "application/xml; charset=utf-8")];
        let mut resp = self.send("PROPFIND", path, &headers, body.as_bytes())?;
        // A folder asked for without its slash may be redirected to it.
        if resp.status().is_redirection() && !path.ends_with('/') {
            resp = self.send("PROPFIND", &format!("{path}/"), &headers, body.as_bytes())?;
        }
        if resp.status() != StatusCode::MULTI_STATUS {
            return Err(status_error(resp.status()));
        }
        resp.body_mut().with_config().limit(MAX_LISTING).read_to_string().map_err(http_error)
    }

    fn propfind(&self, path: &str, depth: u8) -> io::Result<Vec<(String, Stat)>> {
        parse_multistatus(&self.propfind_xml(path, depth, PROPFIND_BODY)?)
    }

    fn expect(&self, resp: io::Result<Response<Body>>) -> io::Result<()> {
        let resp = resp?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(status_error(resp.status()))
        }
    }

    /// Fetch the block of the file at `offset`.
    fn fetch(&self, path: &str, offset: u64) -> io::Result<Vec<u8>> {
        let range = format!("bytes={offset}-{}", offset + BLOCK - 1);
        let mut resp = self.send("GET", path, &[("Range", &range)], &[])?;
        match resp.status() {
            // ureq refuses a body as long as its limit: one byte of room.
            StatusCode::PARTIAL_CONTENT => resp.body_mut().with_config().limit(BLOCK + 1).read_to_vec().map_err(http_error),
            StatusCode::RANGE_NOT_SATISFIABLE => Ok(Vec::new()),
            // The server ignores ranges and sends the whole file: skip to
            // the block.
            StatusCode::OK => {
                let mut reader = resp.into_body().into_with_config().limit(u64::MAX).reader();
                io::copy(&mut (&mut reader).take(offset), &mut io::sink())?;
                let mut out = Vec::new();
                reader.take(BLOCK).read_to_end(&mut out)?;
                Ok(out)
            }
            s => Err(status_error(s)),
        }
    }

    /// Start a streamed `PUT` of `path`; what the writer sends is its body.
    fn upload(&self, path: &str) -> Upload {
        let (tx, rx) = sync_channel::<Vec<u8>>(QUEUE);
        let agent = self.agent.clone();
        let mut req = Request::builder().method(Method::PUT).uri(self.url(path));
        if let Some(a) = &self.auth {
            req = req.header("Authorization", a.as_str());
        }
        let sender = std::thread::Builder::new()
            .name("dav-put".into())
            .spawn(move || {
                let body = SendBody::from_owned_reader(ChannelReader { rx, chunk: Vec::new(), at: 0 });
                let req = req.body(body).map_err(|_| errno(libc::EINVAL))?;
                let resp = agent.run(req).map_err(http_error)?;
                if resp.status().is_success() {
                    Ok(())
                } else {
                    Err(status_error(resp.status()))
                }
            })
            .ok();
        Upload { tx: Some(tx), sender, pos: 0, finished: None }
    }
}

impl Remote for WebDav {
    type Handle = Handle;

    fn stat(&self, path: &str) -> io::Result<Stat> {
        self.propfind(path, 0)?.into_iter().next().map(|(_, st)| st).ok_or_else(|| errno(libc::ENOENT))
    }

    fn list(&self, path: &str) -> io::Result<Vec<(String, Stat)>> {
        let dir = format!("{}/", path.trim_end_matches('/'));
        let mut out = Vec::new();
        for (href, st) in self.propfind(&dir, 1)? {
            let href = href.trim_end_matches('/');
            // The folder itself comes back too.
            if href == dir.trim_end_matches('/') {
                continue;
            }
            if let Some(name) = href.rsplit('/').next().filter(|n| !n.is_empty()) {
                out.push((name.to_string(), st));
            }
        }
        Ok(out)
    }

    fn open(&self, path: &str, mode: OpenMode) -> io::Result<Handle> {
        if mode.read && mode.write || mode.append {
            return Err(errno(libc::ENOTSUP));
        }
        if !mode.write {
            return Ok(Handle::Read { path: path.to_string(), block: Mutex::new(None) });
        }
        if mode.create.is_some() && mode.exclusive && self.stat(path).is_ok() {
            return Err(errno(libc::EEXIST));
        }
        // PUT replaces the whole file: changing one in the middle would lose
        // its end.
        if !mode.truncate && mode.create.is_none() && self.stat(path)?.size > 0 {
            return Err(errno(libc::ENOTSUP));
        }
        Ok(Handle::Write(Mutex::new(self.upload(path))))
    }

    fn read(&self, handle: &Handle, offset: u64, size: u32) -> io::Result<Vec<u8>> {
        let Handle::Read { path, block } = handle else { return Err(errno(libc::EBADF)) };
        let mut block = block.lock().unwrap();
        let end = offset + u64::from(size);
        let covered = block.as_ref().is_some_and(|(start, data)| {
            let stop = start + data.len() as u64;
            // A short block is the file's end: what lies past it is nothing.
            offset >= *start && (end <= stop || (data.len() as u64) < BLOCK)
        });
        if !covered {
            *block = Some((offset, self.fetch(path, offset)?));
        }
        let (start, data) = block.as_ref().unwrap();
        let from = ((offset - start) as usize).min(data.len());
        let to = ((end - start) as usize).min(data.len());
        Ok(data[from..to].to_vec())
    }

    fn write(&self, handle: &Handle, offset: u64, data: &[u8]) -> io::Result<()> {
        let Handle::Write(upload) = handle else { return Err(errno(libc::EBADF)) };
        let mut up = upload.lock().unwrap();
        if up.finished.is_some() {
            return Err(errno(libc::EIO));
        }
        // One stream from start to end: no going back, no gaps.
        if offset != up.pos {
            return Err(errno(libc::ENOTSUP));
        }
        let sent = up.tx.as_ref().is_some_and(|tx| tx.send(data.to_vec()).is_ok());
        if !sent {
            // The request ended early: its answer says why.
            return Err(up.finish().err().unwrap_or_else(|| errno(libc::EIO)));
        }
        up.pos += data.len() as u64;
        Ok(())
    }

    fn flush(&self, handle: &Handle) -> io::Result<()> {
        match handle {
            Handle::Write(up) => up.lock().unwrap().finish(),
            Handle::Read { .. } => Ok(()),
        }
    }

    fn close(&self, handle: Handle) -> io::Result<()> {
        match handle {
            Handle::Write(up) => up.into_inner().unwrap().finish(),
            Handle::Read { .. } => Ok(()),
        }
    }

    fn space(&self, path: &str) -> io::Result<Space> {
        let xml = self.propfind_xml(&format!("{}/", path.trim_end_matches('/')), 0, QUOTA_BODY)?;
        parse_quota(&xml).ok_or_else(|| errno(libc::ENOSYS))
    }

    fn mkdir(&self, path: &str, _perm: u16) -> io::Result<()> {
        self.expect(self.send("MKCOL", path, &[], &[]))
    }

    fn remove_file(&self, path: &str) -> io::Result<()> {
        if self.stat(path)?.kind == Kind::Dir {
            return Err(errno(libc::EISDIR));
        }
        self.expect(self.send("DELETE", path, &[], &[]))
    }

    fn remove_dir(&self, path: &str) -> io::Result<()> {
        // DELETE takes a folder with everything in it; rmdir must not.
        if !self.list(path)?.is_empty() {
            return Err(errno(libc::ENOTEMPTY));
        }
        self.expect(self.send("DELETE", &format!("{}/", path.trim_end_matches('/')), &[], &[]))
    }

    fn rename(&self, from: &str, to: &str, replace: bool) -> io::Result<()> {
        let dest = self.url(to);
        let overwrite = if replace { "T" } else { "F" };
        self.expect(self.send("MOVE", from, &[("Destination", dest.as_str()), ("Overwrite", overwrite)], &[]))
    }

    fn change(&self, path: &str, change: Change) -> io::Result<()> {
        match change.size {
            Some(0) => self.expect(self.send("PUT", path, &[], &[])),
            Some(n) if self.stat(path)?.size != n => Err(errno(libc::ENOTSUP)),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_escaped_per_segment() {
        assert_eq!(encode_path("/a b/ç%/x"), "/a%20b/%C3%A7%25/x");
        assert_eq!(href_path("http://h:8080/a%20b/%C3%A7/"), "/a b/ç/");
        assert_eq!(href_path("/dav/f%C4%B1le"), "/dav/fıle");
    }

    #[test]
    fn http_dates() {
        let t = http_date("Tue, 27 Oct 2026 20:21:00 GMT").unwrap();
        assert_eq!(t.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs(), 1_793_132_460);
        assert!(http_date("dün").is_none());
    }

    #[test]
    fn quota() {
        let xml = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/</d:href><d:propstat><d:prop>
<d:quota-available-bytes>300</d:quota-available-bytes><d:quota-used-bytes>100</d:quota-used-bytes>
</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
        let s = parse_quota(xml).unwrap();
        assert_eq!((s.total, s.avail), (400, 300));
        assert!(parse_quota("<d:multistatus xmlns:d=\"DAV:\"/>").is_none());
    }

    #[test]
    fn multistatus() {
        let xml = r#"<?xml version="1.0"?>
<D:multistatus xmlns:D="DAV:">
 <D:response><D:href>/dav/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
 <D:response><D:href>/dav/a%20b.txt</D:href><D:propstat><D:prop><D:resourcetype/><D:getcontentlength>12</D:getcontentlength></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>
  <D:propstat><D:prop><D:getlastmodified/></D:prop><D:status>HTTP/1.1 404 Not Found</D:status></D:propstat></D:response>
</D:multistatus>"#;
        let e = parse_multistatus(xml).unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!((e[0].0.as_str(), e[0].1.kind), ("/dav/", Kind::Dir));
        assert_eq!((e[1].0.as_str(), e[1].1.kind, e[1].1.size), ("/dav/a b.txt", Kind::File, 12));
    }
}
