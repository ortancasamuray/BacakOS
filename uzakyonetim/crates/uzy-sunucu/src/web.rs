// SPDX-License-Identifier: GPL-3.0-or-later
//! HTTPS web panel: static UI + JSON API.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, FromRequestParts, State};
use axum::http::{header, request::Parts, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use serde::Deserialize;
use serde_json::json;

use crate::{admins, api, auth, files, App};

const INDEX_HTML: &str = include_str!("../../../web/index.html");
const APP_JS: &str = include_str!("../../../web/app.js");
const APP_CSS: &str = include_str!("../../../web/app.css");

/// JSON error with a status code.
pub struct ApiError(pub StatusCode, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "hata": self.1 }))).into_response()
    }
}

pub fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

pub type ApiResult<T = Json<serde_json::Value>> = Result<T, ApiError>;

/// An authenticated admin. State-changing methods also require the CSRF
/// header to match the session's token.
pub struct Admin {
    pub name: String,
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| kv.trim().strip_prefix(&format!("{}=", auth::COOKIE)).map(str::to_string))
}

impl FromRequestParts<Arc<App>> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, Self::Rejection> {
        let unauth = || ApiError(StatusCode::UNAUTHORIZED, "oturum açılmamış".into());
        let token = cookie_token(&parts.headers).ok_or_else(unauth)?;
        let s = app.sessions.get(&token).ok_or_else(unauth)?;
        if parts.method != Method::GET && parts.method != Method::HEAD {
            let sent = parts.headers.get("x-uzy-csrf").and_then(|v| v.to_str().ok()).unwrap_or("");
            if sent.is_empty() || sent != s.csrf {
                return Err(ApiError(StatusCode::FORBIDDEN, "CSRF doğrulaması başarısız".into()));
            }
        }
        Ok(Admin { name: s.admin })
    }
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(|| async { static_file("text/html; charset=utf-8", INDEX_HTML) }))
        .route("/app.js", get(|| async { static_file("text/javascript; charset=utf-8", APP_JS) }))
        .route("/app.css", get(|| async { static_file("text/css; charset=utf-8", APP_CSS) }))
        .route("/api/giris", post(login))
        .route("/api/cikis", post(logout))
        .route("/api/oturum", get(whoami))
        .route("/api/ayarlar", get(get_settings).post(set_settings))
        .route("/api/denetim", get(audit))
        .route("/api/katilim-kodu", post(join_code))
        .merge(api::routes())
        .merge(admins::routes())
        .layer(axum::middleware::map_response(security_headers))
        .with_state(app)
}

fn static_file(ct: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, ct)], body).into_response()
}

async fn security_headers(mut res: Response) -> Response {
    let h = res.headers_mut();
    let set = |h: &mut HeaderMap, k: &'static str, v: &'static str| {
        h.insert(k, HeaderValue::from_static(v));
    };
    set(
        h,
        "content-security-policy",
        "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; \
         connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
    );
    set(h, "x-content-type-options", "nosniff");
    set(h, "referrer-policy", "no-referrer");
    set(h, "x-frame-options", "DENY");
    set(h, "strict-transport-security", "max-age=31536000");
    if !h.contains_key(header::CACHE_CONTROL) {
        set(h, "cache-control", "no-store");
    }
    res
}

#[derive(Deserialize)]
struct LoginReq {
    kullanici: String,
    parola: String,
    kod: String,
}

async fn login(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<LoginReq>,
) -> Response {
    let ip = peer.ip();
    if app.login_limit.blocked(ip, &req.kullanici) {
        return ApiError(StatusCode::TOO_MANY_REQUESTS, "Çok fazla hatalı deneme. 15 dakika sonra tekrar deneyin.".into())
            .into_response();
    }
    let ok = {
        let app = app.clone();
        let (u, p, k) = (req.kullanici.clone(), zeroize::Zeroizing::new(req.parola), req.kod);
        tokio::task::spawn_blocking(move || auth::verify_login(&app.db, &u, &p, &k)).await.unwrap_or(false)
    };
    if !ok {
        app.login_limit.fail(ip, &req.kullanici);
        app.db.audit(&req.kullanici, "-", "giris", &ip.to_string(), "red");
        return ApiError(StatusCode::UNAUTHORIZED, "Kullanıcı adı, parola ya da Bacak Onay kodu hatalı.".into())
            .into_response();
    }
    app.login_limit.success(ip, &req.kullanici);
    app.db.audit(&req.kullanici, "-", "giris", &ip.to_string(), "tamam");
    session_response(&app, &req.kullanici)
}

