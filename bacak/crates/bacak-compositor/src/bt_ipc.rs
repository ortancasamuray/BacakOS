//! IPC köprüsü: bacak-ayarlar ↔ compositor Bluetooth modülü.
//!
//! Protokol: her yönde satır-başlıklı JSON mesajları.
//! Soket: `$XDG_RUNTIME_DIR/bacak-bt.sock`
//!
//! Compositor → istemci (olaylar):
//!   {"t":"powered","on":true}
//!   {"t":"scanning","on":true}
//!   {"t":"devices","list":[{"mac":"..","name":"..","paired":true,"connected":false}]}
//!   {"t":"pairing","device":"Redmi Note 8","passkey":"123456"}
//!   {"t":"toast","msg":"Eşleşti"}
//!
//! İstemci → compositor (komutlar):
//!   {"cmd":"power","on":true}
//!   {"cmd":"scan","on":true}
//!   {"cmd":"pair","mac":"E8:5A:8B:26:C0:32"}
//!   {"cmd":"connect","mac":"..."}
//!   {"cmd":"disconnect","mac":"..."}
//!   {"cmd":"forget","mac":"..."}
//!   {"cmd":"confirm","accept":true}
//!   {"cmd":"get_state"}

#![cfg(feature = "runtime")]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};

type Waker = Arc<Mutex<Option<calloop::LoopSignal>>>;

pub enum IpcCmd {
    Power(bool),
    Scan(bool),
    Pair(String),
    Connect(String),
    Disconnect(String),
    Forget(String),
    Confirm(bool),
    GetState,
}

pub struct IpcDevice {
    pub mac: String,
    pub name: String,
    pub paired: bool,
    pub connected: bool,
}

pub struct BtIpcServer {
    /// IPC istemcisinden gelen komutlar.
    pub cmd_rx: Receiver<IpcCmd>,
    /// İstemciye olay göndermek için; `None` = bağlı istemci yok.
    client_tx: Arc<Mutex<Option<Sender<String>>>>,
    /// Her yeni istemci bağlandığında artar (compositor'un "yeni bağlantı" fark etmesi için).
    pub generation: Arc<AtomicU64>,
    /// Calloop event loop'unu uyandırmak için (IPC thread'den).
    waker: Waker,
    _thread: std::thread::JoinHandle<()>,
}

impl BtIpcServer {
    /// Calloop LoopSignal'i ver — IPC komutları gelince compositor uyanır.
    pub fn set_waker(&self, signal: calloop::LoopSignal) {
        *self.waker.lock().unwrap() = Some(signal);
    }
}

