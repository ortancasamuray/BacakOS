// SPDX-License-Identifier: GPL-3.0-or-later
//! `uzakyonetim-sunucu` — Uzak Yönetim central server.
//!
//! ```text
//! sudo uzakyonetim-sunucu kurulum --adres yonetim.okul.tr [--adres 203.0.113.7]
//! sudo uzakyonetim-sunucu yonetici-ekle <ad>
//! sudo systemctl enable --now uzakyonetim-sunucu      # runs `baslat`
//! ```
//!
//! Data lives in `/var/lib/uzakyonetim-sunucu` (0700): CA key, server
//! certificate, SQLite DB and screenshots. `UZY_VERI` overrides it.

mod admins;
mod api;
mod auth;
mod ca;
mod db;
mod files;
mod gateway;
mod web;

#[cfg(test)]
mod e2e_tests;

use std::io::{BufRead, IsTerminal, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bacakonay_core::{base32, otp, uri, OtpParams};
use serde::{Deserialize, Serialize};

/// `<veri>/ayar.json`
#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    /// Names/IPs agents use to reach this server (certificate SANs). The
    /// first one goes into join codes.
    pub adresler: Vec<String>,
    pub ajan_portu: u16,
    pub panel_adresi: String,
    /// Optional publicly trusted certificate for the panel (e.g. Let's
    /// Encrypt). Agents never use it — they pin the private CA.
    #[serde(default)]
    pub panel_sertifika: Option<PathBuf>,
    #[serde(default)]
    pub panel_anahtar: Option<PathBuf>,
}

pub struct App {
    pub db: db::Db,
    pub ca: Mutex<ca::Ca>,
    pub ca_pem: String,
    pub ca_fp: String,
    pub agents: gateway::Agents,
    pub enroll_limit: gateway::EnrollLimiter,
    pub sessions: auth::Sessions,
    pub login_limit: auth::LoginLimiter,
    pub pending_admins: admins::Pending,
    pub cfg: Config,
    pub data: PathBuf,
}

impl App {
    pub fn screens_dir(&self, id: &str) -> Option<PathBuf> {
        (id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit())).then(|| self.data.join("ekranlar").join(id))
    }

    pub fn join_address(&self) -> String {
        let host = self.cfg.adresler.first().cloned().unwrap_or_default();
        if host.contains(':') {
            format!("[{host}]:{}", self.cfg.ajan_portu)
        } else {
            format!("{host}:{}", self.cfg.ajan_portu)
        }
    }
}

const USAGE: &str = "\
Kullanım:
  uzakyonetim-sunucu kurulum --adres AD_YA_DA_IP [--adres …] [--panel 0.0.0.0:8443] [--ajan-portu 8444]
      CA'yı, sunucu sertifikasını ve ayarları oluşturur (bir kez).
  uzakyonetim-sunucu yonetici-ekle AD
      Panel yöneticisi ekler/günceller: parola + Bacak Onay QR'ı (panelden de eklenebilir).
  uzakyonetim-sunucu baslat
      Web panelini ve ajan kapısını çalıştırır (systemd servisi).";

fn data_dir() -> PathBuf {
    std::env::var_os("UZY_VERI").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/var/lib/uzakyonetim-sunucu"))
}

fn main() -> std::process::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let res = match args.first().map(String::as_str) {
        Some("kurulum") => setup(&args[1..]),
        Some("yonetici-ekle") => add_admin(args.get(1)),
        Some("baslat") => tokio::runtime::Runtime::new().expect("tokio").block_on(serve()),
        _ => {
            eprintln!("{USAGE}");
            return std::process::ExitCode::from(2);
        }
    };
    match res {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("uzakyonetim-sunucu: {e}");
            std::process::ExitCode::from(1)
        }
    }
}

fn flag_values(args: &[String], name: &str) -> Vec<String> {
    args.windows(2).filter(|w| w[0] == name).map(|w| w[1].clone()).collect()
}

