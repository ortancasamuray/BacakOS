//! UNIX-socket IPC server that the greeter connects to.
//!
//! Framing is newline-delimited JSON. The server is single-greeter: BDM runs
//! exactly one greeter at a time, so a simple accept-one / serve-to-completion
//! loop is correct and keeps the privilege boundary easy to reason about.

use crate::auth::AuthSlot;
use crate::power;
use crate::seat;
use bacak_common::config::Config;
use bacak_common::ipc::{GreeterPolicy, Request, Response, PROTOCOL_VERSION};
use bacak_common::sessions;
use bacak_common::users::{PasswdProvider, UserProvider};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::Child;
use std::time::{Duration, Instant};

/// How long to wait for the greeter to connect before giving up. Generous so a
/// slow compositor bringing up its Wayland socket isn't a false positive; the
/// common failure (compositor dies on launch) is caught immediately via
/// [`Child::try_wait`], not this timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Poll interval while waiting for the greeter to connect.
const ACCEPT_POLL: Duration = Duration::from_millis(100);

/// What the greeter session resolved to, returned to the daemon main loop.
pub enum Outcome {
    StartSession {
        user: String,
        session: sessions::Session,
        env: Vec<(String, String)>,
    },
    PowerActionTaken,
    GreeterExited,
}

/// One framed connection to the greeter.
pub struct Conn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Conn {
    fn new(stream: UnixStream) -> std::io::Result<Self> {
        Ok(Self {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
        })
    }

    /// Read one request; `Ok(None)` on clean EOF (greeter disconnected).
    pub fn read_request(&mut self) -> std::io::Result<Option<Request>> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        match serde_json::from_str::<Request>(line.trim_end()) {
            Ok(req) => Ok(Some(req)),
            Err(e) => {
                log::warn!("malformed request dropped: {e}");
                // Treat as a soft error so a buggy greeter can't wedge us.
                Ok(Some(Request::CancelAuth))
            }
        }
    }

    pub fn write_response(&mut self, resp: &Response) -> std::io::Result<()> {
        let mut buf = serde_json::to_vec(resp)?;
        buf.push(b'\n');
        self.writer.write_all(&buf)?;
        self.writer.flush()
    }
}

pub struct GreeterServer {
    listener: UnixListener,
}

