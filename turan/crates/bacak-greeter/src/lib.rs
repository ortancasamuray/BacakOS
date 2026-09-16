//! `bacak_greeter` — the greeter's privilege-free client core.
//!
//! This library knows how to talk to the daemon over the IPC socket and exposes
//! a small, UI-agnostic [`GreeterClient`]. Any frontend — the reference TTY
//! frontend in `main.rs`, or the Wayland/iced GUI behind the `gui` feature —
//! drives the same client. Keeping the protocol logic here (and out of the GUI)
//! means it can be unit-tested without a display.

use bacak_common::ipc::{GreeterPolicy, Request, Response, Secret, PROTOCOL_VERSION};
use bacak_common::power::PowerAction;
use bacak_common::sessions::Session;
use bacak_common::users::User;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

/// Environment variable the daemon sets pointing at the IPC socket.
pub const SOCKET_ENV: &str = "BDM_GREETER_SOCKET";

#[derive(Debug)]
pub enum GreeterError {
    Io(std::io::Error),
    Protocol(String),
    Closed,
}

impl std::fmt::Display for GreeterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GreeterError::Io(e) => write!(f, "io: {e}"),
            GreeterError::Protocol(m) => write!(f, "protocol: {m}"),
            GreeterError::Closed => write!(f, "connection closed by daemon"),
        }
    }
}
impl std::error::Error for GreeterError {}
impl From<std::io::Error> for GreeterError {
    fn from(e: std::io::Error) -> Self {
        GreeterError::Io(e)
    }
}

type Result<T> = std::result::Result<T, GreeterError>;

/// Data the daemon hands back at connect time.
#[derive(Debug, Clone)]
pub struct Welcome {
    pub touchscreen: bool,
    pub last_user: Option<String>,
    pub last_session: Option<String>,
    /// Admin policy the frontend must honour (from the daemon's config).
    pub policy: GreeterPolicy,
}

/// One step the frontend must act on while authenticating.
#[derive(Debug, Clone)]
pub enum AuthStep {
    /// Prompt the user; `echo=false` means mask the input (password/PIN).
    Prompt { message: String, echo: bool },
    /// Display informational/error text and keep going.
    Info { message: String },
    /// Terminal result.
    Done {
        success: bool,
        message: Option<String>,
    },
}

/// Connection to the daemon.
pub struct GreeterClient {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl GreeterClient {
    /// Connect using the socket path in `BDM_GREETER_SOCKET`.
    pub fn connect_from_env() -> Result<(Self, Welcome)> {
        let path = std::env::var(SOCKET_ENV)
            .map_err(|_| GreeterError::Protocol(format!("{SOCKET_ENV} not set")))?;
        Self::connect(path)
    }

    pub fn connect(path: impl AsRef<Path>) -> Result<(Self, Welcome)> {
        let stream = UnixStream::connect(path.as_ref())?;
        let mut client = Self {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
        };
        client.send(&Request::hello())?;
        match client.recv()? {
            Response::Welcome {
                protocol,
                touchscreen,
                last_user,
                last_session,
                policy,
            } => {
                if protocol != PROTOCOL_VERSION {
                    return Err(GreeterError::Protocol(format!(
                        "version mismatch: daemon {protocol}, greeter {PROTOCOL_VERSION}"
                    )));
                }
                Ok((
                    client,
                    Welcome {
                        touchscreen,
                        last_user,
                        last_session,
                        policy,
                    },
                ))
            }
            other => Err(GreeterError::Protocol(format!(
                "expected Welcome, got {other:?}"
            ))),
        }
    }

    pub fn list_users(&mut self) -> Result<Vec<User>> {
        self.send(&Request::ListUsers)?;
        match self.recv()? {
            Response::Users { users } => Ok(users),
            other => Err(GreeterError::Protocol(format!(
                "expected Users, got {other:?}"
            ))),
        }
    }

    pub fn list_sessions(&mut self) -> Result<Vec<Session>> {
        self.send(&Request::ListSessions)?;
        match self.recv()? {
            Response::Sessions { sessions } => Ok(sessions),
            other => Err(GreeterError::Protocol(format!(
                "expected Sessions, got {other:?}"
            ))),
        }
    }

    /// Begin authentication. The frontend then loops calling [`Self::next_step`]
    /// and answering prompts with [`Self::answer`] until a `Done` step.
    pub fn start_auth(&mut self, username: &str) -> Result<AuthStep> {
        self.send(&Request::StartAuth {
            username: username.to_string(),
        })?;
        self.next_step()
    }

    /// Read the next authentication step from the daemon.
    pub fn next_step(&mut self) -> Result<AuthStep> {
        match self.recv()? {
            Response::AuthPrompt { message, echo } => Ok(AuthStep::Prompt { message, echo }),
            Response::AuthInfo { message } => Ok(AuthStep::Info { message }),
            Response::AuthResult { success, message } => Ok(AuthStep::Done { success, message }),
            Response::Error { message } => Ok(AuthStep::Done {
                success: false,
                message: Some(message),
            }),
            other => Err(GreeterError::Protocol(format!(
                "unexpected during auth: {other:?}"
            ))),
        }
    }

    /// Answer a `Prompt` step with a secret, then return the next step.
    /// Answer a `Prompt` step. Accepts anything convertible into a [`Secret`]
    /// (`String`/`&str` for the simple frontends, or a `Secret` the GUI already
    /// holds), so the plaintext stays wrapped end to end.
    pub fn answer(&mut self, secret: impl Into<Secret>) -> Result<AuthStep> {
        self.send(&Request::AuthResponse {
            secret: secret.into(),
        })?;
        self.next_step()
    }

    pub fn cancel_auth(&mut self) -> Result<()> {
        self.send(&Request::CancelAuth)
    }

    /// Launch the chosen session. On success the daemon replies `SessionStarting`
    /// and the greeter should exit so the compositor can hand over.
    pub fn start_session(&mut self, session_id: &str) -> Result<()> {
        self.send(&Request::StartSession {
            session_id: session_id.to_string(),
        })?;
        match self.recv()? {
            Response::SessionStarting => Ok(()),
            Response::Error { message } => Err(GreeterError::Protocol(message)),
            other => Err(GreeterError::Protocol(format!(
                "expected SessionStarting, got {other:?}"
            ))),
        }
    }

    pub fn power(&mut self, action: PowerAction) -> Result<()> {
        self.send(&Request::Power { action })
    }

    fn send(&mut self, req: &Request) -> Result<()> {
        let mut buf = serde_json::to_vec(req).map_err(|e| GreeterError::Protocol(e.to_string()))?;
        buf.push(b'\n');
        self.writer.write_all(&buf)?;
        self.writer.flush()?;
        Ok(())
    }

    fn recv(&mut self) -> Result<Response> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(GreeterError::Closed);
        }
        serde_json::from_str(line.trim_end()).map_err(|e| GreeterError::Protocol(e.to_string()))
    }
}
