// SPDX-License-Identifier: GPL-3.0-or-later
//! Panel admins: first-run setup in the browser, then adding and removing
//! admins from the panel. A new admin only exists once their phone has
//! proved it holds the Bacak Onay secret.
//!
//! First-run setup is open only while there is no admin at all, and only
//! with the one-time token `kurulum` writes to `<veri>/kurulum-jetonu` (the
//! package's postinst prints the panel link carrying it), so whoever reaches
//! the panel first can't claim it.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path as FsPath;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bacakonay_core::{base32, otp, uri, OtpParams};
use serde::Deserialize;
use serde_json::json;
use zeroize::Zeroizing;

use crate::web::{bad, Admin, ApiError, ApiResult};
use crate::{api, auth, db, files, web, App};

pub const SETUP_TOKEN_FILE: &str = "kurulum-jetonu";
const PENDING_TTL: Duration = Duration::from_secs(10 * 60);
/// Limiter key shared by all wrong setup-token attempts.
const SETUP_KEY: &str = "kurulum";

pub fn write_setup_token(dir: &FsPath) -> Result<String, String> {
    let token = files::random_token();
    files::write_private(&dir.join(SETUP_TOKEN_FILE), token.as_bytes())?;
    Ok(token)
}

pub fn remove_setup_token(dir: &FsPath) {
    let _ = std::fs::remove_file(dir.join(SETUP_TOKEN_FILE));
}

fn setup_token_ok(dir: &FsPath, sent: &str) -> bool {
    // Comparing hashes keeps the comparison time independent of the token.
    std::fs::read_to_string(dir.join(SETUP_TOKEN_FILE))
        .is_ok_and(|t| !t.trim().is_empty() && files::sha256_hex(t.trim()) == files::sha256_hex(sent.trim()))
}

struct PendingAdmin {
    password_hash: String,
    secret: Zeroizing<Vec<u8>>,
    at: Instant,
}

/// Admins whose QR has been shown but whose code hasn't been confirmed yet.
#[derive(Default)]
pub struct Pending(Mutex<HashMap<String, PendingAdmin>>);

impl Pending {
    fn put(&self, name: &str, password_hash: String, secret: Zeroizing<Vec<u8>>) {
        let mut m = self.0.lock().unwrap();
        m.retain(|_, p| p.at.elapsed() < PENDING_TTL);
        m.insert(name.into(), PendingAdmin { password_hash, secret, at: Instant::now() });
    }

    /// `(password hash, base32 secret, used TOTP step)` once `code` matches.
    fn confirm(&self, name: &str, code: &str) -> Option<(String, String, u64)> {
        let mut m = self.0.lock().unwrap();
        m.retain(|_, p| p.at.elapsed() < PENDING_TTL);
        let step = otp::verify_totp(&m.get(name)?.secret, &OtpParams::default(), code, db::now() as u64, 1, None)?;
        let p = m.remove(name)?;
        Some((p.password_hash, base32::encode(&p.secret), step))
    }
}

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/kurulum", get(setup_state).post(setup_begin))
        .route("/api/kurulum/onayla", post(setup_confirm))
        .route("/api/yoneticiler", get(list).post(add_begin))
        .route("/api/yoneticiler/onayla", post(add_confirm))
        .route("/api/yoneticiler/{ad}/sil", post(remove))
}

fn check_new_admin(name: &str, password: &str) -> Result<(), ApiError> {
    if !uzy_proto::valid_username(name) {
        return Err(bad("yönetici adı küçük harf/rakam/-/_ olmalı, harfle başlamalı (en fazla 32)"));
    }
    if password.chars().count() < 12 {
        return Err(bad("parola en az 12 karakter olmalı"));
    }
    Ok(())
}

fn check_code(code: &str) -> Result<(), ApiError> {
    if (6..=8).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_digit()) {
        Ok(())
    } else {
        Err(bad("kod 6 haneli olmalı"))
    }
}

/// Hashes the password, generates the TOTP secret and returns its QR.
async fn begin(app: &App, name: &str, password: Zeroizing<String>) -> ApiResult {
    let secret = Zeroizing::new(otp::generate_secret(20).map_err(|e| bad(e.to_string()))?);
    let hash = tokio::task::spawn_blocking(move || auth::hash_password(&password))
        .await
        .map_err(|e| bad(e.to_string()))?
        .map_err(bad)?;
    let qr = api::qr_payload(&uri::totp_uri(name, "uzakyonetim", &secret, &OtpParams::default()))?;
    app.pending_admins.put(name, hash, secret);
    Ok(Json(qr))
}

/// Confirms the code and stores the admin; the confirming code is marked
/// used so it can't also be replayed for a login.
fn finish(app: &App, name: &str, code: &str) -> Result<(), ApiError> {
    check_code(code)?;
    let (hash, secret, step) =
        app.pending_admins.confirm(name, code).ok_or_else(|| bad("kod tutmadı ya da QR'ın süresi doldu"))?;
    app.db.insert_admin(name, &hash, &secret).map_err(bad)?;
    app.db.set_admin_totp_step(name, step);
    Ok(())
}

// ---- first-run setup ---------------------------------------------------------

