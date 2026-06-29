//! Process launching with privilege separation.
//!
//! Two kinds of children are spawned, both with privileges dropped *before*
//! `execve`:
//!   * the **greeter**, dropped to the unprivileged `greeter_user`, rendered on
//!     the Bacak compositor;
//!   * the **user session**, dropped to the authenticated user and handed to
//!     `bacak-session-launcher`.
//!
//! Privilege-drop order matters and is done entirely inside one `pre_exec`
//! closure, in order: `setsid` → `initgroups` → (greeter only) PAM
//! `open_session` → `setgid` → `setuid`. We do NOT use std's `Command::uid`/
//! `gid`, because std applies those *before* the `pre_exec` closure runs — that
//! would drop CAP_SETGID first and make `initgroups`/PAM fail with EPERM (and
//! could leave residual root groups). Dropping uid/gid last, ourselves, fixes
//! both. See `child_setsid_initgroups` / `child_drop_privileges`.

use bacak_common::config::Config;
use std::ffi::CString;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use crate::seat;

/// PGID of the live greeter compositor (0 when none). The compositor `setsid`s
/// in its `pre_exec`, so it leads its own session/group and its PGID equals its
/// PID. We record it so the SIGTERM/SIGINT handler can tear the whole group
/// down on `systemctl stop`/`restart`. Without this the daemon dies on SIGTERM
/// while blocked in `serve()` and leaves the compositor orphaned — a lingering
/// compositor keeps the DRM master, so the next daemon start can't acquire it,
/// crash-loops, and the screen flashes black.
static COMPOSITOR_PGID: AtomicI32 = AtomicI32::new(0);

/// SIGTERM/SIGINT handler. Strictly async-signal-safe: an atomic load, a
/// `killpg`, and `_exit` — nothing that allocates or takes a lock.
extern "C" fn terminate_handler(_sig: i32) {
    let pgid = COMPOSITOR_PGID.load(Ordering::SeqCst);
    // `pgid` is the compositor's own (setsid) group, never the daemon's, so this
    // can only kill the compositor + its clients. SIGKILL: we are exiting and
    // must guarantee the seat / DRM master is released.
    if pgid > 1 {
        unsafe {
            nix::libc::killpg(pgid, nix::libc::SIGKILL);
        }
    }
    unsafe {
        nix::libc::_exit(0);
    }
}

/// Install the compositor-teardown handler for SIGTERM and SIGINT. Call once at
/// startup, before the main loop spawns any compositor.
pub fn install_signal_handlers() {
    use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
    let action = SigAction::new(
        SigHandler::Handler(terminate_handler),
        SaFlags::empty(),
        SigSet::empty(),
    );
    // SAFETY: `terminate_handler` is async-signal-safe (see its doc comment).
    unsafe {
        let _ = sigaction(Signal::SIGTERM, &action);
        let _ = sigaction(Signal::SIGINT, &action);
    }
}

/// Capped exponential backoff for supervised children (the greeter, autologin
/// sessions) so a child that dies immediately and repeatedly can't spin the CPU
/// or flood the journal. A child that ran long enough to be considered healthy
/// resets the backoff.
pub struct Backoff {
    fails: u32,
}

impl Backoff {
    /// A child alive at least this long is treated as a healthy run.
    const HEALTHY: Duration = Duration::from_secs(5);
    /// Upper bound on the sleep between rapid restarts.
    const MAX: Duration = Duration::from_secs(30);

    pub fn new() -> Self {
        Self { fails: 0 }
    }

    /// Clear the failure streak after a successful, healthy cycle.
    pub fn reset(&mut self) {
        self.fails = 0;
    }

    /// Call once a supervised child has exited, passing the [`Instant`] it was
    /// started. Sleeps with capped exponential backoff when the child died too
    /// quickly; otherwise resets the streak and returns immediately.
    pub fn note_exit(&mut self, started: Instant) {
        let alive = started.elapsed();
        if let Some(delay) = self.next_delay(alive) {
            log::warn!(
                "supervised child exited after {alive:?} (<{:?}); {} rapid restart(s), backing off {delay:?}",
                Self::HEALTHY,
                self.fails
            );
            std::thread::sleep(delay);
        }
    }