impl GreeterServer {
    pub fn bind(config: &Config) -> Result<Self, Box<dyn std::error::Error>> {
        let path = &config.daemon.ipc_socket;

        // Remove any stale socket from a previous (crashed) run before binding.
        let _ = std::fs::remove_file(path);

        // Bind under a restrictive umask so the socket is created 0660 from the
        // start. This closes the window between bind() and the chmod below where
        // an inherited-umask socket could briefly be group/world-accessible.
        let prev = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o117));
        let listener = UnixListener::bind(path);
        nix::sys::stat::umask(prev);
        let listener = listener?;

        // Belt and suspenders: only the greeter user (and root) may connect.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
        let (_uid, gid) = seat::greeter_ids(config)?;
        nix::unistd::chown(path.as_path(), None, Some(nix::unistd::Gid::from_raw(gid)))?;

        log::info!("listening for greeter on {}", path.display());
        Ok(Self { listener })
    }

    /// Wait for the greeter to connect, polling the compositor child so an early
    /// death (or no connection within [`CONNECT_TIMEOUT`]) doesn't hang us.
    /// Returns `Ok(None)` when the greeter won't be coming.
    fn accept_greeter(&self, greeter: &mut Child) -> std::io::Result<Option<UnixStream>> {
        self.listener.set_nonblocking(true)?;
        let start = Instant::now();
        let result = loop {
            match self.listener.accept() {
                Ok((stream, _addr)) => {
                    // accept(2) does not inherit O_NONBLOCK; make the intent
                    // explicit so the rest of the protocol uses blocking I/O.
                    stream.set_nonblocking(false)?;
                    break Some(stream);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if let Some(status) = greeter.try_wait()? {
                        log::warn!(
                            "greeter/compositor exited before connecting (status {status}); \
                             not waiting for a connection"
                        );
                        break None;
                    }
                    if start.elapsed() >= CONNECT_TIMEOUT {
                        log::warn!("greeter did not connect within {CONNECT_TIMEOUT:?}; giving up");
                        break None;
                    }
                    std::thread::sleep(ACCEPT_POLL);
                }
                Err(e) => return Err(e),
            }
        };
        // Leave the listener in blocking mode (defensive; this server only ever
        // accepts one greeter).
        self.listener.set_nonblocking(false)?;
        Ok(result)
    }

    /// Accept the greeter and serve until it triggers an outcome.
    ///
    /// `greeter` is the child hosting the greeter (the compositor). While we
    /// wait for it to connect we poll [`Child::try_wait`], so if the compositor
    /// dies before the greeter ever connects we return [`Outcome::GreeterExited`]
    /// instead of blocking forever in `accept()`.
    pub fn serve(
        &mut self,
        config: &Config,
        greeter: &mut Child,
    ) -> Result<Outcome, Box<dyn std::error::Error>> {
        let stream = match self.accept_greeter(greeter)? {
            Some(s) => s,
            None => return Ok(Outcome::GreeterExited),
        };
        let mut conn = Conn::new(stream)?;
        let mut auth = AuthSlot::new(config);

        // Handshake.
        match conn.read_request()? {
            Some(Request::Hello { protocol }) if protocol == PROTOCOL_VERSION => {
                let last_user = config
                    .greeter
                    .remember_last_user
                    .then(|| read_last("user"))
                    .flatten();
                let last_session = config
                    .greeter
                    .remember_last_session
                    .then(|| read_last("session"))
                    .flatten();
                conn.write_response(&Response::Welcome {
                    protocol: PROTOCOL_VERSION,
                    touchscreen: has_touchscreen(),
                    last_user,
                    last_session,
                    policy: GreeterPolicy {
                        greeter: config.greeter.clone(),
                        theme: config.theme.clone(),
                        power: config.power.clone(),
                        accessibility: config.accessibility.clone(),
                        keyboard: config.keyboard.clone(),
                    },
                })?;
            }
            other => {
                conn.write_response(&Response::Error {
                    message: format!("expected Hello v{PROTOCOL_VERSION}"),
                })?;
                log::warn!("bad handshake: {other:?}");
                return Ok(Outcome::GreeterExited);
            }
        }

        loop {
            let Some(req) = conn.read_request()? else {
                return Ok(Outcome::GreeterExited);
            };

            match req {
                Request::ListUsers => {
                    let users = PasswdProvider::default()
                        .login_capable_users()
                        .unwrap_or_default();
                    conn.write_response(&Response::Users { users })?;
                }
                Request::ListSessions => {
                    let sessions = sessions::discover(
                        &config.sessions.wayland_dirs,
                        &config.sessions.xsession_dirs,
                    );
                    conn.write_response(&Response::Sessions { sessions })?;
                }
                Request::StartAuth { username } => {
                    auth.begin(&username, &mut conn)?;
                }
                Request::AuthResponse { .. } | Request::CancelAuth => {
                    // Only meaningful inside an auth flow, which consumes its
                    // own frames. A stray one here just resets state.
                    auth.reset();
                }
                Request::StartSession { session_id } => {
                    match auth.finish_session(&session_id, config, &mut conn)? {
                        Some(outcome) => return Ok(outcome),
                        None => { /* error already reported to greeter */ }
                    }
                }
                Request::Power { action } => {
                    if power::handle(config, action, &mut conn)? {
                        return Ok(Outcome::PowerActionTaken);
                    }
                }
                Request::Hello { .. } => {
                    conn.write_response(&Response::Error {
                        message: "already greeted".into(),
                    })?;
                }
            }
        }
    }
}

/// Best-effort touchscreen detection via the kernel's input device table.
/// Drives whether the greeter shows the virtual keyboard by default.
fn has_touchscreen() -> bool {
    std::fs::read_to_string("/proc/bus/input/devices")
        .map(|t| {
            t.lines().any(|l| {
                let l = l.to_ascii_lowercase();
                l.contains("touchscreen") || l.contains("abs_mt")
            })
        })
        .unwrap_or(false)
}

/// State file: `/var/lib/bacak-display-manager/last-<kind>`.
fn read_last(kind: &str) -> Option<String> {
    let path = format!("/var/lib/bacak-display-manager/last-{kind}");
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn write_last(kind: &str, value: &str) {
    let dir = "/var/lib/bacak-display-manager";
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(format!("{dir}/last-{kind}"), value);
}
