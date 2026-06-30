// Bluetooth yönetimi — BlueZ D-Bus (zbus 5)

pub mod agent;
pub mod obex;

use agent::{BtAgent, PairingRequest};
use obex::{IncomingRequest, ObexAgent, TransferUpdate};

use anyhow::Result;
use futures_util::StreamExt;
use std::collections::HashMap;
use tokio::sync::mpsc;
use zbus::{zvariant::OwnedObjectPath, Connection};

// ─── Paylaşılan cihaz bilgisi ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub path: OwnedObjectPath,  // BlueZ D-Bus yolu — hci0 sabit kodlamasını önler
    pub address: String,
    pub name: String,
    pub paired: bool,
    pub connected: bool,
    pub rssi: i16,
    pub icon: String,
}

// ─── Komutlar / Eventler ──────────────────────────────────────────────────

#[derive(Debug)]
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

#[derive(Debug, Clone)]
pub enum BtEvent {
    Powered(bool),
    Scanning(bool),
    // Tüm cihaz listesi — UI her seferinde bu listeyle modeli yeniden oluşturur
    AllDevices(Vec<DeviceInfo>),
    PairingRequest { device_name: String, passkey: String },
    Toast(String),
    TransferProgress { file: String, progress: f32, status: String },
    IncomingFile { device: String, file_name: String },
}

// ─── BlueZ yardımcıları ───────────────────────────────────────────────────

type ManagedObjects =
    HashMap<OwnedObjectPath, HashMap<String, HashMap<String, zbus::zvariant::OwnedValue>>>;

async fn get_managed_objects(conn: &Connection) -> Result<ManagedObjects> {
    let proxy =
        zbus::Proxy::new(conn, "org.bluez", "/", "org.freedesktop.DBus.ObjectManager").await?;
    Ok(proxy.call_method("GetManagedObjects", &()).await?.body().deserialize()?)
}

async fn adapter_path(conn: &Connection) -> Result<OwnedObjectPath> {
    let objects = get_managed_objects(conn).await?;
    for (path, ifaces) in &objects {
        if ifaces.contains_key("org.bluez.Adapter1") {
            return Ok(path.clone());
        }
    }
    anyhow::bail!("Bluetooth adaptör bulunamadı")
}

async fn adapter_proxy<'c>(conn: &'c Connection, path: &OwnedObjectPath) -> Result<zbus::Proxy<'c>> {
    Ok(zbus::Proxy::new(conn, "org.bluez", path.clone(), "org.bluez.Adapter1").await?)
}

async fn device_proxy<'c>(conn: &'c Connection, path: &OwnedObjectPath) -> Result<zbus::Proxy<'c>> {
    Ok(zbus::Proxy::new(conn, "org.bluez", path.clone(), "org.bluez.Device1").await?)
}

fn parse_device(
    path: &OwnedObjectPath,
    props: &HashMap<String, zbus::zvariant::OwnedValue>,
) -> Option<DeviceInfo> {
    let address = props.get("Address").and_then(|v| String::try_from(v.clone()).ok())?;
    let name = props
        .get("Alias")
        .and_then(|v| String::try_from(v.clone()).ok())
        .or_else(|| props.get("Name").and_then(|v| String::try_from(v.clone()).ok()))
        .unwrap_or_else(|| address.clone());
    let paired = props.get("Paired").and_then(|v| bool::try_from(v.clone()).ok()).unwrap_or(false);
    let connected = props.get("Connected").and_then(|v| bool::try_from(v.clone()).ok()).unwrap_or(false);
    let rssi = props.get("RSSI").and_then(|v| i16::try_from(v.clone()).ok()).unwrap_or(0);
    let icon_raw = props.get("Icon").and_then(|v| String::try_from(v.clone()).ok()).unwrap_or_default();
    let icon = if icon_raw.contains("phone") { "phone" }
               else if icon_raw.contains("headset") || icon_raw.contains("audio") { "headset" }
               else if icon_raw.contains("computer") { "computer" }
               else { "unknown" }.to_string();

    // Adaptör yolları dev_ içermez — sadece cihazları al
    if !path.as_str().contains("/dev_") {
        return None;
    }

    Some(DeviceInfo { path: path.clone(), address, name, paired, connected, rssi, icon })
}

fn devices_from_objects(objects: &ManagedObjects) -> HashMap<String, DeviceInfo> {
    let mut map = HashMap::new();
    for (path, ifaces) in objects {
        if let Some(props) = ifaces.get("org.bluez.Device1") {
            if let Some(dev) = parse_device(path, props) {
                map.insert(dev.address.clone(), dev);
            }
        }
    }
    map
}

