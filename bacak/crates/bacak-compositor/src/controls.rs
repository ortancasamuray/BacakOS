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

// ----- Wi-Fi (NetworkManager) -----------------------------------------

pub fn wifi_enabled() -> bool {
    capture("nmcli", &["radio", "wifi"])
        .map(|s| s.eq_ignore_ascii_case("enabled"))
        .unwrap_or(false)
}

pub fn set_wifi(on: bool) {
    run_ok("nmcli", &["radio", "wifi", if on { "on" } else { "off" }]);
}

/// SSID of the currently-active Wi-Fi connection, if any.
pub fn wifi_ssid() -> Option<String> {
    let out = capture("nmcli", &["-t", "-f", "ACTIVE,SSID", "dev", "wifi"])?;
    for line in out.lines() {
        // `-t` output is colon-separated: "yes:MyNetwork".
        if let Some(rest) = line.strip_prefix("yes:") {
            let ssid = rest.trim();
            if !ssid.is_empty() {
                return Some(ssid.to_string());
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

/// Read NM's *cached* scan instantly (no rescan). While connected the cache
/// reports the associated AP's real signal but 0 for the others until a real
/// scan refreshes them — fine as a first paint, then re-read after a rescan.
pub fn wifi_scan_cached() -> Vec<WifiNet> {
    capture("nmcli", &["-t", "-f", "IN-USE,SIGNAL,SECURITY,SSID", "dev", "wifi", "list"])
        .map(|out| parse_wifi_list(&out))
        .unwrap_or_default()
}

/// Ask NM to start a fresh scan (results arrive asynchronously — poll
/// [`wifi_scan_cached`] for a few seconds afterwards). Best-effort: a rate-limit
/// error is harmless. Authorized in the compositor's active polkit session
/// (`wifi.scan` allow_active=yes).
pub fn wifi_rescan_trigger() {
    let _ = run_ok("nmcli", &["dev", "wifi", "rescan"]);
}

/// Connect to a Wi-Fi network by SSID. `password` is `None` for an open network
/// or one with a saved profile. Blocking (can take several seconds) — run on a
/// background thread. Success is read from nmcli's stdout ("…successfully
/// activated…"); failures print only to stderr, so an empty/other stdout means
/// failure. (We parse stdout rather than the exit status because `SIGCHLD =
/// SIG_IGN` makes the status unreadable — see the note at the top of the file.)
pub fn wifi_connect(ssid: &str, password: Option<&str>) -> (bool, String) {
    let has_pw = password.map(|p| !p.is_empty()).unwrap_or(false);
    // With a fresh password, drop any stale/partial saved profile for this SSID
    // first so nmcli rebuilds a complete one. Re-activating a half-written
    // profile is what yields "802-11-wireless-security.key-mgmt: property is
    // missing". `capture` blocks until delete finishes, so ordering is safe.
    if has_pw {
        let _ = capture("nmcli", &["connection", "delete", "id", ssid]);
    }
    let mut args: Vec<&str> = vec!["device", "wifi", "connect", ssid];
    if let Some(p) = password {
        if !p.is_empty() {
            args.push("password");
            args.push(p);
        }
    }
    // Pin the C locale so the success line is the stable English "successfully
    // activated". Read stdout *and* stderr to EOF (blocks until the connect
    // settles); a failure prints the reason to stderr, which we surface to the
    // UI. Exit status is unreadable under SIGCHLD=SIG_IGN. nmcli's output is
    // tiny, so reading the two pipes in turn can't deadlock.
    let mut child = match Command::new("nmcli")
        .args(&args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return (false, "nmcli çalıştırılamadı".to_string()),
    };
    let mut out = String::new();
    let mut err = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut out);
    }
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut err);
    }
    // Ground truth, locale-independent: did we actually end up on this SSID?
    // (The stdout string is a fast-path; the live check is authoritative and
    // survives a localized nmcli that doesn't honour LC_ALL for every message.)
    let ok = out.to_lowercase().contains("successfully activated")
        || wifi_ssid().as_deref() == Some(ssid);
    let msg = if ok {
        format!("Bağlandı: {ssid}")
    } else {
        // Strip nmcli's "Error: " prefix for a cleaner line.
        let e = err.trim().trim_start_matches("Error:").trim();
        if e.is_empty() {
            "Bağlanılamadı".to_string()
        } else {
            e.to_string()
        }
    };
    (ok, msg)
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

/// Read the active connection's details for a device. Unlike `dev wifi list`,
/// the `device show` / `connection show` terse output is NOT colon-escaped, so
/// we split on the first ':' only (the MAC and IPv6 value keep their colons).
/// Returns `None` if the device has no active NM connection (e.g. unmanaged).
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
        match k {
            "GENERAL.HWADDR" => d.mac = v.to_string(),
            "GENERAL.CONNECTION" => d.conn = v.to_string(),
            // "100 (connected)" → connected; "10 (unmanaged)"/"20"/"30" → not.
            "GENERAL.STATE" => d.connected = v.starts_with("100"),
            "GENERAL.NM-MANAGED" => d.managed = v == "yes",
            _ if k.starts_with("IP4.ADDRESS") && d.ipv4.is_empty() => d.ipv4 = v.to_string(),
            _ if k.starts_with("IP6.ADDRESS") && d.ipv6.is_empty() => d.ipv6 = v.to_string(),
            // Active gateway/DNS (sensible prefills for the static form).
            "IP4.GATEWAY" => d.gateway = v.to_string(),
            _ if k.starts_with("IP4.DNS") && d.dns.is_empty() => d.dns = v.to_string(),
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
            match k {
                "connection.autoconnect" => d.autoconnect = v == "yes",
                "ipv4.method" => d.dhcp = v == "auto",
                _ => {}
            }
        }
    }
    Some(d)
}

