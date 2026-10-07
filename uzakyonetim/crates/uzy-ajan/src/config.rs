// SPDX-License-Identifier: GPL-3.0-or-later
//! Agent files under `/etc/uzakyonetim` and the local policy.

use std::path::PathBuf;

use base64::Engine;
use serde::{Deserialize, Serialize};
use uzy_proto::Policy;

pub const DIR: &str = "/etc/uzakyonetim";

pub fn path(name: &str) -> PathBuf {
    PathBuf::from(DIR).join(name)
}

/// What `kaydol` saves next to the key/certificates.
#[derive(Serialize, Deserialize)]
pub struct Saved {
    /// `host:port` of the agent gateway.
    pub sunucu: String,
    pub makine_id: String,
}

/// The one string the panel shows for "add machine":
/// `uzy1.<base64url(json)>` carrying server address, one-time token and the
/// SHA-256 fingerprint of the server's CA (pinned on first contact).
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct JoinCode {
    pub s: String,
    pub j: String,
    pub f: String,
}

impl JoinCode {
    pub fn parse(code: &str) -> Result<JoinCode, String> {
        let body = code.trim().strip_prefix("uzy1.").ok_or("katılım kodu 'uzy1.' ile başlamalı")?;
        let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| "katılım kodu bozuk")?;
        let c: JoinCode = serde_json::from_slice(&json).map_err(|_| "katılım kodu bozuk")?;
        if c.f.len() != 64 || !c.f.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("katılım kodundaki parmak izi geçersiz".into());
        }
        Ok(c)
    }
}

/// `/etc/uzakyonetim/politika.toml` — tiny `anahtar = true|false` format.
/// Missing file → everything allowed; any unparsable value → that operation off.
pub fn policy() -> Policy {
    let Ok(text) = std::fs::read_to_string(path("politika.toml")) else { return Policy::default() };
    let mut p = Policy::default();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((k, v)) = line.split_once('=') else { continue };
        let on = v.trim() == "true";
        match k.trim() {
            "hesap_acma" => p.hesap_acma = on,
            "hesap_silme" => p.hesap_silme = on,
            "bacakonay" => p.bacakonay = on,
            "ekran_izleme" => p.ekran_izleme = on,
            _ => {}
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_code_roundtrip() {
        let c = JoinCode { s: "10.0.0.5:8444".into(), j: "abc".into(), f: "a".repeat(64) };
        let enc = format!(
            "uzy1.{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&c).unwrap())
        );
        assert_eq!(JoinCode::parse(&enc).unwrap(), c);
        assert!(JoinCode::parse("uzy2.xxx").is_err());
        assert!(JoinCode::parse("uzy1.!!!").is_err());
    }
}