fn send_all(tx: &mpsc::UnboundedSender<BtEvent>, map: &HashMap<String, DeviceInfo>) {
    let _ = tx.send(BtEvent::AllDevices(map.values().cloned().collect()));
}

// ─── Ana döngü ────────────────────────────────────────────────────────────

pub async fn run(
    mut cmd_rx: mpsc::UnboundedReceiver<BtCmd>,
    event_tx: mpsc::UnboundedSender<BtEvent>,
) {
    if let Err(e) = run_inner(&mut cmd_rx, &event_tx).await {
        let _ = event_tx.send(BtEvent::Toast(format!("Bluetooth hatası: {}", e)));
    }
}

async fn run_inner(
    cmd_rx: &mut mpsc::UnboundedReceiver<BtCmd>,
    event_tx: &mpsc::UnboundedSender<BtEvent>,
) -> Result<()> {
    let sys_conn = Connection::system().await?;
    let ses_conn = Connection::session().await?;

    let adp_path = adapter_path(&sys_conn).await?;

    // Adaptör powered durumu
    {
        let adp = adapter_proxy(&sys_conn, &adp_path).await?;
        let powered: bool = adp.get_property("Powered").await.unwrap_or(false);
        let _ = event_tx.send(BtEvent::Powered(powered));
    }

    // İlk cihaz listesi
    let mut device_map = {
        let objects = get_managed_objects(&sys_conn).await?;
        devices_from_objects(&objects)
    };
    send_all(event_tx, &device_map);

    // Eşleşme agent
    let (pair_req_tx, mut pair_req_rx) = mpsc::unbounded_channel::<PairingRequest>();
    let agent_path = OwnedObjectPath::try_from("/org/bacak/ayarlar/BtAgent").unwrap();
    sys_conn.object_server().at(agent_path.clone(), BtAgent { request_tx: pair_req_tx }).await?;
    {
        let mgr = zbus::Proxy::new(&sys_conn, "org.bluez", "/org/bluez", "org.bluez.AgentManager1").await?;
        mgr.call_method("RegisterAgent", &(&agent_path, "DisplayYesNo")).await?;
        mgr.call_method("RequestDefaultAgent", &agent_path).await?;
    }

    // OBEX receive agent
    let bt_dir = format!("{}/Bluetooth", std::env::var("HOME").unwrap_or_default());
    tokio::fs::create_dir_all(&bt_dir).await.ok();
    let (incoming_tx, mut incoming_rx) = mpsc::unbounded_channel::<IncomingRequest>();
    let obex_agent_path = OwnedObjectPath::try_from("/org/bacak/ayarlar/ObexAgent").unwrap();
    ses_conn.object_server().at(obex_agent_path.clone(), ObexAgent {
        incoming_tx,
        bluetooth_dir: bt_dir.clone(),
    }).await?;
    {
        if let Ok(mgr) = zbus::Proxy::new(&ses_conn, "org.bluez.obex", "/org/bluez/obex", "org.bluez.obex.AgentManager1").await {
            let _ = mgr.call_method("RegisterAgent", &obex_agent_path).await;
        }
    }

    // D-Bus sinyalleri
    let obj_mgr = zbus::Proxy::new(&sys_conn, "org.bluez", "/", "org.freedesktop.DBus.ObjectManager").await?;
    let mut ifaces_added   = obj_mgr.receive_signal("InterfacesAdded").await?;
    let mut ifaces_removed = obj_mgr.receive_signal("InterfacesRemoved").await?;

    let adp_proxy = adapter_proxy(&sys_conn, &adp_path).await?;
    let mut adp_props = adp_proxy
        .receive_signal_with_args("PropertiesChanged", &[(0u8, "org.bluez.Adapter1")])
        .await?;

    // Transfer progress
    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<TransferUpdate>();

    let mut pending_pairing:  Option<tokio::sync::oneshot::Sender<bool>> = None;
    let mut pending_incoming: Option<tokio::sync::oneshot::Sender<bool>> = None;

    let mut scanning = false;
    let mut refresh_tick = tokio::time::interval(tokio::time::Duration::from_secs(3));
    refresh_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            // ── Periyodik yenileme (tarama aktifken) ──────────────
            _ = refresh_tick.tick() => {
                if scanning {
                    if let Ok(objects) = get_managed_objects(&sys_conn).await {
                        device_map = devices_from_objects(&objects);
                        send_all(event_tx, &device_map);
                    }
                }
            }

            // ── Kullanıcı komutları ────────────────────────────────
            Some(cmd) = cmd_rx.recv() => match cmd {
                BtCmd::SetPowered(on) => {
                    let adp = adapter_proxy(&sys_conn, &adp_path).await?;
                    let _ = adp.set_property("Powered", on).await;
                    let _ = event_tx.send(BtEvent::Powered(on));
                }
                BtCmd::StartScan => {
                    let adp = adapter_proxy(&sys_conn, &adp_path).await?;
                    let _ = adp.set_property("Discoverable", true).await;
                    let _ = adp.call_method("StartDiscovery", &()).await;
                    scanning = true;
                    let _ = event_tx.send(BtEvent::Scanning(true));
                }
                BtCmd::StopScan => {
                    let adp = adapter_proxy(&sys_conn, &adp_path).await?;
                    let _ = adp.call_method("StopDiscovery", &()).await;
                    scanning = false;
                    let _ = event_tx.send(BtEvent::Scanning(false));
                }
                BtCmd::Pair(address) => {
                    if let Some(dev_info) = device_map.get(&address).cloned() {
                        let p     = dev_info.path.clone();
                        let conn2 = sys_conn.clone();
                        let etx   = event_tx.clone();
                        tokio::spawn(async move {
                            match device_proxy(&conn2, &p).await {
                                Err(e) => { let _ = etx.send(BtEvent::Toast(format!("Hata: {}", e))); }
                                Ok(dev) => match dev.call_method("Pair", &()).await {
                                    Err(e) => { let _ = etx.send(BtEvent::Toast(format!("Eşleşme hatası: {}", e))); }
                                    Ok(_) => {
                                        tokio::time::sleep(tokio::time::Duration::from_millis(700)).await;
                                        if let Ok(objects) = get_managed_objects(&conn2).await {
                                            let mut map = devices_from_objects(&objects);
                                            if let Some(d) = map.get_mut(&address) {
                                                d.paired = true;
                                            }
                                            let _ = etx.send(BtEvent::AllDevices(
                                                map.values().cloned().collect(),
                                            ));
                                        }
                                    }
                                }
                            }
                        });
                    }
                }
                BtCmd::Connect(address) => {
                    if let Some(dev_info) = device_map.get(&address).cloned() {
                        let p     = dev_info.path.clone();
                        let conn2 = sys_conn.clone();
                        let etx   = event_tx.clone();
                        tokio::spawn(async move {
                            if let Ok(dev) = device_proxy(&conn2, &p).await {
                                match dev.call_method("Connect", &()).await {
                                    Ok(_) => {
                                        tokio::time::sleep(tokio::time::Duration::from_millis(400)).await;
                                        if let Ok(objects) = get_managed_objects(&conn2).await {
                                            let _ = etx.send(BtEvent::AllDevices(
                                                devices_from_objects(&objects).values().cloned().collect(),
                                            ));
                                        }
                                    }
                                    Err(e) => { let _ = etx.send(BtEvent::Toast(format!("Bağlantı hatası: {}", e))); }
                                }
                            }
                        });
                    }
                }
                BtCmd::Disconnect(address) => {
                    if let Some(dev_info) = device_map.get(&address).cloned() {
                        let _ = device_proxy(&sys_conn, &dev_info.path).await?
                            .call_method("Disconnect", &()).await;
                        if let Ok(objects) = get_managed_objects(&sys_conn).await {
                            device_map = devices_from_objects(&objects);
                            send_all(event_tx, &device_map);
                        }
                    }
                }
                BtCmd::Forget(address) => {
                    if let Some(dev_info) = device_map.get(&address).cloned() {
                        let adp = adapter_proxy(&sys_conn, &adp_path).await?;
                        let _ = adp.call_method("RemoveDevice", &dev_info.path).await;
                        device_map.remove(&address);
                        send_all(event_tx, &device_map);
                    }
                }
                BtCmd::SendFile { address, path } => {
                    let conn2 = ses_conn.clone();
                    let tx    = progress_tx.clone();
                    let etx   = event_tx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = obex::send_file(&conn2, &address, &path, tx).await {
                            let _ = etx.send(BtEvent::Toast(format!("Gönderme hatası: {}", e)));
                        }
                    });
                }
                BtCmd::ConfirmPairing(accept) => {
                    if let Some(tx) = pending_pairing.take() { let _ = tx.send(accept); }
                }
                BtCmd::AcceptIncoming(accept) => {
                    if let Some(tx) = pending_incoming.take() { let _ = tx.send(accept); }
                }
            },

            // ── Eşleşme isteği ────────────────────────────────────
            Some(req) = pair_req_rx.recv() => {
                let device_name = device_map
                    .values()
                    .find(|d| req.device_path.as_str().contains(&d.address.replace(':', "_")))
                    .map(|d| d.name.clone())
                    .unwrap_or_else(|| req.device_path.to_string());
                pending_pairing = Some(req.response);
                let _ = event_tx.send(BtEvent::PairingRequest { device_name, passkey: req.passkey });
            }

            // ── Gelen dosya isteği ─────────────────────────────────
            Some(req) = incoming_rx.recv() => {
                pending_incoming = Some(req.response);
                let _ = event_tx.send(BtEvent::IncomingFile {
                    device: req.device_address,
                    file_name: req.file_name,
                });
            }

            // ── Transfer ilerleme ──────────────────────────────────
            Some(update) = progress_rx.recv() => {
                let progress = if update.total > 0 {
                    update.transferred as f32 / update.total as f32
                } else if update.status == "complete" { 1.0 } else { 0.0 };
                let status_tr = match update.status.as_str() {
                    "active"    => "Gönderiliyor",
                    "complete"  => "Tamamlandı",
                    "error"     => "Hata",
                    "suspended" => "Duraklatıldı",
                    other       => other,
                }.to_string();
                let _ = event_tx.send(BtEvent::TransferProgress {
                    file: update.file_name,
                    progress,
                    status: status_tr,
                });
            }

            // ── InterfacesAdded ────────────────────────────────────
            Some(msg) = ifaces_added.next() => {
                type IfaceMap = HashMap<String, HashMap<String, zbus::zvariant::OwnedValue>>;
                let parse: zbus::Result<(OwnedObjectPath, IfaceMap)> = msg.body().deserialize();
                if let Ok((dev_path, ifaces)) = parse {
                    if let Some(props) = ifaces.get("org.bluez.Device1") {
                        if let Some(dev) = parse_device(&dev_path, props) {
                            device_map.insert(dev.address.clone(), dev);
                            send_all(event_tx, &device_map);
                        }
                    }
                }
            }

            // ── InterfacesRemoved ──────────────────────────────────
            Some(msg) = ifaces_removed.next() => {
                let parse: zbus::Result<(OwnedObjectPath, Vec<String>)> = msg.body().deserialize();
                if let Ok((dev_path, _)) = parse {
                    let addr = dev_path.as_str().split('/').last()
                        .map(|s| s.trim_start_matches("dev_").replace('_', ":"))
                        .unwrap_or_default();
                    if !addr.is_empty() {
                        device_map.remove(&addr);
                        send_all(event_tx, &device_map);
                    }
                }
            }

            // ── Adaptör PropertiesChanged ──────────────────────────
            Some(msg) = adp_props.next() => {
                type PropMap = HashMap<String, zbus::zvariant::OwnedValue>;
                let parse: zbus::Result<(String, PropMap, Vec<String>)> = msg.body().deserialize();
                if let Ok((_iface, changed, _)) = parse {
                    if let Some(v) = changed.get("Powered") {
                        if let Ok(on) = bool::try_from(v.clone()) {
                            let _ = event_tx.send(BtEvent::Powered(on));
                        }
                    }
                    if let Some(v) = changed.get("Discovering") {
                        if let Ok(s) = bool::try_from(v.clone()) {
                            scanning = s;
                            let _ = event_tx.send(BtEvent::Scanning(s));
                        }
                    }
                }
            }
        }
    }
}

fn bluez_path(address: &str) -> Option<OwnedObjectPath> {
    OwnedObjectPath::try_from(format!(
        "/org/bluez/hci0/dev_{}",
        address.replace(':', "_")
    )).ok()
}

fn find_device_path(
    objects: &ManagedObjects,
    address: &str,
) -> Option<OwnedObjectPath> {
    for (path, ifaces) in objects {
        if let Some(props) = ifaces.get("org.bluez.Device1") {
            let addr = props.get("Address")
                .and_then(|v| String::try_from(v.clone()).ok())
                .unwrap_or_default();
            if addr.eq_ignore_ascii_case(address) {
                return Some(path.clone());
            }
        }
    }
    None
}
