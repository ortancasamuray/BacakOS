//! Client for the compositor's IPC socket.
//!
//! ```no_run
//! let mut c = bacak_sdk::Istemci::baglan()?;
//! c.bildirim("Dışa aktarıldı", "rapor.pdf")?;
//! for p in c.pencereler()? {
//!     println!("{} — {}", p.uygulama, p.baslik);
//! }
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! Calls are blocking with a short timeout ([`ZAMAN_ASIMI`]); a compositor
//! error reply becomes an `io::Error` of kind `Other` carrying its message.
//! Without a BacakOS session [`Istemci::baglan`] fails with `NotFound`, which
//! applications should treat as "no desktop integration" rather than fatal.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use bacak_ipc::{Ekran, Gelen, Gorunum, Istek, Mesaj, Olay, Pencere, MAX_LINE};

/// How long a request waits for its reply.
pub const ZAMAN_ASIMI: Duration = Duration::from_secs(2);

pub struct Istemci {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    /// Events read while waiting for a reply, handed out by [`Olaylar`].
    events: VecDeque<Olay>,
}

impl Istemci {
    /// Connects to the current session's compositor (see
    /// [`bacak_ipc::socket_path`]).
    pub fn baglan() -> io::Result<Istemci> {
        let path = bacak_ipc::socket_path()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "BacakOS oturumu yok (BACAK_IPC / WAYLAND_DISPLAY)"))?;
        Self::baglan_yol(&path)
    }

    pub fn baglan_yol(path: &Path) -> io::Result<Istemci> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(ZAMAN_ASIMI))?;
        stream.set_write_timeout(Some(ZAMAN_ASIMI))?;
        Ok(Istemci { reader: BufReader::new(stream.try_clone()?), writer: stream, next_id: 1, events: VecDeque::new() })
    }

    /// Sends `istek` and returns the reply's `veri`.
    pub fn iste(&mut self, istek: Istek) -> io::Result<Option<serde_json::Value>> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = serde_json::to_vec(&Mesaj { id, istek }).map_err(io::Error::other)?;
        line.push(b'\n');
        self.writer.write_all(&line)?;
        loop {
            match self.read_one()? {
                Gelen::Olay(o) => self.events.push_back(o),
                Gelen::Yanit(y) if y.id == id => {
                    return if y.tamam {
                        Ok(y.veri)
                    } else {
                        Err(io::Error::other(y.hata.unwrap_or_else(|| "compositor isteği reddetti".into())))
                    };
                }
                Gelen::Yanit(_) => {} // a stale reply to a timed-out request
            }
        }
    }

    fn read_one(&mut self) -> io::Result<Gelen> {
        let mut line = String::new();
        let n = io::Read::take(&mut self.reader, MAX_LINE as u64 + 1).read_line(&mut line)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "compositor bağlantıyı kapattı"));
        }
        Gelen::parse(line.trim_end()).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    fn typed<T: serde::de::DeserializeOwned>(&mut self, istek: Istek) -> io::Result<T> {
        let v = self.iste(istek)?.unwrap_or(serde_json::Value::Null);
        serde_json::from_value(v).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// `(protocol version, compositor version)`.
    pub fn surum(&mut self) -> io::Result<(u32, String)> {
        #[derive(serde::Deserialize)]
        struct S {
            protokol: u32,
            compositor: String,
        }
        let s: S = self.typed(Istek::Surum)?;
        Ok((s.protokol, s.compositor))
    }

    /// A transient banner on the active screen. Texts over
    /// [`bacak_ipc::BILDIRIM_MAX`] characters are cut.
    pub fn bildirim(&mut self, baslik: &str, ayrinti: &str) -> io::Result<()> {
        self.iste(Istek::Bildirim { baslik: baslik.into(), ayrinti: ayrinti.into() }).map(|_| ())
    }

    pub fn gorunum(&mut self) -> io::Result<Gorunum> {
        self.typed(Istek::GorunumAl)
    }

    /// Desktop-wide: every BacakOS application and the compositor follow it.
    pub fn koyu_mod_ayarla(&mut self, koyu: bool) -> io::Result<()> {
        self.iste(Istek::KoyuModAyarla { koyu }).map(|_| ())
    }

    /// `Some(true)` opens, `Some(false)` closes, `None` toggles.
    pub fn kontrol_merkezi(&mut self, ac: Option<bool>) -> io::Result<()> {
        self.iste(Istek::KontrolMerkezi { ac }).map(|_| ())
    }

    pub fn pencereler(&mut self) -> io::Result<Vec<Pencere>> {
        self.typed(Istek::Pencereler)
    }

    pub fn ekranlar(&mut self) -> io::Result<Vec<Ekran>> {
        self.typed(Istek::Ekranlar)
    }

    /// Turns this connection into an event stream. The iterator blocks
    /// until the next event and ends when the compositor goes away.
    pub fn abone_ol(mut self) -> io::Result<Olaylar> {
        self.iste(Istek::Abone)?;
        self.writer.set_read_timeout(None)?;
        Ok(Olaylar(self))
    }
}

