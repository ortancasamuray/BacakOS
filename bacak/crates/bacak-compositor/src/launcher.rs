//! Detached process launch for pinned-app dock tiles.
//!
//! Resolving an app id to its command lives in [`crate::icons`]
//! (pure, unit-tested). This module owns only the security-sensitive
//! part: actually spawning that command, fully detached from the
//! compositor.
//!
//! "Detached" means two things:
//!
//! 1. **No zombies.** [`init`] sets `SIGCHLD` to `SIG_IGN`, so the
//!    kernel auto-reaps exited children. The compositor never `wait()`s
//!    on anything, so there's nothing to disturb — this is the
//!    simplest correct reaping strategy for a process that only ever
//!    fire-and-forgets.
//! 2. **Own session.** Each child gets [`libc::setsid`] in a
//!    `pre_exec` hook, so it isn't in the compositor's process group
//!    and a later compositor exit / TTY signal doesn't take it down.
//!
//! Spawn failure is logged and swallowed — a missing binary in the
//! user's pin list must never crash the compositor.
//!
//! Runtime-gated: `libc` and the live process plumbing are only pulled
//! in by the backends.

#![cfg(feature = "runtime")]

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

/// Install the auto-reap disposition. Idempotent; call once per
/// process at backend startup (alongside [`crate::signals::install`]).
pub fn init() {
    // SAFETY: `libc::signal` with `SIG_IGN` is async-signal-safe and
    // sets a process-wide disposition. The compositor manages no child
    // processes itself, so auto-reaping can't drop a status it wanted.
    unsafe {
        libc::signal(libc::SIGCHLD, libc::SIG_IGN);
    }
}

/// Spawn the session's startup client named in `$BACAK_STARTUP`, if set, after
/// the Wayland socket is bound and `WAYLAND_DISPLAY` exported. This is the
/// launch contract a session manager (e.g. the Bacak Display Manager) uses to
/// tell the compositor which client to host — the greeter, or the user's shell.
/// The value is a command line split on whitespace; no-op when unset/empty.
pub fn spawn_startup() {
    let Ok(cmd) = std::env::var("BACAK_STARTUP") else {
        return;
    };
    let argv: Vec<String> = cmd.split_whitespace().map(str::to_owned).collect();
    if argv.is_empty() {
        return;
    }
    tracing::info!(command = %cmd, "spawning session startup client ($BACAK_STARTUP)");
    if !spawn_detached(&argv) {
        tracing::error!(command = %cmd, "failed to spawn $BACAK_STARTUP startup client");
    }
}

