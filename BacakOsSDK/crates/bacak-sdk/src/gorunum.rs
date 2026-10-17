//! The desktop-wide appearance (dark mode).
//!
//! * [`Gorunum::load`] — the current setting (`~/.config/bacak/gorunum.json`;
//!   dark when missing). Works without a compositor.
//! * [`izle`] — call back whenever it changes.
//! * [`ayarla`] — change it for the whole desktop.
//!
//! Applications should not keep a separate dark-mode preference: a theme
//! toggle inside an app calls [`ayarla`], and every app follows.

use std::time::Duration;

pub use bacak_ipc::Gorunum;

use crate::ipc::Istemci;

/// File polling interval while the compositor's IPC isn't reachable.
const POLL: Duration = Duration::from_secs(1);
/// How often a lost IPC connection is retried.
const RETRY: Duration = Duration::from_secs(5);

/// Calls `f` with every new appearance, on a background thread that lives as
/// long as the process. Not called for the initial value — read that with
/// [`Gorunum::load`] first.
///
/// Uses the compositor's `gorunum` event when connected and falls back to
/// polling the settings file (so it also works under another desktop, and
/// across compositor restarts).
///
/// For a GUI toolkit, hop back to the UI thread inside `f` (Slint:
/// `slint::invoke_from_event_loop`; `bacak-sdk-slint::tema_bagla!` does this).
pub fn izle<F>(mut f: F) -> std::thread::JoinHandle<()>
where
    F: FnMut(Gorunum) + Send + 'static,
{
    std::thread::Builder::new()
        .name("bacak-gorunum".into())
        .spawn(move || {
            let mut last = Gorunum::load();
            let mut emit = |g: Gorunum, f: &mut F| {
                if g != last {
                    last = g;
                    f(g);
                }
            };
            loop {
                if let Ok(events) = Istemci::baglan().and_then(Istemci::abone_ol) {
                    // A change may have happened while we weren't subscribed.
                    emit(Gorunum::load(), &mut f);
                    for olay in events {
                        if let bacak_ipc::Olay::Gorunum { koyu } = olay {
                            emit(Gorunum { koyu }, &mut f);
                        }
                    }
                }
                let mut waited = Duration::ZERO;
                while waited < RETRY {
                    std::thread::sleep(POLL);
                    waited += POLL;
                    emit(Gorunum::load(), &mut f);
                }
            }
        })
        .expect("thread spawn")
}

/// Sets dark mode for the whole desktop. Goes through the compositor (which
/// saves it and tells every application); with no compositor reachable the
/// settings file is written directly, which [`izle`] callers pick up too.
pub fn ayarla(koyu: bool) -> std::io::Result<()> {
    match Istemci::baglan().and_then(|mut c| c.koyu_mod_ayarla(koyu)) {
        Ok(()) => Ok(()),
        Err(_) => Gorunum { koyu }.save(),
    }
}
