//! Device integration — Audio · Wi-Fi · Bluetooth.
//!
//! Three surfaces behind a single [`DeviceProvider`] trait. Each surface has
//! typed state, a snapshot accessor, and explicit mutating operations. The
//! Bacak shell binds 1:1 to these methods; on Linux, the production provider
//! will talk to PipeWire / NetworkManager / BlueZ via D-Bus.
//!
//! For development and testing, [`MockProvider`] holds the state in memory and
//! behaves deterministically.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, Error)]
pub enum DeviceError {
    #[error("audio sink not found: {0}")]
    SinkNotFound(String),
    #[error("audio source not found: {0}")]
    SourceNotFound(String),
    #[error("wi-fi network not found: {0}")]
    NetworkNotFound(String),
    #[error("bluetooth device not found: {0}")]
    BtDeviceNotFound(String),
    #[error("volume out of range: {0}")]
    VolumeOutOfRange(u8),
    #[error("operation requires the radio to be on")]
    RadioOff,
    #[error("password required for secured network: {0}")]
    PasswordRequired(String),
    #[error("backend error: {0}")]
    Backend(String),
}

pub type Result<T> = std::result::Result<T, DeviceError>;

// ============================================================================
// Audio
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SinkKind {
    Internal,
    Bluetooth,
    Usb,
    Hdmi,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioSink {
    pub id: String,
    pub name: String,
    pub kind: SinkKind,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioSource {
    pub id: String,
    pub name: String,
    pub kind: SinkKind,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioState {
    pub master_volume: u8,
    pub muted: bool,
    pub sinks: Vec<AudioSink>,
    pub sources: Vec<AudioSource>,
}

impl AudioState {
    pub fn active_sink(&self) -> Option<&AudioSink> {
        self.sinks.iter().find(|s| s.is_default)
    }
    pub fn active_source(&self) -> Option<&AudioSource> {
        self.sources.iter().find(|s| s.is_default)
    }
}

// ============================================================================
// Wi-Fi
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WifiNetwork {
    pub ssid: String,
    pub bssid: String,
    pub signal_dbm: i8,
    pub secured: bool,
    pub frequency_mhz: u16,
}

impl WifiNetwork {
    pub fn bars(&self) -> u8 {
        match self.signal_dbm {
            d if d >= -55 => 4,
            d if d >= -67 => 3,
            d if d >= -78 => 2,
            _ => 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WifiState {
    pub enabled: bool,
    pub connected_ssid: Option<String>,
    pub ip: Option<String>,
    pub networks: Vec<WifiNetwork>,
}

// ============================================================================
// Bluetooth
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BtKind {
    Headphones,
    Speaker,
    Mouse,
    Keyboard,
    Phone,
    Watch,
    Gamepad,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BtDevice {
    pub mac: String,
    pub name: String,
    pub kind: BtKind,
    pub paired: bool,
    pub connected: bool,
    pub battery: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BluetoothState {
    pub enabled: bool,
    pub discovering: bool,
    pub devices: Vec<BtDevice>,
    pub device_name: String,
}

// ============================================================================
// Provider trait
// ============================================================================

pub trait DeviceProvider: Send + Sync {
    fn audio_state(&self) -> Result<AudioState>;
    fn set_volume(&self, v: u8) -> Result<()>;
    fn set_muted(&self, b: bool) -> Result<()>;
    fn select_sink(&self, id: &str) -> Result<()>;
    fn select_source(&self, id: &str) -> Result<()>;

    fn wifi_state(&self) -> Result<WifiState>;
    fn set_wifi_enabled(&self, b: bool) -> Result<()>;
    fn wifi_connect(&self, ssid: &str, password: Option<&str>) -> Result<()>;
    fn wifi_disconnect(&self) -> Result<()>;

    fn bt_state(&self) -> Result<BluetoothState>;
    fn set_bt_enabled(&self, b: bool) -> Result<()>;
    fn bt_scan(&self, on: bool) -> Result<()>;
    fn bt_pair(&self, mac: &str) -> Result<()>;
    fn bt_connect(&self, mac: &str) -> Result<()>;
    fn bt_disconnect(&self, mac: &str) -> Result<()>;
}

// ============================================================================
// MockProvider
// ============================================================================

#[derive(Clone)]
pub struct MockProvider {
    inner: Arc<RwLock<MockState>>,
}

struct MockState {
    audio: AudioState,
    wifi:  WifiState,
    bt:    BluetoothState,
}

impl MockProvider {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(MockState {
                audio: default_audio_state(),
                wifi:  default_wifi_state(),
                bt:    default_bt_state(),
            })),
        }
    }
}

impl Default for MockProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceProvider for MockProvider {
    fn audio_state(&self) -> Result<AudioState> {
        Ok(self.inner.read().audio.clone())
    }

    fn set_volume(&self, v: u8) -> Result<()> {
        if v > 100 {
            return Err(DeviceError::VolumeOutOfRange(v));
        }
        let mut g = self.inner.write();
        g.audio.master_volume = v;
        if v > 0 && g.audio.muted {
            g.audio.muted = false;
        }
        Ok(())
    }

    fn set_muted(&self, b: bool) -> Result<()> {
        self.inner.write().audio.muted = b;
        Ok(())
    }

    fn select_sink(&self, id: &str) -> Result<()> {
        let mut g = self.inner.write();
        if !g.audio.sinks.iter().any(|s| s.id == id) {
            return Err(DeviceError::SinkNotFound(id.into()));
        }
        for s in g.audio.sinks.iter_mut() {
            s.is_default = s.id == id;
        }
        Ok(())
    }

    fn select_source(&self, id: &str) -> Result<()> {
        let mut g = self.inner.write();
        if !g.audio.sources.iter().any(|s| s.id == id) {
            return Err(DeviceError::SourceNotFound(id.into()));
        }
        for s in g.audio.sources.iter_mut() {
            s.is_default = s.id == id;
        }
        Ok(())
    }

    fn wifi_state(&self) -> Result<WifiState> {
        Ok(self.inner.read().wifi.clone())
    }

    fn set_wifi_enabled(&self, b: bool) -> Result<()> {
        let mut g = self.inner.write();
        g.wifi.enabled = b;
        if !b {
            g.wifi.connected_ssid = None;
            g.wifi.ip = None;
        }
        Ok(())
    }

    fn wifi_connect(&self, ssid: &str, password: Option<&str>) -> Result<()> {
        let mut g = self.inner.write();
        if !g.wifi.enabled {
            return Err(DeviceError::RadioOff);
        }
        let net = g
            .wifi
            .networks
            .iter()
            .find(|n| n.ssid == ssid)
            .ok_or_else(|| DeviceError::NetworkNotFound(ssid.into()))?
            .clone();
        if net.secured && password.is_none() {
            return Err(DeviceError::PasswordRequired(ssid.into()));
        }
        g.wifi.connected_ssid = Some(ssid.into());
        g.wifi.ip = Some(format!("192.168.1.{}", 40 + (ssid.len() as u8 % 50)));
        Ok(())
    }

    fn wifi_disconnect(&self) -> Result<()> {
        let mut g = self.inner.write();
        g.wifi.connected_ssid = None;
        g.wifi.ip = None;
        Ok(())
    }

    fn bt_state(&self) -> Result<BluetoothState> {
        Ok(self.inner.read().bt.clone())
    }

    fn set_bt_enabled(&self, b: bool) -> Result<()> {
        let mut g = self.inner.write();
        g.bt.enabled = b;
        if !b {
            g.bt.discovering = false;
            for d in g.bt.devices.iter_mut() {
                d.connected = false;
            }
        }
        Ok(())
    }

    fn bt_scan(&self, on: bool) -> Result<()> {
        let mut g = self.inner.write();
        if !g.bt.enabled {
            return Err(DeviceError::RadioOff);
        }
        g.bt.discovering = on;
        if on && !g.bt.devices.iter().any(|d| d.mac == "ee:ff:01:00:00:01") {
            g.bt.devices.push(BtDevice {
                mac: "ee:ff:01:00:00:01".into(),
                name: "Bacak Watch".into(),
                kind: BtKind::Watch,
                paired: false,
                connected: false,
                battery: None,
            });
        }
        Ok(())
    }

    fn bt_pair(&self, mac: &str) -> Result<()> {
        let mut g = self.inner.write();
        if !g.bt.enabled {
            return Err(DeviceError::RadioOff);
        }
        let d = g
            .bt
            .devices
            .iter_mut()
            .find(|d| d.mac == mac)
            .ok_or_else(|| DeviceError::BtDeviceNotFound(mac.into()))?;
        d.paired = true;
        Ok(())
    }

    fn bt_connect(&self, mac: &str) -> Result<()> {
        let mut g = self.inner.write();
        if !g.bt.enabled {
            return Err(DeviceError::RadioOff);
        }
        let d = g
            .bt
            .devices
            .iter_mut()
            .find(|d| d.mac == mac)
            .ok_or_else(|| DeviceError::BtDeviceNotFound(mac.into()))?;
        if !d.paired {
            d.paired = true;
        }
        d.connected = true;
        Ok(())
    }

    fn bt_disconnect(&self, mac: &str) -> Result<()> {
        let mut g = self.inner.write();
        let d = g
            .bt
            .devices
            .iter_mut()
            .find(|d| d.mac == mac)
            .ok_or_else(|| DeviceError::BtDeviceNotFound(mac.into()))?;
        d.connected = false;
        Ok(())
    }
}

// ============================================================================
// Defaults
// ============================================================================

fn default_audio_state() -> AudioState {
    AudioState {
        master_volume: 72,
        muted: false,
        sinks: vec![
            AudioSink { id: "speakers".into(), name: "Internal Speakers".into(), kind: SinkKind::Internal, is_default: true },
            AudioSink { id: "airpods".into(),  name: "AirPods Pro".into(),       kind: SinkKind::Bluetooth, is_default: false },
            AudioSink { id: "usb".into(),      name: "USB-C Headphones".into(),  kind: SinkKind::Usb,       is_default: false },
        ],
        sources: vec![
            AudioSource { id: "mic".into(),         name: "Internal Microphone".into(), kind: SinkKind::Internal,  is_default: true },
            AudioSource { id: "airpods-mic".into(), name: "AirPods Pro".into(),         kind: SinkKind::Bluetooth, is_default: false },
        ],
    }
}

fn default_wifi_state() -> WifiState {
    WifiState {
        enabled: true,
        connected_ssid: Some("Bacak-Office".into()),
        ip: Some("192.168.1.42".into()),
        networks: vec![
            WifiNetwork { ssid: "Bacak-Office".into(), bssid: "aa:00:00:00:00:01".into(), signal_dbm: -42, secured: true,  frequency_mhz: 5180 },
            WifiNetwork { ssid: "Aegean-Mesh".into(),  bssid: "aa:00:00:00:00:02".into(), signal_dbm: -64, secured: true,  frequency_mhz: 2412 },
            WifiNetwork { ssid: "Guest-WiFi".into(),   bssid: "aa:00:00:00:00:03".into(), signal_dbm: -73, secured: false, frequency_mhz: 2437 },
            WifiNetwork { ssid: "rust-lang".into(),    bssid: "aa:00:00:00:00:04".into(), signal_dbm: -83, secured: true,  frequency_mhz: 2462 },
        ],
    }
}

fn default_bt_state() -> BluetoothState {
    BluetoothState {
        enabled: true,
        discovering: false,
        device_name: "bacak-os".into(),
        devices: vec![
            BtDevice { mac: "aa:bb:00:00:00:01".into(), name: "AirPods Pro".into(),       kind: BtKind::Headphones, paired: true,  connected: true,  battery: Some(78) },
            BtDevice { mac: "aa:bb:00:00:00:02".into(), name: "Magic Mouse".into(),       kind: BtKind::Mouse,      paired: true,  connected: true,  battery: Some(44) },
            BtDevice { mac: "aa:bb:00:00:00:03".into(), name: "Magic Keyboard".into(),    kind: BtKind::Keyboard,   paired: true,  connected: false, battery: None },
            BtDevice { mac: "cc:dd:00:00:00:01".into(), name: "iPhone — Mehmet".into(),   kind: BtKind::Phone,      paired: false, connected: false, battery: None },
            BtDevice { mac: "cc:dd:00:00:00:02".into(), name: "JBL Flip 5".into(),        kind: BtKind::Speaker,    paired: false, connected: false, battery: None },
        ],
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> MockProvider { MockProvider::new() }

    #[test]
    fn audio_volume_clamps_and_unmutes() {
        let p = p();
        p.set_muted(true).unwrap();
        assert!(p.audio_state().unwrap().muted);

        p.set_volume(35).unwrap();
        let s = p.audio_state().unwrap();
        assert_eq!(s.master_volume, 35);
        assert!(!s.muted);

        assert!(matches!(p.set_volume(150), Err(DeviceError::VolumeOutOfRange(150))));
    }

    #[test]
    fn wifi_connect_requires_password_for_secured() {
        let p = p();
        assert!(matches!(
            p.wifi_connect("rust-lang", None),
            Err(DeviceError::PasswordRequired(_))
        ));
        p.wifi_connect("Guest-WiFi", None).unwrap();
        let s = p.wifi_state().unwrap();
        assert_eq!(s.connected_ssid.as_deref(), Some("Guest-WiFi"));
    }

    #[test]
    fn wifi_bars_mapping() {
        let n = WifiNetwork {
            ssid: "x".into(), bssid: "".into(), signal_dbm: -40, secured: false, frequency_mhz: 5180,
        };
        assert_eq!(n.bars(), 4);
        assert_eq!(WifiNetwork { signal_dbm: -70, ..n.clone() }.bars(), 2);
        assert_eq!(WifiNetwork { signal_dbm: -85, ..n }.bars(), 1);
    }

    #[test]
    fn bluetooth_lifecycle() {
        let p = p();
        p.set_bt_enabled(false).unwrap();
        let s = p.bt_state().unwrap();
        assert!(!s.enabled);
        assert!(s.devices.iter().all(|d| !d.connected));

        assert!(matches!(p.bt_scan(true), Err(DeviceError::RadioOff)));

        p.set_bt_enabled(true).unwrap();
        p.bt_scan(true).unwrap();
        assert!(p.bt_state().unwrap().devices.iter().any(|d| d.name == "Bacak Watch"));

        p.bt_pair("cc:dd:00:00:00:01").unwrap();
        p.bt_connect("cc:dd:00:00:00:01").unwrap();
        let d = p.bt_state().unwrap().devices.into_iter().find(|d| d.mac == "cc:dd:00:00:00:01").unwrap();
        assert!(d.paired && d.connected);
    }
}