/// Bring the Ethernet link up or down. When managed, NM connect/disconnect is
/// the clean path (polkit `network-control`, allow_active=yes); for an unmanaged
/// device `connect` makes NM adopt + activate it. Blocking — off the render thread.
pub fn ethernet_set_link(on: bool) {
    if let Some(dev) = net_device("ethernet") {
        let verb = if on { "connect" } else { "disconnect" };
        let _ = capture("nmcli", &["device", verb, &dev]);
    }
}

/// Details of the active Wi-Fi connection. Blocking — **off the render thread**.
pub fn wifi_details() -> Option<WifiDetails> {
    details_for_device(&net_device("wifi")?)
}

/// Details of the active Ethernet connection. Blocking — **off the render thread**.
pub fn ethernet_details() -> Option<WifiDetails> {
    details_for_device(&net_device("ethernet")?)
}

/// Convenience: details for the wifi/ethernet device by kind.
pub fn net_details(eth: bool) -> Option<WifiDetails> {
    if eth {
        ethernet_details()
    } else {
        wifi_details()
    }
}

/// Toggle the connection's auto-reconnect. Fire-and-forget.
pub fn wifi_set_autoconnect(conn: &str, on: bool) {
    let v = if on { "yes" } else { "no" };
    let _ = capture("nmcli", &["connection", "modify", conn, "connection.autoconnect", v]);
}

/// Switch the connection to DHCP and reactivate it. Returns success.
pub fn wifi_set_dhcp(conn: &str) -> bool {
    let _ = capture(
        "nmcli",
        &[
            "connection", "modify", conn, "ipv4.method", "auto", "ipv4.addresses", "",
            "ipv4.gateway", "", "ipv4.dns", "",
        ],
    );
    wifi_reactivate(conn)
}

/// Switch the connection to a static IPv4 config and reactivate it.
pub fn wifi_set_static(conn: &str, ip_prefix: &str, gateway: &str, dns: &str) -> bool {
    let _ = capture(
        "nmcli",
        &[
            "connection", "modify", conn, "ipv4.method", "manual", "ipv4.addresses", ip_prefix,
            "ipv4.gateway", gateway, "ipv4.dns", dns,
        ],
    );
    wifi_reactivate(conn)
}

/// Re-apply a connection's (just-changed) config; blocks until it settles.
fn wifi_reactivate(conn: &str) -> bool {
    let mut child = match Command::new("nmcli")
        .args(["connection", "up", conn])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let mut out = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut out);
    }
    out.to_lowercase().contains("successfully activated")
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
