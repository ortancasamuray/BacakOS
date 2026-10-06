// SPDX-License-Identifier: GPL-3.0-or-later
//! `uzakyonetim-ajan` — connects this BacakOS machine to an Uzak Yönetim server.
//!
//! ```text
//! sudo uzakyonetim-ajan kaydol <katılım-kodu> [--ad MAKINE_ADI] [--zorla]
//! sudo systemctl enable --now uzakyonetim-ajan     # runs `calistir`
//! sudo uzakyonetim-ajan durum
//! ```

mod config;
mod ops;
mod screen;
mod tls;

use std::os::unix::fs::OpenOptionsExt;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use uzy_proto::{read_frame, write_frame, AgentMsg, Command, Hello, Payload, ServerMsg, PROTOCOL_VERSION};

use config::{JoinCode, Saved};

const USAGE: &str = "\
Kullanım:
  uzakyonetim-ajan kaydol <katılım-kodu> [--ad MAKINE_ADI] [--zorla]
      Bu makineyi yönetim sunucusuna kaydeder (panel → Makine ekle).
  uzakyonetim-ajan calistir     Sunucuya bağlanır (systemd servisi bunu çalıştırır).
  uzakyonetim-ajan durum        Kayıt ve yerel politika durumunu gösterir.";

fn main() -> std::process::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("uzakyonetim-ajan: root yetkisi gerekli (sudo).");
        return std::process::ExitCode::from(1);
    }
    let rt = tokio::runtime::Runtime::new().expect("tokio");
    let res = match args.first().map(String::as_str) {
        Some("kaydol") => rt.block_on(enroll(&args[1..])),
        Some("calistir") => rt.block_on(run_forever()),
        Some("durum") => status(),
        _ => {
            eprintln!("{USAGE}");
            return std::process::ExitCode::from(2);
        }
    };
    match res {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("uzakyonetim-ajan: {e}");
            std::process::ExitCode::from(1)
        }
    }
}

fn write_private(name: &str, data: &[u8], mode: u32) -> Result<(), String> {
    let p = config::path(name);
    let tmp = p.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(data).and_then(|_| f.sync_all()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
}

async fn enroll(args: &[String]) -> Result<(), String> {
    let code = args.iter().find(|a| !a.starts_with("--")).ok_or("katılım kodu gerekli")?;
    let join = JoinCode::parse(code)?;
    if config::path("ajan.crt").exists() && !args.iter().any(|a| a == "--zorla") {
        return Err("bu makine zaten kayıtlı; yeniden kaydetmek için --zorla ekleyin".into());
    }
    let name = args
        .iter()
        .position(|a| a == "--ad")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| std::fs::read_to_string("/etc/hostname").unwrap_or_default().trim().to_string());

    // Fresh key pair generated here; only the CSR (public key) leaves the machine.
    let key = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
    params.distinguished_name.push(rcgen::DnType::CommonName, name.clone());
    let csr = params.serialize_request(&key).map_err(|e| e.to_string())?.pem().map_err(|e| e.to_string())?;

    let pinned = Arc::new(tls::PinnedCa::new(&join.f));
    let mut conn = tls::connect_enroll(&join.s, pinned.clone()).await?;
    write_frame(&mut conn, &AgentMsg::Kayit { jeton: join.j.clone(), csr_pem: csr, makine_adi: name.clone() })
        .await
        .map_err(|e| e.to_string())?;
    let reply: ServerMsg = read_frame(&mut conn).await.map_err(|e| e.to_string())?.ok_or("sunucu yanıt vermedi")?;
    let (cert, ca, id) = match reply {
        ServerMsg::KayitTamam { sertifika_pem, ca_pem, makine_id } => (sertifika_pem, ca_pem, makine_id),
        ServerMsg::KayitRed { neden } => return Err(format!("sunucu kaydı reddetti: {neden}")),
        _ => return Err("beklenmeyen yanıt".into()),
    };
    // The CA we're told to trust must be the one we pinned.
    let ca_der = rustls_pemfile::certs(&mut ca.as_bytes()).next().ok_or("CA yok")?.map_err(|e| e.to_string())?;
    if tls::fingerprint(&ca_der) != join.f.to_ascii_lowercase() {
        return Err("sunucunun gönderdiği CA sabitlenen parmak iziyle eşleşmiyor".into());
    }

    std::fs::create_dir_all(config::DIR).map_err(|e| e.to_string())?;
    write_private("ajan.key", key.serialize_pem().as_bytes(), 0o600)?;
    write_private("ajan.crt", cert.as_bytes(), 0o644)?;
    write_private("ca.crt", ca.as_bytes(), 0o644)?;
    let saved = Saved { sunucu: join.s.clone(), makine_id: id.clone() };
    write_private("sunucu.json", &serde_json::to_vec_pretty(&saved).unwrap(), 0o644)?;
    println!("✓ '{name}' sunucuya kaydedildi (makine kimliği {id}).");
    println!("  Başlatmak için: sudo systemctl enable --now uzakyonetim-ajan");
    Ok(())
}

fn status() -> Result<(), String> {
    match std::fs::read(config::path("sunucu.json")) {
        Ok(b) => {
            let s: Saved = serde_json::from_slice(&b).map_err(|e| e.to_string())?;
            println!("Kayıtlı sunucu : {}\nMakine kimliği : {}", s.sunucu, s.makine_id);
        }
        Err(_) => println!("Bu makine bir yönetim sunucusuna kayıtlı değil."),
    }
    let p = config::policy();
    let yn = |b: bool| if b { "izinli" } else { "KAPALI" };
    println!(
        "Yerel politika : hesap açma {}, Bacak Onay {}, ekran izleme {}",
        yn(p.hesap_acma),
        yn(p.bacakonay),
        yn(p.ekran_izleme)
    );
    Ok(())
}