async fn setup_state(State(app): State<Arc<App>>) -> ApiResult {
    Ok(Json(json!({ "gerekli": app.db.admin_count() == 0 })))
}

#[derive(Deserialize)]
struct SetupReq {
    jeton: String,
    ad: String,
    parola: String,
}

/// Shared gate of both setup steps: no admin yet and the right token.
fn setup_gate(app: &App, peer: SocketAddr, token: &str) -> Result<(), ApiError> {
    let ip = peer.ip();
    if app.login_limit.blocked(ip, SETUP_KEY) {
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "Çok fazla hatalı deneme. 15 dakika sonra tekrar deneyin.".into()));
    }
    if app.db.admin_count() > 0 {
        return Err(bad("kurulum zaten tamamlanmış; giriş yapın"));
    }
    if !setup_token_ok(&app.data, token) {
        app.login_limit.fail(ip, SETUP_KEY);
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Kurulum bağlantısı geçersiz. Paket kurulumunun yazdığı bağlantıyı kullanın \
             (sudo cat /var/lib/uzakyonetim-sunucu/kurulum-jetonu)."
                .into(),
        ));
    }
    Ok(())
}

async fn setup_begin(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<SetupReq>,
) -> ApiResult {
    setup_gate(&app, peer, &req.jeton)?;
    check_new_admin(&req.ad, &req.parola)?;
    begin(&app, &req.ad, Zeroizing::new(req.parola)).await
}

#[derive(Deserialize)]
struct SetupConfirmReq {
    jeton: String,
    ad: String,
    kod: String,
}

async fn setup_confirm(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<SetupConfirmReq>,
) -> Response {
    let res = setup_gate(&app, peer, &req.jeton).and_then(|_| finish(&app, &req.ad, &req.kod));
    if let Err(e) = res {
        return e.into_response();
    }
    remove_setup_token(&app.data);
    app.db.audit(&req.ad, "-", "ilk_kurulum", &peer.ip().to_string(), "tamam");
    web::session_response(&app, &req.ad)
}

// ---- admin management --------------------------------------------------------

async fn list(State(app): State<Arc<App>>, admin: Admin) -> ApiResult {
    let rows: Vec<_> = app
        .db
        .admins()
        .into_iter()
        .map(|(ad, olusturma)| json!({ "sen": ad == admin.name, "ad": ad, "olusturma": olusturma }))
        .collect();
    Ok(Json(json!(rows)))
}

#[derive(Deserialize)]
struct AddReq {
    ad: String,
    parola: String,
}

async fn add_begin(State(app): State<Arc<App>>, _admin: Admin, Json(req): Json<AddReq>) -> ApiResult {
    check_new_admin(&req.ad, &req.parola)?;
    if app.db.admin(&req.ad).is_some() {
        return Err(bad(format!("{} zaten yönetici", req.ad)));
    }
    begin(&app, &req.ad, Zeroizing::new(req.parola)).await
}

#[derive(Deserialize)]
struct AddConfirmReq {
    ad: String,
    kod: String,
}

async fn add_confirm(State(app): State<Arc<App>>, admin: Admin, Json(req): Json<AddConfirmReq>) -> ApiResult {
    finish(&app, &req.ad, &req.kod)?;
    app.db.audit(&admin.name, "-", "yonetici_ekle", &req.ad, "tamam");
    Ok(Json(json!({ "tamam": true })))
}

async fn remove(State(app): State<Arc<App>>, admin: Admin, Path(ad): Path<String>) -> ApiResult {
    if ad == admin.name {
        return Err(bad("kendi hesabınızı silemezsiniz"));
    }
    if app.db.admin_count() <= 1 {
        return Err(bad("son yönetici silinemez"));
    }
    if !app.db.remove_admin(&ad) {
        return Err(bad("yönetici bulunamadı"));
    }
    app.sessions.remove_admin(&ad);
    app.db.audit(&admin.name, "-", "yonetici_sil", &ad, "tamam");
    Ok(Json(json!({ "tamam": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_admin_needs_matching_code() {
        let p = Pending::default();
        let secret = otp::generate_secret(20).unwrap();
        let code = otp::totp(&secret, &OtpParams::default(), db::now() as u64);
        p.put("ayse", "hash".into(), Zeroizing::new(secret));
        let wrong = if code == "000000" { "111111" } else { "000000" };
        assert!(p.confirm("ayse", wrong).is_none());
        assert!(p.confirm("mehmet", &code).is_none());
        let (hash, _, _) = p.confirm("ayse", &code).unwrap();
        assert_eq!(hash, "hash");
        assert!(p.confirm("ayse", &code).is_none(), "onaylanan bekleyen kayıt silinmeli");
    }

    #[test]
    fn setup_token_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!setup_token_ok(dir.path(), ""), "dosya yokken kurulum kapalı olmalı");
        let t = write_setup_token(dir.path()).unwrap();
        assert!(setup_token_ok(dir.path(), &t));
        assert!(!setup_token_ok(dir.path(), "yanlis"));
        assert!(!setup_token_ok(dir.path(), ""));
        remove_setup_token(dir.path());
        assert!(!setup_token_ok(dir.path(), &t));
    }
}
