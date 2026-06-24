//! Authentication orchestration: the bridge between the greeter's IPC frames
//! and the PAM stack.
//!
//! The greeter sends `StartAuth{username}`. We run a PAM conversation, relaying
//! each PAM prompt to the greeter as an `AuthPrompt`/`AuthInfo` and feeding the
//! greeter's `AuthResponse` secrets back to PAM. The secret never leaves this
//! process except to PAM, and is dropped as soon as PAM consumes it.
//!
//! Two backends, both driven through the same synchronous `Conversation`:
//! * default build → a dev-only authenticator (accepts password `bacak`).
//! * `--features system-pam` → real libpam via `bacak_pam::system`, whose
//!   conversation callback runs synchronously on this thread, so the relay
//!   below is used directly with no worker thread or channels.

use crate::ipc::{self, Conn, Outcome};
use bacak_common::config::{Config, Language};
use bacak_common::ipc::{Response, Secret};
use bacak_common::sessions;
use bacak_pam::{AuthError, AuthResult, Authenticator, Conversation, Prompt};

/// Per-greeter authentication state, reset between attempts.
pub struct AuthSlot {
    authed_user: Option<String>,
    authenticator: Option<Box<dyn Authenticator>>,
    default_session: String,
    language: Language,
}

impl AuthSlot {
    pub fn new(config: &Config) -> Self {
        Self {
            authed_user: None,
            authenticator: None,
            default_session: config.sessions.default_session.clone(),
            language: config.greeter.language,
        }
    }

    pub fn reset(&mut self) {
        if let Some(mut a) = self.authenticator.take() {
            let _ = a.close_session();
        }
        self.authed_user = None;
    }

    /// Drive a full authentication attempt for `username`, reporting the result
    /// to the greeter over `conn`.
    pub fn begin(
        &mut self,
        username: &str,
        conn: &mut Conn,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.reset();

        let mut authenticator = build_authenticator();
        let result = {
            let mut conv = IpcConversation { conn };
            authenticator.authenticate(username, &mut conv)
        };

        match result {
            Ok(user) => {
                log::info!("authentication succeeded for '{}'", user.username);
                self.authed_user = Some(user.username);
                self.authenticator = Some(authenticator);
                conn.write_response(&Response::AuthResult {
                    success: true,
                    message: None,
                })?;
            }
            Err(e) => {
                log::info!("authentication failed for '{username}': {e}");
                self.authed_user = None;
                conn.write_response(&Response::AuthResult {
                    success: false,
                    message: Some(display_auth_error(&e, self.language)),
                })?;
            }
        }
        Ok(())
    }

    /// After a successful auth, resolve the chosen session, open the PAM
    /// session, and produce the launch [`Outcome`]. Returns `Ok(None)` if not
    /// authenticated or the session id is unknown (error already sent).
    pub fn finish_session(
        &mut self,
        session_id: &str,
        config: &Config,
        conn: &mut Conn,
    ) -> Result<Option<Outcome>, Box<dyn std::error::Error>> {
        let Some(user) = self.authed_user.clone() else {
            conn.write_response(&Response::Error {
                message: "not authenticated".into(),
            })?;
            return Ok(None);
        };

        let chosen = if session_id.is_empty() {
            &self.default_session
        } else {
            session_id
        };
        let sessions = sessions::discover(
            &config.sessions.wayland_dirs,
            &config.sessions.xsession_dirs,
        );
        let Some(session) = sessions.into_iter().find(|s| s.id == chosen) else {
            conn.write_response(&Response::Error {
                message: format!("unknown session '{chosen}'"),
            })?;
            return Ok(None);
        };

        // The user's logind/PAM session is opened in the session child's
        // pre_exec (see `launch::run_session` with `bacak-display-manager`), so
        // the user's compositor is the session leader and is granted the seat.
        // We deliberately do NOT open it here (that would make the daemon the
        // leader and create a duplicate session). The authenticator is kept only
        // to release the auth handle on reset.
        let env = Vec::new();

        // Remember selections for next time, honouring the admin's config.
        if config.greeter.remember_last_user {
            ipc::write_last("user", &user);
        }
        if config.greeter.remember_last_session {
            ipc::write_last("session", &session.id);
        }

        conn.write_response(&Response::SessionStarting)?;
        log::info!("starting session '{}' for '{}'", session.id, user);
        Ok(Some(Outcome::StartSession { user, session, env }))
    }
}

