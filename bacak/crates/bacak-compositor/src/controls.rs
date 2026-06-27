//! System-control shims for the Control Center tiles.
//!
//! Every function is *best-effort*: it shells out to a standard CLI
//! (`nmcli`, `bluetoothctl`, `wpctl`, `systemctl`) and silently swallows
//! a missing tool or a non-zero exit. A desktop without NetworkManager
//! or PipeWire still runs — the matching tile just reflects no change
//! rather than crashing the compositor.
//!
//! Queries (`*_enabled`, `volume`, `wifi_ssid`, `datetime`) block on the
//! child process, so callers run them **once when the panel opens**, not
//! per frame. Mutations fire-and-forget.
//!
//! Pure Rust + the host's own utilities — no D-Bus crate, no web layer.

#![cfg(feature = "runtime")]

use std::io::Read;
use std::process::{Command, Stdio};

// IMPORTANT: the compositor installs `SIGCHLD = SIG_IGN` (see `launcher.rs`),
// so the kernel auto-reaps children and any `wait()` on them fails with
// `ECHILD`. That means `Command::status()`/`output()` — which `wait()`
// internally — return an *error* even though the command ran fine, silently
// losing all output. So we never rely on the exit status here: mutations
// fire-and-forget, and reads drain the child's stdout to EOF (which already
// proves the child finished) instead of waiting on it.

/// Run a command fire-and-forget. The child still execs and takes effect; we
/// can't read its exit status under `SIG_IGN`, so the bool only reports whether
/// the spawn itself succeeded. Stdout/stderr discarded. Never panics.
fn run_ok(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

/// Run a command and capture its trimmed stdout, or `None` if it couldn't be
/// spawned. Reads stdout to EOF (blocks until the child exits) rather than
/// `wait()`-ing, so it works despite the global `SIGCHLD = SIG_IGN`.
fn capture(cmd: &str, args: &[&str]) -> Option<String> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let mut s = String::new();
    child.stdout.take()?.read_to_string(&mut s).ok()?;
    Some(s.trim().to_string())
}

// ----- Wi-Fi (wpa_supplicant / sysfs) ----------------------------------
//
// nmcli is not required. Status is read from sysfs; scanning and connecting
// go through wpa_cli when the control socket exists at /run/wpa_supplicant/<dev>.

const WPA_CTRL: &str = "/run/wpa_supplicant";

/// Find the first wireless device from sysfs (has a `wireless/` subdirectory).
fn wifi_device_sysfs() -> Option<String> {
    let entries = std::fs::read_dir("/sys/class/net").ok()?;
    for entry in entries.flatten() {
        let dev = entry.file_name().to_string_lossy().to_string();
        if std::path::Path::new(&format!("/sys/class/net/{dev}/wireless")).exists() {
            return Some(dev);
        }
    }
    None
}

/// True when the Wi-Fi interface has IFF_UP set.
pub fn wifi_enabled() -> bool {
    if let Some(dev) = wifi_device_sysfs() {
        let path = format!("/sys/class/net/{dev}/flags");
        if let Ok(s) = std::fs::read_to_string(&path) {
            if let Ok(f) = u64::from_str_radix(s.trim().trim_start_matches("0x"), 16) {
                return f & 0x1 != 0; // IFF_UP
            }
        }
    }
    false
}

