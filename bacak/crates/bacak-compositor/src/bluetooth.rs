//! Persistent `bluetoothctl` coprocess for the Bluetooth settings panel.
//!
//! BlueZ pairing needs an interactive *agent* (PIN / passkey prompts), which a
//! one-shot `bluetoothctl <cmd>` can't provide. So we keep one long-lived
//! `bluetoothctl` child with piped stdin/stdout, register its default agent, and
//! a reader thread parses its (ANSI-coloured, interactive) output into
//! [`BtEvent`]s delivered on a channel. Commands are written to its stdin.
//!
//! Pure Rust + the host's `bluetoothctl` — no D-Bus crate (matches `controls`).
//!
//! The child is long-lived; we never `wait()` on it (the global
//! `SIGCHLD = SIG_IGN` would make that fail anyway — see `controls`), we just
//! read its stdout pipe until EOF and `kill()` it on drop.
#![cfg(feature = "runtime")]

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Where the Bluetooth diagnostic log is written (full raw stream + commands).
/// A real file we can both read without root, so pairing can be debugged offline.
fn log_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    std::path::PathBuf::from(home).join("bacak-bt.log")
}

/// Shared append-only log handle. `None` if the file couldn't be opened.
type LogFile = Arc<Mutex<Option<std::fs::File>>>;

/// Append one timestamped line to the log (best-effort, never panics).
fn log_line(log: &LogFile, line: &str) {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    if let Ok(mut g) = log.lock() {
        if let Some(f) = g.as_mut() {
            let _ = writeln!(f, "{t:.3} {line}");
            let _ = f.flush();
        }
    }
}

/// One parsed event from the `bluetoothctl` stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BtEvent {
    /// Adapter power changed.
    Powered(bool),
    /// Discovery (scan) started / stopped.
    Discovering(bool),
    /// A device appeared or its name/alias updated. `name` is best-effort (may
    /// equal the dashed MAC until the real name arrives in a later event).
    Device { mac: String, name: String },
    /// A device's connection state changed.
    Connected { mac: String, connected: bool },
    /// A device's paired state changed.
    Paired { mac: String, paired: bool },
    /// A device was removed from the known list.
    Removed { mac: String },
    /// Agent: confirm this passkey matches the one on the peer (reply yes/no).
    ConfirmPasskey { mac: String, passkey: String },
    /// Agent: show this passkey/PIN on screen for the user to enter on the peer.
    DisplayPasskey { mac: String, passkey: String },
    /// Agent: the peer wants us to type a PIN code (enter via the keyboard).
    RequestPin { mac: String },
    /// Agent: the peer wants us to type a numeric passkey.
    RequestPasskey { mac: String },
    /// A failure line (pairing/connection), surfaced to the UI as status.
    Failed(String),
}

/// Strip ANSI CSI sequences (colour `…m`, erase-line `…K`, etc.) from a line.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // ESC [ … <final-byte 0x40..0x7e>
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&d) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Remove the leading `[bluetoothctl]>` prompt(s) and surrounding whitespace a
/// line may carry, leaving the bare event text.
fn clean(line: &str) -> String {
    let mut s = strip_ansi(line);
    // The interactive prompt can be repeated/embedded; drop every occurrence.
    while let Some(p) = s.find("[bluetoothctl]>") {
        s.replace_range(p..p + "[bluetoothctl]>".len(), "");
    }
    s.trim().to_string()
}