fn setup(args: &[String]) -> Result<(), String> {
    let dir = data_dir();
    let adresler = flag_values(args, "--adres");
    if adresler.is_empty() {
        return Err("en az bir --adres gerekli (ajanların sunucuya ulaşacağı ad ya da IP)".into());
    }
    if dir.join("ca.key").exists() {
        return Err(format!("{} zaten kurulu", dir.display()));
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    let ca = ca::Ca::create(&dir)?;
    let (crt, key) = ca.issue_server(&adresler)?;
    files::write_public(&dir.join("sunucu.crt"), crt.as_bytes())?;
    files::write_private(&dir.join("sunucu.key"), key.as_bytes())?;
    let cfg = Config {
        adresler,
        ajan_portu: flag_values(args, "--ajan-portu").first().and_then(|p| p.parse().ok()).unwrap_or(uzy_proto::AGENT_PORT),
        panel_adresi: flag_values(args, "--panel").first().cloned().unwrap_or_else(|| "0.0.0.0:8443".into()),
        panel_sertifika: None,
        panel_anahtar: None,
    };
    files::write_private(&dir.join("ayar.json"), &serde_json::to_vec_pretty(&cfg).unwrap())?;
    db::Db::open(&dir.join("uzakyonetim.db"))?;
    let token = admins::write_setup_token(&dir)?;
    println!("✓ Kurulum tamam: {}", dir.display());
    println!("  CA parmak izi (SHA-256): {}", ca.fingerprint());
    println!("  İlk yöneticiyi tarayıcıdan oluşturun: {}", setup_link(&cfg, &token));
    Ok(())
}

/// Panel link that opens the first-admin screen. The token rides in the
/// fragment, so it never reaches server or proxy logs.
fn setup_link(cfg: &Config, token: &str) -> String {
    let host = cfg.adresler.first().map(String::as_str).unwrap_or("localhost");
    let host = if host.contains(':') { format!("[{host}]") } else { host.to_string() };
    let port = cfg.panel_adresi.rsplit(':').next().unwrap_or("8443");
    format!("https://{host}:{port}/#kurulum={token}")
}

fn read_line_hidden(prompt: &str) -> Result<zeroize::Zeroizing<String>, String> {
    print!("{prompt}");
    std::io::stdout().flush().ok();
    let fd = libc::STDIN_FILENO;
    let mut old: libc::termios = unsafe { std::mem::zeroed() };
    let tty = unsafe { libc::tcgetattr(fd, &mut old) } == 0;
    if tty {
        let mut new = old;
        new.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &new) };
    }
    let mut line = zeroize::Zeroizing::new(String::new());
    let r = std::io::stdin().lock().read_line(&mut line);
    if tty {
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &old) };
        println!();
    }
    r.map_err(|e| e.to_string())?;
    let trimmed = zeroize::Zeroizing::new(line.trim_end_matches(['\r', '\n']).to_string());
    Ok(trimmed)
}

fn add_admin(name: Option<&String>) -> Result<(), String> {
    let name = name.ok_or("yönetici adı gerekli")?;
    if !uzy_proto::valid_username(name) {
        return Err("yönetici adı küçük harf/rakam/-/_ olmalı".into());
    }
    if !std::io::stdin().is_terminal() {
        return Err("etkileşimli terminal gerekli".into());
    }
    let dir = data_dir();
    let db = db::Db::open(&dir.join("uzakyonetim.db"))?;
    let pw = read_line_hidden("Parola (en az 12 karakter): ")?;
    if pw.chars().count() < 12 {
        return Err("parola en az 12 karakter olmalı".into());
    }
    if *read_line_hidden("Parola (tekrar): ")? != *pw {
        return Err("parolalar eşleşmiyor".into());
    }
    let params = OtpParams::default();
    let secret = zeroize::Zeroizing::new(otp::generate_secret(20).map_err(|e| e.to_string())?);
    let link = uri::totp_uri(name, "uzakyonetim", &secret, &params);
    let qr = qrcode::QrCode::new(link.as_bytes()).map_err(|e| e.to_string())?;
    let text = qr
        .render::<qrcode::render::unicode::Dense1x2>()
        .dark_color(qrcode::render::unicode::Dense1x2::Light)
        .light_color(qrcode::render::unicode::Dense1x2::Dark)
        .quiet_zone(true)
        .build();
    println!("\nBacak Onay ile okutun (panel girişi için):\n{text}");
    println!("Elle giriş anahtarı: {}\n", base32::encode(&secret));
    for _ in 0..3 {
        let code = read_line_hidden("Uygulamadaki kod: ")?;
        if otp::verify_totp(&secret, &params, &code, db::now() as u64, 1, None).is_some() {
            db.add_admin(name, &auth::hash_password(&pw)?, &base32::encode(&secret))?;
            db.audit("yerel", "-", "yonetici_ekle", name, "tamam");
            admins::remove_setup_token(&dir);
            println!("✓ {name} panel yöneticisi olarak eklendi.");
            return Ok(());
        }
        println!("✗ Kod tutmadı, tekrar deneyin.");
    }
    Err("3 hatalı deneme; yönetici eklenmedi".into())
}

