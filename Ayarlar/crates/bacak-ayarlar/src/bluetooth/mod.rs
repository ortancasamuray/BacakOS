//! Compositor IPC istemcisi — Bluetooth olaylarını compositor'dan alır,
//! komutları compositor'a gönderir. zbus / D-Bus bağımlılığı yok.
//!
//! Soket: `$XDG_RUNTIME_DIR/bacak-bt.sock`
//! Protokol: satır-başlıklı JSON (ayrıntı için compositor/src/bt_ipc.rs)

pub mod obex;
use obex::TransferUpdate;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use tokio::sync::mpsc;

// ─── Paylaşılan tipler ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub address: String,
    pub name: String,
    pub paired: bool,
    pub connected: bool,
    pub rssi: i16,
    pub icon: String,
}

pub enum BtCmd {
    SetPowered(bool),
    StartScan,
    StopScan,
    Pair(String),
    Connect(String),
    Disconnect(String),
    Forget(String),
    SendFile { address: String, path: String },
    ConfirmPairing(bool),
    AcceptIncoming(bool),
}

pub enum BtEvent {
    Powered(bool),
    Scanning(bool),
    AllDevices(Vec<DeviceInfo>),
    PairingRequest { device_name: String, passkey: String },
    TransferProgress { file: String, progress: f32, status: String },
    IncomingFile { device: String, file_name: String },
    Toast(String),
}

// ─── Soket yolu ───────────────────────────────────────────────────────────────

fn socket_path() -> std::path::PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
    std::path::PathBuf::from(runtime).join("bacak-bt.sock")
}

// ─── IPC giriş noktası ────────────────────────────────────────────────────────