/// Bring the Wi-Fi radio up (ip link + wpa_supplicant) or down.
pub fn set_wifi(on: bool) {
    let Some(dev) = wifi_device_sysfs() else { return };
    if on {
        run_ok("ip", &["link", "set", &dev, "up"]);
        // Start wpa_supplicant daemon for this interface if not already running.
        let socket = format!("{WPA_CTRL}/{dev}");
        if !std::path::Path::new(&socket).exists() {
            let conf = "/tmp/bacak-wpa.conf";
            if !std::path::Path::new(conf).exists() {
                let _ = std::fs::write(
                    conf,
                    "ctrl_interface=/run/wpa_supplicant\nctrl_interface_group=0\nupdate_config=1\n",
                );
            }
            run_ok(
                "wpa_supplicant",
                &["-B", "-i", &dev, "-c", conf, "-P", &format!("/tmp/wpa_{dev}.pid")],
            );
        }
    } else {
        // Kill the wpa_supplicant instance we started for this interface.
        let pid_file = format!("/tmp/wpa_{dev}.pid");
        if let Ok(s) = std::fs::read_to_string(&pid_file) {
            if let Ok(pid) = s.trim().parse::<u32>() {
                run_ok("kill", &[&pid.to_string()]);
            }
        }
        run_ok("ip", &["link", "set", &dev, "down"]);
    }
}

/// True when the wpa_supplicant control socket for the Wi-Fi device exists.
fn wpa_socket_exists(dev: &str) -> bool {
    std::path::Path::new(&format!("{WPA_CTRL}/{dev}")).exists()
}

/// Run a wpa_cli command and capture its output. Returns None when the socket
/// is unavailable (wpa_supplicant not managing the interface yet).
fn wpa_cli(dev: &str, args: &[&str]) -> Option<String> {
    if !wpa_socket_exists(dev) {
        return None;
    }
    let mut full: Vec<&str> = vec!["-i", dev, "-p", WPA_CTRL];
    full.extend_from_slice(args);
    capture("wpa_cli", &full)
}

