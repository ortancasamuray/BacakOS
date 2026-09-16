//! UI string table for the greeter and the daemon's user-facing messages.
//!
//! Dependency-light by design (no i18n crate): one [`Ui`] struct of
//! `&'static str` fields, with one `static` instance per [`Language`]. Adding a
//! language is a new `static`; adding a string is a new field plus a value in
//! each table — both checked at compile time, with no runtime key lookup.
//!
//! ```
//! # use bacak_common::config::Language;
//! let t = Language::Tr.ui();
//! assert_eq!(t.log_in, "Giriş yap");
//! ```

use crate::config::Language;

/// Every user-facing string, resolved for one language.
pub struct Ui {
    // Login form.
    pub subtitle: &'static str,
    pub user: &'static str,
    pub password: &'static str,
    pub session: &'static str,
    pub log_in: &'static str,
    pub username_hint: &'static str,
    pub password_hint: &'static str,
    // Prompt phase.
    pub submit: &'static str,
    pub cancel: &'static str,
    // Power actions.
    pub shutdown: &'static str,
    pub restart: &'static str,
    pub suspend: &'static str,
    pub hibernate: &'static str,
    // Transient status.
    pub connecting: &'static str,
    pub starting_session: &'static str,
    pub authenticating: &'static str,
    // On-screen keyboard.
    pub osk_active: &'static str,
    pub osk_space: &'static str,
    pub osk_back: &'static str,
    pub osk_shift: &'static str,
    pub osk_enter: &'static str,
    // Messages (greeter-local validation + daemon auth results).
    pub choose_user: &'static str,
    pub auth_failed: &'static str,
    pub password_expired: &'static str,
    pub account_unavailable: &'static str,
}

impl Language {
    /// The string table for this language.
    pub fn ui(self) -> &'static Ui {
        match self {
            Language::Tr => &TR,
            Language::En => &EN,
        }
    }
}

static TR: Ui = Ui {
    subtitle: "Oturum Yöneticisi",
    user: "Kullanıcı",
    password: "Parola",
    session: "Oturum",
    log_in: "Giriş yap",
    username_hint: "kullanıcı adı",
    password_hint: "Parola",
    submit: "Gönder",
    cancel: "İptal",
    shutdown: "Kapat",
    restart: "Yeniden başlat",
    suspend: "Uyku",
    hibernate: "Hazırda beklet",
    connecting: "Görüntü yöneticisine bağlanılıyor…",
    starting_session: "Oturum başlatılıyor…",
    authenticating: "Kimlik doğrulanıyor…",
    osk_active: "Ekran klavyesi açık",
    osk_space: "Boşluk",
    osk_back: "Sil",
    osk_shift: "Shift",
    osk_enter: "Enter",
    choose_user: "Bir kullanıcı seçin.",
    auth_failed: "Kimlik doğrulama başarısız.",
    password_expired: "Parolanızın süresi dolmuş.",
    account_unavailable: "Bu hesap şu anda kullanılamıyor.",
};

static EN: Ui = Ui {
    subtitle: "Display Manager",
    user: "User",
    password: "Password",
    session: "Session",
    log_in: "Log in",
    username_hint: "username",
    password_hint: "Password",
    submit: "Submit",
    cancel: "Cancel",
    shutdown: "Shut down",
    restart: "Restart",
    suspend: "Suspend",
    hibernate: "Hibernate",
    connecting: "Connecting to display manager…",
    starting_session: "Starting session…",
    authenticating: "Authenticating…",
    osk_active: "On-screen keyboard active",
    osk_space: "Space",
    osk_back: "Back",
    osk_shift: "Shift",
    osk_enter: "Enter",
    choose_user: "Choose a user.",
    auth_failed: "Authentication failed.",
    password_expired: "Your password has expired.",
    account_unavailable: "This account is currently unavailable.",
};