/// Parse one cleaned, newline-terminated line into an event, if meaningful.
fn parse_line(line: &str) -> Option<BtEvent> {
    let s = clean(line);
    if s.is_empty() {
        return None;
    }
    // Controller property changes.
    if let Some(rest) = s.strip_prefix("[CHG] Controller ") {
        // "<MAC> Powered: yes" / "<MAC> Discovering: yes"
        if let Some((_, prop)) = rest.split_once(' ') {
            if let Some(v) = prop.strip_prefix("Powered: ") {
                return Some(BtEvent::Powered(v == "yes"));
            }
            if let Some(v) = prop.strip_prefix("Discovering: ") {
                return Some(BtEvent::Discovering(v == "yes"));
            }
        }
        return None;
    }
    // Device discovery / property change / removal.
    for (tag, removed) in [("[NEW] Device ", false), ("[CHG] Device ", false), ("[DEL] Device ", true)] {
        if let Some(rest) = s.strip_prefix(tag) {
            let (mac, tail) = rest.split_once(' ').unwrap_or((rest, ""));
            if !is_mac(mac) {
                return None;
            }
            if removed {
                return Some(BtEvent::Removed { mac: mac.to_string() });
            }
            // Property updates we care about.
            if let Some(v) = tail.strip_prefix("Connected: ") {
                return Some(BtEvent::Connected { mac: mac.to_string(), connected: v == "yes" });
            }
            if let Some(v) = tail.strip_prefix("Paired: ") {
                return Some(BtEvent::Paired { mac: mac.to_string(), paired: v == "yes" });
            }
            if let Some(v) = tail.strip_prefix("Name: ").or_else(|| tail.strip_prefix("Alias: ")) {
                return Some(BtEvent::Device { mac: mac.to_string(), name: v.trim().to_string() });
            }
            if tag == "[NEW] Device " {
                // Initial sighting: `tail` is the name (often the dashed MAC).
                return Some(BtEvent::Device { mac: mac.to_string(), name: tail.trim().to_string() });
            }
            return None; // other [CHG] props (RSSI/UUIDs/…) ignored
        }
    }
    // Plain `devices` output: "Device <MAC> <name>".
    if let Some(rest) = s.strip_prefix("Device ") {
        let (mac, name) = rest.split_once(' ').unwrap_or((rest, ""));
        if is_mac(mac) {
            return Some(BtEvent::Device { mac: mac.to_string(), name: name.trim().to_string() });
        }
    }
    // Failures — but ignore the benign discovery start/stop races that happen
    // whenever the adapter is mid power-state change (NotReady / Failed).
    if s.starts_with("Failed to ") || s.contains("org.bluez.Error") {
        if s.contains("discovery") {
            return None;
        }
        return Some(BtEvent::Failed(s));
    }
    None
}