/// SSID of the currently-associated Wi-Fi network, if any.
pub fn wifi_ssid() -> Option<String> {
    let dev = wifi_device_sysfs()?;
    let out = wpa_cli(&dev, &["status"])?;
    for line in out.lines() {
        if let Some(v) = line.strip_prefix("ssid=") {
            let s = v.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// One scanned Wi-Fi network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiNet {
    pub ssid: String,
    /// Signal strength 0–100.
    pub signal: u8,
    /// `true` if the network is encrypted (needs a password the first time).
    pub secured: bool,
    /// `true` if this is the currently-connected network.
    pub active: bool,
}

/// Split one `nmcli -t` (terse) line into its fields. nmcli escapes a literal
/// `:` inside a value as `\:` and a `\` as `\\`; we unescape while splitting on
/// the unescaped `:` separators.
fn split_terse(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(&n) = chars.peek() {
                    cur.push(n);
                    chars.next();
                } else {
                    cur.push('\\');
                }
            }
            ':' => fields.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    fields.push(cur);
    fields
}

/// Parse `nmcli -t -f IN-USE,SIGNAL,SECURITY,SSID dev wifi list` output into the
/// strongest-first, SSID-deduped network list.
fn parse_wifi_list(out: &str) -> Vec<WifiNet> {
    let mut seen = std::collections::HashSet::new();
    let mut nets = Vec::new();
    for line in out.lines() {
        let f = split_terse(line);
        if f.len() < 4 {
            continue;
        }
        let active = f[0].trim() == "*";
        let signal: u8 = f[1].trim().parse().unwrap_or(0);
        let sec = f[2].trim();
        let secured = !sec.is_empty() && sec != "--";
        // SSID is the last field; it may itself have contained ':' (already
        // unescaped+split, so re-join any tail fields).
        let ssid = f[3..].join(":");
        let ssid = ssid.trim().to_string();
        if ssid.is_empty() || !seen.insert(ssid.clone()) {
            continue;
        }
        nets.push(WifiNet { ssid, signal, secured, active });
    }
    nets.sort_by(|a, b| b.active.cmp(&a.active).then(b.signal.cmp(&a.signal)));
    nets
}

/// Parse `wpa_cli scan_results` output (tab-separated, first line is header).
/// Format: BSSID\tFREQ\tSIGNAL(dBm)\tFLAGS\tSSID
fn parse_wpa_scan(out: &str, active_ssid: Option<&str>) -> Vec<WifiNet> {
    let mut seen = std::collections::HashSet::new();
    let mut nets = Vec::new();
    for line in out.lines() {
        if line.starts_with("bssid") { continue; } // header
        let parts: Vec<&str> = line.splitn(5, '\t').collect();
        if parts.len() < 5 { continue; }
        let signal_dbm: i32 = parts[2].trim().parse().unwrap_or(-100);
        // Convert dBm to 0–100 quality: 2*(dBm+100) clamped.
        let signal = ((2 * (signal_dbm + 100)).clamp(0, 100)) as u8;
        let flags = parts[3].trim();
        let secured = flags.contains("WPA") || flags.contains("WEP");
        let ssid = parts[4].trim().to_string();
        if ssid.is_empty() || !seen.insert(ssid.clone()) { continue; }
        let active = active_ssid.map(|a| a == ssid).unwrap_or(false);
        nets.push(WifiNet { ssid, signal, secured, active });
    }
    nets.sort_by(|a, b| b.active.cmp(&a.active).then(b.signal.cmp(&a.signal)));
    nets
}

/// Return wpa_supplicant's cached scan results instantly.
pub fn wifi_scan_cached() -> Vec<WifiNet> {
    let Some(dev) = wifi_device_sysfs() else { return Vec::new() };
    let active = wifi_ssid();
    wpa_cli(&dev, &["scan_results"])
        .map(|out| parse_wpa_scan(&out, active.as_deref()))
        .unwrap_or_default()
}

/// Ask wpa_supplicant to start a background scan. Best-effort.
pub fn wifi_rescan_trigger() {
    if let Some(dev) = wifi_device_sysfs() {
        let _ = wpa_cli(&dev, &["scan"]);
    }
}

/// Connect to a Wi-Fi network using wpa_cli. Blocking — run on a background thread.
pub fn wifi_connect(ssid: &str, password: Option<&str>) -> (bool, String) {
    let Some(dev) = wifi_device_sysfs() else {
        return (false, "Wi-Fi arayüzü bulunamadı".to_string());
    };
    if !wpa_socket_exists(&dev) {
        return (false, "wpa_supplicant bağlantı soketi yok — Wi-Fi etkinleştirin".to_string());
    }

    // Add a new network slot and get its ID.
    let id_out = match wpa_cli(&dev, &["add_network"]) {
        Some(s) => s,
        None => return (false, "wpa_cli hatası".to_string()),
    };
    let net_id = id_out.trim();
    if net_id.parse::<u32>().is_err() {
        return (false, format!("Ağ eklenemedi: {net_id}"));
    }

    // SSID must be double-quoted in wpa_cli set_network.
    let ssid_arg = format!("\"{}\"", ssid.replace('"', "\\\""));
    let _ = wpa_cli(&dev, &["set_network", net_id, "ssid", &ssid_arg]);

    if let Some(pw) = password.filter(|p| !p.is_empty()) {
        let psk_arg = format!("\"{}\"", pw.replace('"', "\\\""));
        let _ = wpa_cli(&dev, &["set_network", net_id, "psk", &psk_arg]);
    } else {
        let _ = wpa_cli(&dev, &["set_network", net_id, "key_mgmt", "NONE"]);
    }

    let _ = wpa_cli(&dev, &["select_network", net_id]);
    let _ = wpa_cli(&dev, &["save_config"]);

    // Poll for COMPLETED state (up to ~10 s).
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Some(out) = wpa_cli(&dev, &["status"]) {
            if out.contains("wpa_state=COMPLETED") {
                // Request DHCP for the new connection.
                run_ok("dhcpcd", &["-n", &dev]);
                return (true, format!("Bağlandı: {ssid}"));
            }
        }
    }

    // Clean up the failed network slot.
    let _ = wpa_cli(&dev, &["remove_network", net_id]);
    (false, "Bağlanılamadı".to_string())
}

