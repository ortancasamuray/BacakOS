// SPDX-License-Identifier: GPL-3.0-or-later
//! Machine endpoints: list, screenshots, accounts, Bacak Onay, revoke.
//! Every state change is written to the audit log.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use serde::Deserialize;
use serde_json::json;
use uzy_proto::{valid_username, Command, Payload};

use crate::web::{bad, Admin, ApiError, ApiResult};
use crate::App;

const CALL_TIMEOUT: Duration = Duration::from_secs(30);

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/makineler", get(list))
        .route("/api/makineler/{id}/ekran", get(screenshot))
        .route("/api/makineler/{id}/gecmis", get(history))
        .route("/api/makineler/{id}/gecmis/{ts}", get(history_image))
        .route("/api/makineler/{id}/ekran-al", post(take_screenshot))
        .route("/api/makineler/{id}/kullanicilar", get(users))
        .route("/api/makineler/{id}/hesap", post(create_account))
        .route("/api/makineler/{id}/bacakonay", post(totp_begin))
        .route("/api/makineler/{id}/bacakonay/onayla", post(totp_confirm))
        .route("/api/makineler/{id}/bacakonay/kaldir", post(totp_remove))
        .route("/api/makineler/{id}/iptal", post(revoke))
}

fn check_id(id: &str) -> Result<(), ApiError> {
    if id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(bad("geçersiz makine kimliği"))
    }
}

fn check_user(u: &str) -> Result<(), ApiError> {
    if valid_username(u) {
        Ok(())
    } else {
        Err(bad("kullanıcı adı küçük harf/rakam/-/_ olmalı, harfle başlamalı (en fazla 32)"))
    }
}

/// Run a command on the agent and record the outcome.
async fn call(app: &App, admin: &str, id: &str, action: &str, detail: &str, cmd: Command) -> Result<Payload, ApiError> {
    check_id(id)?;
    let r = app.agents.call(id, cmd, CALL_TIMEOUT).await;
    app.db.audit(admin, id, action, detail, match &r {
        Ok(_) => "tamam",
        Err(e) => e,
    });
    r.map_err(|e| ApiError(axum::http::StatusCode::BAD_GATEWAY, e))
}

async fn list(State(app): State<Arc<App>>, _a: Admin) -> ApiResult {
    let list: Vec<_> = app
        .db
        .machines()
        .into_iter()
        .map(|m| {
            let mut v = serde_json::to_value(&m).unwrap();
            v["cevrimici"] = json!(app.agents.online(&m.id));
            v
        })
        .collect();
    Ok(Json(json!(list)))
}

fn jpeg(path: std::path::PathBuf) -> Result<Response, ApiError> {
    let bytes = std::fs::read(path).map_err(|_| ApiError(axum::http::StatusCode::NOT_FOUND, "görüntü yok".into()))?;
    Ok(([(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "private, no-store")], bytes).into_response())
}

async fn screenshot(State(app): State<Arc<App>>, _a: Admin, Path(id): Path<String>) -> Result<Response, ApiError> {
    check_id(&id)?;
    jpeg(app.screens_dir(&id).ok_or(bad("kimlik"))?.join("son.jpg"))
}

async fn history(State(app): State<Arc<App>>, _a: Admin, Path(id): Path<String>) -> ApiResult {
    check_id(&id)?;
    let dir = app.screens_dir(&id).ok_or(bad("kimlik"))?;
    let mut ts: Vec<u64> = std::fs::read_dir(dir)
        .map(|it| {
            it.filter_map(|e| e.ok()?.path().file_stem()?.to_str()?.parse().ok()).collect()
        })
        .unwrap_or_default();
    ts.sort_unstable_by(|a, b| b.cmp(a));
    Ok(Json(json!(ts)))
}

async fn history_image(
    State(app): State<Arc<App>>,
    _a: Admin,
    Path((id, ts)): Path<(String, u64)>,
) -> Result<Response, ApiError> {
    check_id(&id)?;
    jpeg(app.screens_dir(&id).ok_or(bad("kimlik"))?.join(format!("{ts}.jpg")))
}

async fn take_screenshot(State(app): State<Arc<App>>, admin: Admin, Path(id): Path<String>) -> ApiResult {
    match call(&app, &admin.name, &id, "ekran_al", "", Command::EkranAl).await? {
        Payload::Ekran(shot) => {
            crate::gateway::store_screenshot(&app, &id, &shot).map_err(bad)?;
            Ok(Json(json!({ "zaman": shot.zaman })))
        }
        _ => Err(bad("beklenmeyen yanıt")),
    }
}

