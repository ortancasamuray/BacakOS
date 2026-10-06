// SPDX-License-Identifier: GPL-3.0-or-later
//! The fixed set of local operations the server may request.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use bacakonay_core::store::{Enrollment, Store};
use bacakonay_core::{otp, uri, OtpParams};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use uzy_proto::{valid_username, Payload, UserInfo};
use zeroize::Zeroizing;

/// Human accounts live in this uid range (Debian `adduser` defaults).
const UID_MIN: u32 = 1000;
const UID_MAX: u32 = 59999;
/// Supplementary groups for a new desktop user. Never `sudo`.
const DESKTOP_GROUPS: &str = "audio,video,plugdev";
const ADMIN_GROUPS: &[&str] = &["sudo", "wheel", "admin", "root"];
/// A pending Bacak Onay secret expires if the phone never confirms it.
const PENDING_TTL_SECS: u64 = 600;

/// Secrets of started-but-unconfirmed enrollments, in memory only.
pub struct PendingTotp {
    map: Mutex<HashMap<String, (Zeroizing<Vec<u8>>, Instant)>>,
}

impl PendingTotp {
    pub fn new() -> Self {
        PendingTotp { map: Mutex::new(HashMap::new()) }
    }
}

struct Passwd {
    name: String,
    uid: u32,
    gecos: String,
}

fn read_passwd() -> Vec<Passwd> {
    std::fs::read_to_string("/etc/passwd")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() >= 7).then(|| Passwd {
                name: f[0].to_string(),
                uid: f[2].parse().unwrap_or(0),
                gecos: f[4].split(',').next().unwrap_or("").to_string(),
            })
        })
        .collect()
}

fn admin_members() -> Vec<String> {
    std::fs::read_to_string("/etc/group")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() >= 4 && ADMIN_GROUPS.contains(&f[0])).then(|| f[3].to_string())
        })
        .flat_map(|m| m.split(',').map(str::to_string).collect::<Vec<_>>())
        .filter(|m| !m.is_empty())
        .collect()
}

fn human_user(name: &str) -> Result<Passwd, String> {
    if !valid_username(name) {
        return Err(format!("geçersiz kullanıcı adı: {name}"));
    }
    read_passwd()
        .into_iter()
        .find(|p| p.name == name && (UID_MIN..=UID_MAX).contains(&p.uid))
        .ok_or_else(|| format!("{name} bu makinede normal bir kullanıcı değil"))
}

pub fn list_users() -> Result<Payload, String> {
    let store = Store::system();
    let admins = admin_members();
    let liste = read_passwd()
        .into_iter()
        .filter(|p| (UID_MIN..=UID_MAX).contains(&p.uid))
        .map(|p| UserInfo {
            bacakonay: store.is_enrolled(&p.name).unwrap_or(false),
            yonetici: admins.contains(&p.name),
            tam_ad: p.gecos,
            uid: p.uid,
            kullanici: p.name,
        })
        .collect();
    Ok(Payload::Kullanicilar { liste })
}

async fn run(cmd: &str, args: &[&str], stdin: Option<&str>) -> Result<(), String> {
    let mut c = Command::new(cmd);
    c.args(args).stdout(Stdio::null()).stderr(Stdio::piped());
    if stdin.is_some() {
        c.stdin(Stdio::piped());
    }
    let mut child = c.spawn().map_err(|e| format!("{cmd} çalıştırılamadı: {e}"))?;
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(input.as_bytes()).await.map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().await.map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{cmd}: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

pub async fn create_account(user: &str, full_name: &str, password: &str) -> Result<Payload, String> {
    if !valid_username(user) {
        return Err(format!("geçersiz kullanıcı adı: {user}"));
    }
    if read_passwd().iter().any(|p| p.name == user) {
        return Err(format!("{user} zaten var"));
    }
    if password.chars().count() < 8 {
        return Err("parola en az 8 karakter olmalı".into());
    }
    // GECOS: no ':' or ',' (field separators) and no control characters.
    let gecos: String = full_name.chars().filter(|c| !matches!(c, ':' | ',') && !c.is_control()).take(64).collect();
    run(
        "useradd",
        &["--create-home", "--shell", "/bin/bash", "--groups", DESKTOP_GROUPS, "--comment", &gecos, "--", user],
        None,
    )
    .await?;
    // Password via stdin, never argv (argv is world-readable in /proc).
    let line = Zeroizing::new(format!("{user}:{password}\n"));
    if let Err(e) = run("chpasswd", &[], Some(&line)).await {
        // Don't leave a passwordless account behind.
        let _ = run("userdel", &["--remove", "--", user], None).await;
        return Err(e);
    }
    log::info!("hesap açıldı: {user}");
    Ok(Payload::Tamam)
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()).unwrap_or_else(|_| "bacakos".into())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn totp_begin(pending: &PendingTotp, user: &str) -> Result<Payload, String> {
    human_user(user)?;
    let params = OtpParams::default();
    let secret = Zeroizing::new(otp::generate_secret(params.algorithm.recommended_secret_len()).map_err(|e| e.to_string())?);
    let link = uri::totp_uri(user, &hostname(), &secret, &params);
    let mut map = pending.map.lock().unwrap();
    map.retain(|_, (_, t)| t.elapsed().as_secs() < PENDING_TTL_SECS);
    map.insert(user.to_string(), (secret, Instant::now()));
    Ok(Payload::OtpUri { uri: link })
}

/// Only a code from the phone activates 2FA, so a bad scan can't lock the user out.
pub fn totp_confirm(pending: &PendingTotp, user: &str, code: &str) -> Result<Payload, String> {
    human_user(user)?;
    let mut map = pending.map.lock().unwrap();
    let Some((secret, started)) = map.get(user) else {
        return Err("bekleyen kayıt yok — QR'ı yeniden oluşturun".into());
    };
    if started.elapsed().as_secs() >= PENDING_TTL_SECS {
        map.remove(user);
        return Err("kayıt süresi doldu — QR'ı yeniden oluşturun".into());
    }
    let params = OtpParams::default();
    if otp::verify_totp(secret, &params, code, now(), 1, None).is_none() {
        return Err("kod tutmadı — telefonun saatini kontrol edip yeni kodu deneyin".into());
    }
    let e = Enrollment { secret: secret.clone(), params, last_step: None };
    Store::system().save(user, &e).map_err(|e| e.to_string())?;
    map.remove(user);
    log::info!("Bacak Onay etkinleştirildi: {user}");
    Ok(Payload::Tamam)
}

pub fn totp_remove(user: &str) -> Result<Payload, String> {
    human_user(user)?;
    Store::system().remove(user).map_err(|e| e.to_string())?;
    log::info!("Bacak Onay kaldırıldı: {user}");
    Ok(Payload::Tamam)
}
