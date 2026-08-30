//! Address bar for the embedded browser panel (`webengine`) — a thin
//! always-visible strip (back/forward + URL/search field) above the
//! Servo content. Editing the field doesn't draw a keyboard of our own:
//! `app.rs` calls `Window::set_ime_allowed`/`set_ime_cursor_area` (the
//! standard cross-platform winit IME API, backed on Wayland by
//! `zwp_text_input_v3`), which is exactly what `bacak-compositor`'s own
//! system on-screen keyboard (`bacak/crates/bacak-compositor/src/
//! text_input.rs`) watches to show/hide itself. Taps on that OSK arrive
//! back here as ordinary synthesized `wl_keyboard` events — the same
//! `WindowEvent::KeyboardInput` path `App::key_input` already handles —
//! so there's no second keyboard implementation to keep in sync with the
//! rest of the desktop (font, layout, Turkish letters, etc. all come free).

use glam::Vec2;

use crate::stroke::{push_line, push_rect, Vertex};

pub const BAR_H: f32 = 48.0;
const BTN_W: f32 = 48.0;
const MAX_CHARS: usize = 96;

const COLOR_BAR_BG: [f32; 4] = [0.16, 0.17, 0.21, 1.0];
const COLOR_BTN: [f32; 4] = [0.22, 0.24, 0.30, 1.0];
const COLOR_BTN_DISABLED: [f32; 4] = [0.16, 0.17, 0.21, 1.0];
const COLOR_FIELD_BG: [f32; 4] = [0.06, 0.07, 0.09, 1.0];
const COLOR_FIELD_EDITING: [f32; 4] = [0.09, 0.11, 0.15, 1.0];
const COLOR_TEXT: [f32; 4] = [0.95, 0.95, 0.97, 1.0];

/// What a tap on the bar means for the caller. Back/Forward/Field are
/// acted on by `app.rs` (it owns the real `webengine::WebPanel` and the
/// window, which `UrlBar` doesn't know about).
pub enum UrlBarHit {
    Back,
    Forward,
    /// The field was tapped — caller should call
    /// `Window::set_ime_allowed(true)` so the compositor's OSK shows.
    FieldTapped,
    /// Committed (physical/OSK Enter while editing) — `String` is the
    /// raw typed text, not yet resolved to a URL (see `resolve_input` in
    /// `app.rs`).
    Go(String),
    None,
}

pub struct UrlBar {
    pub text: String,
    pub editing: bool,
}

impl UrlBar {
    pub fn new(initial: &str) -> Self {
        Self { text: initial.to_string(), editing: false }
    }

    fn back_rect(top_left: Vec2) -> (Vec2, Vec2) {
        (top_left, Vec2::new(BTN_W, BAR_H))
    }

    fn forward_rect(top_left: Vec2) -> (Vec2, Vec2) {
        (top_left + Vec2::new(BTN_W, 0.0), Vec2::new(BTN_W, BAR_H))
    }

    /// The field's rect in screen space — `app.rs` also uses this to
    /// place `Window::set_ime_cursor_area` (where the compositor may
    /// anchor IME-related UI).
    pub fn field_rect(top_left: Vec2, bar_width: f32) -> (Vec2, Vec2) {
        let x = top_left.x + BTN_W * 2.0 + 6.0;
        (Vec2::new(x, top_left.y + 6.0), Vec2::new(bar_width - BTN_W * 2.0 - 12.0, BAR_H - 12.0))
    }

    /// Fixed — the bar never grows for a keyboard anymore (that's the
    /// compositor's OSK, drawn by the compositor above everything,
    /// including this window).
    pub fn total_height(&self) -> f32 {
        BAR_H
    }

    /// True if `p` lands on the bar — callers use this to decide "swallow
    /// this touch, don't forward it to the web content or the canvas".
    pub fn contains(&self, bar_top_left: Vec2, bar_width: f32, p: Vec2) -> bool {
        rect_contains((bar_top_left, Vec2::new(bar_width, BAR_H)), p)
    }