// ----- Wi-Fi connection details (the active network) -------------------

/// Details of a network connection, for the per-network settings screen.
#[derive(Debug, Clone, Default)]
pub struct WifiDetails {
    /// NetworkManager connection name (used for `connection modify`). Empty when
    /// the device is unmanaged (e.g. an ifupdown/static Ethernet).
    pub conn: String,
    pub autoconnect: bool,
    /// `true` = DHCP (ipv4.method auto), `false` = manual/static.
    pub dhcp: bool,
    pub mac: String,
    pub ipv4: String,
    pub ipv6: String,
    pub gateway: String,
    pub dns: String,
    /// Link is up / carrying (GENERAL.STATE ≥ 100). Drives the Ethernet on/off.
    pub connected: bool,
    /// NetworkManager is managing the device (`GENERAL.NM-MANAGED`).
    pub managed: bool,
    /// The device name (e.g. "enp3s0") — needed for link up/down.
    pub device: String,
}

/// Find the first physical ethernet device from sysfs (no nmcli needed).
/// Criteria: ARPHRD_ETHER (type=1), no `wireless/` subdir, has `device` symlink.
fn eth_device_sysfs() -> Option<String> {
    let entries = std::fs::read_dir("/sys/class/net").ok()?;
    for entry in entries.flatten() {
        let dev = entry.file_name().to_string_lossy().to_string();
        if dev == "lo" {
            continue;
        }
        let base = format!("/sys/class/net/{dev}");
        let Ok(t) = std::fs::read_to_string(format!("{base}/type")) else { continue };
        if t.trim() != "1" {
            continue;
        }
        if std::path::Path::new(&format!("{base}/wireless")).exists() {
            continue;
        }
        if !std::path::Path::new(&format!("{base}/device")).exists() {
            continue;
        }
        return Some(dev);
    }
    None
}

/// True when the first physical ethernet device has an active link.
/// Reads /sys/class/net/<dev>/operstate — works without nmcli.
pub fn ethernet_link_up() -> bool {
    if let Some(dev) = eth_device_sysfs() {
        let path = format!("/sys/class/net/{dev}/operstate");
        if let Ok(state) = std::fs::read_to_string(&path) {
            return matches!(state.trim(), "up" | "unknown");
        }
    }
    false
}

/// The Wi-Fi device name (e.g. "wlp2s0"), skipping the p2p pseudo-device.
/// First device of `dev_type` ("wifi" / "ethernet"), skipping p2p pseudo-devices.
fn net_device(dev_type: &str) -> Option<String> {
    let out = capture("nmcli", &["-t", "-f", "DEVICE,TYPE", "device"])?;
    for line in out.lines() {
        if let Some((dev, ty)) = line.split_once(':') {
            if ty == dev_type && !dev.contains("p2p") {
                return Some(dev.to_string());
            }
        }
    }
    None
}

/// nmcli terse mode escapes `:` as `\:` and `\` as `\\` in values.
/// Unescape after splitting on the first raw `:` (the key never contains `:`).
fn nmcli_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(':') => { out.push(':'); chars.next(); }
                Some('\\') => { out.push('\\'); chars.next(); }
                _ => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Read the active connection's details for a device.
