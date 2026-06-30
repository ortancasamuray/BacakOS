// BlueZ pairing agent — org.bluez.Agent1 D-Bus servisi

use tokio::sync::{mpsc, oneshot};
use zbus::{fdo, interface, zvariant::OwnedObjectPath};

pub struct PairingRequest {
    pub device_path: OwnedObjectPath,
    pub passkey: String,
    pub response: oneshot::Sender<bool>,
}

pub struct BtAgent {
    pub request_tx: mpsc::UnboundedSender<PairingRequest>,
}

#[interface(name = "org.bluez.Agent1")]
impl BtAgent {
    async fn release(&self) {}

    async fn request_pin_code(&self, _device: OwnedObjectPath) -> fdo::Result<String> {
        // Android SSP kullanır, PIN nadiren istenir
        Err(fdo::Error::NotSupported("SSP kullanın".to_string()))
    }

    async fn display_pin_code(&self, _device: OwnedObjectPath, _pincode: String) {}

    async fn request_passkey(&self, _device: OwnedObjectPath) -> fdo::Result<u32> {
        Err(fdo::Error::NotSupported("SSP kullanın".to_string()))
    }

    async fn display_passkey(
        &self,
        _device: OwnedObjectPath,
        _passkey: u32,
        _entered: u16,
    ) {
    }

    async fn request_confirmation(
        &self,
        device: OwnedObjectPath,
        passkey: u32,
    ) -> fdo::Result<()> {
        let (tx, rx) = oneshot::channel::<bool>();
        let req = PairingRequest {
            device_path: device,
            passkey: format!("{:06}", passkey),
            response: tx,
        };
        self.request_tx
            .send(req)
            .map_err(|_| fdo::Error::Failed("Agent kapalı".to_string()))?;

        match rx.await {
            Ok(true) => Ok(()),
            Ok(false) => Err(fdo::Error::Failed("Kullanıcı reddetti".to_string())),
            Err(_) => Err(fdo::Error::Failed("Yanıt alınamadı".to_string())),
        }
    }

    async fn authorize_service(
        &self,
        _device: OwnedObjectPath,
        _uuid: String,
    ) -> fdo::Result<()> {
        Ok(())
    }

    async fn cancel(&self) {}
}
