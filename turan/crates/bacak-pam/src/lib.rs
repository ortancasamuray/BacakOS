//! PAM authentication for BDM.
//!
//! ## Design
//!
//! Authentication is modelled as a *conversation*: PAM drives the exchange by
//! emitting prompts ("Password:", "Enter PIN:", info, errors) and the front-end
//! supplies answers. We expose that as the [`Conversation`] trait so the same
//! authenticator works whether the answers come from the greeter over IPC, from
//! a test harness, or from an autologin path that has no prompts at all.
//!
//! ## Privilege separation
//!
//! `pam_authenticate` + `pam_setcred` + `pam_open_session` must run with the
//! privilege to validate credentials and create the session record. In BDM that
//! is the **daemon** (root), never the greeter. The greeter only relays prompt
//! text and secrets over the local socket. After `pam_open_session` succeeds,
//! the daemon forks, and the child drops to the target uid/gid before `execve`
//! of the session — see `bacak-session-launcher`.
//!
//! ## PAM service name
//!
//! BDM ships `/etc/pam.d/bacak-display-manager`. The conversation runs under
//! that service so administrators can compose policy (pam_unix, pam_systemd,
//! pam_faillock, fingerprint/PIN modules, …) the standard way.

use bacak_common::ipc::Secret;
use std::fmt;

pub const PAM_SERVICE: &str = "bacak-display-manager";

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("authentication failed")]
    AuthFailed,
    #[error("account unavailable: {0}")]
    AccountUnavailable(String),
    #[error("credentials expired")]
    CredentialsExpired,
    #[error("the conversation was aborted")]
    Aborted,
    #[error("pam error ({code}): {message}")]
    Pam { code: i32, message: String },
    #[error("conversation supplied no answer")]
    NoAnswer,
}

pub type AuthResult<T> = Result<T, AuthError>;

/// A single message PAM asks the front-end to handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// Ask for input that must be hidden (password, PIN).
    SecretInput(String),
    /// Ask for input that may be echoed (username, OTP token shown on a fob).
    VisibleInput(String),
    /// Display informational text; no answer expected.
    Info(String),
    /// Display an error; no answer expected.
    Error(String),
}

/// Front-end side of a PAM conversation.
///
/// Implementors translate prompts into UI (or canned answers) and return the
/// secret/visible reply. For `Info`/`Error` they return `Ok(None)`.
///
/// The reply is a [`Secret`] (not a bare `String`) so the plaintext is scrubbed
/// from memory when it drops, rather than lingering as an extra heap copy on the
/// path to libpam.
pub trait Conversation {
    fn handle(&mut self, prompt: &Prompt) -> AuthResult<Option<Secret>>;
}

/// Identity established by a successful authentication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthedUser {
    pub username: String,
}

/// The authenticator abstraction the daemon programs against.
///
/// Two implementations exist:
/// * [`mock::MockAuthenticator`] — always available, for tests/dev.
/// * `system::SystemAuthenticator` — real libpam, behind `--features system-pam`.
pub trait Authenticator {
    /// Run the full `authenticate` + `acct_mgmt` flow for `username`, using
    /// `conv` to obtain secrets. On success the session is *not* yet opened;
    /// call [`Authenticator::open_session`].
    fn authenticate(
        &mut self,
        username: &str,
        conv: &mut dyn Conversation,
    ) -> AuthResult<AuthedUser>;

    /// Establish credentials and open the PAM session (`pam_setcred` +
    /// `pam_open_session`). Returns the environment PAM wants exported into the
    /// session (e.g. `XDG_*`, `DBUS_*`, krb5 ccache) as `KEY=VALUE` pairs.
    fn open_session(&mut self) -> AuthResult<Vec<(String, String)>>;

    /// Close the PAM session on logout (`pam_close_session` + `pam_setcred`
    /// delete).
    fn close_session(&mut self) -> AuthResult<()>;
}

impl fmt::Display for AuthedUser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.username)
    }
}

pub mod mock;

#[cfg(feature = "system-pam")]
pub mod system;