/// Events from [`Istemci::abone_ol`].
pub struct Olaylar(Istemci);

impl Iterator for Olaylar {
    type Item = Olay;

    fn next(&mut self) -> Option<Olay> {
        if let Some(o) = self.0.events.pop_front() {
            return Some(o);
        }
        loop {
            match self.0.read_one() {
                Ok(Gelen::Olay(o)) => return Some(o),
                Ok(Gelen::Yanit(_)) => {}
                Err(e) if e.kind() == io::ErrorKind::InvalidData => {}
                Err(_) => return None,
            }
        }
    }
}

/// One-shot notification; silently does nothing outside a BacakOS session.
pub fn bildir(baslik: &str, ayrinti: &str) -> io::Result<()> {
    match Istemci::baglan() {
        Ok(mut c) => c.bildirim(baslik, ayrinti),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bacak_ipc::Yanit;
    use std::os::unix::net::UnixListener;

    /// A one-connection fake compositor answering from `answer`.
    fn fake(answer: fn(&Mesaj) -> Vec<String>) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ipc.sock");
        let l = UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut w = s.try_clone().unwrap();
            for line in BufReader::new(s).lines() {
                let m: Mesaj = serde_json::from_str(&line.unwrap()).unwrap();
                for out in answer(&m) {
                    w.write_all(out.as_bytes()).unwrap();
                    w.write_all(b"\n").unwrap();
                }
            }
        });
        (dir, path)
    }

    fn json<T: serde::Serialize>(v: &T) -> String {
        serde_json::to_string(v).unwrap()
    }

    #[test]
    fn typed_calls_and_errors() {
        let (_d, path) = fake(|m| match &m.istek {
            Istek::Pencereler => vec![
                json(&Olay::Odak { pencere: Some(7) }),
                json(&Yanit::ok(
                    m.id,
                    Some(serde_json::json!([{"id":7,"uygulama":"altay","baslik":"Ev","calisma_alani":0,"odakli":true}])),
                )),
            ],
            Istek::Bildirim { .. } => vec![json(&Yanit::err(m.id, "başlık boş olamaz"))],
            _ => vec![json(&Yanit::ok(m.id, None))],
        });
        let mut c = Istemci::baglan_yol(&path).unwrap();
        let p = c.pencereler().unwrap();
        assert_eq!(p[0].uygulama, "altay");
        let e = c.bildirim("", "").unwrap_err();
        assert_eq!(e.to_string(), "başlık boş olamaz");
        // The event that arrived before the reply is not lost.
        let mut events = c.abone_ol().unwrap();
        assert_eq!(events.next(), Some(Olay::Odak { pencere: Some(7) }));
    }

    #[test]
    fn subscription_streams_events() {
        let (_d, path) = fake(|m| match &m.istek {
            Istek::Abone => vec![json(&Yanit::ok(m.id, None)), json(&Olay::Gorunum { koyu: false })],
            _ => vec![],
        });
        let mut events = Istemci::baglan_yol(&path).unwrap().abone_ol().unwrap();
        assert_eq!(events.next(), Some(Olay::Gorunum { koyu: false }));
    }
}