fn load_config(dir: &Path) -> Result<Config, String> {
    serde_json::from_slice(&std::fs::read(dir.join("ayar.json")).map_err(|e| format!("ayar.json: {e} — önce `kurulum`"))?)
        .map_err(|e| e.to_string())
}

async fn serve() -> Result<(), String> {
    let dir = data_dir();
    let cfg = load_config(&dir)?;
    let ca = ca::Ca::load(&dir)?;
    let db = db::Db::open(&dir.join("uzakyonetim.db"))?;
    if db.admin_count() == 0 {
        log::warn!("hiç yönetici yok — panelde ilk yöneticiyi oluşturun (bağlantı: kurulum-jetonu)");
    }
    let read = |n: &str| std::fs::read_to_string(dir.join(n)).map_err(|e| format!("{n}: {e}"));
    let (crt, key) = (read("sunucu.crt")?, read("sunucu.key")?);
    let gw_cfg = gateway::tls_config(&ca.cert_pem, &crt, &key)?;

    let app = Arc::new(App {
        db,
        ca_pem: ca.cert_pem.clone(),
        ca_fp: ca.fingerprint(),
        ca: Mutex::new(ca),
        agents: Default::default(),
        enroll_limit: Default::default(),
        sessions: Default::default(),
        login_limit: Default::default(),
        pending_admins: Default::default(),
        cfg: cfg.clone(),
        data: dir.clone(),
    });

    let agent_addr: SocketAddr = format!("0.0.0.0:{}", cfg.ajan_portu).parse().map_err(|e| format!("{e}"))?;
    let listener = tokio::net::TcpListener::bind(agent_addr).await.map_err(|e| format!("{agent_addr}: {e}"))?;
    let gw = tokio::spawn(gateway::serve(app.clone(), listener, gw_cfg));

    // Panel: an external (e.g. Let's Encrypt) cert if configured, else ours.
    let (pcrt, pkey) = match (&cfg.panel_sertifika, &cfg.panel_anahtar) {
        (Some(c), Some(k)) => (std::fs::read(c).map_err(|e| e.to_string())?, std::fs::read(k).map_err(|e| e.to_string())?),
        _ => (format!("{crt}{}", app.ca_pem).into_bytes(), key.into_bytes()),
    };
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem(pcrt, pkey).await.map_err(|e| e.to_string())?;
    let panel_addr: SocketAddr = cfg.panel_adresi.parse().map_err(|e| format!("panel_adresi: {e}"))?;
    log::info!("web paneli: https://{}", panel_addr);
    let router = web::router(app.clone());
    let panel = axum_server::bind_rustls(panel_addr, tls)
        .serve(router.into_make_service_with_connect_info::<SocketAddr>());
    tokio::select! {
        r = panel => r.map_err(|e| e.to_string()),
        r = gw => r.map_err(|e| e.to_string())?.map_err(|e| e.to_string()),
        _ = tokio::signal::ctrl_c() => Ok(()),
    }
}