/// Parse an agent prompt (which arrives WITHOUT a trailing newline, so the
/// reader scans the unterminated tail for these). The MAC isn't always in the
/// prompt; callers pair it with the device currently being acted on.
fn parse_prompt(tail: &str) -> Option<BtEvent> {
    let s = clean(tail);
    // "[agent] Confirm passkey 123456 (yes/no):"
    if let Some(p) = s.find("Confirm passkey ") {
        let rest = &s[p + "Confirm passkey ".len()..];
        let code: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !code.is_empty() {
            return Some(BtEvent::ConfirmPasskey { mac: String::new(), passkey: code });
        }
    }
    // "[agent] Confirm pairing (yes/no):" → treat as confirm with empty passkey.
    if s.contains("Confirm pairing") && s.contains("(yes/no)") {
        return Some(BtEvent::ConfirmPasskey { mac: String::new(), passkey: String::new() });
    }
    // "[agent] Passkey: 123456" (display-only on this side).
    if let Some(p) = s.find("Passkey: ") {
        let code: String = s[p + "Passkey: ".len()..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if !code.is_empty() {
            return Some(BtEvent::DisplayPasskey { mac: String::new(), passkey: code });
        }
    }
    if s.contains("Enter PIN code") {
        return Some(BtEvent::RequestPin { mac: String::new() });
    }
    if s.contains("Enter passkey") {
        return Some(BtEvent::RequestPasskey { mac: String::new() });
    }
    None
}

fn is_mac(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    parts.len() == 6 && parts.iter().all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// A minimal BlueZ OBEX Object-Push agent (Python + python-dbus). Receiving a
/// file over Bluetooth needs an agent registered with `org.bluez.obex` that
/// authorises incoming pushes; without it BlueZ rejects the transfer and the
/// phone reports "can't send". This one auto-accepts and saves to ~/Downloads.
/// (Pure host tooling — the compositor stays D-Bus-crate-free.)
const OBEX_AGENT_PY: &str = r#"#!/usr/bin/env python3
import os, sys, time, dbus, dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

HOME = os.path.expanduser("~")
SAVE_DIR = os.path.join(HOME, "Downloads")
os.makedirs(SAVE_DIR, exist_ok=True)
LOG = os.path.join(HOME, ".cache", "bacak", "obex-agent.log")
AGENT_PATH = "/bacak/obex/agent"
BUS = "org.bluez.obex"

def log(m):
    try:
        with open(LOG, "a") as f: f.write("%.0f %s\n" % (time.time(), m))
    except Exception: pass

class Agent(dbus.service.Object):
    @dbus.service.method("org.bluez.obex.Agent1", in_signature="o", out_signature="s")
    def AuthorizePush(self, path):
        b = dbus.SessionBus()
        props = dbus.Interface(b.get_object(BUS, path), "org.freedesktop.DBus.Properties")
        name = os.path.basename(str(props.Get("org.bluez.obex.Transfer1", "Name")))
        dest = os.path.join(SAVE_DIR, name or "dosya")
        base, ext = os.path.splitext(dest)
        i = 1
        while os.path.exists(dest):
            dest = "%s-%d%s" % (base, i, ext); i += 1
        log("AuthorizePush -> %s" % dest)
        return dest
    @dbus.service.method("org.bluez.obex.Agent1", in_signature="", out_signature="")
    def Cancel(self): log("Cancel")
    @dbus.service.method("org.bluez.obex.Agent1", in_signature="", out_signature="")
    def Release(self): log("Release")

DBusGMainLoop(set_as_default=True)
try:
    bus = dbus.SessionBus()
    Agent(bus, AGENT_PATH)
    mgr = dbus.Interface(bus.get_object(BUS, "/org/bluez/obex"), "org.bluez.obex.AgentManager1")
    mgr.RegisterAgent(AGENT_PATH)
    log("registered OK (bus=%s)" % os.environ.get("DBUS_SESSION_BUS_ADDRESS", "?"))
except Exception as e:
    log("register FAILED: %s" % e)
    sys.exit(0)
GLib.MainLoop().run()
"#;

/// Python scripti: obexd D-Bus API'sı üzerinden dosya gönder.
/// bluetooth-sendto veya başka araç GEREKTIRMEZ — obexd (bluez-obexd) yeterli.
const OBEX_SENDER_PY: &str = r#"#!/usr/bin/env python3
import os, sys, time, dbus, dbus.mainloop.glib
from gi.repository import GLib

HOME = os.path.expanduser("~")
LOG = os.path.join(HOME, ".cache", "bacak", "obex-send.log")

def log(m):
    try:
        with open(LOG, "a") as f: f.write("%.0f %s\n" % (time.time(), m))
    except Exception: pass

def main():
    if len(sys.argv) < 3:
        log("Kullanim: obex-send.py <mac> <dosya>"); sys.exit(1)
    mac = sys.argv[1]
    path = os.path.abspath(sys.argv[2])
    if not os.path.exists(path):
        log("Dosya bulunamadi: " + path); sys.exit(1)
    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    loop = GLib.MainLoop()
    try:
        client = dbus.Interface(
            bus.get_object("org.bluez.obex", "/org/bluez/obex"),
            "org.bluez.obex.Client1")
        log("Oturum aciliyor: " + mac)
        session_path = client.CreateSession(mac, {"Target": dbus.String("opp")})
        opp = dbus.Interface(
            bus.get_object("org.bluez.obex", session_path),
            "org.bluez.obex.ObjectPush1")
        log("Dosya gonderiliyor: " + path)
        transfer_path, _ = opp.SendFile(path)
        iface = dbus.Interface(
            bus.get_object("org.bluez.obex", transfer_path),
            "org.freedesktop.DBus.Properties")
        def check():
            try:
                status = str(iface.Get("org.bluez.obex.Transfer1", "Status"))
                if status == "complete":
                    log("Tamamlandi: " + path)
                    try: client.RemoveSession(session_path)
                    except: pass
                    loop.quit(); return False
                elif status == "error":
                    log("Hata: transfer basarisiz")
                    try: client.RemoveSession(session_path)
                    except: pass
                    loop.quit(); return False
            except Exception as e:
                log("Kontrol hatasi: " + str(e))
                loop.quit(); return False
            return True
        GLib.timeout_add(500, check)
        loop.run()
    except dbus.exceptions.DBusException as e:
        log("D-Bus hatasi: " + str(e)); sys.exit(1)

if __name__ == "__main__":
    main()
"#;

/// Dosyayı Bluetooth üzerinden gönder — obexd D-Bus OPP kullanır.
/// Arka planda çalışır; compositor'u bloklamaz.
pub fn send_file_obex(mac: &str, path: &str) {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let dir = std::path::PathBuf::from(&home).join(".cache/bacak");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let script = dir.join("obex-send.py");
    if std::fs::write(&script, OBEX_SENDER_PY).is_err() {
        return;
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
    let user_bus = format!("unix:path={runtime}/bus");
    let mac = mac.to_string();
    let path = path.to_string();
    std::thread::spawn(move || {
        let _ = Command::new("python3")
            .arg(&script)
            .arg(&mac)
            .arg(&path)
            .env("DBUS_SESSION_BUS_ADDRESS", &user_bus)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    });
}

/// Write the OBEX agent script and launch it (detached, fire-and-forget) so
/// incoming Bluetooth file transfers are accepted into ~/Downloads. Idempotent:
/// a second instance exits when it finds an agent already registered.
pub fn start_obex_receiver() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let dir = std::path::PathBuf::from(&home).join(".cache/bacak");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let script = dir.join("obex-agent.py");
    if std::fs::write(&script, OBEX_AGENT_PY).is_err() {
        return;
    }
    // The OPP server that bluetoothd routes incoming files to is the *systemd
    // user* obexd on `$XDG_RUNTIME_DIR/bus` — NOT the compositor's private
    // session bus (a separate obexd there gives NoReply / doesn't see the push).
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
    let user_bus = format!("unix:path={runtime}/bus");
    let _ = Command::new("python3")
        .arg(&script)
        .env("DBUS_SESSION_BUS_ADDRESS", &user_bus)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Run a one-shot `bluetoothctl <args>` and return trimmed stdout, or `None`.
/// Reads stdout to EOF (works under the global `SIGCHLD = SIG_IGN`; see
/// `controls`). Used for quick state queries alongside the live coprocess.
fn bctl_capture(args: &[&str]) -> Option<String> {
    let mut child = Command::new("bluetoothctl")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let mut s = String::new();
    child.stdout.take()?.read_to_string(&mut s).ok()?;
    Some(s)
}

/// Parse `Device <MAC> <name>` lines (stripping ANSI) into `(mac, name)` pairs.
fn parse_device_lines(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|l| {
            let s = clean(l);
            let rest = s.strip_prefix("Device ")?;
            let (mac, name) = rest.split_once(' ').unwrap_or((rest, ""));
            is_mac(mac).then(|| (mac.to_string(), name.trim().to_string()))
        })
        .collect()
}

/// Devices BlueZ already has bonded (`bluetoothctl devices Paired`).
pub fn paired_devices() -> Vec<(String, String)> {
    bctl_capture(&["devices", "Paired"]).map(|o| parse_device_lines(&o)).unwrap_or_default()
}

/// MACs currently connected (`bluetoothctl devices Connected`).
pub fn connected_macs() -> Vec<String> {
    bctl_capture(&["devices", "Connected"])
        .map(|o| parse_device_lines(&o).into_iter().map(|(m, _)| m).collect())
        .unwrap_or_default()
}

/// `bluetoothctl info <mac>` → (batarya 0-100, icon adı).
/// Batarya yoksa `None`; icon yoksa boş string.
pub fn device_info(mac: &str) -> (Option<u8>, String) {
    let out = match bctl_capture(&["info", mac]) {
        Some(s) => s,
        None => return (None, String::new()),
    };
    let mut battery: Option<u8> = None;
    let mut icon = String::new();
    for line in out.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Battery Percentage:") {
            // "0x50 (80)" → 80
            if let (Some(a), Some(b)) = (rest.find('('), rest.find(')')) {
                battery = rest[a + 1..b].trim().parse::<u8>().ok();
            }
        } else if let Some(rest) = line.strip_prefix("Icon:") {
            icon = rest.trim().to_string();
        }
    }
    (battery, icon)
}

