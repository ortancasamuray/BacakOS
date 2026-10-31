//! Address bar for the embedded browser panel (`webengine`) — a thin
//! always-visible strip (back/forward + a draw-mode toggle + URL/search
//! field) above the Servo content. The draw toggle lets a teacher
//! annotate directly on a live web page: while active, touches over the
//! page are routed to the ordinary canvas drawing path (`app.rs::
//! browser_region` returns `Outside` instead of `Content`) instead of
//! reaching Servo, using whatever tool/pen is selected on the main
//! toolbar — same ink, same page-strokes, just happening to land where
//! the page is currently drawn. Editing the field doesn't draw a keyboard of our own:
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

use crate::font_atlas::{FontAtlas, GlyphVertex};
use crate::stroke::{push_line, push_rect, Vertex};

pub const BAR_H: f32 = 48.0;
const BTN_W: f32 = 48.0;
const MAX_CHARS: usize = 96;

const COLOR_BAR_BG: [f32; 4] = [0.16, 0.17, 0.21, 1.0];
const COLOR_BTN: [f32; 4] = [0.22, 0.24, 0.30, 1.0];
const COLOR_BTN_DISABLED: [f32; 4] = [0.16, 0.17, 0.21, 1.0];
const COLOR_BTN_ACTIVE: [f32; 4] = [0.23, 0.51, 0.96, 1.0]; // matches the toolbar's accent
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
    /// Toggles whether touches over the page draw ink (using the main
    /// toolbar's current tool/pen) instead of reaching the page itself —
    /// see `App::browser_draw_mode`.
    ToggleDraw,
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

    fn draw_toggle_rect(top_left: Vec2) -> (Vec2, Vec2) {
        (top_left + Vec2::new(BTN_W * 2.0, 0.0), Vec2::new(BTN_W, BAR_H))
    }

    /// The field's rect in screen space — `app.rs` also uses this to
    /// place `Window::set_ime_cursor_area` (where the compositor may
    /// anchor IME-related UI).
    pub fn field_rect(top_left: Vec2, bar_width: f32) -> (Vec2, Vec2) {
        let x = top_left.x + BTN_W * 3.0 + 6.0;
        (Vec2::new(x, top_left.y + 6.0), Vec2::new(bar_width - BTN_W * 3.0 - 12.0, BAR_H - 12.0))
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
        if rect_contains(Self::draw_toggle_rect(bar_top_left), p) {
            return UrlBarHit::ToggleDraw;
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
    /// keyboard. Only called while `editing` — see `App::key_input`. Kept
    /// as typed: URL paths and queries are case-sensitive.
    pub fn type_char(&mut self, c: char) {
        if self.text.chars().count() < MAX_CHARS {
            self.text.push(c);
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
        draw_mode: bool,
        font: &FontAtlas,
        out_v: &mut Vec<Vertex>,
        out_i: &mut Vec<u32>,
        glyph_v: &mut Vec<GlyphVertex>,
        glyph_i: &mut Vec<u32>,
    ) {
        push_rect(bar_top_left, Vec2::new(bar_width, BAR_H), COLOR_BAR_BG, out_v, out_i);

        let (back_pos, back_size) = Self::back_rect(bar_top_left);
        push_rect(back_pos, back_size, if can_go_back { COLOR_BTN } else { COLOR_BTN_DISABLED }, out_v, out_i);
        draw_arrow(back_pos + back_size / 2.0, -1.0, out_v, out_i);

        let (fwd_pos, fwd_size) = Self::forward_rect(bar_top_left);
        push_rect(fwd_pos, fwd_size, if can_go_forward { COLOR_BTN } else { COLOR_BTN_DISABLED }, out_v, out_i);
        draw_arrow(fwd_pos + fwd_size / 2.0, 1.0, out_v, out_i);

        let (draw_pos, draw_size) = Self::draw_toggle_rect(bar_top_left);
        push_rect(draw_pos, draw_size, if draw_mode { COLOR_BTN_ACTIVE } else { COLOR_BTN }, out_v, out_i);
        draw_pen_icon(draw_pos + draw_size / 2.0, out_v, out_i);

        let (field_pos, field_size) = Self::field_rect(bar_top_left, bar_width);
        push_rect(field_pos, field_size, if self.editing { COLOR_FIELD_EDITING } else { COLOR_FIELD_BG }, out_v, out_i);
        let text_h = field_size.y * 0.42;
        let text_pos = field_pos + Vec2::new(10.0, (field_size.y - text_h) / 2.0 - text_h * 0.15);
        let shown = fit_text(&self.text, field_size.x - 24.0, self.editing, |t| font.measure(t, text_h));
        let width = font.push_text(&shown, text_pos, text_h, COLOR_TEXT, glyph_v, glyph_i);
        if self.editing {
            push_line(
                text_pos + Vec2::new(width + 3.0, -text_h * 0.15),
                text_pos + Vec2::new(width + 3.0, text_h * 1.25),
                2.0,
                COLOR_TEXT,
                out_v,
                out_i,
            );
        }
    }
}

/// `text` cut to fit `max_w` (as measured by `measure`): while editing the
/// end stays visible (where the cursor is), otherwise the start, with "…"
/// marking the cut.
fn fit_text(text: &str, max_w: f32, keep_end: bool, measure: impl Fn(&str) -> f32) -> String {
    if measure(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    for n in (0..chars.len()).rev() {
        let cut: String = if keep_end {
            std::iter::once('…').chain(chars[chars.len() - n..].iter().copied()).collect()
        } else {
            chars[..n].iter().copied().chain(std::iter::once('…')).collect()
        };
        if measure(&cut) <= max_w {
            return cut;
        }
    }
    "…".to_string()
}

fn draw_arrow(center: Vec2, dir: f32, out_v: &mut Vec<Vertex>, out_i: &mut Vec<u32>) {
    let tip = center + Vec2::new(dir * 8.0, 0.0);
    push_line(tip, tip + Vec2::new(-dir * 8.0, -8.0), 3.0, COLOR_TEXT, out_v, out_i);
    push_line(tip, tip + Vec2::new(-dir * 8.0, 8.0), 3.0, COLOR_TEXT, out_v, out_i);
}

/// A simple diagonal pen glyph — same visual language as the toolbar's
/// own Pen icon, so the toggle reads as "drawing" at a glance.
fn draw_pen_icon(center: Vec2, out_v: &mut Vec<Vertex>, out_i: &mut Vec<u32>) {
    let a = center + Vec2::new(-7.0, 7.0);
    let b = center + Vec2::new(7.0, -7.0);
    push_line(a, b, 3.5, COLOR_TEXT, out_v, out_i);
    crate::stroke::push_circle(a, 2.5, COLOR_TEXT, 10, out_v, out_i);
}

fn rect_contains((top_left, size): (Vec2, Vec2), point: Vec2) -> bool {
    point.x >= top_left.x && point.x <= top_left.x + size.x && point.y >= top_left.y && point.y <= top_left.y + size.y
}

#[cfg(test)]
mod tests {
    use super::fit_text;

    // One unit per character.
    fn w(t: &str) -> f32 {
        t.chars().count() as f32
    }

    #[test]
    fn long_text_is_cut_at_the_cursor_end_only_while_editing() {
        assert_eq!(fit_text("eba.gov.tr", 20.0, true, w), "eba.gov.tr");
        assert_eq!(fit_text("https://eba.gov.tr/ders", 10.0, false, w), "https://e…");
        assert_eq!(fit_text("https://eba.gov.tr/ders", 10.0, true, w), "…v.tr/ders");
    }
}