/// Spawn `argv` detached. `argv[0]` is the program, the rest its
/// arguments (already field-code-stripped by
/// [`crate::icons::resolve_exec`]). Returns whether the child started.
/// Never panics; an empty argv or a spawn error just yields `false`.
pub fn spawn_detached(argv: &[String]) -> bool {
    let Some((prog, args)) = argv.split_first() else {
        return false;
    };
    let mut cmd = Command::new(prog);
    cmd.args(args).stdin(Stdio::null());
    // Pure Wayland session, no X server. Steer toolkits that default to
    // X11 toward the Wayland backend so they don't die with "cannot open
    // display" / "could not load the Qt platform plugin xcb". GTK and
    // Firefox already auto-detect Wayland from $WAYLAND_DISPLAY; these
    // cover Qt, SDL, Clutter, Electron and Mozilla explicitly. Qt keeps an
    // `xcb` fallback for builds without the wayland plugin.
    cmd.env("QT_QPA_PLATFORM", "wayland;xcb")
        // Qt's xcb (XWayland) backend uploads images via the MIT-SHM
        // `ShmPutImage` request, which our XWayland answers with BadMatch —
        // crashing Qt apps that fall back to xcb because they bundle a Qt
        // without the wayland plugin (e.g. OnlyOffice DesktopEditors). Disable
        // Qt's shared-memory image path. It's an xcb-backend-only variable, so
        // native-Wayland / non-Qt apps ignore it; the only cost is Qt-on-xcb
        // using a plain `PutImage` instead of SHM.
        .env("QT_X11_NO_MITSHM", "1")
        .env("SDL_VIDEODRIVER", "wayland")
        .env("CLUTTER_BACKEND", "wayland")
        .env("MOZ_ENABLE_WAYLAND", "1")
        .env("ELECTRON_OZONE_PLATFORM_HINT", "auto")
        .env("XDG_SESSION_TYPE", "wayland")
        // QtWebEngine's bundled Chromium aborts at startup
        // ("credentials.cc Check failed") when it can't set up its
        // namespace sandbox — common on systems without unprivileged
        // user namespaces. Disable it so QtWebEngine apps (e.g.
        // OpenBoard) start. Scoped to QtWebEngine only; trades that
        // sandbox layer for being able to run the app at all.
        .env("QTWEBENGINE_DISABLE_SANDBOX", "1");
    // (The `SAL_DISABLESKIA=1` workaround for LibreOffice's black rendering was
    // removed once dma-buf import landed — see the zwp_linux_dmabuf_v1 support;
    // LO's Skia backend now renders natively via dma-buf.)
    // Pure Wayland session, no X server: Chromium/Electron default to the
    // X11 Ozone backend and die with "Missing X server or $DISPLAY".
    // Steer the known browser family to Wayland. (GTK, Qt and Firefox
    // auto-detect Wayland from $WAYLAND_DISPLAY, so they need nothing.)
    let base = std::path::Path::new(prog)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if matches!(
        base,
        "chromium"
            | "chromium-browser"
            | "chrome"
            | "google-chrome"
            | "google-chrome-stable"
            | "brave"
            | "brave-browser"
            | "microsoft-edge"
            | "vivaldi-stable"
            | "opera"
    ) {
        cmd.arg("--ozone-platform=wayland");
    }
    // OnlyOffice DesktopEditors bundles CEF/Chromium for its document UI, and
    // CEF's *GPU process* initialises independently of XWayland's GLAMOR — it
    // still SIGSEGVs under our XWayland (in `CAscApplicationManager::
    // OnDestroyWindow` as CEF tears the half-started browser down) even with
    // linux-dmabuf v4 feedback in place (verified 2026-06-02: same crash). So a
    // GPU-side fix at the compositor doesn't reach Chromium's GPU process;
    // forcing Mesa software rendering for this app is the working escape hatch.
    // Scoped per-app so everything else keeps hardware acceleration. (The dmabuf
    // v4 feedback in udev_runtime.rs is kept — it still helps other XWayland GPU
    // apps; it just doesn't fix Chromium's separate GPU process.)
    // Match defensively: the app ships several names for the same binary
    // (`onlyoffice-desktopeditors`, the `desktopeditors` symlink, the real
    // `DesktopEditors`), and the .desktop may use any of them. Case-insensitive
    // substring so we can't miss the one the launcher actually resolved.
    let bl = base.to_ascii_lowercase();
    if bl.contains("desktopeditors") || bl.contains("onlyoffice") {
        // OnlyOffice bundles a Qt with BROKEN Wayland support. The global
        // `QT_QPA_PLATFORM=wayland;xcb` makes it load its wayland QPA plugin
        // first; even though it then fails over to xcb, having loaded the broken
        // plugin corrupts it and it SIGSEGVs during CEF teardown
        // (`CAscApplicationManager::OnDestroyWindow`). KWin hit the exact same
        // class of crash and fixed it by no longer forcing the wayland platform
        // (KDE bug 450000). So **force plain xcb** (XWayland) for this app — the
        // known-good path — overriding the global. `LIBGL_ALWAYS_SOFTWARE` stays
        // for CEF's GPU process.
        cmd.env("QT_QPA_PLATFORM", "xcb");
        cmd.env("LIBGL_ALWAYS_SOFTWARE", "1");
    }
    // Capture stdout+stderr to a per-app logfile so launch failures and
    // early crashes are diagnosable (previously both went to /dev/null,
    // hiding every error). Best-effort: fall back to null if the file
    // can't be created. Path: `$TMPDIR/bacak-launch-<prog>.log`.
    let slug: String = std::path::Path::new(prog)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("app")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
        .collect();
    let log_path = std::env::temp_dir().join(format!("bacak-launch-{slug}.log"));
    match std::fs::File::create(&log_path) {
        Ok(file) => {
            let err = file.try_clone().ok();
            cmd.stdout(Stdio::from(file));
            match err {
                Some(e) => { cmd.stderr(Stdio::from(e)); }
                None => { cmd.stderr(Stdio::null()); }
            }
        }
        Err(_) => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    // SAFETY: `setsid` is async-signal-safe and the only call made
    // between fork and exec; it just moves the child into a new
    // session so it outlives the compositor.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(?e, prog, "pinned-app launch failed");
            false
        }
    }
}

/// Push the named env vars (already `set_var`'d on our own process) into the
/// **D-Bus session activation environment** and the **systemd `--user`
/// manager**, so apps we *don't* launch ourselves still see them.
///
/// Why this is needed: [`spawn_detached`] children inherit the compositor's
/// env, so `DISPLAY`/`WAYLAND_DISPLAY` reach anything launched from the dock or
/// apps menu. But D-Bus-activated and systemd-user services are started by
/// those managers with *their own* recorded environment — not ours. The classic
/// victim is **mate-terminal**, whose window is served by a D-Bus-activated
/// factory: a shell opened in it has no `DISPLAY`, so any X11/XWayland app run
/// from that shell (e.g. `onlyoffice-desktopeditors`, which we force onto xcb)
/// dies with *"Could not connect to an X display"*. `gnome-terminal` and other
/// activated services hit the same gap.
///
/// `dbus-update-activation-environment --systemd` updates both the D-Bus
/// activation env and (via `systemctl --user import-environment`) the systemd
/// user manager in one call. Best-effort: a missing tool or non-zero exit is
/// logged and swallowed — it must never be fatal. Call once `WAYLAND_DISPLAY`
/// is bound, and again once `DISPLAY` is known (XWayland ready).
pub fn export_to_session(vars: &[&str]) {
    if vars.is_empty() {
        return;
    }
    let mut cmd = Command::new("dbus-update-activation-environment");
    cmd.arg("--systemd")
        .args(vars)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match cmd.status() {
        Ok(s) if s.success() => {
            tracing::info!(?vars, "exported env to D-Bus/systemd activation environment");
        }
        Ok(s) => tracing::warn!(?vars, code = ?s.code(),
            "dbus-update-activation-environment exited non-zero"),
        Err(e) => tracing::warn!(?e,
            "dbus-update-activation-environment unavailable; D-Bus-activated apps \
             (e.g. mate-terminal) may not see DISPLAY/WAYLAND_DISPLAY"),
    }
}
