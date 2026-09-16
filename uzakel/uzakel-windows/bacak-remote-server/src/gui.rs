//! Graphical pairing window (Win32, via `native-windows-gui`) — the default
//! way to run this on Windows, replacing the console PIN prompt with a
//! small always-visible panel: a PIN entry (read off the BacakOS screen),
//! a status line that turns green once a client actually pairs, a button
//! to end the current session without closing the app, and a system-tray
//! icon (with a balloon notification on pairing) so the operator can tell
//! it's running even when the window isn't in front. See `main.rs`'s
//! module doc and `bacak-compositor`'s `remote_desktop.rs` for the other
//! half of this handshake (that's where the PIN is generated).
//!
//! Screen capture/network run on their own OS thread with their own tokio
//! runtime (`run_session`, in `main.rs`), entirely separate from the Win32
//! message loop this module owns on the main thread — the two only ever
//! talk through the two plain `std::sync::mpsc` channels below, plus a
//! `tokio::sync::watch<bool>` running the other way (GUI → network thread)
//! that "Eşleşmeyi Bitir" flips to ask the current session to stop:
//!
//! - `start_session` spawns a brand new network thread (with its own tokio
//!   runtime) for every PIN submission, handing it the PIN directly — no
//!   `PairRequest` can be answered before the operator actually typed
//!   something in, since nothing is listening until that thread exists.
//!   Ending a session and typing in a new PIN starts a completely new
//!   thread rather than trying to reuse the old one.
//! - `status_tx`/`status_rx`: pairing progress, network thread → GUI,
//!   delivered as a [`SessionStatus`] and woken up via an [`nwg::Notice`]
//!   (see `dialog_multithreading_d.rs` in the `native-windows-gui` examples
//!   for the same pattern) rather than a polling timer. One `status_rx` is
//!   built once and outlives every session; `status_tx` is cloned into
//!   each session's `StatusChannel` (see `main.rs`).

use std::cell::{Cell, RefCell};
use std::sync::mpsc::{Receiver, Sender};

use native_windows_derive as nwd;
use native_windows_gui as nwg;
use nwd::NwgUi;
use nwg::NativeUi;

use crate::{run_session, Args, SessionStatus, StatusChannel};

#[derive(Default, NwgUi)]
pub struct PairingWindow {
    // The app's own icon (`resources/app.ico`, embedded into the .exe by
    // `build.rs`/`resources/app.rc` at resource ID 1) — loaded from the
    // running executable itself, not a file on disk, so it survives the
    // exe being copied/renamed. Used for both the window's title bar and
    // the tray icon below.
    #[nwg_resource]
    embed: nwg::EmbedResource,

    #[nwg_resource(source_embed: Some(&data.embed), source_embed_id: 1)]
    app_icon: nwg::Icon,

    #[nwg_control(size: (380, 230), position: (300, 300), title: "Bacak Remote — Eşleştirme", icon: Some(&data.app_icon), flags: "WINDOW|VISIBLE")]
    #[nwg_events( OnWindowClose: [PairingWindow::on_close], OnKeyEnter: [PairingWindow::on_submit] )]
    window: nwg::Window,

    #[nwg_control(text: "BacakOS ekranında gösterilen PIN'i girin:", size: (340, 20), position: (16, 16))]
    pin_prompt: nwg::Label,

    #[nwg_control(text: "", size: (200, 28), position: (16, 44), limit: 6, focus: true)]
    pin_input: nwg::TextInput,

    #[nwg_control(text: "Eşleştir", size: (124, 28), position: (232, 44))]
    #[nwg_events( OnButtonClick: [PairingWindow::on_submit] )]
    submit_button: nwg::Button,

    #[nwg_control(text: "PIN'i girip Eşleştir'e basın.", size: (340, 80), position: (16, 88))]
    status_label: nwg::Label,

    #[nwg_control(text: "Eşleşmeyi Bitir", size: (168, 28), position: (16, 176), enabled: false)]
    #[nwg_events( OnButtonClick: [PairingWindow::on_end] )]
    end_button: nwg::Button,

    #[nwg_control(text: "Kapat", size: (168, 28), position: (188, 176))]
    #[nwg_events( OnButtonClick: [PairingWindow::on_close] )]
    close_button: nwg::Button,

    // Feature "notice" — woken from the network thread via `NoticeSender`
    // whenever a `SessionStatus` is pushed onto `status_rx`, instead of
    // polling on a timer.
    #[nwg_control]
    #[nwg_events( OnNotice: [PairingWindow::on_status_notice] )]
    status_notice: nwg::Notice,

    #[nwg_control(icon: Some(&data.app_icon), tip: Some("Bacak Remote — Uzak Masaüstü"))]
    tray: nwg::TrayNotification,

    status_tx: RefCell<Option<Sender<SessionStatus>>>,
    status_rx: RefCell<Option<Receiver<SessionStatus>>>,
    notice_sender: RefCell<Option<nwg::NoticeSender>>,
    shutdown_tx: RefCell<Option<tokio::sync::watch::Sender<bool>>>,
    args: RefCell<Option<Args>>,
    submitted: Cell<bool>,
}

