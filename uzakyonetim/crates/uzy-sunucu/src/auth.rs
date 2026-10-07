// SPDX-License-Identifier: GPL-3.0-or-later
//! Panel login: password (Argon2id) **and** a Bacak Onay TOTP code, then a
//! server-side session in an `HttpOnly; Secure; SameSite=Strict` cookie plus
//! a CSRF token required on every state-changing request.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use bacakonay_core::{base32, otp, OtpParams};

use crate::db::{self, Db};
use crate::files;

pub const COOKIE: &str = "uzy_oturum";
const IDLE: Duration = Duration::from_secs(30 * 60);
const ABSOLUTE: Duration = Duration::from_secs(8 * 60 * 60);

pub fn hash_password(pw: &str) -> Result<String, String> {
    let salt = SaltString::encode_b64(&files::random_bytes::<16>()).map_err(|e| e.to_string())?;
    Argon2::default().hash_password(pw.as_bytes(), &salt).map(|h| h.to_string()).map_err(|e| e.to_string())
}

fn verify_password(pw: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
}

/// Burns the same Argon2 time for unknown users, so response timing doesn't
/// reveal which admin names exist.
static DUMMY_HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

#[derive(Clone)]
pub struct Session {
    pub admin: String,
    pub csrf: String,
    created: Instant,
    last: Instant,
}

#[derive(Default)]
pub struct Sessions(Mutex<HashMap<String, Session>>);

impl Sessions {
    /// Returns `(cookie token, csrf)`. Only the token's hash is kept.
    pub fn create(&self, admin: &str) -> (String, String) {
        let token = files::random_token();
        let csrf = files::random_token();
        let now = Instant::now();
        let mut m = self.0.lock().unwrap();
        m.retain(|_, s| s.last.elapsed() < IDLE && s.created.elapsed() < ABSOLUTE);
        m.insert(files::sha256_hex(&token), Session { admin: admin.into(), csrf: csrf.clone(), created: now, last: now });
        (token, csrf)
    }

    pub fn get(&self, token: &str) -> Option<Session> {
        let mut m = self.0.lock().unwrap();
        let key = files::sha256_hex(token);
        let s = m.get_mut(&key)?;
        if s.last.elapsed() >= IDLE || s.created.elapsed() >= ABSOLUTE {
            m.remove(&key);
            return None;
        }
        s.last = Instant::now();
        Some(s.clone())
    }

    pub fn remove(&self, token: &str) {
        self.0.lock().unwrap().remove(&files::sha256_hex(token));
    }

    /// Ends every session of a removed admin.
    pub fn remove_admin(&self, admin: &str) {
        self.0.lock().unwrap().retain(|_, s| s.admin != admin);
    }
}

/// 5 failures per IP or per user name within 15 min → locked for the rest
/// of the window.
#[derive(Default)]
pub struct LoginLimiter(Mutex<HashMap<String, (u32, Instant)>>);

impl LoginLimiter {
    const MAX: u32 = 5;
    const WINDOW: Duration = Duration::from_secs(15 * 60);

    fn keys(ip: IpAddr, user: &str) -> [String; 2] {
        [format!("ip:{ip}"), format!("u:{}", user.to_lowercase())]
    }

    pub fn blocked(&self, ip: IpAddr, user: &str) -> bool {
        let mut m = self.0.lock().unwrap();
        m.retain(|_, (_, t)| t.elapsed() < Self::WINDOW);
        Self::keys(ip, user).iter().any(|k| m.get(k).is_some_and(|(n, _)| *n >= Self::MAX))
    }

    pub fn fail(&self, ip: IpAddr, user: &str) {
        let mut m = self.0.lock().unwrap();
        for k in Self::keys(ip, user) {
            m.entry(k).or_insert((0, Instant::now())).0 += 1;
        }
    }

    pub fn success(&self, ip: IpAddr, user: &str) {
        let mut m = self.0.lock().unwrap();
        for k in Self::keys(ip, user) {
            m.remove(&k);
        }
    }
}

/// Full login check. The same generic error for every failure mode.
pub fn verify_login(db: &Db, user: &str, password: &str, code: &str) -> bool {
    let Some(admin) = db.admin(user) else {
        let dummy = DUMMY_HASH.get_or_init(|| hash_password("bos-parola-zamanlama").unwrap_or_default());
        let _ = verify_password(password, dummy);
        return false;
    };
    if !verify_password(password, &admin.password_hash) {
        return false;
    }
    let Some(secret) = base32::decode(&admin.totp_b32) else { return false };
    match otp::verify_totp(&secret, &OtpParams::default(), code, db::now() as u64, 1, admin.totp_last_step) {
        Some(step) => {
            db.set_admin_totp_step(user, step);
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_needs_password_and_fresh_code() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        let secret = otp::generate_secret(20).unwrap();
        db.add_admin("yonetici", &hash_password("dogru-parola-123").unwrap(), &base32::encode(&secret)).unwrap();
        let code = otp::totp(&secret, &OtpParams::default(), db::now() as u64);

        assert!(!verify_login(&db, "yonetici", "yanlis", &code));
        assert!(!verify_login(&db, "yok", "dogru-parola-123", &code));
        assert!(verify_login(&db, "yonetici", "dogru-parola-123", &code));
        assert!(!verify_login(&db, "yonetici", "dogru-parola-123", &code), "aynı kod ikinci kez geçmemeli");
    }

    #[test]
    fn limiter_blocks_after_five_failures() {
        let l = LoginLimiter::default();
        let ip: IpAddr = "10.0.0.9".parse().unwrap();
        for _ in 0..5 {
            assert!(!l.blocked(ip, "a"));
            l.fail(ip, "a");
        }
        assert!(l.blocked(ip, "a"));
        assert!(l.blocked(ip, "baska"), "aynı IP'den başka kullanıcı da engellenmeli");
        assert!(l.blocked("10.0.0.10".parse().unwrap(), "A"), "aynı kullanıcı başka IP'den de engellenmeli");
    }

    #[test]
    fn sessions_roundtrip_and_logout() {
        let s = Sessions::default();
        let (tok, csrf) = s.create("yonetici");
        let got = s.get(&tok).unwrap();
        assert_eq!(got.admin, "yonetici");
        assert_eq!(got.csrf, csrf);
        s.remove(&tok);
        assert!(s.get(&tok).is_none());
        assert!(s.get("uydurma").is_none());
    }
}
