//! Clean-shutdown signal handling.
//!
//! `SIGTERM` (the session manager / `kill`) and `SIGINT` (Ctrl-C in a
//! dev TTY) set a process-global flag. The backends' main loops poll
//! [`shutdown_requested`] each iteration and flush the session before
//! exiting, closing the periodic-save debounce window.
//!
//! The handler is async-signal-safe: it only does a relaxed atomic
//! store. Everything else happens back on the main loop.
//!
//! Runtime-gated: `libc` is an optional dependency pulled in only by
//! the live backends.

#![cfg(feature = "runtime")]

use std::sync::atomic::{AtomicBool, Ordering};

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn handler(_sig: libc::c_int) {
    SHUTDOWN.store(true, Ordering::Relaxed);
}

/// Install the SIGTERM/SIGINT handler. Idempotent — re-installing the
/// same handler is harmless. Best-effort: a failure here just means no
/// graceful flush, never a crash.
pub fn install() {
    // SAFETY: `libc::signal` with an `extern "C"` handler that only
    // performs a relaxed atomic store is async-signal-safe.
    let h = handler as *const () as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGTERM, h);
        libc::signal(libc::SIGINT, h);
    }
}

/// True once a shutdown signal has been received.
pub fn shutdown_requested() -> bool {
    SHUTDOWN.load(Ordering::Relaxed)
}
