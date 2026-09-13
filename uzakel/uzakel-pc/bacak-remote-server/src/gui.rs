//! Graphical pairing window (Win32, via `native-windows-gui`) — the default
//! way to run this on Windows, replacing the console PIN prompt with a
//! small always-visible panel: a PIN entry (read off the BacakOS screen),
//! and a status line that turns green once a client actually pairs. See
//! `main.rs`'s module doc and `bacak-compositor`'s `remote_desktop.rs` for
//! the other half of this handshake (that's where the PIN is generated).
//!
//! Screen capture/network run on their own OS thread with their own tokio
//! runtime (`run_session`, in `main.rs`), entirely separate from the Win32
//! message loop this module owns on the main thread — the two only ever
//! talk through the two plain `std::sync::mpsc` channels below:
//!
//! - `event_tx`/`event_rx`: the PIN, once submitted, GUI → network thread.
//!   The network thread blocks on this before it does anything at all, so
//!   no `PairRequest` can be answered before the operator actually typed
//!   something in.
//! - `status_tx`/`status_rx`: pairing progress, network thread → GUI,
//!   delivered as a [`SessionStatus`] and woken up via an [`nwg::Notice`]
//!   (see `dialog_multithreading_d.rs` in the `native-windows-gui` examples
//!   for the same pattern) rather than a polling timer.

use std::cell::{Cell, RefCell};
use std::sync::mpsc::{Receiver, Sender};

use native_windows_derive as nwd;
use native_windows_gui as nwg;
use nwd::NwgUi;
use nwg::NativeUi;

use crate::{run_session, Args, SessionStatus, StatusChannel};

#[derive(Default, NwgUi)]
pub struct PairingWindow {
    #[nwg_control(size: (380, 200), position: (300, 300), title: "Bacak Remote — Eşleştirme", flags: "WINDOW|VISIBLE")]
    #[nwg_events( OnWindowClose: [PairingWindow::on_close], OnKeyEnter: [PairingWindow::on_submit] )]
    window: nwg::Window,

    #[nwg_control(text: "BacakOS ekranında gösterilen PIN'i girin:", size: (340, 20), position: (16, 16))]
    pin_prompt: nwg::Label,

    #[nwg_control(text: "", size: (200, 28), position: (16, 44), limit: 6, focus: true)]
    pin_input: nwg::TextInput,

    #[nwg_control(text: "Eşleştir", size: (124, 28), position: (232, 44))]
    #[nwg_events( OnButtonClick: [PairingWindow::on_submit] )]
    submit_button: nwg::Button,

    #[nwg_control(text: "PIN'i girip Eşleştir'e basın.", size: (340, 100), position: (16, 88))]
    status_label: nwg::Label,

    // Feature "notice" — woken from the network thread via `NoticeSender`
    // whenever a `SessionStatus` is pushed onto `status_rx`, instead of
    // polling on a timer.
    #[nwg_control]
    #[nwg_events( OnNotice: [PairingWindow::on_status_notice] )]
    status_notice: nwg::Notice,

    event_tx: RefCell<Option<Sender<u32>>>,
    status_rx: RefCell<Option<Receiver<SessionStatus>>>,
    submitted: Cell<bool>,
}

impl PairingWindow {
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
        let Some(tx) = self.event_tx.borrow_mut().take() else { return };
        if tx.send(pin).is_err() {
            self.status_label.set_text("Hata: ağ iş parçacığı başlatılamadı.");
            return;
        }
        self.submitted.set(true);
        self.pin_input.set_enabled(false);
        self.submit_button.set_enabled(false);
        self.status_label.set_text("PIN gönderildi — BacakOS'tan bağlantı bekleniyor…");
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
                }
                SessionStatus::Rejected { from } => {
                    self.status_label.set_text(&format!("Yanlış PIN denemesi: {from} — tekrar denenebilir."));
                }
            }
        }
    }

    fn on_close(&self) {
        nwg::stop_thread_dispatch();
    }
}

/// Entry point used by `main.rs` in place of the console flow: builds the
/// window, starts the network thread (blocked on the PIN the window will
/// send it), and runs the Win32 message loop until the window closes.
pub fn run(args: Args) -> anyhow::Result<()> {
    nwg::init().map_err(|e| anyhow::anyhow!("native-windows-gui init failed: {e}"))?;
    let _ = nwg::Font::set_global_family("Segoe UI");

    let (event_tx, event_rx) = std::sync::mpsc::channel::<u32>();
    let (status_tx, status_rx) = std::sync::mpsc::channel::<SessionStatus>();

    let ui = PairingWindow::build_ui(Default::default()).map_err(|e| anyhow::anyhow!("failed to build pairing window: {e}"))?;
    *ui.event_tx.borrow_mut() = Some(event_tx);
    *ui.status_rx.borrow_mut() = Some(status_rx);
    let notice_sender = ui.status_notice.sender();

    std::thread::Builder::new().name("bacak-remote-net".into()).spawn(move || {
        let Ok(pin) = event_rx.recv() else { return }; // window closed before a PIN was ever submitted
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                tracing::error!("tokio runtime: {e}");
                return;
            }
        };
        // Every `StatusChannel::send` also fires the notice, so the window
        // wakes up for each status as it happens rather than only once at
        // the very end (see the module doc's "how the two threads talk").
        let status_channel = StatusChannel::new(status_tx, move || notice_sender.notice());
        if let Err(e) = runtime.block_on(run_session(args, pin, Some(status_channel))) {
            tracing::error!("session ended: {e}");
        }
    })?;

    nwg::dispatch_thread_events();
    Ok(())
}
