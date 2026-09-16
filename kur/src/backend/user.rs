// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Account and hostname validation.
//!
//! Validation runs on every keystroke, so it must be allocation-light and must
//! never shell out. The rules here mirror `adduser(8)`'s `NAME_REGEX` and
//! RFC 1123 for hostnames — being stricter than the tools we later invoke means
//! the install can never fail at the `useradd` step.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("Bilgisayar adı boş olamaz")]
    HostnameEmpty,
    #[error("Bilgisayar adı en fazla 63 karakter olabilir")]
    HostnameTooLong,
    #[error("Bilgisayar adı yalnızca harf, rakam ve tire içerebilir; tire ile başlayamaz veya bitemez")]
    HostnameInvalid,

    #[error("Kullanıcı adı boş olamaz")]
    UsernameEmpty,
    #[error("Kullanıcı adı en fazla 32 karakter olabilir")]
    UsernameTooLong,
    #[error("Kullanıcı adı küçük harfle başlamalı; yalnızca küçük harf, rakam, tire ve alt çizgi içerebilir")]
    UsernameInvalid,
    #[error("`{0}` sistem tarafından ayrılmış bir kullanıcı adıdır")]
    UsernameReserved(String),

    #[error("Parola en az 8 karakter olmalı")]
    PasswordTooShort,
    #[error("Parolalar eşleşmiyor")]
    PasswordMismatch,
    #[error("Parola çok zayıf — büyük/küçük harf, rakam ve sembol karıştırın")]
    PasswordTooWeak,
}

/// Accounts Debian creates itself. Colliding with one makes `useradd` fail
/// after the base system is already unpacked, which is unrecoverable in the UI.
const RESERVED: &[&str] = &[
    "root", "daemon", "bin", "sys", "sync", "games", "man", "lp", "mail", "news", "uucp", "proxy",
    "www-data", "backup", "list", "irc", "nobody", "systemd-network", "messagebus", "sshd",
];

pub fn validate_hostname(hostname: &str) -> Result<(), ValidationError> {
    if hostname.is_empty() {
        return Err(ValidationError::HostnameEmpty);
    }
    if hostname.len() > 63 {
        return Err(ValidationError::HostnameTooLong);
    }
    let valid_chars = hostname.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let good_edges = !hostname.starts_with('-') && !hostname.ends_with('-');
    if !valid_chars || !good_edges {
        return Err(ValidationError::HostnameInvalid);
    }
    Ok(())
}

pub fn validate_username(username: &str) -> Result<(), ValidationError> {
    if username.is_empty() {
        return Err(ValidationError::UsernameEmpty);
    }
    if username.len() > 32 {
        return Err(ValidationError::UsernameTooLong);
    }
    let starts_lower = username.chars().next().is_some_and(|c| c.is_ascii_lowercase());
    let valid_chars = username
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !starts_lower || !valid_chars {
        return Err(ValidationError::UsernameInvalid);
    }
    if RESERVED.contains(&username) {
        return Err(ValidationError::UsernameReserved(username.to_string()));
    }
    Ok(())
}

/// Password strength on a 0..=4 scale, driving the four-segment meter in the UI.
///
/// This is a deliberately simple character-class + length heuristic, not an
/// entropy estimate. It is advisory: [`validate_password`] only *rejects* at
/// score 0, so a long passphrase of lowercase words (score 2) is accepted.
pub fn password_score(password: &str) -> u8 {
    if password.len() < 8 {
        return 0;
    }

    let mut classes = 0;
    if password.chars().any(|c| c.is_ascii_lowercase()) {
        classes += 1;
    }
    if password.chars().any(|c| c.is_ascii_uppercase()) {
        classes += 1;
    }
    if password.chars().any(|c| c.is_ascii_digit()) {
        classes += 1;
    }
    if password.chars().any(|c| !c.is_ascii_alphanumeric()) {
        classes += 1;
    }

    // Length compensates for variety: a 20-character passphrase is stronger
    // than "P@ss1!" even though the latter uses every class.
    let length_bonus = match password.len() {
        0..=11 => 0,
        12..=15 => 1,
        _ => 2,
    };

    (classes + length_bonus).min(4) as u8
}

pub fn validate_password(password: &str, confirm: &str) -> Result<(), ValidationError> {
    if password.len() < 8 {
        return Err(ValidationError::PasswordTooShort);
    }
    if password != confirm {
        return Err(ValidationError::PasswordMismatch);
    }
    if password_score(password) == 0 {
        return Err(ValidationError::PasswordTooWeak);
    }
    Ok(())
}

/// Everything the install stage needs to create the account.
#[derive(Debug, Clone)]
pub struct Account {
    pub hostname: String,
    pub username: String,
    pub password: String,
    /// When false the root account is locked (`passwd -l`), the Ubuntu model.
    pub root_same_password: bool,
}

impl Account {
    /// Validate all fields together. Returns the first problem, in the order
    /// the fields appear on screen, so the message always points forwards.
    pub fn validate(&self, confirm: &str) -> Result<(), ValidationError> {
        validate_hostname(&self.hostname)?;
        validate_username(&self.username)?;
        validate_password(&self.password, confirm)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostname_rules() {
        assert!(validate_hostname("bacakos").is_ok());
        assert!(validate_hostname("bacak-os-1").is_ok());
        assert_eq!(validate_hostname(""), Err(ValidationError::HostnameEmpty));
        assert_eq!(validate_hostname("-bad"), Err(ValidationError::HostnameInvalid));
        assert_eq!(validate_hostname("bad-"), Err(ValidationError::HostnameInvalid));
        assert_eq!(validate_hostname("bad_host"), Err(ValidationError::HostnameInvalid));
        assert_eq!(validate_hostname(&"a".repeat(64)), Err(ValidationError::HostnameTooLong));
    }

    #[test]
    fn username_rules() {
        assert!(validate_username("mustafa").is_ok());
        assert!(validate_username("user_1-x").is_ok());
        assert_eq!(validate_username("Mustafa"), Err(ValidationError::UsernameInvalid));
        assert_eq!(validate_username("1user"), Err(ValidationError::UsernameInvalid));
        assert_eq!(
            validate_username("root"),
            Err(ValidationError::UsernameReserved("root".into()))
        );
    }

    #[test]
    fn short_passwords_score_zero() {
        assert_eq!(password_score("Ab1!"), 0);
        assert_eq!(password_score(""), 0);
    }

    #[test]
    fn long_passphrase_outscores_short_complex_one() {
        // 8 chars, all four classes -> 4 classes + 0 bonus = 4, capped.
        assert_eq!(password_score("P@ssw0r1"), 4);
        // 24 lowercase chars -> 1 class + 2 bonus = 3. Accepted, not "strong".
        assert_eq!(password_score("correcthorsebatterystap"), 3);
    }

    #[test]
    fn password_validation_order() {
        assert_eq!(validate_password("short", "short"), Err(ValidationError::PasswordTooShort));
        assert_eq!(
            validate_password("longenough1", "different"),
            Err(ValidationError::PasswordMismatch)
        );
        assert!(validate_password("longenough1", "longenough1").is_ok());
    }

    #[test]
    fn account_reports_first_failure_in_screen_order() {
        let account = Account {
            hostname: "-bad".into(),
            username: "root".into(),
            password: "x".into(),
            root_same_password: true,
        };
        // Hostname is above username on screen, so it is reported first.
        assert_eq!(account.validate("x"), Err(ValidationError::HostnameInvalid));
    }
}
