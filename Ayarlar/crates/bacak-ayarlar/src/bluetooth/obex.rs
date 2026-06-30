// OBEX dosya transferi — gönderme ve alma

use anyhow::Result;
use tokio::sync::{mpsc, oneshot};
use zbus::{fdo, interface, zvariant::OwnedObjectPath, Connection};
use std::collections::HashMap;

// ─── OBEX Receive Agent ────────────────────────────────────────────────────

pub struct IncomingRequest {
    pub device_address: String,
    pub file_name: String,
    pub save_path: String,
    pub response: oneshot::Sender<bool>,
}

pub struct ObexAgent {
    pub incoming_tx: mpsc::UnboundedSender<IncomingRequest>,
    pub bluetooth_dir: String,
}

#[interface(name = "org.bluez.obex.Agent1")]
impl ObexAgent {
    async fn release(&self) {}

    async fn cancel(&self) {}

    async fn authorize_push(
        &self,
        transfer: OwnedObjectPath,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<String> {
        // Transfer proxy'den dosya adı ve cihaz adresini al
        let transfer_proxy = zbus::Proxy::new(
            conn,
            "org.bluez.obex",
            transfer.as_ref(),
            "org.bluez.obex.Transfer1",
        )
        .await
        .map_err(|e| fdo::Error::Failed(e.to_string()))?;

        let file_name: String = transfer_proxy
            .get_property("Name")
            .await
            .unwrap_or_else(|_| "bilinmeyen_dosya".to_string());

        let device_address: String = transfer_proxy
            .get_property::<String>("Session")
            .await
            .unwrap_or_default();

        let safe_name = sanitize_filename(&file_name);
        let save_path = format!("{}/{}", self.bluetooth_dir, safe_name);

        let (tx, rx) = oneshot::channel::<bool>();
        let req = IncomingRequest {
            device_address,
            file_name: file_name.clone(),
            save_path: save_path.clone(),
            response: tx,
        };

        self.incoming_tx
            .send(req)
            .map_err(|_| fdo::Error::Failed("ObexAgent kapalı".to_string()))?;

        match rx.await {
            Ok(true) => Ok(save_path),
            Ok(false) => Err(fdo::Error::Failed("Kullanıcı reddetti".to_string())),
            Err(_) => Err(fdo::Error::Failed("Yanıt alınamadı".to_string())),
        }
    }
}

fn sanitize_filename(name: &str) -> String {
    let base = std::path::Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("dosya");
    base.chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') { c } else { '_' })
        .collect()
}

// ─── OBEX Send ────────────────────────────────────────────────────────────

pub struct TransferUpdate {
    pub file_name: String,
    pub transferred: u64,
    pub total: u64,
    pub status: String, // "active" | "complete" | "error" | "suspended"
}

pub async fn send_file(
    conn: &Connection,
    device_address: &str,
    file_path: &str,
    progress_tx: mpsc::UnboundedSender<TransferUpdate>,
) -> Result<()> {
    // obexd session aç
    let client = zbus::Proxy::new(
        conn,
        "org.bluez.obex",
        "/org/bluez/obex",
        "org.bluez.obex.Client1",
    )
    .await?;

    let mut args = HashMap::<&str, zbus::zvariant::Value<'_>>::new();
    args.insert("Target", zbus::zvariant::Value::from("opp"));

    let session_path: OwnedObjectPath = client
        .call_method("CreateSession", &(device_address, args))
        .await?
        .body()
        .deserialize()?;

    // ObjectPush üzerinden dosya gönder
    let push = zbus::Proxy::new(
        conn,
        "org.bluez.obex",
        session_path.as_ref(),
        "org.bluez.obex.ObjectPush1",
    )
    .await?;

    // Mutlak yol gerekli
    let abs_path = if file_path.starts_with('/') {
        file_path.to_string()
    } else {
        format!(
            "{}/{}",
            std::env::var("HOME").unwrap_or_default(),
            file_path
        )
    };

    let (transfer_path, props): (OwnedObjectPath, HashMap<String, zbus::zvariant::OwnedValue>) =
        push.call_method("SendFile", &abs_path).await?.body().deserialize()?;

    let file_name = std::path::Path::new(&abs_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&abs_path)
        .to_string();

    let total: u64 = props
        .get("Size")
        .and_then(|v| u64::try_from(v.clone()).ok())
        .unwrap_or(0);

    // Transfer progress izle
    let transfer = zbus::Proxy::new(
        conn,
        "org.bluez.obex",
        transfer_path.as_ref(),
        "org.bluez.obex.Transfer1",
    )
    .await?;

    loop {
        let status: String = transfer
            .get_property("Status")
            .await
            .unwrap_or_else(|_| "error".to_string());

        let transferred: u64 = transfer
            .get_property("Transferred")
            .await
            .unwrap_or(0u64);

        let _ = progress_tx.send(TransferUpdate {
            file_name: file_name.clone(),
            transferred,
            total,
            status: status.clone(),
        });

        match status.as_str() {
            "complete" | "error" => break,
            _ => {}
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    // Session kapat
    let _ = client.call_method("RemoveSession", &session_path).await;
    Ok(())
}