/// Returns `None` if the device cannot be found in nmcli output.
fn details_for_device(dev: &str) -> Option<WifiDetails> {
    let show = capture(
        "nmcli",
        &[
            "-t",
            "-f",
            "GENERAL.HWADDR,GENERAL.CONNECTION,GENERAL.STATE,GENERAL.NM-MANAGED,IP4.ADDRESS,IP6.ADDRESS,IP4.GATEWAY,IP4.DNS",
            "device",
            "show",
            dev,
        ],
    )?;
    let mut d = WifiDetails { device: dev.to_string(), ..Default::default() };
    for line in show.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = nmcli_unescape(v);
        match k {
            "GENERAL.HWADDR" => d.mac = v,
            "GENERAL.CONNECTION" => d.conn = v,
            // "100 (connected)" → connected; "10 (unmanaged)"/"20"/"30" → not.
            "GENERAL.STATE" => d.connected = v.starts_with("100"),
            "GENERAL.NM-MANAGED" => d.managed = v == "yes",
            _ if k.starts_with("IP4.ADDRESS") && d.ipv4.is_empty() => d.ipv4 = v,
            _ if k.starts_with("IP6.ADDRESS") && d.ipv6.is_empty() => d.ipv6 = v,
            // Active gateway/DNS (sensible prefills for the static form).
            "IP4.GATEWAY" => d.gateway = v,
            _ if k.starts_with("IP4.DNS") && d.dns.is_empty() => d.dns = v,
            _ => {}
        }
    }
    if d.mac.is_empty() {
        return None; // not a real device
    }
    // Connection-level props only exist when NM has an active profile.
    if !d.conn.is_empty() {
        let props = capture(
            "nmcli",
            &["-t", "-f", "connection.autoconnect,ipv4.method", "connection", "show", &d.conn],
        )
        .unwrap_or_default();
        for line in props.lines() {
            let Some((k, v)) = line.split_once(':') else { continue };
            let v = nmcli_unescape(v);
            match k {
                "connection.autoconnect" => d.autoconnect = v == "yes",
                "ipv4.method" => d.dhcp = v == "auto",
                _ => {}
            }
        }
    }
    Some(d)
}

/// Bring the Ethernet link up or down via `ip link set`.
pub fn ethernet_set_link(on: bool) {
    if let Some(dev) = eth_device_sysfs() {
        run_ok("ip", &["link", "set", &dev, if on { "up" } else { "down" }]);
    }
}

/// Details of the active Wi-Fi connection via wpa_cli + sysfs.
pub fn wifi_details() -> Option<WifiDetails> {
    let dev = wifi_device_sysfs()?;
    let mac = std::fs::read_to_string(format!("/sys/class/net/{dev}/address"))
        .map(|s| s.trim().to_uppercase())
        .unwrap_or_default();

    let out = wpa_cli(&dev, &["status"])?;
    let mut ssid = String::new();
    let mut connected = false;
    for line in out.lines() {
        if let Some(v) = line.strip_prefix("ssid=") { ssid = v.trim().to_string(); }
        if line.trim() == "wpa_state=COMPLETED" { connected = true; }
    }

    let mut ipv4 = String::new();
    let mut ipv6 = String::new();
    if let Some(a) = capture("ip", &["addr", "show", &dev]) {
        for line in a.lines() {
            let t = line.trim();
            if t.starts_with("inet ") && ipv4.is_empty() {
                if let Some(addr) = t.split_whitespace().nth(1) { ipv4 = addr.to_string(); }
            } else if t.starts_with("inet6 ") && ipv6.is_empty() {
                if let Some(addr) = t.split_whitespace().nth(1) { ipv6 = addr.to_string(); }
            }
        }
    }

    let mut gateway = String::new();
    if let Some(r) = capture("ip", &["route", "show", "default", "dev", &dev]) {
        for line in r.lines() {
            let mut p = line.split_whitespace();
            if p.next() == Some("default") && p.next() == Some("via") {
                if let Some(gw) = p.next() { gateway = gw.to_string(); }
            }
        }
    }

    Some(WifiDetails {
        device: dev,
        mac,
        conn: ssid,
        autoconnect: true,
        dhcp: true,
        ipv4,
        ipv6,
        gateway,
        dns: String::new(),
        connected,
        managed: true,
    })
}

