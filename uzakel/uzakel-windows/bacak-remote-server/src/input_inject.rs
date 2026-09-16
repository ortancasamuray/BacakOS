//! Virtual input injection on the host, via `enigo` (uinput on Linux, Win32
//! `SendInput` on Windows, CGEvent on macOS — one crate, all three targets).
//!
//! Known gap: `enigo` drives a single system pointer, so true multi-touch
//! (pinch/two-finger pan as independent contacts) can't be injected as real
//! touch points here — only the first active finger drives the pointer, and
//! a second concurrent finger is ignored. Real multi-touch injection needs a
//! dedicated virtual touch device (Linux: a second `/dev/uinput` node
//! advertising `ABS_MT_*`, mirroring how `uzakel/uzakel-android/daemon/src/input_manager.rs`
//! owns its own uinput device) — worth lifting into its own backend once
//! gesture translation (pinch/pan) is designed, rather than folding it into
//! this pointer-shaped injector.
//!
//! [`Injector`] itself never crosses an `.await` — [`run_injector_thread`]
//! owns it on a plain OS thread instead, the same reason `capture.rs`
//! builds `scrap::Capturer` inside its own thread rather than taking one by
//! value: a real-Mac build (found running this crate there for the first
//! time, not by inspection) failed with "future cannot be sent between
//! threads safely" because `enigo`'s macOS backend holds a raw
//! `NonNull<CGEventSource>` — `Send` on Windows/Linux, not on macOS. Moving
//! it into a `tokio::spawn`'d future (as `network::run_input_listener` did
//! before) is exactly the platform-dependently-`Send` situation that broke.

use bacak_remote_proto::{InputEvent, PointerButton as ProtoButton};
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Mouse, Settings};

pub struct Injector {
    enigo: Enigo,
    /// The reference size `enigo::Mouse::move_mouse(.., Coordinate::Abs)`
    /// scales against on this platform (Windows: `GetSystemMetrics(SM_CXSCREEN/
    /// SM_CYSCREEN)`, via `enigo`'s own `main_display()`) — **not** the video
    /// capture's reported resolution. On at least one real VM the two
    /// disagreed (capture 1400x1050 vs `SM_CXSCREEN/CYSCREEN` 1024x768,
    /// stale guest-display metrics after a resolution change), which made
    /// every absolute move land off by the ratio between them — worse near
    /// the edges. Normalizing against `main_display()` instead of
    /// `screen_width`/`screen_height` keeps `move_absolute`'s output in the
    /// same reference frame `move_mouse` itself will rescale it against.
    abs_display: (i32, i32),
    /// The finger currently driving the pointer, so a second concurrent touch
    /// doesn't fight it for control (see the module-level limitation above).
    active_finger: Option<u32>,
}

impl Injector {
    pub fn new(screen_width: u32, screen_height: u32) -> anyhow::Result<Self> {
        let enigo = Enigo::new(&Settings::default()).map_err(|e| anyhow::anyhow!("enigo init failed: {e}"))?;
        // Prefer enigo's own idea of the screen (what it will actually scale
        // absolute moves against); fall back to the capture size if that
        // query fails, rather than erroring the whole session out over it.
        let abs_display = enigo.main_display().unwrap_or((screen_width as i32, screen_height as i32));
        tracing::info!("capture={screen_width}x{screen_height} enigo main_display={abs_display:?}");
        Ok(Self { enigo, abs_display, active_finger: None })
    }

    pub fn inject(&mut self, event: InputEvent) -> anyhow::Result<()> {
        match event {
            InputEvent::PointerMotion { dx, dy } => {
                self.enigo.move_mouse(dx as i32, dy as i32, Coordinate::Rel)?;
            }
            InputEvent::PointerButton { button, pressed } => {
                let direction = if pressed { Direction::Press } else { Direction::Release };
                self.enigo.button(map_button(button), direction)?;
            }
            InputEvent::PointerScroll { dx, dy } => {
                if dy.abs() > f32::EPSILON {
                    self.enigo.scroll(dy.round() as i32, Axis::Vertical)?;
                }
                if dx.abs() > f32::EPSILON {
                    self.enigo.scroll(dx.round() as i32, Axis::Horizontal)?;
                }
            }
            InputEvent::TouchDown { finger_id, x, y } => {
                if self.active_finger.is_none() {
                    self.active_finger = Some(finger_id);
                    self.move_absolute(x, y)?;
                    self.enigo.button(Button::Left, Direction::Press)?;
                } else {
                    tracing::warn!(
                        "TouchDown finger {finger_id} ignored — finger {:?} still active (its TouchUp may have been lost)",
                        self.active_finger
                    );
                }
            }
            InputEvent::TouchMotion { finger_id, x, y } => {
                if self.active_finger == Some(finger_id) {
                    self.move_absolute(x, y)?;
                }
            }
            InputEvent::TouchUp { finger_id } => {
                if self.active_finger == Some(finger_id) {
                    self.enigo.button(Button::Left, Direction::Release)?;
                    self.active_finger = None;
                }
            }
        }
        Ok(())
    }

    fn move_absolute(&mut self, norm_x: f32, norm_y: f32) -> anyhow::Result<()> {
        let (w, h) = self.abs_display;
        let x = (norm_x.clamp(0.0, 1.0) * w as f32) as i32;
        let y = (norm_y.clamp(0.0, 1.0) * h as f32) as i32;
        self.enigo.move_mouse(x, y, Coordinate::Abs)?;
        Ok(())
    }
}

fn map_button(button: ProtoButton) -> Button {
    match button {
        ProtoButton::Left => Button::Left,
        ProtoButton::Right => Button::Right,
        ProtoButton::Middle => Button::Middle,
    }
}

/// Spawns [`Injector`] on its own OS thread and returns a channel to feed
/// it decoded [`InputEvent`]s — see the module doc for why it can't just be
/// handed to `tokio::spawn` the way it used to be. `std::sync::mpsc`, not
/// `tokio::sync::mpsc`: nothing on the sending side (`network::
/// run_input_listener`) needs to `.await` a send, and this way the receive
/// loop below stays a plain blocking `recv()`, no local `tokio::runtime`
/// needed on this thread just to drive one channel.
pub fn run_injector_thread(screen_width: u32, screen_height: u32) -> anyhow::Result<std::sync::mpsc::Sender<InputEvent>> {
    let (tx, rx) = std::sync::mpsc::channel::<InputEvent>();
    // `Injector::new` has to run *inside* the spawned thread, not before
    // it: on macOS, `std::thread::Builder::spawn` itself refused to compile
    // otherwise — its closure has to be `Send` to hand off to the new
    // thread at all, and a closure capturing a not-`Send` `Injector` isn't
    // (same underlying reason as the module doc's `tokio::spawn` story,
    // just enforced at a different point). `ready_rx.recv()` below still
    // makes a construction failure (e.g. accessibility permission not yet
    // granted on macOS) fail the whole session start synchronously, same
    // as when `main.rs` called `Injector::new` directly — it just has to
    // cross a channel to get back here instead of a plain `?`.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();
    std::thread::Builder::new().name("bacak-remote-input".into()).spawn(move || {
        let mut injector = match Injector::new(screen_width, screen_height) {
            Ok(i) => {
                let _ = ready_tx.send(Ok(()));
                i
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };
        while let Ok(event) = rx.recv() {
            if let Err(e) = injector.inject(event) {
                tracing::warn!("input injection failed: {e}");
            }
        }
        tracing::info!("input injector thread ending: sender dropped");
    })?;
    ready_rx.recv().map_err(|_| anyhow::anyhow!("input injector thread died before initializing"))??;
    Ok(tx)
}
