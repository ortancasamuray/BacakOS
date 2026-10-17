//! BacakOS IPC server: the `bacak-ipc` protocol (BacakOsSDK) on a Unix socket
//! in `$XDG_RUNTIME_DIR`, so applications can show notifications, follow the
//! desktop-wide dark mode, list windows/outputs and open the Control Center.
//!
//! Everything here is non-blocking and polled once per main-loop tick (the
//! udev loop wakes at least every 16 ms) — same pattern as the Uzak Yönetim
//! monitor flag — so no extra calloop source is needed in either backend.
//! [`crate::state::BacakState::poll_ipc`] owns the request handling; this
//! module only moves bytes.
//!
//! Access control is the socket's location: `$XDG_RUNTIME_DIR` is `0700` and
//! owned by the session user, and the socket itself is chmod `0600`.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use bacak_ipc::{Mesaj, Olay, Yanit, MAX_LINE};
use tracing::{info, warn};

/// Connections beyond this are refused (closed right after accept).
const MAX_CLIENTS: usize = 64;
/// A client that doesn't read its replies/events is dropped past this backlog.
const MAX_OUT: usize = 1024 * 1024;

pub type ClientId = u64;

struct Client {
    id: ClientId,
    stream: UnixStream,
    inbuf: Vec<u8>,
    out: Vec<u8>,
    subscribed: bool,
    dead: bool,
}

pub struct IpcServer {
    listener: UnixListener,
    path: PathBuf,
    clients: Vec<Client>,
    next_id: ClientId,
}

impl IpcServer {
    /// Binds the socket for `wayland_display` and exports `$BACAK_IPC` to our
    /// children. `None` (logged) when there's no runtime dir, or another live
    /// compositor already owns the socket — the session then simply runs
    /// without IPC.
    pub fn bind(wayland_display: &str) -> Option<IpcServer> {
        let server = Self::bind_at(bacak_ipc::socket_path_for(wayland_display)?)?;
        std::env::set_var(bacak_ipc::SOCKET_ENV, &server.path);
        Some(server)
    }

    fn bind_at(path: PathBuf) -> Option<IpcServer> {
        if path.exists() {
            if UnixStream::connect(&path).is_ok() {
                warn!(path = %path.display(), "IPC soketi başka bir compositor'da; IPC kapalı");
                return None;
            }
            let _ = std::fs::remove_file(&path); // stale, from a crashed run
        }
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                warn!(path = %path.display(), error = %e, "IPC soketi açılamadı");
                return None;
            }
        };
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        if let Err(e) = listener.set_nonblocking(true) {
            warn!(error = %e, "IPC soketi bloklamasız yapılamadı");
            return None;
        }
        info!(path = %path.display(), "IPC soketi dinleniyor");
        Some(IpcServer { listener, path, clients: Vec::new(), next_id: 1 })
    }

    pub fn has_subscribers(&self) -> bool {
        self.clients.iter().any(|c| c.subscribed && !c.dead)
    }

    /// Accepts new connections and returns every complete request line read
    /// since the last call. Malformed lines get an error reply right here.
    pub fn poll(&mut self) -> Vec<(ClientId, Mesaj)> {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if self.clients.len() >= MAX_CLIENTS || stream.set_nonblocking(true).is_err() {
                        continue; // dropped → closed
                    }
                    let id = self.next_id;
                    self.next_id += 1;
                    self.clients.push(Client { id, stream, inbuf: Vec::new(), out: Vec::new(), subscribed: false, dead: false });
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => {
                    warn!(error = %e, "IPC accept");
                    break;
                }
            }
        }

        let mut requests = Vec::new();
        let mut buf = [0u8; 4096];
        for c in &mut self.clients {
            loop {
                match c.stream.read(&mut buf) {
                    Ok(0) => {
                        c.dead = true;
                        break;
                    }
                    Ok(n) => c.inbuf.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => {
                        c.dead = true;
                        break;
                    }
                }
            }
            while let Some(nl) = c.inbuf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = c.inbuf.drain(..=nl).collect();
                let line = String::from_utf8_lossy(&line[..line.len() - 1]);
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Mesaj>(line) {
                    Ok(m) => requests.push((c.id, m)),
                    Err(e) => {
                        // Echo the id when there is one so the client can match it.
                        let id = serde_json::from_str::<serde_json::Value>(line)
                            .ok()
                            .and_then(|v| v.get("id").and_then(|i| i.as_u64()))
                            .unwrap_or(0);
                        queue(c, &Yanit::err(id, format!("geçersiz istek: {e}")));
                    }
                }
            }
            if c.inbuf.len() > MAX_LINE {
                c.dead = true;
            }
        }
        self.flush();
        requests
    }

    pub fn reply(&mut self, client: ClientId, y: Yanit) {
        if let Some(c) = self.clients.iter_mut().find(|c| c.id == client) {
            queue(c, &y);
        }
    }

    pub fn subscribe(&mut self, client: ClientId) {
        if let Some(c) = self.clients.iter_mut().find(|c| c.id == client) {
            c.subscribed = true;
        }
    }

    pub fn broadcast(&mut self, olay: &Olay) {
        for c in self.clients.iter_mut().filter(|c| c.subscribed) {
            queue(c, olay);
        }
    }

    /// Writes as much queued output as the sockets take, then forgets closed
    /// or stalled clients.
    pub fn flush(&mut self) {
        for c in &mut self.clients {
            while !c.out.is_empty() && !c.dead {
                match c.stream.write(&c.out) {
                    Ok(0) => c.dead = true,
                    Ok(n) => {
                        c.out.drain(..n);
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => c.dead = true,
                }
            }
            if c.out.len() > MAX_OUT {
                c.dead = true;
            }
        }
        self.clients.retain(|c| !c.dead);
    }
}