/// Details of the ethernet interface using sysfs + `ip addr show`.
/// Works without NetworkManager.
pub fn ethernet_details() -> Option<WifiDetails> {
    let dev = eth_device_sysfs()?;
    let base = format!("/sys/class/net/{dev}");

    let mac = std::fs::read_to_string(format!("{base}/address"))
        .map(|s| s.trim().to_uppercase())
        .unwrap_or_default();
    if mac.is_empty() {
        return None;
    }

    let connected = std::fs::read_to_string(format!("{base}/operstate"))
        .map(|s| matches!(s.trim(), "up" | "unknown"))
        .unwrap_or(false);

    let mut ipv4 = String::new();
    let mut ipv6 = String::new();
    if let Some(out) = capture("ip", &["addr", "show", &dev]) {
        for line in out.lines() {
            let line = line.trim();
            if line.starts_with("inet ") && ipv4.is_empty() {
                if let Some(addr) = line.split_whitespace().nth(1) {
                    ipv4 = addr.to_string();
                }
            } else if line.starts_with("inet6 ") && ipv6.is_empty() {
                if let Some(addr) = line.split_whitespace().nth(1) {
                    ipv6 = addr.to_string();
                }
            }
        }
    }

    let mut gateway = String::new();
    if let Some(out) = capture("ip", &["route", "show", "default", "dev", &dev]) {
        for line in out.lines() {
            let mut parts = line.split_whitespace();
            if parts.next() == Some("default") && parts.next() == Some("via") {
                if let Some(gw) = parts.next() {
                    gateway = gw.to_string();
                }
            }
        }
    }

    let mut dns = String::new();
    if let Ok(resolv) = std::fs::read_to_string("/etc/resolv.conf") {
        for line in resolv.lines() {
            if let Some(rest) = line.trim().strip_prefix("nameserver") {
                let ns = rest.trim();
                if !ns.is_empty() && !ns.starts_with('#') {
                    dns = ns.to_string();
                    break;
                }
            }
        }
    }

    Some(WifiDetails {
        device: dev,
        mac,
        conn: String::new(),
        autoconnect: false,
        dhcp: true,
        ipv4,
        ipv6,
        gateway,
        dns,
        connected,
        managed: false,
    })
}

/// Convenience: details for the wifi/ethernet device by kind.
pub fn net_details(eth: bool) -> Option<WifiDetails> {
    if eth {
        ethernet_details()
    } else {
        wifi_details()
    }
}

/// Toggle auto-reconnect for a saved wpa_supplicant network (no-op without nmcli).
pub fn wifi_set_autoconnect(_conn: &str, _on: bool) {}

/// Switch to DHCP: runs dhcpcd. Returns true if dhcpcd spawned.
pub fn wifi_set_dhcp(_conn: &str) -> bool {
    if let Some(dev) = wifi_device_sysfs() {
        run_ok("dhcpcd", &["-n", &dev])
    } else {
        false
    }
}

/// Switch to static IP via `ip addr` + `ip route`. Returns true if commands spawned.
pub fn wifi_set_static(_conn: &str, ip_prefix: &str, gateway: &str, _dns: &str) -> bool {
    let Some(dev) = wifi_device_sysfs() else { return false };
    run_ok("ip", &["addr", "add", ip_prefix, "dev", &dev]);
    if !gateway.is_empty() {
        run_ok("ip", &["route", "replace", "default", "via", gateway, "dev", &dev]);
    }
    true
}

// ----- Bluetooth -------------------------------------------------------

pub fn bt_enabled() -> bool {
    capture("bluetoothctl", &["show"])
        .map(|s| {
            s.lines()
                .any(|l| l.trim().eq_ignore_ascii_case("Powered: yes"))
        })
        .unwrap_or(false)
}

pub fn set_bt(on: bool) {
    run_ok("bluetoothctl", &["power", if on { "on" } else { "off" }]);
}

// ----- Volume (PipeWire / WirePlumber) ---------------------------------

const SINK: &str = "@DEFAULT_AUDIO_SINK@";
const SOURCE: &str = "@DEFAULT_AUDIO_SOURCE@";