async fn run_forever() -> Result<(), String> {
    let saved: Saved = serde_json::from_slice(
        &std::fs::read(config::path("sunucu.json")).map_err(|_| "kayıtlı değil — önce `kaydol`")?,
    )
    .map_err(|e| e.to_string())?;
    let pending = Arc::new(ops::PendingTotp::new());
    let mut backoff = 5u64;
    let shutdown = async {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        tokio::select! { _ = term.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
    };
    tokio::pin!(shutdown);
    loop {
        let session = session(&saved.sunucu, pending.clone());
        tokio::select! {
            r = session => {
                screen::set_indicator(false);
                match r {
                    Ok(()) => { log::info!("sunucu bağlantıyı kapattı"); backoff = 5; }
                    Err(e) => log::warn!("bağlantı: {e} — {backoff} sn sonra yeniden denenecek"),
                }
            }
            _ = &mut shutdown => {
                screen::set_indicator(false);
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(60);
    }
}

async fn session(addr: &str, pending: Arc<ops::PendingTotp>) -> Result<(), String> {
    let read = |n| std::fs::read(config::path(n)).map_err(|e| format!("{n}: {e}"));
    let conn = tls::connect_mtls(addr, &read("ca.crt")?, &read("ajan.crt")?, &read("ajan.key")?).await?;
    let (mut rd, mut wr) = tokio::io::split(conn);
    let policy = config::policy();

    // All outgoing frames go through one writer task.
    let (tx, mut rx) = mpsc::channel::<AgentMsg>(32);
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if write_frame(&mut wr, &m).await.is_err() {
                break;
            }
        }
    });
    let hello = Hello {
        protokol: PROTOCOL_VERSION,
        makine_adi: std::fs::read_to_string("/etc/hostname").unwrap_or_default().trim().to_string(),
        isletim_sistemi: os_release(),
        ajan_surumu: env!("CARGO_PKG_VERSION").into(),
        politika: policy,
    };
    tx.send(AgentMsg::Merhaba(hello)).await.map_err(|e| e.to_string())?;
    log::info!("{addr} sunucusuna bağlanıldı");

    // Periodic screenshots, re-armed whenever the server changes the interval.
    let (interval_tx, mut interval_rx) = watch::channel(0u32);
    let shots_tx = tx.clone();
    let shooter = tokio::spawn(async move {
        loop {
            let secs = *interval_rx.borrow_and_update();
            let on = secs > 0 && policy.ekran_izleme;
            screen::set_indicator(on);
            if !on {
                if interval_rx.changed().await.is_err() {
                    break;
                }
                continue;
            }
            match screen::capture().await {
                Ok(s) => {
                    if shots_tx.send(AgentMsg::Ekran(s)).await.is_err() {
                        break;
                    }
                }
                Err(e) => log::debug!("ekran alınamadı: {e}"),
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(secs as u64)) => {}
                r = interval_rx.changed() => if r.is_err() { break },
            }
        }
    });

    let result = loop {
        let msg: Option<ServerMsg> = match read_frame(&mut rd).await {
            Ok(m) => m,
            Err(e) => break Err(e.to_string()),
        };
        let Some(msg) = msg else { break Ok(()) };
        match msg {
            ServerMsg::Ayarlar { ekran_araligi_sn } => {
                // The server re-sends settings as a keepalive; only a real
                // change may re-arm the screenshot timer.
                let new = if ekran_araligi_sn == 0 { 0 } else { ekran_araligi_sn.clamp(10, 3600) };
                interval_tx.send_if_modified(|v| std::mem::replace(v, new) != new);
            }
            ServerMsg::Komut { id, komut } => {
                let pending = pending.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let sonuc = execute(komut, policy, &pending).await;
                    let _ = tx.send(AgentMsg::Yanit { id, sonuc }).await;
                });
            }
            _ => {}
        }
    };
    shooter.abort();
    writer.abort();
    result
}

async fn execute(cmd: Command, policy: uzy_proto::Policy, pending: &ops::PendingTotp) -> Result<Payload, String> {
    let denied = |what: &str| Err(format!("bu makinenin yerel politikası {what} işlemine izin vermiyor"));
    match cmd {
        Command::KullanicilariListele => ops::list_users(),
        Command::HesapAc { kullanici, tam_ad, parola } => {
            let parola = zeroize::Zeroizing::new(parola);
            if !policy.hesap_acma {
                return denied("hesap açma");
            }
            ops::create_account(&kullanici, &tam_ad, &parola).await
        }
        Command::BacakonayBaslat { kullanici } if policy.bacakonay => ops::totp_begin(pending, &kullanici),
        Command::BacakonayOnayla { kullanici, kod } if policy.bacakonay => ops::totp_confirm(pending, &kullanici, &kod),
        Command::BacakonayKaldir { kullanici } if policy.bacakonay => ops::totp_remove(&kullanici),
        Command::BacakonayBaslat { .. } | Command::BacakonayOnayla { .. } | Command::BacakonayKaldir { .. } => {
            denied("Bacak Onay")
        }
        Command::EkranAl if policy.ekran_izleme => {
            let shot = screen::capture().await?;
            Ok(Payload::Ekran(shot))
        }
        Command::EkranAl => denied("ekran izleme"),
    }
}

fn os_release() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string())))
        .unwrap_or_else(|| "BacakOS".into())
}
