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
    pub path: OwnedObjectPath,
    pub address: String,
    pub name: String,
    pub paired: bool,
    pub connected: bool,
    pub rssi: i16,
    pub icon: String,
}

// ─── UI ↔ BT komutları ────────────────────────────────────────────────────

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
    DeviceAdded(DeviceInfo),
    DeviceRemoved(String), // address
    DeviceChanged(DeviceInfo),
    PairingRequest { device_name: String, passkey: String },
    Toast(String),
    TransferProgress { file: String, progress: f32, status: String },
    IncomingFile { device: String, file_name: String },
}

// ─── BlueZ proxy'leri ────────────────────────────────────────────────────

async fn get_system_bus() -> Result<Connection> {
    Ok(Connection::system().await?)
}

async fn get_session_bus() -> Result<Connection> {
    Ok(Connection::session().await?)
}

// ObjectManager ile tüm nesneleri al
async fn get_managed_objects(
    conn: &Connection,
) -> Result<HashMap<OwnedObjectPath, HashMap<String, HashMap<String, zbus::zvariant::OwnedValue>>>> {
    let proxy = zbus::Proxy::new(conn, "org.bluez", "/", "org.freedesktop.DBus.ObjectManager")
        .await?;
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
    let address: String = props.get("Address").and_then(|v| String::try_from(v.clone()).ok())?;
    let name: String = props
        .get("Alias")
        .and_then(|v| String::try_from(v.clone()).ok())
        .or_else(|| props.get("Name").and_then(|v| String::try_from(v.clone()).ok()))
        .unwrap_or_else(|| address.clone());
    let paired = props
        .get("Paired")
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false);
    let connected = props
        .get("Connected")
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false);
    let rssi = props
        .get("RSSI")
        .and_then(|v| i16::try_from(v.clone()).ok())
        .unwrap_or(0);
    let icon = props
        .get("Icon")
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default();
    let icon_str = if icon.contains("phone") {
        "phone"
    } else if icon.contains("headset") || icon.contains("audio") {
        "headset"
    } else if icon.contains("computer") {
        "computer"
    } else {
        "unknown"
    }
    .to_string();

    Some(DeviceInfo {
        path: path.clone(),
        address,
        name,
        paired,
        connected,
        rssi,
        icon: icon_str,
    })
}

