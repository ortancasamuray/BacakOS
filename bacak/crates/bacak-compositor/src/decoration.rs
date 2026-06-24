//! Server-side decoration geometry — pure, no Smithay, unit-testable like
//! [`crate::carousel`] / [`crate::focus`].
//!
//! When a toplevel negotiates `Mode::ServerSide` (see the `XdgDecorationHandler`
//! in `handlers.rs`), the compositor draws a title bar for it. The WM still
//! tracks the **content** rect (`Window.geom`); the title bar is laid out
//! directly *above* that content, so floating windows need no geometry change
//! and only maximise/snap reserve `BAR_H` at the top of their region.

use crate::wm::Rect;

/// Title-bar height in logical px.
pub const BAR_H: f32 = 32.0;
/// Square close-button side.
pub const BTN: f32 = 22.0;
/// Inset of the close button from the bar's right/top edges.
pub const BTN_MARGIN: f32 = 5.0;

/// The title bar for a window whose **content** is `content`: a strip of
/// height [`BAR_H`] directly above it.
pub fn bar_rect(content: Rect) -> Rect {
    Rect::new(content.x, content.y - BAR_H, content.w, BAR_H)
}

/// The close button: a square pinned to the right of the title bar.
pub fn close_rect(content: Rect) -> Rect {
    let bar = bar_rect(content);
    Rect::new(
        bar.x + bar.w - BTN - BTN_MARGIN,
        bar.y + (BAR_H - BTN) / 2.0,
        BTN,
        BTN,
    )
}

/// What a pointer at `(px, py)` hit on a decorated window's chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoHit {
    /// The close button — release closes the window.
    Close,
    /// The draggable part of the title bar — press starts a move, a
    /// double-press maximises.
    Drag,
    /// Not on the decoration (the content, or elsewhere).
    None,
}

/// Hit-test the decoration of a window whose content is `content`. The close
/// button takes precedence over the rest of the bar.
pub fn hit(content: Rect, px: f32, py: f32) -> DecoHit {
    if close_rect(content).contains(px, py) {
        DecoHit::Close
    } else if bar_rect(content).contains(px, py) {
        DecoHit::Drag
    } else {
        DecoHit::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content() -> Rect {
        Rect::new(100.0, 200.0, 640.0, 480.0)
    }

    #[test]
    fn bar_sits_directly_above_content() {
        let b = bar_rect(content());
        assert_eq!(b.x, 100.0);
        assert_eq!(b.y, 200.0 - BAR_H);
        assert_eq!(b.w, 640.0);
        assert_eq!(b.h, BAR_H);
        // Bar's bottom edge meets the content's top edge.
        assert!((b.y + b.h - 200.0).abs() < 1e-3);
    }

    #[test]
    fn close_button_is_inside_the_bar_on_the_right() {
        let c = content();
        let bar = bar_rect(c);
        let btn = close_rect(c);
        assert!(btn.x >= bar.x && btn.x + btn.w <= bar.x + bar.w);
        assert!(btn.y >= bar.y && btn.y + btn.h <= bar.y + bar.h);
        // It's on the right half.
        assert!(btn.x > bar.x + bar.w / 2.0);
    }

    #[test]
    fn hit_test_close_beats_drag_beats_none() {
        let c = content();
        let btn = close_rect(c);
        // Centre of the close button → Close.
        assert_eq!(hit(c, btn.x + btn.w / 2.0, btn.y + btn.h / 2.0), DecoHit::Close);
        // Left of the bar (clear of the button) → Drag.
        assert_eq!(hit(c, c.x + 10.0, c.y - BAR_H / 2.0), DecoHit::Drag);
        // Inside the content (below the bar) → None.
        assert_eq!(hit(c, c.x + 10.0, c.y + 10.0), DecoHit::None);
        // Above the bar → None.
        assert_eq!(hit(c, c.x + 10.0, c.y - BAR_H - 5.0), DecoHit::None);
    }
}
