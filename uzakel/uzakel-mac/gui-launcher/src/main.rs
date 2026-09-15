//! A macOS-native pairing window for `bacak-remote-server`, mirroring what
//! `../../uzakel-windows/bacak-remote-server/src/gui.rs` does on Windows —
//! see that file's module doc and `../README.md` for why this is a
//! *separate* binary that spawns the real server as a subprocess, rather
//! than a module inside that shared, cross-platform crate.
//!
//! No structured status channel exists across a process boundary the way
//! `gui.rs`'s `StatusChannel` does in-process — this window only knows
//! "I started the process" / "I asked it to stop", not "it actually
//! paired". A future pass could tail the child's stdout for `tracing`'s
//! log lines (see `network.rs` for the exact strings: "paired
//! successfully" / "gave wrong PIN") to show real status; deliberately
//! not attempted here to keep this first real build small enough to
//! verify end-to-end on real hardware in one sitting.

use std::cell::{OnceCell, RefCell};
use std::process::Child;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject as ObjcNSObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSButton, NSTextField, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{ns_string, MainThreadMarker, NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

/// Where to find the real server binary. This launcher isn't bundled
/// alongside it yet (see `../README.md`'s "Known gap" — `build_mac.sh`
/// only puts `bacak-remote-server` itself in the `.app`), so for this
/// first real build/run it just looks next to itself first (the shape a
/// real bundle would eventually have — both binaries in `Contents/MacOS/`)
/// and falls back to the known dev-checkout path this was actually
/// verified against.
fn find_server_binary() -> std::path::PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("bacak-remote-server");
            if sibling.exists() {
                return sibling;
            }
        }
    }
    std::path::PathBuf::from(
        std::env::var("BACAK_REMOTE_SERVER_BIN")
            .unwrap_or_else(|_| "/Users/os/uzakel-test/uzakel-windows/target/release/bacak-remote-server".to_string()),
    )
}

#[derive(Default)]
struct AppDelegateIvars {
    window: OnceCell<Retained<NSWindow>>,
    pin_field: OnceCell<Retained<NSTextField>>,
    status_label: OnceCell<Retained<NSTextField>>,
    end_button: OnceCell<Retained<NSButton>>,
    child: RefCell<Option<Child>>,
}

define_class!(
    #[unsafe(super = ObjcNSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppDelegateIvars]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, notification: &NSNotification) {
            let mtm = self.mtm();
            let app = notification.object()
                .unwrap()
                .downcast::<NSApplication>()
                .unwrap();

            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(380.0, 220.0)),
                    NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            unsafe { window.setReleasedWhenClosed(false) };
            window.setTitle(ns_string!("Bacak Remote — Eşleştirme"));
            let view = window.contentView().expect("window must have content view");

            let prompt = NSTextField::labelWithString(ns_string!("BacakOS ekranında gösterilen PIN'i girin:"), mtm);
            prompt.setFrame(NSRect::new(NSPoint::new(16.0, 176.0), NSSize::new(340.0, 20.0)));
            view.addSubview(&prompt);

            let pin_field = NSTextField::new(mtm);
            pin_field.setFrame(NSRect::new(NSPoint::new(16.0, 144.0), NSSize::new(200.0, 26.0)));
            view.addSubview(&pin_field);

            let submit = unsafe {
                NSButton::buttonWithTitle_target_action(
                    ns_string!("Eşleştir"),
                    Some(self),
                    Some(sel!(onSubmit:)),
                    mtm,
                )
            };
            submit.setFrame(NSRect::new(NSPoint::new(232.0, 142.0), NSSize::new(124.0, 30.0)));
            view.addSubview(&submit);

            let status_label = NSTextField::labelWithString(ns_string!("PIN'i girip Eşleştir'e basın."), mtm);
            status_label.setFrame(NSRect::new(NSPoint::new(16.0, 96.0), NSSize::new(340.0, 40.0)));
            view.addSubview(&status_label);

            let end_button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    ns_string!("Eşleşmeyi Bitir"),
                    Some(self),
                    Some(sel!(onEnd:)),
                    mtm,
                )
            };
            end_button.setFrame(NSRect::new(NSPoint::new(16.0, 24.0), NSSize::new(168.0, 30.0)));
            end_button.setEnabled(false);
            view.addSubview(&end_button);

            let close = unsafe {
                NSButton::buttonWithTitle_target_action(
                    ns_string!("Kapat"),
                    Some(self),
                    Some(sel!(onClose:)),
                    mtm,
                )
            };
            close.setFrame(NSRect::new(NSPoint::new(196.0, 24.0), NSSize::new(160.0, 30.0)));
            view.addSubview(&close);

            window.center();
            window.setDelegate(Some(ProtocolObject::from_ref(self)));
            window.makeKeyAndOrderFront(None);

            let _ = self.ivars().window.set(window);
            let _ = self.ivars().pin_field.set(pin_field);
            let _ = self.ivars().status_label.set(status_label);
            let _ = self.ivars().end_button.set(end_button);

            app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }
    }

    unsafe impl NSWindowDelegate for AppDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            self.kill_child();
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
    }

    impl AppDelegate {
        #[unsafe(method(onSubmit:))]
        fn on_submit(&self, _sender: &AnyObject) {
            let Some(pin_field) = self.ivars().pin_field.get() else { return };
            let Some(status_label) = self.ivars().status_label.get() else { return };
            let pin_text = pin_field.stringValue().to_string();
            let pin_text = pin_text.trim();
            if pin_text.is_empty() || !pin_text.chars().all(|c| c.is_ascii_digit()) {
                status_label.setStringValue(ns_string!("Geçersiz PIN — BacakOS ekranındaki 6 haneli sayıyı girin."));
                return;
            }

            let server_bin = find_server_binary();
            match std::process::Command::new(&server_bin)
                .args(["--pin", pin_text, "--no-gui"])
                .spawn()
            {
                Ok(child) => {
                    *self.ivars().child.borrow_mut() = Some(child);
                    status_label.setStringValue(&NSString::from_str(&format!(
                        "PIN gönderildi ({server_bin} çalışıyor) — BacakOS'tan bağlantı bekleniyor…",
                        server_bin = server_bin.display()
                    )));
                    if let Some(end_button) = self.ivars().end_button.get() {
                        end_button.setEnabled(true);
                    }
                }
                Err(e) => {
                    status_label.setStringValue(&NSString::from_str(&format!(
                        "Sunucu başlatılamadı ({}): {e}",
                        server_bin.display()
                    )));
                }
            }
        }

        #[unsafe(method(onEnd:))]
        fn on_end(&self, _sender: &AnyObject) {
            self.kill_child();
            if let Some(status_label) = self.ivars().status_label.get() {
                status_label.setStringValue(ns_string!("Oturum sonlandırıldı."));
            }
            if let Some(end_button) = self.ivars().end_button.get() {
                end_button.setEnabled(false);
            }
        }

        #[unsafe(method(onClose:))]
        fn on_close(&self, _sender: &AnyObject) {
            self.kill_child();
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars::default());
        unsafe { msg_send![super(this), init] }
    }

    /// No graceful `Bye` here (see the module doc — no channel into the
    /// child's live session) — a plain `SIGKILL` via `Child::kill`. Good
    /// enough for "end the session from this side" while this launcher is
    /// still a subprocess wrapper rather than sharing `run_session`'s
    /// state directly.
    fn kill_child(&self) {
        if let Some(mut child) = self.ivars().child.borrow_mut().take() {
            let _ = child.kill();
        }
    }
}

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    let delegate = AppDelegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}