/// BlueZ icon adını Türkçe cihaz türüne çevir.
pub fn icon_to_type(icon: &str) -> &'static str {
    match icon {
        "phone" => "Telefon",
        "audio-headset" => "Kulaklık",
        "audio-headphones" => "Kulaklık",
        "audio-card" => "Ses Kartı",
        "input-keyboard" => "Klavye",
        "input-mouse" => "Fare",
        "input-gaming" => "Oyun Kolu",
        "input-tablet" => "Tablet",
        "computer" => "Bilgisayar",
        "printer" => "Yazıcı",
        "camera-photo" => "Fotoğraf Makinesi",
        "camera-video" => "Kamera",
        "modem" => "Modem",
        _ => "",
    }
}

/// Handle to the persistent `bluetoothctl` coprocess.
pub struct BtCtl {
    child: Child,
    stdin: Mutex<ChildStdin>,
    log: LogFile,
    /// Parsed events from the reader thread. Drained by the compositor each tick.
    pub rx: Receiver<BtEvent>,
}

impl BtCtl {
    /// Spawn `bluetoothctl`, register its default agent, and start the reader
    /// thread. `None` if the binary is missing.
    pub fn start() -> Option<Self> {
        // Fresh log per session (truncate), so it stays small and relevant.
        let log: LogFile = Arc::new(Mutex::new(
            std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(log_path()).ok(),
        ));
        log_line(&log, "== bluetoothctl session ==");

        let mut child = Command::new("bluetoothctl")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        let (tx, rx) = channel();
        let rlog = log.clone();
        std::thread::spawn(move || reader_loop(stdout, tx, rlog));
        let stdin = child.stdin.take()?;
        let me = BtCtl { child, stdin: Mutex::new(stdin), log, rx };
        // KeyboardDisplay agent → we get passkey/PIN prompts.
        me.send("agent KeyboardDisplay");
        me.send("default-agent");
        // OBEX receiver so phones can push files to us (→ ~/Downloads).
        start_obex_receiver();
        Some(me)
    }