impl PairingWindow {
    /// Starts a brand new network thread for one pairing attempt — called
    /// once from `run` for the first PIN, and again from `on_status_notice`
    /// every time a previous session ends and the operator submits another
    /// one. Each call gets its own tokio runtime, its own `watch<bool>`
    /// shutdown channel (stashed in `shutdown_tx` for `on_end`), and its own
    /// `StatusChannel` wrapping a fresh clone of the long-lived `status_tx`.
    fn start_session(&self, pin: u32) {
        let Some(args) = self.args.borrow().clone() else { return };
        let Some(status_tx) = self.status_tx.borrow().clone() else { return };
        let Some(notice_sender) = *self.notice_sender.borrow() else { return };

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        *self.shutdown_tx.borrow_mut() = Some(shutdown_tx);

        std::thread::Builder::new()
            .name("bacak-remote-net".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Runtime::new() {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::error!("tokio runtime: {e}");
                        return;
                    }
                };
                let status_channel = StatusChannel::new(status_tx, move || notice_sender.notice());
                if let Err(e) = runtime.block_on(run_session(args, pin, Some(status_channel), Some(shutdown_rx))) {
                    tracing::error!("session ended: {e}");
                }
            })
            .expect("spawn bacak-remote-net thread");
    }

    fn on_submit(&self) {
        if self.submitted.get() {
            return;
        }
        let text = self.pin_input.text();
        let pin: u32 = match text.trim().parse() {
            Ok(p) => p,
            Err(_) => {
                self.status_label.set_text("Geçersiz PIN — BacakOS ekranındaki 6 haneli sayıyı girin.");
                return;
            }
        };
        self.submitted.set(true);
        self.pin_input.set_enabled(false);
        self.submit_button.set_enabled(false);
        self.end_button.set_enabled(true);
        self.status_label.set_text("PIN gönderildi — BacakOS'tan bağlantı bekleniyor…");
        self.start_session(pin);
    }

    /// "Eşleşmeyi Bitir" — asks the current session's network thread to
    /// stop (see `run_session`'s `shutdown_rx`) without closing the window
    /// or the process; [`SessionStatus::Ended`] (via `on_status_notice`)
    /// does the actual UI reset once the thread has actually wound down,
    /// so this only disables the button to guard against a double-click.
    fn on_end(&self) {
        if let Some(tx) = self.shutdown_tx.borrow_mut().take() {
            let _ = tx.send(true);
        }
        self.end_button.set_enabled(false);
        self.status_label.set_text("Bağlantı sonlandırılıyor…");
    }

    /// Drains every [`SessionStatus`] queued since the last notice — a
    /// `Notice` only says "something happened", not how many times, so a
    /// burst (e.g. a couple of wrong-PIN attempts landing close together)
    /// must be drained in a loop rather than assumed to be exactly one.
    fn on_status_notice(&self) {
        let borrow = self.status_rx.borrow();
        let Some(rx) = borrow.as_ref() else { return };
        while let Ok(status) = rx.try_recv() {
            match status {
                SessionStatus::WaitingForClient => {
                    self.status_label.set_text("Ekran paylaşımı başladı — BacakOS'tan bağlantı bekleniyor…");
                }
                SessionStatus::Paired { client_name, addr } => {
                    self.status_label.set_text(&format!("✓ Eşleşti — '{client_name}' ({addr}) bağlandı, ekran paylaşılıyor."));
                    let flags = nwg::TrayNotificationFlags::USER_ICON | nwg::TrayNotificationFlags::LARGE_ICON;
                    self.tray.show(
                        &format!("'{client_name}' bağlandı — ekran paylaşılıyor."),
                        Some("Bacak Remote"),
                        Some(flags),
                        Some(&self.app_icon),
                    );
                }
                SessionStatus::Rejected { from } => {
                    self.status_label.set_text(&format!("Yanlış PIN denemesi: {from} — tekrar denenebilir."));
                }
                SessionStatus::Ended => {
                    self.status_label.set_text("Oturum sona erdi. Yeni bir PIN girip tekrar eşleştirebilirsiniz.");
                    self.end_button.set_enabled(false);
                    self.pin_input.set_text("");
                    self.pin_input.set_enabled(true);
                    self.pin_input.set_focus();
                    self.submit_button.set_enabled(true);
                    self.submitted.set(false);
                }
            }
        }
    }

    fn on_close(&self) {
        if let Some(tx) = self.shutdown_tx.borrow_mut().take() {
            let _ = tx.send(true);
        }
        nwg::stop_thread_dispatch();
    }
}

/// Entry point used by `main.rs` in place of the console flow: detaches the
/// console window `--pin`/`--no-gui` still rely on (this mode has no use
/// for it — see the module doc), builds the window and its tray icon,
/// wires up the channels `start_session` needs, and runs the Win32 message
/// loop until the window closes.
pub fn run(args: Args) -> anyhow::Result<()> {
    // Launching this binary (double-click, or a shortcut with no console
    // attached) auto-allocates one anyway since it's still console-
    // subsystem — free it immediately so the operator only ever sees the
    // pairing window, not a black cmd box sitting behind it. Harmless if
    // there was no console to free (e.g. `AttachConsole`d one already
    // detached) — `FreeConsole` just returns 0 and is ignored.
    unsafe {
        winapi::um::wincon::FreeConsole();
    }

    nwg::init().map_err(|e| anyhow::anyhow!("native-windows-gui init failed: {e}"))?;
    let _ = nwg::Font::set_global_family("Segoe UI");

    let (status_tx, status_rx) = std::sync::mpsc::channel::<SessionStatus>();

    let ui = PairingWindow::build_ui(Default::default()).map_err(|e| anyhow::anyhow!("failed to build pairing window: {e}"))?;
    *ui.status_tx.borrow_mut() = Some(status_tx);
    *ui.status_rx.borrow_mut() = Some(status_rx);
    *ui.notice_sender.borrow_mut() = Some(ui.status_notice.sender());
    *ui.args.borrow_mut() = Some(args);

    nwg::dispatch_thread_events();
    Ok(())
}