/// Conversation that relays prompts to / answers from the greeter over IPC.
struct IpcConversation<'a> {
    conn: &'a mut Conn,
}

impl<'a> Conversation for IpcConversation<'a> {
    fn handle(&mut self, prompt: &Prompt) -> AuthResult<Option<Secret>> {
        use bacak_common::ipc::Request;
        match prompt {
            Prompt::SecretInput(msg) | Prompt::VisibleInput(msg) => {
                let echo = matches!(prompt, Prompt::VisibleInput(_));
                self.conn
                    .write_response(&Response::AuthPrompt {
                        message: msg.clone(),
                        echo,
                    })
                    .map_err(|_| AuthError::Aborted)?;
                match self.conn.read_request().map_err(|_| AuthError::Aborted)? {
                    // Hand the `Secret` straight through to the PAM layer — no
                    // intermediate plaintext `String` copy. It is scrubbed on drop.
                    Some(Request::AuthResponse { secret }) => Ok(Some(secret)),
                    Some(Request::CancelAuth) | None => Err(AuthError::Aborted),
                    Some(_) => Err(AuthError::Aborted),
                }
            }
            Prompt::Info(msg) => {
                let _ = self.conn.write_response(&Response::AuthInfo {
                    message: msg.clone(),
                });
                Ok(None)
            }
            Prompt::Error(msg) => {
                let _ = self.conn.write_response(&Response::AuthInfo {
                    message: msg.clone(),
                });
                Ok(None)
            }
        }
    }
}

/// Failure messages are deliberately generic to avoid a user-enumeration or
/// password-content oracle. Only account-state issues get specific text.
fn display_auth_error(e: &AuthError, lang: Language) -> String {
    let t = lang.ui();
    match e {
        AuthError::CredentialsExpired => t.password_expired.into(),
        AuthError::AccountUnavailable(_) => t.account_unavailable.into(),
        _ => t.auth_failed.into(),
    }
}

#[cfg(not(feature = "system-pam"))]
fn build_authenticator() -> Box<dyn Authenticator> {
    // DEV ONLY: accepts the password "bacak" for whatever username is entered,
    // so the greeter UI can be exercised without a real PAM stack. The daemon
    // logs a loud warning at startup for non-system-pam builds.
    Box::new(MockSeeded)
}

#[cfg(not(feature = "system-pam"))]
struct MockSeeded;

#[cfg(not(feature = "system-pam"))]
impl Authenticator for MockSeeded {
    fn authenticate(
        &mut self,
        username: &str,
        conv: &mut dyn Conversation,
    ) -> AuthResult<bacak_pam::AuthedUser> {
        let answer = conv
            .handle(&Prompt::SecretInput("Password: ".into()))?
            .ok_or(AuthError::NoAnswer)?;
        if answer.expose() == "bacak" {
            Ok(bacak_pam::AuthedUser {
                username: username.to_string(),
            })
        } else {
            conv.handle(&Prompt::Error("Authentication failed".into()))?;
            Err(AuthError::AuthFailed)
        }
    }
    fn open_session(&mut self) -> AuthResult<Vec<(String, String)>> {
        Ok(Vec::new())
    }
    fn close_session(&mut self) -> AuthResult<()> {
        Ok(())
    }
}

#[cfg(feature = "system-pam")]
fn build_authenticator() -> Box<dyn Authenticator> {
    // Real PAM. `tty` should be the seat VT; resolved from the environment the
    // service sets (`XDG_VTNR`). See systemd/bacak-display-manager.service.
    let tty = std::env::var("XDG_VTNR")
        .map(|n| format!("tty{n}"))
        .unwrap_or_else(|_| "tty1".into());
    Box::new(bacak_pam::system::SystemAuthenticator::new(tty))
}