async fn users(State(app): State<Arc<App>>, _a: Admin, Path(id): Path<String>) -> ApiResult {
    check_id(&id)?;
    // Read-only: not audited per call.
    match app.agents.call(&id, Command::KullanicilariListele, CALL_TIMEOUT).await {
        Ok(Payload::Kullanicilar { liste }) => Ok(Json(json!(liste))),
        Ok(_) => Err(bad("beklenmeyen yanıt")),
        Err(e) => Err(ApiError(axum::http::StatusCode::BAD_GATEWAY, e)),
    }
}

/// QR of the `otpauth://` URI as an SVG data URL + the base32 key for manual
/// entry. Shown once, never stored on the server.
fn qr_payload(uri: &str) -> Result<serde_json::Value, ApiError> {
    let code = qrcode::QrCode::new(uri.as_bytes()).map_err(|e| bad(e.to_string()))?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(240, 240)
        .quiet_zone(true)
        .build();
    let secret = uri
        .split(['?', '&'])
        .find_map(|kv| kv.strip_prefix("secret="))
        .unwrap_or("")
        .to_string();
    Ok(json!({
        "qr": format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(svg)),
        "anahtar": secret,
    }))
}

#[derive(Deserialize)]
struct AccountReq {
    kullanici: String,
    tam_ad: String,
    parola: String,
    #[serde(default)]
    bacakonay: bool,
}

async fn create_account(
    State(app): State<Arc<App>>,
    admin: Admin,
    Path(id): Path<String>,
    Json(req): Json<AccountReq>,
) -> ApiResult {
    check_user(&req.kullanici)?;
    let parola = zeroize::Zeroizing::new(req.parola);
    if parola.chars().count() < 8 {
        return Err(bad("parola en az 8 karakter olmalı"));
    }
    let cmd = Command::HesapAc { kullanici: req.kullanici.clone(), tam_ad: req.tam_ad.clone(), parola: parola.to_string() };
    call(&app, &admin.name, &id, "hesap_ac", &req.kullanici, cmd).await?;
    if !req.bacakonay {
        return Ok(Json(json!({ "tamam": true })));
    }
    let cmd = Command::BacakonayBaslat { kullanici: req.kullanici.clone() };
    match call(&app, &admin.name, &id, "bacakonay_baslat", &req.kullanici, cmd).await? {
        Payload::OtpUri { uri } => Ok(Json(qr_payload(&uri)?)),
        _ => Err(bad("beklenmeyen yanıt")),
    }
}

#[derive(Deserialize)]
struct UserReq {
    kullanici: String,
}

async fn totp_begin(State(app): State<Arc<App>>, admin: Admin, Path(id): Path<String>, Json(req): Json<UserReq>) -> ApiResult {
    check_user(&req.kullanici)?;
    let cmd = Command::BacakonayBaslat { kullanici: req.kullanici.clone() };
    match call(&app, &admin.name, &id, "bacakonay_baslat", &req.kullanici, cmd).await? {
        Payload::OtpUri { uri } => Ok(Json(qr_payload(&uri)?)),
        _ => Err(bad("beklenmeyen yanıt")),
    }
}

#[derive(Deserialize)]
struct ConfirmReq {
    kullanici: String,
    kod: String,
}

async fn totp_confirm(State(app): State<Arc<App>>, admin: Admin, Path(id): Path<String>, Json(req): Json<ConfirmReq>) -> ApiResult {
    check_user(&req.kullanici)?;
    if !(6..=8).contains(&req.kod.len()) || !req.kod.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad("kod 6 haneli olmalı"));
    }
    let cmd = Command::BacakonayOnayla { kullanici: req.kullanici.clone(), kod: req.kod };
    call(&app, &admin.name, &id, "bacakonay_onayla", &req.kullanici, cmd).await?;
    Ok(Json(json!({ "tamam": true })))
}

async fn totp_remove(State(app): State<Arc<App>>, admin: Admin, Path(id): Path<String>, Json(req): Json<UserReq>) -> ApiResult {
    check_user(&req.kullanici)?;
    let cmd = Command::BacakonayKaldir { kullanici: req.kullanici.clone() };
    call(&app, &admin.name, &id, "bacakonay_kaldir", &req.kullanici, cmd).await?;
    Ok(Json(json!({ "tamam": true })))
}

async fn revoke(State(app): State<Arc<App>>, admin: Admin, Path(id): Path<String>) -> ApiResult {
    check_id(&id)?;
    if !app.db.revoke(&id) {
        return Err(bad("makine bulunamadı"));
    }
    app.agents.disconnect(&id);
    app.db.audit(&admin.name, &id, "makine_iptal", "", "tamam");
    Ok(Json(json!({ "tamam": true })))
}
