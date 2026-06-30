//! OBEX dosya gönderme — obexd D-Bus üzerinden (agent gerektirmez).
//! Alma tarafı compositor'un Python ajanı tarafından yönetilir (~/Downloads).

use anyhow::Result;

pub struct TransferUpdate {
    pub file_name: String,
    pub transferred: u64,
    pub total: u64,
    pub status: String,
}

/// obex-send CLI aracını kullanarak dosya gönder (blocking, spawn_blocking ile).
pub async fn send_file_cli(device_address: &str, file_path: &str) -> Result<()> {
    let abs = if file_path.starts_with('/') {
        file_path.to_string()
    } else {
        format!("{}/{}", std::env::var("HOME").unwrap_or_default(), file_path)
    };
    let addr = device_address.to_string();
    tokio::task::spawn_blocking(move || {
        let status = std::process::Command::new("bluetooth-sendto")
            .args(["--device", &addr, &abs])
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!("bluetooth-sendto başarısız: {:?}", status.code()))
        }
    })
    .await?
}