/// `(level 0.0..=1.0, muted)`. Falls back to `(0.5, false)` if `wpctl`
/// is unavailable so the slider still has a sane starting position.
pub fn volume() -> (f32, bool) {
    let Some(out) = capture("wpctl", &["get-volume", SINK]) else {
        return (0.5, false);
    };
    // e.g. "Volume: 0.55" or "Volume: 0.55 [MUTED]"
    let muted = out.contains("[MUTED]");
    let level = out
        .split_whitespace()
        .find_map(|tok| tok.parse::<f32>().ok())
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);
    (level, muted)
}

pub fn set_volume(level: f32) {
    let v = level.clamp(0.0, 1.0);
    run_ok("wpctl", &["set-volume", SINK, &format!("{v:.2}")]);
}

pub fn set_mute(mute: bool) {
    run_ok("wpctl", &["set-mute", SINK, if mute { "1" } else { "0" }]);
}

/// `(level 0.0..=1.0, muted)` for the default microphone input.
pub fn mic_volume() -> (f32, bool) {
    let Some(out) = capture("wpctl", &["get-volume", SOURCE]) else {
        return (0.75, false);
    };
    let muted = out.contains("[MUTED]");
    let level = out
        .split_whitespace()
        .find_map(|tok| tok.parse::<f32>().ok())
        .unwrap_or(0.75)
        .clamp(0.0, 1.0);
    (level, muted)
}

pub fn set_mic_volume(level: f32) {
    let v = level.clamp(0.0, 1.0);
    run_ok("wpctl", &["set-volume", SOURCE, &format!("{v:.2}")]);
}

// ----- Audio device enumeration (PipeWire / WirePlumber) ---------------