// ─── Ana yönetim döngüsü ──────────────────────────────────────────────────

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
    let sys_conn = get_system_bus().await?;
    let ses_conn = get_session_bus().await?;

    // Adaptör bul
    let adp_path = adapter_path(&sys_conn).await?;

    // Adaptör powered durumunu oku
    {
        let adp = adapter_proxy(&sys_conn, &adp_path).await?;
        let powered: bool = adp.get_property("Powered").await.unwrap_or(false);
        let _ = event_tx.send(BtEvent::Powered(powered));
    }

    // Mevcut cihazları listele
    {
        let objects = get_managed_objects(&sys_conn).await?;
        for (path, ifaces) in &objects {
            if let Some(props) = ifaces.get("org.bluez.Device1") {
                if let Some(dev) = parse_device(path, props) {
                    let _ = event_tx.send(BtEvent::DeviceAdded(dev));
                }
            }
        }
    }

    // Eşleşme agent kaydı
    let (pair_req_tx, mut pair_req_rx) = mpsc::unbounded_channel::<PairingRequest>();
    let agent = BtAgent { request_tx: pair_req_tx };
    let agent_path = OwnedObjectPath::try_from("/org/bacak/ayarlar/BtAgent").unwrap();
    sys_conn
        .object_server()
        .at(agent_path.clone(), agent)
        .await?;
    {
        let mgr = zbus::Proxy::new(
            &sys_conn,
            "org.bluez",
            "/org/bluez",
            "org.bluez.AgentManager1",
        )
        .await?;
        mgr.call_method("RegisterAgent", &(&agent_path, "DisplayYesNo"))
            .await?;
        mgr.call_method("RequestDefaultAgent", &agent_path).await?;
    }

    // OBEX receive agent
    let bt_dir = format!("{}/Bluetooth", std::env::var("HOME").unwrap_or_default());
    tokio::fs::create_dir_all(&bt_dir).await.ok();
    let (incoming_tx, mut incoming_rx) = mpsc::unbounded_channel::<IncomingRequest>();
    let obex_agent = ObexAgent {
        incoming_tx,
        bluetooth_dir: bt_dir.clone(),
    };
    let obex_agent_path = OwnedObjectPath::try_from("/org/bacak/ayarlar/ObexAgent").unwrap();
    ses_conn
        .object_server()
        .at(obex_agent_path.clone(), obex_agent)
        .await?;
    {
        let obex_mgr = zbus::Proxy::new(
            &ses_conn,
            "org.bluez.obex",
            "/org/bluez/obex",
            "org.bluez.obex.AgentManager1",
        )
        .await;
        if let Ok(mgr) = obex_mgr {
            let _ = mgr.call_method("RegisterAgent", &obex_agent_path).await;
        }
    }

    // InterfacesAdded / InterfacesRemoved sinyalleri
    let obj_mgr = zbus::Proxy::new(
        &sys_conn,
        "org.bluez",
        "/",
        "org.freedesktop.DBus.ObjectManager",
    )
    .await?;
    let mut ifaces_added = obj_mgr.receive_signal("InterfacesAdded").await?;
    let mut ifaces_removed = obj_mgr.receive_signal("InterfacesRemoved").await?;

    // PropertiesChanged
    let adp_prop = adapter_proxy(&sys_conn, &adp_path).await?;
    let mut adp_props_changed = adp_prop
        .receive_signal_with_args("PropertiesChanged", &[(0u8, "org.bluez.Adapter1")])
        .await?;

    // Transfer progress kanalı
    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<TransferUpdate>();

    // Pairing onay bekleyen oneshot
    let mut pending_pairing: Option<tokio::sync::oneshot::Sender<bool>> = None;
    // Incoming dosya onay bekleyen
    let mut pending_incoming: Option<tokio::sync::oneshot::Sender<bool>> = None;

    // Tarama sırasında cihaz adlarını güncellemek için 3sn interval
    let mut scanning = false;
    let mut refresh_tick = tokio::time::interval(tokio::time::Duration::from_secs(3));
    refresh_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            // ── Periyodik cihaz adı/durum yenileme (tarama aktifken) ─
            _ = refresh_tick.tick() => {
                if scanning {
                    let objects = get_managed_objects(&sys_conn).await?;
                    for (p, ifaces) in &objects {
                        if let Some(props) = ifaces.get("org.bluez.Device1") {
                            if let Some(dev) = parse_device(p, props) {
                                let _ = event_tx.send(BtEvent::DeviceChanged(dev));
                            }
                        }
                    }
                }
            }

            // ── Kullanıcı komutları ──────────────────────────────────
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
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
                        let objects = get_managed_objects(&sys_conn).await?;
                        let path = find_device_path(&objects, &address);
                        if let Some(p) = path {
                            let dev = device_proxy(&sys_conn, &p).await?;
                            match dev.call_method("Pair", &()).await {
                                Ok(_) => {
                                    // Eşleşme tamamlandı — cihaz bilgisini tazele
                                    let fresh = get_managed_objects(&sys_conn).await?;
                                    if let Some(ifaces) = fresh.get(&p) {
                                        if let Some(props) = ifaces.get("org.bluez.Device1") {
                                            if let Some(info) = parse_device(&p, props) {
                                                let _ = event_tx.send(BtEvent::DeviceChanged(info));
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    let _ = event_tx.send(BtEvent::Toast(
                                        format!("Eşleşme hatası: {}", e),
                                    ));
                                }
                            }
                        }
                    }
                    BtCmd::Connect(address) => {
                        let objects = get_managed_objects(&sys_conn).await?;
                        if let Some(p) = find_device_path(&objects, &address) {
                            let dev = device_proxy(&sys_conn, &p).await?;
                            match dev.call_method("Connect", &()).await {
                                Ok(_) => {
                                    let fresh = get_managed_objects(&sys_conn).await?;
                                    if let Some(ifaces) = fresh.get(&p) {
                                        if let Some(props) = ifaces.get("org.bluez.Device1") {
                                            if let Some(info) = parse_device(&p, props) {
                                                let _ = event_tx.send(BtEvent::DeviceChanged(info));
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    let _ = event_tx.send(BtEvent::Toast(format!("Bağlantı hatası: {}", e)));
                                }
                            }
                        }
                    }
                    BtCmd::Disconnect(address) => {
                        let objects = get_managed_objects(&sys_conn).await?;
                        if let Some(p) = find_device_path(&objects, &address) {
                            let dev = device_proxy(&sys_conn, &p).await?;
                            let _ = dev.call_method("Disconnect", &()).await;
                            // Bağlantı durumunu güncelle
                            let fresh = get_managed_objects(&sys_conn).await?;
                            if let Some(ifaces) = fresh.get(&p) {
                                if let Some(props) = ifaces.get("org.bluez.Device1") {
                                    if let Some(info) = parse_device(&p, props) {
                                        let _ = event_tx.send(BtEvent::DeviceChanged(info));
                                    }
                                }
                            }
                        }
                    }
                    BtCmd::Forget(address) => {
                        let objects = get_managed_objects(&sys_conn).await?;
                        if let Some(p) = find_device_path(&objects, &address) {
                            let adp = adapter_proxy(&sys_conn, &adp_path).await?;
                            let _ = adp.call_method("RemoveDevice", &p).await;
                            let _ = event_tx.send(BtEvent::DeviceRemoved(address));
                        }
                    }
                    BtCmd::SendFile { address, path } => {
                        let conn = ses_conn.clone();
                        let tx = progress_tx.clone();
                        let ev_tx = event_tx.clone();
                        tokio::spawn(async move {
                            if let Err(e) = obex::send_file(&conn, &address, &path, tx).await {
                                let _ = ev_tx.send(BtEvent::Toast(format!("Gönderme hatası: {}", e)));
                            }
                        });
                    }
                    BtCmd::ConfirmPairing(accept) => {
                        if let Some(tx) = pending_pairing.take() {
                            let _ = tx.send(accept);
                        }
                    }
                    BtCmd::AcceptIncoming(accept) => {
                        if let Some(tx) = pending_incoming.take() {
                            let _ = tx.send(accept);
                        }
                    }
                }
            }

            // ── Eşleşme isteği (agent'tan) ────────────────────────
            Some(req) = pair_req_rx.recv() => {
                let device_name = {
                    let objects = get_managed_objects(&sys_conn).await.unwrap_or_default();
                    objects.get(&req.device_path)
                        .and_then(|i| i.get("org.bluez.Device1"))
                        .and_then(|p| p.get("Alias").or_else(|| p.get("Name")))
                        .and_then(|v| String::try_from(v.clone()).ok())
                        .unwrap_or_else(|| req.device_path.to_string())
                };
                pending_pairing = Some(req.response);
                let _ = event_tx.send(BtEvent::PairingRequest {
                    device_name,
                    passkey: req.passkey,
                });
            }

            // ── Gelen dosya isteği (obex agent'tan) ───────────────
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
                } else {
                    if update.status == "complete" { 1.0 } else { 0.0 }
                };
                let status_tr = match update.status.as_str() {
                    "active"    => "Gönderiliyor".to_string(),
                    "complete"  => "Tamamlandı".to_string(),
                    "error"     => "Hata".to_string(),
                    "suspended" => "Duraklatıldı".to_string(),
                    other       => other.to_string(),
                };
                let _ = event_tx.send(BtEvent::TransferProgress {
                    file: update.file_name,
                    progress,
                    status: status_tr,
                });
            }

            // ── InterfacesAdded sinyali ────────────────────────────
            Some(msg) = ifaces_added.next() => {
                type IfaceMap = HashMap<String, HashMap<String, zbus::zvariant::OwnedValue>>;
                let parse: zbus::Result<(OwnedObjectPath, IfaceMap)> = msg.body().deserialize();
                if let Ok((dev_path, ifaces)) = parse {
                    if let Some(props) = ifaces.get("org.bluez.Device1") {
                        if let Some(dev) = parse_device(&dev_path, props) {
                            let _ = event_tx.send(BtEvent::DeviceAdded(dev));
                        }
                    }
                }
            }

            // ── InterfacesRemoved sinyali ──────────────────────────
            Some(msg) = ifaces_removed.next() => {
                let parse: zbus::Result<(OwnedObjectPath, Vec<String>)> = msg.body().deserialize();
                if let Ok((dev_path, _ifaces)) = parse {
                    let addr = dev_path.as_str().split('/').last()
                        .map(|s| s.trim_start_matches("dev_").replace('_', ":"))
                        .unwrap_or_default();
                    if !addr.is_empty() {
                        let _ = event_tx.send(BtEvent::DeviceRemoved(addr));
                    }
                }
            }

            // ── Adaptör PropertiesChanged ──────────────────────────
            Some(msg) = adp_props_changed.next() => {
                type PropMap = HashMap<String, zbus::zvariant::OwnedValue>;
                let parse: zbus::Result<(String, PropMap, Vec<String>)> = msg.body().deserialize();
                if let Ok((_iface, changed, _inv)) = parse {
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

fn find_device_path(
    objects: &HashMap<OwnedObjectPath, HashMap<String, HashMap<String, zbus::zvariant::OwnedValue>>>,
    address: &str,
) -> Option<OwnedObjectPath> {
    for (path, ifaces) in objects {
        if let Some(props) = ifaces.get("org.bluez.Device1") {
            let addr = props
                .get("Address")
                .and_then(|v| String::try_from(v.clone()).ok())
                .unwrap_or_default();
            if addr.eq_ignore_ascii_case(address) {
                return Some(path.clone());
            }
        }
    }
    None
}