    /// Append a UI-side diagnostic line to the same Bluetooth log file.
    pub fn log(&self, msg: &str) {
        log_line(&self.log, &format!("[ui] {msg}"));
    }

    /// Write one command line to the coprocess (newline appended). Best-effort.
    pub fn send(&self, cmd: &str) {
        tracing::info!(">> {cmd}");
        log_line(&self.log, &format!(">> {cmd}"));
        if let Ok(mut w) = self.stdin.lock() {
            let _ = w.write_all(cmd.as_bytes());
            let _ = w.write_all(b"\n");
            let _ = w.flush();
        }
    }

    pub fn power(&self, on: bool) {
        self.send(if on { "power on" } else { "power off" });
    }
    pub fn scan(&self, on: bool) {
        self.send(if on { "scan on" } else { "scan off" });
    }
    pub fn list_devices(&self) {
        self.send("devices");
    }
    pub fn pair(&self, mac: &str) {
        self.send(&format!("pair {mac}"));
    }
    pub fn connect(&self, mac: &str) {
        self.send(&format!("connect {mac}"));
    }
    pub fn disconnect(&self, mac: &str) {
        self.send(&format!("disconnect {mac}"));
    }
    pub fn trust(&self, mac: &str) {
        self.send(&format!("trust {mac}"));
    }
    pub fn remove(&self, mac: &str) {
        self.send(&format!("remove {mac}"));
    }
    /// Reply to an agent yes/no prompt (passkey confirmation / pairing).
    pub fn agent_yes(&self, yes: bool) {
        self.send(if yes { "yes" } else { "no" });
    }
    /// Reply to an agent PIN/passkey request with the typed value.
    pub fn agent_value(&self, value: &str) {
        self.send(value);
    }
}

impl Drop for BtCtl {
    fn drop(&mut self) {
        self.send("quit");
        let _ = self.child.kill();
    }
}