impl BtIpcServer {
    pub fn start() -> Self {
        let path = socket_path();
        let _ = std::fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("BT IPC soket bağlanamadı: {e}");
                // Boş dummy döndür
                let (cmd_tx, cmd_rx) = channel();
                let _ = cmd_tx;
                return BtIpcServer {
                    cmd_rx,
                    client_tx: Arc::new(Mutex::new(None)),
                    generation: Arc::new(AtomicU64::new(0)),
                    waker: Arc::new(Mutex::new(None)),
                    _thread: std::thread::spawn(|| {}),
                };
            }
        };

        let (cmd_tx, cmd_rx) = channel::<IpcCmd>();
        let client_tx: Arc<Mutex<Option<Sender<String>>>> = Arc::new(Mutex::new(None));
        let client_tx2 = client_tx.clone();
        let generation = Arc::new(AtomicU64::new(0));
        let gen2 = generation.clone();
        let waker: Waker = Arc::new(Mutex::new(None));
        let waker2 = waker.clone();

        let thread = std::thread::Builder::new()
            .name("bt-ipc".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let (evt_tx, evt_rx) = channel::<String>();
                    *client_tx2.lock().unwrap() = Some(evt_tx);
                    gen2.fetch_add(1, Ordering::Relaxed);
                    // Yeni istemci bağlandı → compositor'u uyandır
                    if let Some(sig) = waker2.lock().unwrap().as_ref() { sig.wakeup(); }

                    // Okuyucu: soket satırları → cmd_tx + compositor'u uyandır
                    let stream_r = match stream.try_clone() {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    let cmd_tx2 = cmd_tx.clone();
                    let waker3 = waker2.clone();
                    std::thread::Builder::new()
                        .name("bt-ipc-r".into())
                        .spawn(move || {
                            for line in BufReader::new(stream_r).lines().flatten() {
                                if let Some(cmd) = parse_cmd(&line) {
                                    let _ = cmd_tx2.send(cmd);
                                    if let Some(sig) = waker3.lock().unwrap().as_ref() { sig.wakeup(); }
                                }
                            }
                        })
                        .ok();

                    // Yazıcı: evt_rx → soket
                    let mut w = stream;
                    for json in evt_rx {
                        if writeln!(w, "{json}").is_err() {
                            break;
                        }
                    }
                    *client_tx2.lock().unwrap() = None;
                }
            })
            .expect("bt-ipc thread spawn");

        BtIpcServer { cmd_rx, client_tx, generation, waker, _thread: thread }
    }

    pub fn has_client(&self) -> bool {
        self.client_tx.lock().unwrap().is_some()
    }

    pub fn try_recv_cmd(&self) -> Option<IpcCmd> {
        match self.cmd_rx.try_recv() {
            Ok(c) => Some(c),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => None,
        }
    }

    pub fn send_powered(&self, on: bool) {
        self.emit(format!(r#"{{"t":"powered","on":{on}}}"#));
    }

    pub fn send_scanning(&self, on: bool) {
        self.emit(format!(r#"{{"t":"scanning","on":{on}}}"#));
    }

    pub fn send_devices(&self, devices: &[IpcDevice]) {
        let list: String = devices
            .iter()
            .map(|d| {
                format!(
                    r#"{{"mac":"{}","name":"{}","paired":{},"connected":{}}}"#,
                    d.mac,
                    d.name.replace('"', "\\\""),
                    d.paired,
                    d.connected
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        self.emit(format!(r#"{{"t":"devices","list":[{list}]}}"#));
    }

    pub fn send_pairing(&self, device: &str, passkey: &str) {
        self.emit(format!(
            r#"{{"t":"pairing","device":"{}","passkey":"{}"}}"#,
            device.replace('"', "\\\""),
            passkey
        ));
    }

    pub fn send_toast(&self, msg: &str) {
        self.emit(format!(r#"{{"t":"toast","msg":"{}"}}"#, msg.replace('"', "\\\"")));
    }

    fn emit(&self, json: String) {
        if let Some(tx) = self.client_tx.lock().unwrap().as_ref() {
            let _ = tx.send(json);
        }
    }
}

pub fn socket_path() -> std::path::PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
    std::path::PathBuf::from(runtime).join("bacak-bt.sock")
}

fn parse_cmd(line: &str) -> Option<IpcCmd> {
    let line = line.trim();
    if line.contains(r#""cmd":"power""#) {
        return Some(IpcCmd::Power(line.contains(r#""on":true"#)));
    }
    if line.contains(r#""cmd":"scan""#) {
        return Some(IpcCmd::Scan(line.contains(r#""on":true"#)));
    }
    if line.contains(r#""cmd":"pair""#) {
        return Some(IpcCmd::Pair(extract_str(line, "mac")?));
    }
    if line.contains(r#""cmd":"connect""#) {
        return Some(IpcCmd::Connect(extract_str(line, "mac")?));
    }
    if line.contains(r#""cmd":"disconnect""#) {
        return Some(IpcCmd::Disconnect(extract_str(line, "mac")?));
    }
    if line.contains(r#""cmd":"forget""#) {
        return Some(IpcCmd::Forget(extract_str(line, "mac")?));
    }
    if line.contains(r#""cmd":"confirm""#) {
        return Some(IpcCmd::Confirm(line.contains(r#""accept":true"#)));
    }
    if line.contains(r#""cmd":"get_state""#) {
        return Some(IpcCmd::GetState);
    }
    None
}

fn extract_str(json: &str, key: &str) -> Option<String> {
    let pat = format!(r#""{key}":""#);
    let start = json.find(&pat)? + pat.len();
    let end = json[start..].find('"')? + start;
    Some(json[start..end].to_string())
}
