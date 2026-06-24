//! `bacak-display-manager` — the privileged BDM daemon.
//!
//! Responsibilities (and *only* these — everything else is delegated):
//!  1. Read `/etc/bacak-display-manager.conf`.
//!  2. Claim the seat / VT and prepare `/run/bacak-display-manager`.
//!  3. Either run the **autologin** fast-path, or launch the unprivileged
//!     **greeter** on the Bacak compositor and serve it over a UNIX socket.
//!  4. Drive PAM on the greeter's behalf (it never sees PAM directly).
//!  5. On success, fork → drop privileges → hand off to
//!     `bacak-session-launcher`, which execs the chosen session.
//!  6. On logout, reap the session and return to the greeter.
//!
//! This file wires the pieces together; the logic lives in the submodules.

mod auth;
mod ipc;
mod launch;
mod power;
mod seat;

use bacak_common::config::Config;
use std::process::ExitCode;

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .init();

    if let Err(e) = run() {
        log::error!("fatal: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    require_root()?;

    let config = Config::load_system()?;
    log::info!(
        "BDM starting (seat={}, compositor={})",
        config.daemon.seat,
        config.daemon.compositor.display()
    );

    seat::prepare_runtime_dir(&config)?;

    // Tear the greeter compositor down on SIGTERM/SIGINT (e.g. `systemctl
    // restart`). Otherwise the daemon, blocked in `serve()`, dies without
    // killing the compositor, which lingers holding the DRM master and makes the
    // next daemon start crash-loop (flashing black screen).
    launch::install_signal_handlers();

    #[cfg(not(feature = "system-pam"))]
    log::warn!(
        "built WITHOUT `system-pam`: authentication uses the MOCK backend. \
         Do not deploy this build."
    );

    if config.autologin.enabled {
        if let Some(user) = config.autologin.user.clone() {
            log::info!("autologin enabled for '{user}'");
            return launch::run_autologin(&config, &user);
        }
        log::warn!("autologin enabled but no user set; falling back to greeter");
    }

    // Main loop: show greeter → authenticate → start session → wait → repeat.
    let mut backoff = launch::Backoff::new();
    loop {
        let mut server = ipc::GreeterServer::bind(&config)?;
        let started = std::time::Instant::now();
        let mut greeter = launch::spawn_greeter(&config)?;
        let outcome = server.serve(&config, &mut greeter)?;
        launch::reap(greeter);

        match outcome {
            ipc::Outcome::StartSession { user, session, env } => {
                // Open the user's logind session in the child's pre_exec via the
                // `bacak-display-manager` PAM service (same verified mechanism as
                // autologin), so the user's compositor becomes the session leader
                // and is granted the seat. Auth already happened; this opens the
                // session only (acct_mgmt + setcred + open_session).
                launch::run_session(&config, &user, &session, env, Some("bacak-display-manager"))?;
                log::info!("session for '{user}' ended; returning to greeter");
                // A completed login cycle is a healthy run.
                backoff.reset();
            }
            ipc::Outcome::PowerActionTaken => {
                log::info!("power action handled; daemon exiting to systemd");
                return Ok(());
            }
            ipc::Outcome::GreeterExited => {
                log::warn!("greeter exited unexpectedly; restarting");
                // Back off if the greeter is crash-looping (e.g. no compositor).
                backoff.note_exit(started);
            }
        }
    }
}

fn require_root() -> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: getuid is always safe; it has no preconditions.
    let uid = unsafe { libc_getuid() };
    if uid != 0 {
        return Err("bacak-display-manager must run as root (uid 0)".into());
    }
    Ok(())
}

// Avoid a libc dependency for a single call.
extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}