#[derive(Clone)]
pub struct AudioSink {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// List available audio output sinks by parsing `wpctl status`.
/// Returns an empty vec if wpctl is unavailable or has no sinks.
pub fn list_sinks() -> Vec<AudioSink> {
    let Some(out) = capture("wpctl", &["status"]) else {
        return Vec::new();
    };
    let mut in_sinks = false;
    let mut sinks = Vec::new();
    for line in out.lines() {
        if line.contains("Sinks:") {
            in_sinks = true;
            continue;
        }
        if in_sinks {
            // Exit on the next section header.
            if line.contains("Sources:") || line.contains("Filters:") || line.contains("Chains:") {
                break;
            }
            // Strip box-drawing chars; typical line:
            //   "│  ├─ 51. * Built-in Audio Analog Stereo [vol: 0.55]"
            let clean: String = line
                .chars()
                .filter(|c| !matches!(*c, '│' | '├' | '─' | '└'))
                .collect();
            let trimmed = clean.trim();
            // Expect "ID. [*] Name [vol: ...]"
            let mut parts = trimmed.splitn(2, ". ");
            let id_str = parts.next().unwrap_or("").trim();
            let rest = parts.next().unwrap_or("").trim();
            if id_str.parse::<u32>().is_err() || rest.is_empty() {
                continue;
            }
            let is_default = rest.starts_with('*');
            let name_part = rest.trim_start_matches('*').trim();
            let name = if let Some(pos) = name_part.rfind('[') {
                name_part[..pos].trim().to_string()
            } else {
                name_part.to_string()
            };
            sinks.push(AudioSink {
                id: id_str.to_string(),
                name: if name.is_empty() { format!("Cihaz {id_str}") } else { name },
                is_default,
            });
        }
    }
    sinks
}

/// List available audio input sources by parsing `wpctl status`.
pub fn list_sources() -> Vec<AudioSink> {
    let Some(out) = capture("wpctl", &["status"]) else {
        return Vec::new();
    };
    let mut in_sources = false;
    let mut sources = Vec::new();
    for line in out.lines() {
        if line.contains("Sources:") {
            in_sources = true;
            continue;
        }
        if in_sources {
            if line.contains("Filters:") || line.contains("Chains:") || line.contains("Sinks:") {
                break;
            }
            let clean: String = line
                .chars()
                .filter(|c| !matches!(*c, '│' | '├' | '─' | '└'))
                .collect();
            let trimmed = clean.trim();
            let mut parts = trimmed.splitn(2, ". ");
            let id_str = parts.next().unwrap_or("").trim();
            let rest = parts.next().unwrap_or("").trim();
            if id_str.parse::<u32>().is_err() || rest.is_empty() {
                continue;
            }
            let is_default = rest.starts_with('*');
            let name_part = rest.trim_start_matches('*').trim();
            let name = if let Some(pos) = name_part.rfind('[') {
                name_part[..pos].trim().to_string()
            } else {
                name_part.to_string()
            };
            sources.push(AudioSink {
                id: id_str.to_string(),
                name: if name.is_empty() { format!("Cihaz {id_str}") } else { name },
                is_default,
            });
        }
    }
    sources
}

/// Set the WirePlumber default audio input source by node ID.
pub fn set_default_source(id: &str) {
    run_ok("wpctl", &["set-default", id]);
    // Move existing source-outputs (recording streams) to the new device.
    let Some(src_name) = capture("pactl", &["get-default-source"]) else { return };
    let src_name = src_name.trim();
    if src_name.is_empty() { return; }
    let Some(outputs) = capture("pactl", &["list", "short", "source-outputs"]) else { return };
    for line in outputs.lines() {
        if let Some(stream_id) = line.split_whitespace().next() {
            run_ok("pactl", &["move-source-output", stream_id, src_name]);
        }
    }
}

/// Set the WirePlumber default audio output sink by node ID, then move all
/// existing sink-inputs (running app streams) to the new device so they
/// switch immediately without needing to restart the apps.
pub fn set_default_sink(id: &str) {
    run_ok("wpctl", &["set-default", id]);
    // After wpctl updates the default, ask pactl (pipewire-pulse) for the
    // canonical sink name and migrate every running stream to it.
    let Some(sink_name) = capture("pactl", &["get-default-sink"]) else { return };
    let sink_name = sink_name.trim();
    if sink_name.is_empty() {
        return;
    }
    let Some(inputs) = capture("pactl", &["list", "short", "sink-inputs"]) else { return };
    for line in inputs.lines() {
        if let Some(stream_id) = line.split_whitespace().next() {
            run_ok("pactl", &["move-sink-input", stream_id, sink_name]);
        }
    }
}

// ----- Power -----------------------------------------------------------

pub fn power_off() {
    run_ok("systemctl", &["poweroff"]);
}

pub fn reboot() {
    run_ok("systemctl", &["reboot"]);
}

/// End the session: raise `SIGTERM` on ourselves. The signal handler in
/// [`crate::signals`] flips the shutdown flag, the backend loop flushes
/// and exits cleanly, and the session manager (GDM) returns to the login
/// screen — i.e. a logout.
pub fn logout() {
    // SAFETY: `raise` with a standard signal number is async-signal-safe
    // and only nudges our own already-installed handler.
    unsafe {
        libc::raise(libc::SIGTERM);
    }
}

// ----- Clock -----------------------------------------------------------

/// Localised "HH:MM · Weekday DD Month" header line, via `date(1)` so we
/// pull in no calendar/timezone crate. Empty string on failure.
pub fn datetime() -> String {
    capture("date", &["+%H:%M  ·  %A, %d %B"]).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_terse_unescapes_colons() {
        // nmcli pads the IN-USE column with a space; `wifi_scan` trims it.
        assert_eq!(split_terse(" :100:WPA2:Samm_IoT"), [" ", "100", "WPA2", "Samm_IoT"]);
        // An SSID containing a literal ':' is escaped by nmcli as '\:'.
        assert_eq!(split_terse("*:80:WPA2:My\\:Net"), ["*", "80", "WPA2", "My:Net"]);
    }
}
