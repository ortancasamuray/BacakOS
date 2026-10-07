// SPDX-License-Identifier: GPL-3.0-or-later
//! Uzak Yönetim wire protocol between the agent (`uzy-ajan`, one per BacakOS
//! machine) and the server (`uzy-sunucu`).
//!
//! Transport: TLS 1.2/1.3 on the agent port (default 8444). The **agent
//! dials out** to the server, so machines behind NAT / on other networks need
//! no open ports. Frames are `[u32 big-endian length][JSON]`, capped at
//! [`MAX_FRAME`].
//!
//! The command set is deliberately closed: there is no "run this shell
//! command". A compromised server can only ask for what [`Command`] lists, and
//! each machine's local policy file can switch any of those off.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Default TCP port of the agent gateway (mTLS).
pub const AGENT_PORT: u16 = 8444;
/// Largest frame accepted (a scaled JPEG screenshot fits comfortably).
pub const MAX_FRAME: usize = 8 * 1024 * 1024;
pub const PROTOCOL_VERSION: u32 = 1;

/// Agent → server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "tur", rename_all = "snake_case")]
pub enum AgentMsg {
    /// Only valid on a connection **without** a client certificate: trade a
    /// one-time join token for a signed client certificate.
    Kayit { jeton: String, csr_pem: String, makine_adi: String },
    /// First frame of every authenticated (client-cert) connection.
    Merhaba(Hello),
    Yanit { id: u64, sonuc: Result<Payload, String> },
    Ekran(Screenshot),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protokol: u32,
    pub makine_adi: String,
    pub isletim_sistemi: String,
    pub ajan_surumu: String,
    pub politika: Policy,
}

/// What this machine's local admin allows (`/etc/uzakyonetim/politika.toml`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Policy {
    pub hesap_acma: bool,
    /// Missing in messages from agents older than account deletion.
    #[serde(default = "enabled")]
    pub hesap_silme: bool,
    pub bacakonay: bool,
    pub ekran_izleme: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Policy { hesap_acma: true, hesap_silme: true, bacakonay: true, ekran_izleme: true }
    }
}

fn enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Screenshot {
    /// Unix seconds.
    pub zaman: u64,
    /// Whose session was on screen (`None` = login screen).
    pub oturum: Option<String>,
    pub jpeg_b64: String,
}

/// Server → agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "tur", rename_all = "snake_case")]
pub enum ServerMsg {
    /// Answer to [`AgentMsg::Kayit`].
    KayitTamam { sertifika_pem: String, ca_pem: String, makine_id: String },
    KayitRed { neden: String },
    /// Answer to [`AgentMsg::Merhaba`] and on every settings change.
    Ayarlar { ekran_araligi_sn: u32 },
    Komut { id: u64, komut: Command },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "islem", rename_all = "snake_case")]
pub enum Command {
    KullanicilariListele,
    /// Create a normal (non-admin) local account.
    HesapAc { kullanici: String, tam_ad: String, parola: String },
    /// Delete a normal (non-admin) local account with its home directory.
    HesapSil { kullanici: String },
    /// Start a Bacak Onay enrollment: a fresh secret is held **in the agent's
    /// memory** and its `otpauth://` URI returned for the QR. Nothing is
    /// written until [`Command::BacakonayOnayla`] proves the phone has it.
    BacakonayBaslat { kullanici: String },
    BacakonayOnayla { kullanici: String, kod: String },
    BacakonayKaldir { kullanici: String },
    EkranAl,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "tur", rename_all = "snake_case")]
pub enum Payload {
    Tamam,
    Kullanicilar { liste: Vec<UserInfo> },
    OtpUri { uri: String },
    Ekran(Screenshot),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserInfo {
    pub kullanici: String,
    pub tam_ad: String,
    pub uid: u32,
    pub bacakonay: bool,
    pub yonetici: bool,
}

/// Account names the agent will create/touch: lowercase POSIX-ish, no path
/// or option tricks. Mirrors Debian's default `NAME_REGEX`.
pub fn valid_username(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && (b[0].is_ascii_lowercase() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'))
}

pub async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(w: &mut W, msg: &T) -> std::io::Result<()> {
    let body = serde_json::to_vec(msg).map_err(std::io::Error::other)?;
    if body.len() > MAX_FRAME {
        return Err(std::io::Error::other("çerçeve çok büyük"));
    }
    w.write_all(&(body.len() as u32).to_be_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await
}

/// `Ok(None)` on clean EOF.
pub async fn read_frame<R: AsyncRead + Unpin, T: for<'de> Deserialize<'de>>(r: &mut R) -> std::io::Result<Option<T>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(std::io::Error::other("çerçeve sınırı aşıldı"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    serde_json::from_slice(&body).map(Some).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames() {
        for ok in ["ayse", "ali_veli", "ogr-12", "_svc"] {
            assert!(valid_username(ok), "{ok}");
        }
        for bad in ["", "Ayse", "1ali", "a b", "../x", "-x", "ş", "averyveryveryveryveryverylongname1"] {
            assert!(!valid_username(bad), "{bad}");
        }
    }

    #[test]
    fn policy_from_older_agent() {
        // Agents before account deletion don't send `hesap_silme`.
        let p: Policy = serde_json::from_str(r#"{"hesap_acma":false,"bacakonay":true,"ekran_izleme":true}"#).unwrap();
        assert!(!p.hesap_acma && p.hesap_silme);
    }

    #[tokio::test]
    async fn frame_roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let msg = ServerMsg::Komut { id: 7, komut: Command::BacakonayBaslat { kullanici: "ayse".into() } };
        write_frame(&mut a, &msg).await.unwrap();
        let got: ServerMsg = read_frame(&mut b).await.unwrap().unwrap();
        assert!(matches!(got, ServerMsg::Komut { id: 7, komut: Command::BacakonayBaslat { .. } }));
        drop(a);
        assert!(read_frame::<_, ServerMsg>(&mut b).await.unwrap().is_none());
    }
}
