//! Virtual input injection on the host, via `enigo` (uinput on Linux, Win32
//! `SendInput` on Windows, CGEvent on macOS — one crate, all three targets).
//!
//! Known gap: `enigo` drives a single system pointer, so true multi-touch
//! (pinch/two-finger pan as independent contacts) can't be injected as real
//! touch points here — only the first active finger drives the pointer, and
//! a second concurrent finger is ignored. Real multi-touch injection needs a
//! dedicated virtual touch device (Linux: a second `/dev/uinput` node
//! advertising `ABS_MT_*`, mirroring how `uzakel/daemon/src/input_manager.rs`
//! owns its own uinput device) — worth lifting into its own backend once
//! gesture translation (pinch/pan) is designed, rather than folding it into
//! this pointer-shaped injector.

use bacak_remote_proto::{InputEvent, PointerButton as ProtoButton};
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Mouse, Settings};

pub struct Injector {
    enigo: Enigo,
    screen_width: u32,
    screen_height: u32,
    /// The finger currently driving the pointer, so a second concurrent touch
    /// doesn't fight it for control (see the module-level limitation above).
    active_finger: Option<u32>,
}

impl Injector {
    pub fn new(screen_width: u32, screen_height: u32) -> anyhow::Result<Self> {
        let enigo = Enigo::new(&Settings::default()).map_err(|e| anyhow::anyhow!("enigo init failed: {e}"))?;
        Ok(Self { enigo, screen_width, screen_height, active_finger: None })
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
        let x = (norm_x.clamp(0.0, 1.0) * self.screen_width as f32) as i32;
        let y = (norm_y.clamp(0.0, 1.0) * self.screen_height as f32) as i32;
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
