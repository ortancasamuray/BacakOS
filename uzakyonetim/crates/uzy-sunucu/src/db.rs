// SPDX-License-Identifier: GPL-3.0-or-later
//! SQLite store (`<veri>/uzakyonetim.db`, 0600).

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub struct Db(Mutex<Connection>);

pub struct Admin {
    pub name: String,
    pub password_hash: String,
    pub totp_b32: String,
    pub totp_last_step: Option<u64>,
}

#[derive(Serialize, Clone)]
pub struct Machine {
    pub id: String,
    pub ad: String,
    pub iptal: bool,
    pub kayit_zamani: i64,
    pub son_gorulme: i64,
    pub isletim_sistemi: String,
    pub ajan_surumu: String,
    pub politika: serde_json::Value,
    pub son_ekran: i64,
    pub son_ekran_oturum: Option<String>,
}

#[derive(Serialize)]
pub struct AuditRow {
    pub zaman: i64,
    pub yonetici: String,
    pub makine: String,
    pub islem: String,
    pub ayrinti: String,
    pub sonuc: String,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS yoneticiler (
    ad TEXT PRIMARY KEY, parola_hash TEXT NOT NULL, totp_b32 TEXT NOT NULL,
    totp_son_adim INTEGER, olusturma INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS makineler (
    id TEXT PRIMARY KEY, ad TEXT NOT NULL, sertifika_fp TEXT NOT NULL UNIQUE,
    iptal INTEGER NOT NULL DEFAULT 0, kayit_zamani INTEGER NOT NULL,
    son_gorulme INTEGER NOT NULL DEFAULT 0, isletim_sistemi TEXT NOT NULL DEFAULT '',
    ajan_surumu TEXT NOT NULL DEFAULT '', politika TEXT NOT NULL DEFAULT '{}',
    son_ekran INTEGER NOT NULL DEFAULT 0, son_ekran_oturum TEXT);
CREATE TABLE IF NOT EXISTS jetonlar (
    hash TEXT PRIMARY KEY, son_gecerlilik INTEGER NOT NULL,
    kullanildi INTEGER NOT NULL DEFAULT 0, olusturan TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS denetim (
    id INTEGER PRIMARY KEY AUTOINCREMENT, zaman INTEGER NOT NULL, yonetici TEXT NOT NULL,
    makine TEXT NOT NULL, islem TEXT NOT NULL, ayrinti TEXT NOT NULL, sonuc TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS ayarlar (anahtar TEXT PRIMARY KEY, deger TEXT NOT NULL);
";

impl Db {
    pub fn open(path: &Path) -> Result<Db, String> {
        let existed = path.exists();
        let c = Connection::open(path).map_err(|e| e.to_string())?;
        if !existed {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;").map_err(|e| e.to_string())?;
        c.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
        Ok(Db(Mutex::new(c)))
    }

    fn c(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.0.lock().unwrap()
    }

    // --- admins ---

    pub fn add_admin(&self, name: &str, hash: &str, totp_b32: &str) -> Result<(), String> {
        self.c()
            .execute(
                "INSERT INTO yoneticiler (ad, parola_hash, totp_b32, olusturma) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(ad) DO UPDATE SET parola_hash=?2, totp_b32=?3, totp_son_adim=NULL",
                params![name, hash, totp_b32, now()],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn admin(&self, name: &str) -> Option<Admin> {
        self.c()
            .query_row(
                "SELECT ad, parola_hash, totp_b32, totp_son_adim FROM yoneticiler WHERE ad=?1",
                params![name],
                |r| {
                    Ok(Admin {
                        name: r.get(0)?,
                        password_hash: r.get(1)?,
                        totp_b32: r.get(2)?,
                        totp_last_step: r.get::<_, Option<i64>>(3)?.map(|v| v as u64),
                    })
                },
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_admin_totp_step(&self, name: &str, step: u64) {
        let _ = self.c().execute("UPDATE yoneticiler SET totp_son_adim=?2 WHERE ad=?1", params![name, step as i64]);
    }

    pub fn admin_count(&self) -> i64 {
        self.c().query_row("SELECT COUNT(*) FROM yoneticiler", [], |r| r.get(0)).unwrap_or(0)
    }

    /// Unlike `add_admin`, never overwrites: one panel admin must not be able
    /// to replace another's password and TOTP secret.
    pub fn insert_admin(&self, name: &str, hash: &str, totp_b32: &str) -> Result<(), String> {
        let n = self
            .c()
            .execute(
                "INSERT OR IGNORE INTO yoneticiler (ad, parola_hash, totp_b32, olusturma) VALUES (?1, ?2, ?3, ?4)",
                params![name, hash, totp_b32, now()],
            )
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err(format!("{name} zaten yönetici"));
        }
        Ok(())
    }

    /// `(name, created)` pairs, oldest first.
    pub fn admins(&self) -> Vec<(String, i64)> {
        let c = self.c();
        let Ok(mut st) = c.prepare("SELECT ad, olusturma FROM yoneticiler ORDER BY olusturma, ad") else {
            return Vec::new();
        };
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    pub fn remove_admin(&self, name: &str) -> bool {
        self.c().execute("DELETE FROM yoneticiler WHERE ad=?1", params![name]).is_ok_and(|n| n > 0)
    }

    // --- join tokens (only the SHA-256 is stored) ---

    pub fn add_token(&self, hash: &str, ttl_secs: i64, by: &str) -> Result<(), String> {
        self.c()
            .execute(
                "INSERT INTO jetonlar (hash, son_gecerlilik, olusturan) VALUES (?1, ?2, ?3)",
                params![hash, now() + ttl_secs, by],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Atomically consume a valid, unused, unexpired token.
    pub fn consume_token(&self, hash: &str) -> bool {
        self.c()
            .execute(
                "UPDATE jetonlar SET kullanildi=1 WHERE hash=?1 AND kullanildi=0 AND son_gecerlilik>?2",
                params![hash, now()],
            )
            .map(|n| n == 1)
            .unwrap_or(false)
    }

    // --- machines ---

    pub fn add_machine(&self, id: &str, name: &str, cert_fp: &str) -> Result<(), String> {
        self.c()
            .execute(
                "INSERT INTO makineler (id, ad, sertifika_fp, kayit_zamani) VALUES (?1, ?2, ?3, ?4)",
                params![id, name, cert_fp, now()],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Machine id for an active (not revoked) client certificate.
    pub fn machine_by_cert(&self, fp: &str) -> Option<String> {
        self.c()
            .query_row("SELECT id FROM makineler WHERE sertifika_fp=?1 AND iptal=0", params![fp], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    pub fn touch_machine(&self, id: &str, name: &str, os: &str, ver: &str, policy: &str) {
        let _ = self.c().execute(
            "UPDATE makineler SET son_gorulme=?2, ad=?3, isletim_sistemi=?4, ajan_surumu=?5, politika=?6 WHERE id=?1",
            params![id, now(), name, os, ver, policy],
        );
    }

    pub fn seen(&self, id: &str) {
        let _ = self.c().execute("UPDATE makineler SET son_gorulme=?2 WHERE id=?1", params![id, now()]);
    }

    pub fn set_screenshot(&self, id: &str, at: i64, session: Option<&str>) {
        let _ = self.c().execute(
            "UPDATE makineler SET son_ekran=?2, son_ekran_oturum=?3 WHERE id=?1",
            params![id, at, session],
        );
    }

    pub fn revoke(&self, id: &str) -> bool {
        self.c().execute("UPDATE makineler SET iptal=1 WHERE id=?1", params![id]).map(|n| n == 1).unwrap_or(false)
    }

    pub fn machines(&self) -> Vec<Machine> {
        let c = self.c();
        let mut st = c
            .prepare(
                "SELECT id, ad, iptal, kayit_zamani, son_gorulme, isletim_sistemi, ajan_surumu, politika,
                        son_ekran, son_ekran_oturum FROM makineler ORDER BY iptal, ad",
            )
            .unwrap();
        st.query_map([], |r| {
            Ok(Machine {
                id: r.get(0)?,
                ad: r.get(1)?,
                iptal: r.get::<_, i64>(2)? != 0,
                kayit_zamani: r.get(3)?,
                son_gorulme: r.get(4)?,
                isletim_sistemi: r.get(5)?,
                ajan_surumu: r.get(6)?,
                politika: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
                son_ekran: r.get(8)?,
                son_ekran_oturum: r.get(9)?,
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    // --- audit ---

    pub fn audit(&self, admin: &str, machine: &str, action: &str, detail: &str, result: &str) {
        let _ = self.c().execute(
            "INSERT INTO denetim (zaman, yonetici, makine, islem, ayrinti, sonuc) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![now(), admin, machine, action, detail, result],
        );
    }

    pub fn audit_log(&self, limit: i64) -> Vec<AuditRow> {
        let c = self.c();
        let mut st = c
            .prepare(
                "SELECT d.zaman, d.yonetici, COALESCE(m.ad, d.makine), d.islem, d.ayrinti, d.sonuc
                 FROM denetim d LEFT JOIN makineler m ON m.id = d.makine ORDER BY d.id DESC LIMIT ?1",
            )
            .unwrap();
        st.query_map(params![limit], |r| {
            Ok(AuditRow {
                zaman: r.get(0)?,
                yonetici: r.get(1)?,
                makine: r.get(2)?,
                islem: r.get(3)?,
                ayrinti: r.get(4)?,
                sonuc: r.get(5)?,
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    // --- settings ---

    pub fn setting(&self, key: &str) -> Option<String> {
        self.c()
            .query_row("SELECT deger FROM ayarlar WHERE anahtar=?1", params![key], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_setting(&self, key: &str, value: &str) {
        let _ = self.c().execute(
            "INSERT INTO ayarlar (anahtar, deger) VALUES (?1, ?2) ON CONFLICT(anahtar) DO UPDATE SET deger=?2",
            params![key, value],
        );
    }

    pub fn screenshot_interval(&self) -> u32 {
        self.setting("ekran_araligi_sn").and_then(|v| v.parse().ok()).unwrap_or(60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_single_use_and_expire() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        db.add_token("h1", 60, "admin").unwrap();
        assert!(db.consume_token("h1"));
        assert!(!db.consume_token("h1"), "ikinci kullanım reddedilmeli");
        db.add_token("h2", -1, "admin").unwrap();
        assert!(!db.consume_token("h2"), "süresi dolmuş jeton reddedilmeli");
        assert!(!db.consume_token("yok"));
    }

    #[test]
    fn revoked_machine_has_no_identity() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).unwrap();
        db.add_machine("m1", "lab-01", "fp1").unwrap();
        assert_eq!(db.machine_by_cert("fp1").as_deref(), Some("m1"));
        assert!(db.revoke("m1"));
        assert!(db.machine_by_cert("fp1").is_none());
    }
}