/// Read the coprocess stdout in chunks, emitting events for complete lines and
/// for un-terminated agent prompts (which never get a newline).
fn reader_loop(
    mut stdout: std::process::ChildStdout,
    tx: std::sync::mpsc::Sender<BtEvent>,
    log: LogFile,
) {
    let mut buf = [0u8; 4096];
    let mut acc = String::new();
    loop {
        match stdout.read(&mut buf) {
            Ok(0) => break, // EOF: coprocess exited
            Ok(n) => acc.push_str(&String::from_utf8_lossy(&buf[..n])),
            Err(_) => break,
        }
        // Emit complete lines. An agent prompt may arrive WITH a trailing
        // newline (some bluetoothctl builds echo one), so try the prompt parser
        // too — otherwise the passkey/PIN request would be silently dropped.
        while let Some(nl) = acc.find('\n') {
            let line: String = acc.drain(..=nl).collect();
            // Log only pairing/agent/failure lines (ground truth for diagnosing
            // a future device), not the whole RSSI-churn stream.
            let c = clean(&line);
            if !c.is_empty()
                && (c.contains("agent")
                    || c.contains("asskey")
                    || c.contains("PIN")
                    || c.contains("Paired")
                    || c.contains("Pairing")
                    || c.contains("Failed")
                    || c.contains("Authentic"))
            {
                log_line(&log, &format!("<< {c}"));
            }
            if let Some(ev) = parse_line(&line).or_else(|| parse_prompt(&line)) {
                if tx.send(ev).is_err() {
                    return;
                }
            }
        }
        // The remaining tail has no newline — check for an un-terminated prompt.
        if let Some(ev) = parse_prompt(&acc) {
            log_line(&log, &format!("<<tail {}", clean(&acc)));
            if tx.send(ev).is_err() {
                return;
            }
            acc.clear(); // consumed the prompt
        } else {
            let t = clean(&acc);
            if !t.is_empty() && (t.contains("yes/no") || t.contains("PIN") || t.contains("asskey")) {
                // A prompt we DON'T yet parse — log it raw so we can add it.
                log_line(&log, &format!("<<tail? {t}"));
            }
        }
    }
    log_line(&log, "== bluetoothctl exited ==");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_controller_and_devices() {
        assert_eq!(
            parse_line("[CHG] Controller 40:9C:A7:38:0A:F7 Powered: yes\n"),
            Some(BtEvent::Powered(true))
        );
        assert_eq!(
            parse_line("[CHG] Controller 40:9C:A7:38:0A:F7 Discovering: no\n"),
            Some(BtEvent::Discovering(false))
        );
        assert_eq!(
            parse_line("[NEW] Device D8:5D:E2:69:DC:B2 D8-5D-E2-69-DC-B2\n"),
            Some(BtEvent::Device { mac: "D8:5D:E2:69:DC:B2".into(), name: "D8-5D-E2-69-DC-B2".into() })
        );
        assert_eq!(
            parse_line("[CHG] Device D8:5D:E2:69:DC:B2 Name: BRAVIA 4K\n"),
            Some(BtEvent::Device { mac: "D8:5D:E2:69:DC:B2".into(), name: "BRAVIA 4K".into() })
        );
        assert_eq!(
            parse_line("[CHG] Device D8:5D:E2:69:DC:B2 Connected: yes\n"),
            Some(BtEvent::Connected { mac: "D8:5D:E2:69:DC:B2".into(), connected: true })
        );
        assert_eq!(
            parse_line("Device 63:B7:52:BF:F4:61 Sony WH-1000XM4\n"),
            Some(BtEvent::Device { mac: "63:B7:52:BF:F4:61".into(), name: "Sony WH-1000XM4".into() })
        );
        // RSSI/UUID changes are ignored.
        assert_eq!(parse_line("[CHG] Device 63:B7:52:BF:F4:61 RSSI: -52\n"), None);
    }

    #[test]
    fn strips_prompt_noise() {
        // The captured stream prefixes lines with the prompt + erase-line code.
        assert_eq!(
            parse_line("[bluetoothctl]> \u{1b}[K[CHG] Controller 40:9C:A7:38:0A:F7 Powered: yes\n"),
            Some(BtEvent::Powered(true))
        );
    }

    #[test]
    fn parses_agent_prompts() {
        assert_eq!(
            parse_prompt("[agent] Confirm passkey 123456 (yes/no): "),
            Some(BtEvent::ConfirmPasskey { mac: String::new(), passkey: "123456".into() })
        );
        assert_eq!(
            parse_prompt("[agent] Enter PIN code: "),
            Some(BtEvent::RequestPin { mac: String::new() })
        );
        assert_eq!(
            parse_prompt("[agent] Enter passkey (number in 0-999999): "),
            Some(BtEvent::RequestPasskey { mac: String::new() })
        );
    }

    #[test]
    fn confirm_passkey_survives_a_trailing_newline() {
        // Some builds echo a newline after the prompt → it arrives as a complete
        // line. The reader tries `parse_prompt` per-line, so it must still match.
        assert_eq!(
            parse_prompt("[bluetoothctl]> \u{1b}[K[agent] Confirm passkey 097541 (yes/no):\n"),
            Some(BtEvent::ConfirmPasskey { mac: String::new(), passkey: "097541".into() })
        );
        // A `[CHG] Device … Passkey: NNN` display line (parse_line ignores it,
        // then parse_prompt surfaces it as a display passkey).
        assert!(parse_line("[CHG] Device D8:5D:E2:69:DC:B2 Passkey: 097541\n").is_none());
        assert_eq!(
            parse_prompt("[CHG] Device D8:5D:E2:69:DC:B2 Passkey: 097541\n"),
            Some(BtEvent::DisplayPasskey { mac: String::new(), passkey: "097541".into() })
        );
    }
}