pub async fn run(mut cmd_rx: mpsc::UnboundedReceiver<BtCmd>, event_tx: mpsc::UnboundedSender<BtEvent>) {
    // Compositor'a bağlanmayı bekle — compositor geç başlamış olabilir.
    let stream = loop {
        match UnixStream::connect(socket_path()) {
            Ok(s) => break s,
            Err(_) => {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            }
        }
    };
    eprintln!("[IPC] compositor'a bağlandı");

    let write_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => { eprintln!("[IPC] clone hatası: {e}"); return; }
    };

    // Yazıcı kanalı
    let (write_tx, mut write_rx) = mpsc::unbounded_channel::<String>();

    // Okuyucu görevi — compositor'dan JSON satırları al
    let event_tx2 = event_tx.clone();
    let write_tx2 = write_tx.clone();
    tokio::spawn(async move {
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            match line {
                Ok(l) => handle_ipc_event(&l, &event_tx2, &write_tx2),
                Err(_) => break,
            }
        }
        eprintln!("[IPC] compositor bağlantısı kesildi");
    });

    // Yazıcı görevi — yazma isteklerini soket'e ilet
    tokio::spawn(async move {
        let mut ws = write_stream;
        while let Some(json) = write_rx.recv().await {
            if writeln!(ws, "{json}").is_err() {
                break;
            }
        }
    });

    // Başlangıç durumunu iste
    write_tx.send(r#"{"cmd":"get_state"}"#.into()).ok();

    // Komut döngüsü — UI'dan gelen komutları compositor'a ilet
    while let Some(cmd) = cmd_rx.recv().await {
        let json = match cmd {
            BtCmd::SetPowered(on) => format!(r#"{{"cmd":"power","on":{on}}}"#),
            BtCmd::StartScan    => r#"{"cmd":"scan","on":true}"#.into(),
            BtCmd::StopScan     => r#"{"cmd":"scan","on":false}"#.into(),
            BtCmd::Pair(mac)    => format!(r#"{{"cmd":"pair","mac":"{mac}"}}"#),
            BtCmd::Connect(mac) => format!(r#"{{"cmd":"connect","mac":"{mac}"}}"#),
            BtCmd::Disconnect(mac) => format!(r#"{{"cmd":"disconnect","mac":"{mac}"}}"#),
            BtCmd::Forget(mac)  => format!(r#"{{"cmd":"forget","mac":"{mac}"}}"#),
            BtCmd::ConfirmPairing(accept) => format!(r#"{{"cmd":"confirm","accept":{accept}}}"#),
            BtCmd::SendFile { address, path } => {
                // OBEX gönderme: doğrudan obexd üzerinden (agent gerektirmez)
                let tx = event_tx.clone();
                let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<TransferUpdate>();
                tokio::spawn(async move {
                    match obex::send_file_cli(&address, &path).await {
                        Ok(_) => { let _ = tx.send(BtEvent::Toast("Dosya gönderildi".into())); }
                        Err(e) => { let _ = tx.send(BtEvent::Toast(format!("Gönderim hatası: {e}"))); }
                    }
                    drop(progress_rx);
                });
                continue;
            }
            BtCmd::AcceptIncoming(_) => continue, // compositor otomatik kabul eder
        };
        write_tx.send(json).ok();
    }
}

// ─── Compositor olayını parse et ─────────────────────────────────────────────

fn handle_ipc_event(
    line: &str,
    event_tx: &mpsc::UnboundedSender<BtEvent>,
    _write_tx: &mpsc::UnboundedSender<String>,
) {
    let line = line.trim();
    if line.is_empty() { return; }

    // {"t":"powered","on":true}
    if line.contains(r#""t":"powered""#) {
        let on = line.contains(r#""on":true"#);
        event_tx.send(BtEvent::Powered(on)).ok();
        return;
    }

    // {"t":"scanning","on":true}
    if line.contains(r#""t":"scanning""#) {
        let on = line.contains(r#""on":true"#);
        event_tx.send(BtEvent::Scanning(on)).ok();
        return;
    }

    // {"t":"toast","msg":"..."}
    if line.contains(r#""t":"toast""#) {
        let msg = extract_str(line, "msg").unwrap_or_default();
        event_tx.send(BtEvent::Toast(msg)).ok();
        return;
    }

    // {"t":"pairing","device":"...","passkey":"..."}
    if line.contains(r#""t":"pairing""#) {
        let device = extract_str(line, "device").unwrap_or_default();
        let passkey = extract_str(line, "passkey").unwrap_or_default();
        event_tx.send(BtEvent::PairingRequest { device_name: device, passkey }).ok();
        return;
    }

    // {"t":"devices","list":[...]}
    if line.contains(r#""t":"devices""#) {
        let devices = parse_device_list(line);
        event_tx.send(BtEvent::AllDevices(devices)).ok();
    }
}

fn parse_device_list(line: &str) -> Vec<DeviceInfo> {
    // Basit JSON array parser — serde bağımlılığı eklememek için elle.
    // Format: [{"mac":"..","name":"..","paired":true,"connected":false},...]
    let start = match line.find('[') {
        Some(i) => i + 1,
        None => return Vec::new(),
    };
    let end = match line.rfind(']') {
        Some(i) => i,
        None => return Vec::new(),
    };
    let inner = &line[start..end];

    let mut devices = Vec::new();
    // Her `{...}` bloğunu ayır
    let mut depth = 0usize;
    let mut obj_start = 0usize;
    for (i, ch) in inner.char_indices() {
        match ch {
            '{' => {
                if depth == 0 { obj_start = i; }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let obj = &inner[obj_start..=i];
                    if let Some(d) = parse_device_obj(obj) {
                        devices.push(d);
                    }
                }
            }
            _ => {}
        }
    }
    devices
}

fn parse_device_obj(obj: &str) -> Option<DeviceInfo> {
    let mac = extract_str(obj, "mac")?;
    let name = extract_str(obj, "name").unwrap_or_else(|| mac.clone());
    let paired = obj.contains(r#""paired":true"#);
    let connected = obj.contains(r#""connected":true"#);
    Some(DeviceInfo {
        address: mac,
        name,
        paired,
        connected,
        rssi: 0,
        icon: String::new(),
    })
}

fn extract_str(json: &str, key: &str) -> Option<String> {
    let pat = format!(r#""{key}":""#);
    let start = json.find(&pat)? + pat.len();
    let end = json[start..].find('"')? + start;
    Some(json[start..end].to_string())
}