    /// Pure backoff policy: update the failure streak for a child that lived
    /// `alive`, returning the delay to wait (or `None` when it was healthy).
    /// Split out from [`Self::note_exit`] so it is testable without sleeping.
    fn next_delay(&mut self, alive: Duration) -> Option<Duration> {
        if alive >= Self::HEALTHY {
            self.fails = 0;
            return None;
        }
        self.fails += 1;
        // 2, 4, 8, 16 s, then held at the cap.
        let secs = 2u64.saturating_pow(self.fails.min(4));
        Some(Duration::from_secs(secs).min(Self::MAX))
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

/// New session + supplementary groups, run inside the forked child **while
/// still root**. `setsid` is tolerant of EPERM (already a group leader).
///
/// IMPORTANT: this must run before [`child_drop_privileges`]. We deliberately do
/// NOT use std's `Command::uid`/`gid`, because std applies those *before* the
/// `pre_exec` closure — which would drop CAP_SETGID and make `initgroups`/PAM
/// fail with EPERM. By doing every privileged step here, in order, we keep root
/// until the final drop.
fn child_setsid_initgroups(user: &std::ffi::CStr, gid: u32) -> std::io::Result<()> {
    match nix::unistd::setsid() {
        Ok(_) => {}
        Err(nix::errno::Errno::EPERM) => {}
        Err(e) => return Err(std::io::Error::from(e)),
    }
    nix::unistd::initgroups(user, nix::unistd::Gid::from_raw(gid)).map_err(std::io::Error::from)?;
    Ok(())
}

/// Drop to (`gid`, `uid`) — gid first, then uid — as the **last** step in the
/// child, after any root-only setup (initgroups, PAM session).
fn child_drop_privileges(uid: u32, gid: u32) -> std::io::Result<()> {
    nix::unistd::setgid(nix::unistd::Gid::from_raw(gid)).map_err(std::io::Error::from)?;
    nix::unistd::setuid(nix::unistd::Uid::from_raw(uid)).map_err(std::io::Error::from)?;
    Ok(())
}

/// Launch the unprivileged greeter on the compositor.
/// Returns `(child, greeter_uid)` so the caller can pass the uid to `reap`.
pub fn spawn_greeter(config: &Config) -> Result<(Child, u32), Box<dyn std::error::Error>> {
    let (uid, gid) = seat::greeter_ids(config)?;
    let runtime_dir = seat::xdg_runtime_dir(uid);
    let _ = std::fs::create_dir_all(&runtime_dir);
    let _ = std::fs::set_permissions(
        &runtime_dir,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    );
    let _ = nix::unistd::chown(
        runtime_dir.as_str(),
        Some(nix::unistd::Uid::from_raw(uid)),
        Some(nix::unistd::Gid::from_raw(gid)),
    );

    // The compositor hosts the greeter as its only client. Everything privileged
    // happens in ONE correctly-ordered pre_exec (see `child_setsid_initgroups` /
    // `child_drop_privileges`): setsid → initgroups → [PAM open_session] →
    // setgid → setuid. We do NOT use std's uid/gid (it drops privileges before
    // pre_exec runs, which would EPERM the root-only steps).
    let mut cmd = Command::new(&config.daemon.compositor);
    cmd.env_clear()
        .env("HOME", format!("/var/lib/{}", config.daemon.greeter_user))
        .env("USER", &config.daemon.greeter_user)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("XDG_SEAT", &config.daemon.seat)
        .env("XDG_SESSION_TYPE", "wayland")
        .env("BDM_GREETER_SOCKET", &config.daemon.ipc_socket)
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        // Surface the compositor's own info logs plus smithay's DRM/seat
        // warnings in the journal. Suppresses verbose GL spam from the
        // renderer. Harmless for compositors that ignore RUST_LOG.
        .env("RUST_LOG", "bacak_compositor=info,smithay::backend::drm=warn,smithay::backend::session=info")
        // Launch contract understood by both compositors: the real
        // `bacak-compositor` reads `$BACAK_STARTUP` and hosts it once its Wayland
        // socket is up; the weston prototype wrapper reads `--greeter`. Both
        // point at the greeter, so either compositor works unchanged.
        .env("BACAK_STARTUP", "/usr/bin/bacak-greeter")
        .arg("--greeter")
        .arg("/usr/bin/bacak-greeter");

    let user_c = CString::new(config.daemon.greeter_user.as_str())?;

    // Prepare the optional greeter logind session (PAM `bacak-greeter`). Opening
    // it in the child — which becomes the compositor — makes that process the
    // logind session leader, so weston/libseat is granted the seat.
    #[cfg(feature = "system-pam")]
    let pam_session: Option<(CString, CString, CString, Vec<CString>)> =
        if config.daemon.register_greeter_session {
            let vtnr = std::env::var("XDG_VTNR").unwrap_or_else(|_| "1".into());
            let putenv = [
                ("XDG_SESSION_CLASS", "greeter"),
                ("XDG_SESSION_TYPE", "wayland"),
                ("XDG_SEAT", config.daemon.seat.as_str()),
                ("XDG_VTNR", vtnr.as_str()),
            ]
            .into_iter()
            .map(|(k, v)| CString::new(format!("{k}={v}")).expect("env has no NUL"))
            .collect();
            log::info!(
                "registering logind 'greeter' session on {}",
                config.daemon.seat
            );
            Some((
                CString::new("bacak-greeter")?,
                CString::new(config.daemon.greeter_user.as_str())?,
                CString::new(format!("tty{vtnr}"))?,
                putenv,
            ))
        } else {
            None
        };

    // SAFETY: runs in the just-forked, single-threaded child before execve.
    unsafe {
        cmd.pre_exec(move || {
            child_setsid_initgroups(&user_c, gid)?;
            #[cfg(feature = "system-pam")]
            if let Some((service, user, tty, putenv)) = &pam_session {
                bacak_pam::system::open_session_preexec(service, user, tty, putenv).map_err(
                    |code| {
                        std::io::Error::other(format!(
                            "pam_open_session(bacak-greeter) failed: {code}"
                        ))
                    },
                )?;
            }
            child_drop_privileges(uid, gid)?;
            Ok(())
        });
    }

    log::info!("spawning greeter as uid={uid}");
    let child = cmd.spawn()?;
    // `setsid` in the pre_exec makes the compositor its own session/group leader,
    // so its PGID equals this PID. Record it so the signal handler and `reap`
    // can tear down the whole group, not just the compositor process.
    COMPOSITOR_PGID.store(child.id() as i32, Ordering::SeqCst);
    Ok((child, uid))
}

/// Tear down the greeter compositor and its entire session group — the greeter
/// client and anything else it spawned — so nothing is left holding the seat or
/// DRM master. The compositor `setsid`s into its own group, so `killpg` here
/// targets that group only, never the daemon's. `child.kill()` is a belt-and-
/// braces guarantee on the compositor PID itself.
///
/// After killing processes, `loginctl terminate-user` is called to formally
/// close the greeter's PAM/logind session. Without this, pam_open_session
/// stays open (pam_close_session is never called because the child was
/// SIGKILL'd), logind keeps the seat taken and the next session compositor
/// can't acquire DRM master.
pub fn reap(mut child: Child, greeter_uid: u32) {
    let pgid = nix::unistd::Pid::from_raw(child.id() as i32);
    use nix::sys::signal::{killpg, Signal};

    // Graceful first, so the compositor can release the DRM master cleanly.
    let _ = killpg(pgid, Signal::SIGTERM);
    for _ in 0..20 {
        if matches!(child.try_wait(), Ok(Some(_))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Then force anything still alive in the group.
    let _ = killpg(pgid, Signal::SIGKILL);
    let _ = child.kill();
    let _ = child.wait();
    COMPOSITOR_PGID.store(0, Ordering::SeqCst);

    // Force-close the greeter's logind session so DRM master is released
    // before the next compositor starts. pam_close_session is never called
    // (the compositor was killed, not gracefully shut down), so logind keeps
    // the session registered and holds DRM master on behalf of it.
    let uid_str = greeter_uid.to_string();
    let _ = std::process::Command::new("loginctl")
        .args(["terminate-user", &uid_str])
        .status();
}

/// Run a user session: drop to the user, set up the environment, exec the
/// session launcher, and wait for it to exit.
pub fn run_session(
    config: &Config,
    username: &str,
    session: &bacak_common::sessions::Session,
    pam_env: Vec<(String, String)>,
    logind: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let user = seat::lookup_user(username)?;
    let runtime_dir = seat::xdg_runtime_dir(user.uid);
    let _ = std::fs::create_dir_all(&runtime_dir);
    let _ = nix::unistd::chown(
        runtime_dir.as_str(),
        Some(nix::unistd::Uid::from_raw(user.uid)),
        Some(nix::unistd::Gid::from_raw(user.gid)),
    );

    let session_type = match session.kind {
        bacak_common::sessions::SessionType::Wayland => "wayland",
        bacak_common::sessions::SessionType::X11 => "x11",
    };

    let launcher = std::path::Path::new("/usr/bin/bacak-session-launcher");
    let mut cmd = Command::new(launcher);
    cmd.env_clear()
        .env("HOME", &user.home)
        .env("USER", &user.name)
        .env("LOGNAME", &user.name)
        .env("SHELL", &user.shell)
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("XDG_SEAT", &config.daemon.seat)
        .env("XDG_SESSION_CLASS", "user")
        .env("XDG_SESSION_TYPE", session_type)
        // The launcher receives the session command and desktop id to export.
        .env("BDM_SESSION_EXEC", &session.exec)
        .env("BDM_SESSION_ID", &session.id)
        .current_dir(&user.home);

    if let Some(names) = &session.desktop_names {
        cmd.env("XDG_CURRENT_DESKTOP", names);
    }
    for (k, v) in pam_env {
        cmd.env(k, v);
    }

    let user_c = CString::new(user.name.as_str())?;
    let uid = user.uid;
    let gid = user.gid;

    // Optionally open a logind session for the user in the child's pre_exec, so
    // the user's session process (and its compositor) is the logind session
    // leader and is granted the seat — same mechanism as the greeter. `logind`
    // is the PAM service (e.g. `bacak-autologin`); None keeps the old behaviour.
    #[cfg(feature = "system-pam")]
    let pam_session: Option<(CString, CString, CString, Vec<CString>)> = match logind {
        Some(service) => {
            let vtnr = std::env::var("XDG_VTNR").unwrap_or_else(|_| "1".into());
            let putenv = [
                ("XDG_SESSION_CLASS", "user"),
                ("XDG_SESSION_TYPE", session_type),
                ("XDG_SEAT", config.daemon.seat.as_str()),
                ("XDG_VTNR", vtnr.as_str()),
            ]
            .into_iter()
            .map(|(k, v)| CString::new(format!("{k}={v}")).expect("env has no NUL"))
            .collect();
            log::info!("opening logind '{service}' session for user '{username}'");
            Some((
                CString::new(service)?,
                CString::new(user.name.as_str())?,
                CString::new(format!("tty{vtnr}"))?,
                putenv,
            ))
        }
        None => None,
    };
    #[cfg(not(feature = "system-pam"))]
    let _ = logind;

    // SAFETY: runs in the just-forked, single-threaded child before execve.
    unsafe {
        cmd.pre_exec(move || {
            child_setsid_initgroups(&user_c, gid)?;
            #[cfg(feature = "system-pam")]
            if let Some((service, user, tty, putenv)) = &pam_session {
                bacak_pam::system::open_session_preexec(service, user, tty, putenv).map_err(
                    |code| std::io::Error::other(format!("pam_open_session (user) failed: {code}")),
                )?;
            }
            child_drop_privileges(uid, gid)?;
            Ok(())
        });
    }

    log::info!("session launcher starting for uid={uid}");
    let status = cmd.spawn()?.wait()?;
    log::info!("session exited: {status}");
    Ok(())
}

/// Autologin fast-path: no greeter, straight into the session.
pub fn run_autologin(config: &Config, username: &str) -> Result<(), Box<dyn std::error::Error>> {
    let sessions = bacak_common::sessions::discover(
        &config.sessions.wayland_dirs,
        &config.sessions.xsession_dirs,
    );
    let wanted = config
        .autologin
        .session
        .clone()
        .unwrap_or_else(|| config.sessions.default_session.clone());
    let session = sessions
        .into_iter()
        .find(|s| s.id == wanted)
        .ok_or_else(|| format!("autologin session '{wanted}' not found"))?;

    // Open the user's logind session via the `bacak-autologin` PAM service (no
    // prompt; pam_systemd registers it with logind) so the user's compositor is
    // granted the seat — the same mechanism as the greeter.
    let mut backoff = Backoff::new();
    loop {
        let started = Instant::now();
        run_session(
            config,
            username,
            &session,
            Vec::new(),
            Some("bacak-autologin"),
        )?;
        log::info!("autologin session ended; restarting");
        // Guard against a session that exits instantly looping forever.
        backoff.note_exit(started);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_run_clears_the_streak() {
        let mut b = Backoff::new();
        // A couple of fast crashes build up a streak…
        assert!(b.next_delay(Duration::from_millis(10)).is_some());
        assert!(b.next_delay(Duration::from_millis(10)).is_some());
        // …then a healthy run resets it (no delay) …
        assert_eq!(b.next_delay(Backoff::HEALTHY), None);
        // …so the next fast crash starts again at the shortest delay.
        assert_eq!(b.next_delay(Duration::ZERO), Some(Duration::from_secs(2)));
    }

    #[test]
    fn rapid_restarts_grow_then_cap() {
        let mut b = Backoff::new();
        let fast = Duration::from_millis(1);
        assert_eq!(b.next_delay(fast), Some(Duration::from_secs(2)));
        assert_eq!(b.next_delay(fast), Some(Duration::from_secs(4)));
        assert_eq!(b.next_delay(fast), Some(Duration::from_secs(8)));
        assert_eq!(b.next_delay(fast), Some(Duration::from_secs(16)));
        // Capped from here on.
        assert_eq!(b.next_delay(fast), Some(Duration::from_secs(16)));
        assert_eq!(b.next_delay(fast), Some(Duration::from_secs(16)));
    }
}