fn queue<T: serde::Serialize>(c: &mut Client, msg: &T) {
    if let Ok(mut line) = serde_json::to_vec(msg) {
        line.push(b'\n');
        c.out.extend_from_slice(&line);
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    /// Unique per test: tests run in parallel.
    fn server(name: &str) -> IpcServer {
        let path = std::env::temp_dir().join(format!("bacak-ipc-test-{}-{name}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        IpcServer::bind_at(path).unwrap()
    }

    #[test]
    fn request_reply_and_events() {
        let mut s = server("istek");
        let mut c = UnixStream::connect(&s.path).unwrap();
        c.write_all(b"{\"id\":1,\"istek\":\"surum\"}\n{\"id\":2,\"istek\":\"abone\"}\nbozuk\n").unwrap();
        let mut got = Vec::new();
        for _ in 0..50 {
            got.extend(s.poll());
            if got.len() == 2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(got.len(), 2);
        let (cid, m) = &got[1];
        assert_eq!(m.istek, bacak_ipc::Istek::Abone);
        s.subscribe(*cid);
        s.reply(*cid, Yanit::ok(2, None));
        s.broadcast(&Olay::Gorunum { koyu: false });
        s.flush();

        let mut r = BufReader::new(c);
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        assert!(line.contains("geçersiz istek"), "{line}");
        line.clear();
        r.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), r#"{"id":2,"tamam":true}"#);
        line.clear();
        r.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), r#"{"olay":"gorunum","koyu":false}"#);
    }

    /// The real request handling in `BacakState::poll_ipc` (headless state,
    /// one default output). Read-only requests plus the on-screen ones; dark
    /// mode is left out because it writes the user's `gorunum.json`.
    #[test]
    fn state_serves_requests() {
        use smithay::reexports::wayland_server::Display;
        let display: Display<crate::state::BacakState> = Display::new().unwrap();
        let mut state = crate::state::BacakState::new(&display, "ipc-test-seat");
        state.ipc = Some(server("durum"));
        let c = UnixStream::connect(&state.ipc.as_ref().unwrap().path).unwrap();
        (&c).write_all(
            concat!(
                r#"{"id":1,"istek":"surum"}"#, "\n",
                r#"{"id":2,"istek":"ekranlar"}"#, "\n",
                r#"{"id":3,"istek":"pencereler"}"#, "\n",
                r#"{"id":4,"istek":"bildirim","baslik":"Merhaba","ayrinti":"dünya"}"#, "\n",
                r#"{"id":5,"istek":"bildirim","baslik":"  "}"#, "\n",
                r#"{"id":6,"istek":"kontrol_merkezi","ac":true}"#, "\n",
                r#"{"id":7,"istek":"gorunum_al"}"#, "\n",
            )
            .as_bytes(),
        )
        .unwrap();
        c.set_read_timeout(Some(std::time::Duration::from_millis(20))).unwrap();
        let mut r = BufReader::new(&c);
        let mut lines: Vec<serde_json::Value> = Vec::new();
        for _ in 0..200 {
            state.poll_ipc();
            let mut line = String::new();
            while r.read_line(&mut line).is_ok_and(|n| n > 0) {
                lines.push(serde_json::from_str(line.trim()).unwrap());
                line.clear();
            }
            if lines.len() == 7 {
                break;
            }
        }
        assert_eq!(lines.len(), 7, "{lines:?}");
        assert_eq!(lines[0]["veri"]["protokol"], bacak_ipc::PROTOCOL_VERSION);
        assert!(lines[1]["veri"][0]["genislik"].as_f64().unwrap() > 0.0, "{}", lines[1]);
        assert_eq!(lines[2]["veri"], serde_json::json!([]));
        assert_eq!(lines[3]["tamam"], true);
        assert_eq!(lines[4]["tamam"], false, "boş başlık reddedilmeli");
        assert_eq!(lines[5]["tamam"], true);
        assert_eq!(lines[6]["veri"]["koyu"], state.dark_mode);
        assert!(state.toast.is_some(), "bildirim gösterilmeli");
        assert!(state.control_center.is_some(), "Kontrol Merkezi açılmalı");
    }

    #[test]
    fn live_socket_is_not_stolen_and_is_removed_on_drop() {
        let s = server("calma");
        assert!(IpcServer::bind_at(s.path.clone()).is_none());
        let path = s.path.clone();
        drop(s);
        assert!(!path.exists());
    }
}