    pub fn press_at(&mut self, bar_top_left: Vec2, bar_width: f32, p: Vec2, current_url: Option<&str>) -> UrlBarHit {
        if rect_contains(Self::back_rect(bar_top_left), p) {
            self.editing = false;
            return UrlBarHit::Back;
        }
        if rect_contains(Self::forward_rect(bar_top_left), p) {
            self.editing = false;
            return UrlBarHit::Forward;
        }
        if rect_contains(Self::field_rect(bar_top_left, bar_width), p) {
            if !self.editing {
                if let Some(u) = current_url {
                    self.text = u.to_string();
                }
            }
            self.editing = true;
            return UrlBarHit::FieldTapped;
        }
        UrlBarHit::None
    }

    /// Types one character from a real (or the compositor's on-screen)
    /// keyboard. Only called while `editing` — see `App::key_input`.
    /// Upper-cased because `font5x7` (this field's display font) only
    /// has uppercase glyphs — lowercase would just render as a blank
    /// gap. URLs and search terms are case-insensitive in practice, so
    /// nothing is lost by normalizing on the way in.
    pub fn type_char(&mut self, c: char) {
        if self.text.chars().count() < MAX_CHARS {
            self.text.push(c.to_ascii_uppercase());
        }
    }

    pub fn backspace(&mut self) {
        self.text.pop();
    }

    /// Commits the field — called on physical/OSK Enter.
    pub fn commit(&mut self) -> String {
        self.editing = false;
        self.text.clone()
    }

    /// Stops editing without committing (e.g. Back/Forward tapped, or
    /// the panel closed) — caller should pair this with
    /// `Window::set_ime_allowed(false)`.
    pub fn cancel_editing(&mut self) {
        self.editing = false;
    }

    pub fn render(
        &self,
        bar_top_left: Vec2,
        bar_width: f32,
        can_go_back: bool,
        can_go_forward: bool,
        out_v: &mut Vec<Vertex>,
        out_i: &mut Vec<u32>,
    ) {
        push_rect(bar_top_left, Vec2::new(bar_width, BAR_H), COLOR_BAR_BG, out_v, out_i);

        let (back_pos, back_size) = Self::back_rect(bar_top_left);
        push_rect(back_pos, back_size, if can_go_back { COLOR_BTN } else { COLOR_BTN_DISABLED }, out_v, out_i);
        draw_arrow(back_pos + back_size / 2.0, -1.0, out_v, out_i);

        let (fwd_pos, fwd_size) = Self::forward_rect(bar_top_left);
        push_rect(fwd_pos, fwd_size, if can_go_forward { COLOR_BTN } else { COLOR_BTN_DISABLED }, out_v, out_i);
        draw_arrow(fwd_pos + fwd_size / 2.0, 1.0, out_v, out_i);

        let (field_pos, field_size) = Self::field_rect(bar_top_left, bar_width);
        push_rect(field_pos, field_size, if self.editing { COLOR_FIELD_EDITING } else { COLOR_FIELD_BG }, out_v, out_i);
        let cell = Vec2::new(field_size.y * 0.30, field_size.y * 0.55);
        let text_pos = field_pos + Vec2::new(10.0, field_size.y * 0.22);
        let width = crate::font5x7::push_text(&self.text, text_pos, cell, COLOR_TEXT, out_v, out_i);
        if self.editing {
            push_line(
                text_pos + Vec2::new(width + 2.0, -2.0),
                text_pos + Vec2::new(width + 2.0, cell.y + 2.0),
                2.0,
                COLOR_TEXT,
                out_v,
                out_i,
            );
        }
    }
}

fn draw_arrow(center: Vec2, dir: f32, out_v: &mut Vec<Vertex>, out_i: &mut Vec<u32>) {
    let tip = center + Vec2::new(dir * 8.0, 0.0);
    push_line(tip, tip + Vec2::new(-dir * 8.0, -8.0), 3.0, COLOR_TEXT, out_v, out_i);
    push_line(tip, tip + Vec2::new(-dir * 8.0, 8.0), 3.0, COLOR_TEXT, out_v, out_i);
}

fn rect_contains((top_left, size): (Vec2, Vec2), point: Vec2) -> bool {
    point.x >= top_left.x && point.x <= top_left.x + size.x && point.y >= top_left.y && point.y <= top_left.y + size.y
}
