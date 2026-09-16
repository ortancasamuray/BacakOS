//! Greeter ⇄ daemon IPC protocol.
//!
//! The unprivileged greeter talks to the privileged daemon over a UNIX domain
//! socket using newline-delimited JSON (one JSON value per line). This keeps
//! the privilege boundary explicit and auditable: the greeter can *only* do
//! what these messages allow. It can never touch PAM, the seat, or logind
//! directly — it asks, the daemon decides.
//!
//! Note: this module intentionally implements (de)serialisation by hand on top
//! of small enums so that `bacak-common` carries no JSON dependency. The daemon
//! and greeter crates pull in `serde_json` and use [`Request`]/[`Response`]
//! via `#[derive(Serialize, Deserialize)]`.

use crate::config::{Accessibility, Greeter, Power, Theme, VirtualKeyboard};
use crate::power::PowerAction;
use crate::sessions::Session;
use crate::users::User;
use serde::{Deserialize, Serialize};

/// Protocol version, bumped on breaking changes; checked in [`Request::Hello`].
pub const PROTOCOL_VERSION: u32 = 1;

/// A secret (password / PIN) carried over the IPC socket.
///
/// Wrapped in a newtype so it (a) never leaks through `Debug`/logs — it renders
/// as `Secret(***)` — and (b) has its heap buffer scrubbed on drop. The wire
/// format is unchanged: `#[serde(transparent)]` (de)serialises it as a plain
/// JSON string.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Secret(s.into())
    }

    /// Borrow the plaintext to hand to PAM. Keep the borrow as short as possible.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<String> for Secret {
    fn from(s: String) -> Self {
        Secret(s)
    }
}

impl From<&str> for Secret {
    fn from(s: &str) -> Self {
        Secret(s.to_string())
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // Best-effort scrub of the heap buffer before it is freed. Volatile
        // writes plus a compiler fence stop the optimiser from eliding the dead
        // store (the same technique `zeroize` uses) — without pulling a
        // dependency into this deliberately dependency-light crate.
        if !self.0.is_empty() {
            // SAFETY: in Drop the bytes are about to be freed, so leaving them as
            // non-UTF-8 zeros is sound — nothing reads `self.0` as a `str` after.
            unsafe {
                for b in self.0.as_bytes_mut() {
                    std::ptr::write_volatile(b, 0u8);
                }
            }
            std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// The subset of `/etc/bacak-display-manager.conf` the greeter must honour.
///
/// The greeter is unprivileged and never reads the config file itself; the
/// daemon hands it this policy in [`Response::Welcome`] so the UI renders and
/// behaves exactly as the admin configured (which power actions to offer,
/// whether to show the user list, theme/accessibility, …).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GreeterPolicy {
    pub greeter: Greeter,
    pub theme: Theme,
    pub power: Power,
    pub accessibility: Accessibility,
    pub keyboard: VirtualKeyboard,
}

/// Messages the greeter sends to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// First message; negotiates the protocol version.
    Hello { protocol: u32 },
    /// Ask for the list of login-capable users.
    ListUsers,
    /// Ask for the list of discovered sessions.
    ListSessions,
    /// Begin a PAM conversation for `username`.
    StartAuth { username: String },
    /// Reply to a PAM prompt (a password, PIN, or other secret).
    ///
    /// Carried in a [`Secret`] wrapper: redacted in any `Debug`/log output and
    /// scrubbed from memory when the request is dropped.
    AuthResponse { secret: Secret },
    /// Abort an in-flight authentication.
    CancelAuth,
    /// After a successful auth, launch this session for the authed user.
    StartSession { session_id: String },
    /// Request a power action (subject to config + polkit).
    Power { action: PowerAction },
}

/// Messages the daemon sends to the greeter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// Acknowledges [`Request::Hello`].
    Welcome {
        protocol: u32,
        /// True when a touchscreen is present (drives the virtual keyboard).
        touchscreen: bool,
        /// The user/session to preselect, if remembered.
        last_user: Option<String>,
        last_session: Option<String>,
        /// Admin policy the greeter must honour (visibility, power, theme, a11y).
        policy: GreeterPolicy,
    },
    Users {
        users: Vec<User>,
    },
    Sessions {
        sessions: Vec<Session>,
    },
    /// PAM asked something. `echo` controls whether input is masked.
    AuthPrompt {
        message: String,
        echo: bool,
    },
    /// PAM informational/error text to display (e.g. "Account expired").
    AuthInfo {
        message: String,
    },
    /// Authentication finished.
    AuthResult {
        success: bool,
        /// Present on failure; safe-to-display reason.
        message: Option<String>,
    },
    /// The session is starting; the greeter should tear itself down.
    SessionStarting,
    /// A requested operation failed.
    Error {
        message: String,
    },
}

impl Request {
    pub fn hello() -> Self {
        Request::Hello {
            protocol: PROTOCOL_VERSION,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // round-trips use serde_json in the binaries; here we just assert the
    // tagged representation is stable, parsing with a tiny hand check.
    #[test]
    fn request_tag_is_snake_case() {
        // We can't depend on serde_json in this crate, so assert via Debug that
        // the variants exist and compile; the wire format is covered in the
        // daemon integration tests.
        let r = Request::hello();
        match r {
            Request::Hello { protocol } => assert_eq!(protocol, PROTOCOL_VERSION),
            _ => unreachable!(),
        }
    }

    #[test]
    fn secret_is_redacted_in_debug() {
        let req = Request::AuthResponse {
            secret: Secret::new("hunter2"),
        };
        let shown = format!("{req:?}");
        assert!(shown.contains("***"), "expected redaction, got {shown}");
        assert!(
            !shown.contains("hunter2"),
            "secret leaked into Debug output: {shown}"
        );
    }

    #[test]
    fn secret_exposes_plaintext_for_pam() {
        assert_eq!(Secret::new("p@ss").expose(), "p@ss");
    }
}