/// Starts a session for `admin`: cookie plus the CSRF token for the UI.
pub fn session_response(app: &App, admin: &str) -> Response {
    let (token, csrf) = app.sessions.create(admin);
    let cookie = format!("{}={token}; Path=/; HttpOnly; Secure; SameSite=Strict", auth::COOKIE);
    (
        [(header::SET_COOKIE, cookie)],
        Json(json!({ "yonetici": admin, "csrf": csrf })),
    )
        .into_response()
}

async fn logout(State(app): State<Arc<App>>, headers: HeaderMap, admin: Admin) -> Response {
    if let Some(t) = cookie_token(&headers) {
        app.sessions.remove(&t);
    }
    app.db.audit(&admin.name, "-", "cikis", "", "tamam");
    let cookie = format!("{}=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0", auth::COOKIE);
    ([(header::SET_COOKIE, cookie)], Json(json!({}))).into_response()
}

async fn whoami(State(app): State<Arc<App>>, headers: HeaderMap) -> ApiResult {
    let s = cookie_token(&headers)
        .and_then(|t| app.sessions.get(&t))
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "oturum açılmamış".into()))?;
    Ok(Json(json!({ "yonetici": s.admin, "csrf": s.csrf })))
}

async fn get_settings(State(app): State<Arc<App>>, _a: Admin) -> ApiResult {
    Ok(Json(json!({ "ekran_araligi_sn": app.db.screenshot_interval() })))
}

#[derive(Deserialize)]
struct SettingsReq {
    ekran_araligi_sn: u32,
}

async fn set_settings(State(app): State<Arc<App>>, admin: Admin, Json(req): Json<SettingsReq>) -> ApiResult {
    let v = req.ekran_araligi_sn;
    if v != 0 && !(10..=3600).contains(&v) {
        return Err(bad("aralık 0 (kapalı) ya da 10–3600 sn olmalı"));
    }
    app.db.set_setting("ekran_araligi_sn", &v.to_string());
    app.agents.broadcast_settings(v).await;
    app.db.audit(&admin.name, "-", "ekran_araligi", &v.to_string(), "tamam");
    Ok(Json(json!({ "ekran_araligi_sn": v })))
}

async fn audit(State(app): State<Arc<App>>, _a: Admin) -> ApiResult {
    Ok(Json(json!(app.db.audit_log(300))))
}

#[derive(Deserialize)]
struct JoinReq {
    #[serde(default = "default_ttl")]
    gecerlilik_dk: i64,
}

fn default_ttl() -> i64 {
    60
}

/// One-time join code: `uzy1.<base64url {s, j, f}>`. Only the token's hash
/// is stored; the code itself is shown to the admin once.
async fn join_code(State(app): State<Arc<App>>, admin: Admin, Json(req): Json<JoinReq>) -> ApiResult {
    let ttl = req.gecerlilik_dk.clamp(5, 24 * 60);
    let token = files::random_token();
    app.db.add_token(&files::sha256_hex(&token), ttl * 60, &admin.name).map_err(bad)?;
    let body = json!({ "s": app.join_address(), "j": token, "f": app.ca_fp });
    let code = format!(
        "uzy1.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&body).unwrap())
    );
    app.db.audit(&admin.name, "-", "katilim_kodu", &format!("{ttl} dk"), "tamam");
    Ok(Json(json!({
        "kod": code,
        "gecerlilik_dk": ttl,
        "komut": format!("sudo uzakyonetim-ajan kaydol {code}"),
    })))
}
