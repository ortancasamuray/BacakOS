// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading the replies of `sudo bacakonay arayuz …` (protocol in
//! `bacakonay-cli/src/main.rs`).

/// Whether two-step login is on for the logged-in user.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Durum {
    Acik,
    Kapali,
}

/// One reply line of `bacakonay arayuz kur`.
#[derive(PartialEq, Eq, Debug)]
pub enum Satir {
    Kayitli,
    Uri(String),
    Anahtar(String),
    Tamam,
    /// "1/3": which attempt failed.
    Hatali(String),
    Iptal,
    Vazgecildi,
    /// Not protocol: an error message (stderr is merged in) or sudo noise.
    Diger(String),
}

pub fn satir(l: &str) -> Satir {
    let l = l.trim_end();
    match l.split_once(' ') {
        Some(("URI", v)) => Satir::Uri(v.to_string()),
        Some(("ANAHTAR", v)) => Satir::Anahtar(v.to_string()),
        Some(("HATALI", v)) => Satir::Hatali(v.to_string()),
        _ => match l {
            "KAYITLI" => Satir::Kayitli,
            "TAMAM" => Satir::Tamam,
            "IPTAL" => Satir::Iptal,
            "VAZGECILDI" => Satir::Vazgecildi,
            _ => Satir::Diger(l.to_string()),
        },
    }
}

/// The state from `arayuz durum`'s output, if it is protocol at all.
pub fn durum(cikti: &str) -> Option<Durum> {
    cikti.lines().find_map(|l| match l.split_whitespace().next() {
        Some("ACIK") => Some(Durum::Acik),
        Some("KAPALI") => Some(Durum::Kapali),
        _ => None,
    })
}

/// What to tell the user when the command failed: bacakonay's own message
/// when there is one, otherwise it was sudo turning the password down (wrong
/// password, or an account that may not use sudo — sudo's own wording is
/// English and not worth showing).
pub fn hata(cikti: &str) -> String {
    match cikti.lines().find_map(|l| l.strip_prefix("bacakonay: ")) {
        Some(m) => format!("İşlem yapılamadı: {m}"),
        None => "Parola yanlış ya da bu hesap yönetici (sudo) yetkisine sahip değil.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_enroll_replies() {
        assert_eq!(satir("URI otpauth://totp/x?secret=AB\n"), Satir::Uri("otpauth://totp/x?secret=AB".into()));
        assert_eq!(satir("ANAHTAR ABCD EFGH"), Satir::Anahtar("ABCD EFGH".into()));
        assert_eq!(satir("HATALI 2/3"), Satir::Hatali("2/3".into()));
        assert_eq!(satir("TAMAM"), Satir::Tamam);
        assert_eq!(satir("KAYITLI"), Satir::Kayitli);
        assert_eq!(satir("[sudo] password for x:"), Satir::Diger("[sudo] password for x:".into()));
    }

    #[test]
    fn tells_sudo_refusal_from_bacakonay_errors() {
        assert_eq!(durum("ACIK SHA1 6 30\n"), Some(Durum::Acik));
        assert_eq!(durum("KAPALI\n"), Some(Durum::Kapali));
        assert_eq!(durum("Sorry, try again.\n"), None);
        assert!(hata("sudo: 1 incorrect password attempt\n").starts_with("Parola yanlış"));
        assert_eq!(hata("bacakonay: güvensiz kayıt: x\n"), "İşlem yapılamadı: güvensiz kayıt: x");
    }
}
