//! Pure carousel kinematics for the Recent-Apps Overview — no Smithay, so the
//! kinetic math (selection, fling + snap, overscroll, per-card transform,
//! hit-test) is unit-testable headless, like [`crate::focus`].
//!
//! Scroll is tracked in pixels by a critically-damped [`crate::animation::Spring`]
//! living on the `Overview`: its `pos` is the horizontal offset of the centred
//! card from index 0. `pos / spacing` rounds to the selected index. Inertia is
//! the spring's own velocity (injected on release), snap is `retarget`, and the
//! per-frame update is `step` — so "fling to the nearest card" is one primitive.
//!
//! **Uniform cards (2026-05-26).** Every card renders at the *same* fixed outer
//! size — there is no centre-enlarge / side-shrink. The focused card is marked
//! by the accent ring only (see `render::render_overview`), never by scaling.
//! Because cards no longer shrink, the step between them ([`spacing`]) must be
//! at least a card width + gap so full-size frames don't overlap; it is derived
//! from the output bounds rather than a fixed constant.

use crate::wm::Rect;

/// Card frame width as a fraction of the output width. Fixed for every card.
pub const CARD_W_FRAC: f32 = 0.36;
/// Card frame height as a fraction of the output height. Fixed for every card.
pub const CARD_H_FRAC: f32 = 0.52;
/// Gap between adjacent card frames, as a fraction of the output width. The
/// centre-to-centre step ([`spacing`]) is `(CARD_W_FRAC + GAP_FRAC) * width`,
/// guaranteeing uniform full-size frames never overlap and spacing is equal.
pub const GAP_FRAC: f32 = 0.05;
/// Seconds of release velocity projected forward to pick the snap target.
pub const FLING_SECONDS: f64 = 0.22;
/// Rubber-band factor for dragging past the first / last card.
pub const OVERSCROLL: f64 = 0.3;

/// Horizontal distance (px) between adjacent card centres for an output of
/// these `bounds`. Equals one card width plus the inter-card gap, so the
/// uniform full-size frames sit edge-to-edge-plus-gap with equal spacing.
pub fn spacing(bounds: Rect) -> f64 {
    ((CARD_W_FRAC + GAP_FRAC) * bounds.w) as f64
}

/// Which axis a single-finger / mouse drag committed to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DragAxis {
    Undecided,
    Horizontal,
    Vertical,
}

/// Per-card render geometry derived from the scroll offset.
#[derive(Clone, Copy, Debug)]
pub struct CardT {
    pub rect: Rect,
    pub scale: f32,
    pub opacity: f32,
    /// `|index − centred index|`, clamped — 0 at the centre. Render uses it for
    /// z-order (smaller = on top) and to pick the focus ring.
    pub dist: f32,
}

/// Scroll px that centres card `i`, given the output's `spacing`.
pub fn index_scroll(i: usize, spacing: f64) -> f64 {
    i as f64 * spacing
}

/// The card index nearest the current scroll position.
pub fn selected(scroll_px: f64, n: usize, spacing: f64) -> usize {
    if n == 0 || spacing <= 0.0 {
        return 0;
    }
    ((scroll_px / spacing).round() as i64).clamp(0, n as i64 - 1) as usize
}

/// Clamp a raw drag position, rubber-banding past the ends.
pub fn clamp_overscroll(raw: f64, n: usize, spacing: f64) -> f64 {
    if n == 0 {
        return 0.0;
    }
    let hi = (n as f64 - 1.0) * spacing;
    if raw < 0.0 {
        raw * OVERSCROLL
    } else if raw > hi {
        hi + (raw - hi) * OVERSCROLL
    } else {
        raw
    }
}

/// Snap-target index after a fling: project the release velocity forward and
/// round to the nearest card. `velocity_px` is finger px/s (positive = moving
/// right, which scrolls toward lower indices).
pub fn fling_target(scroll_px: f64, velocity_px: f64, n: usize, spacing: f64) -> usize {
    if n == 0 || spacing <= 0.0 {
        return 0;
    }
    let predicted = scroll_px - velocity_px * FLING_SECONDS;
    ((predicted / spacing).round() as i64).clamp(0, n as i64 - 1) as usize
}

/// Render geometry for card `i` given the scroll offset and output bounds.
///
/// **Uniform:** the frame is the same fixed size for every card — only the
/// horizontal position changes with the scroll. `scale` is always `1.0` and
/// `opacity` always `1.0`; the focused card is distinguished by the accent
/// ring in the renderer, never by size. `dist` is still reported so the
/// renderer can z-order (centre on top) and cull off-screen cards.
pub fn card_transform(i: usize, scroll_px: f64, bounds: Rect) -> CardT {
    let spacing = spacing(bounds);
    let dx = i as f64 * spacing - scroll_px; // px from screen centre
    let dist = if spacing > 0.0 {
        ((dx / spacing).abs() as f32).min(3.0)
    } else {
        0.0
    };
    let w = CARD_W_FRAC * bounds.w; // identical for every card
    let h = CARD_H_FRAC * bounds.h;
    let cx = bounds.x + bounds.w / 2.0 + dx as f32;
    let cy = bounds.y + bounds.h / 2.0;
    CardT {
        rect: Rect::new(cx - w / 2.0, cy - h / 2.0, w, h),
        scale: 1.0,
        opacity: 1.0,
        dist,
    }
}

/// The topmost card index under `(x, y)`, if any — the inverse of
/// [`card_transform`]. Nearest-to-centre wins when cards overlap.
pub fn hit_card(x: f32, y: f32, scroll_px: f64, n: usize, bounds: Rect) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for i in 0..n {
        let t = card_transform(i, scroll_px, bounds);
        if t.rect.contains(x, y) && best.map(|(_, d)| t.dist < d).unwrap_or(true) {
            best = Some((i, t.dist));
        }
    }
    best.map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: Rect = Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 };

    fn sp() -> f64 {
        spacing(B)
    }

    #[test]
    fn selected_rounds_to_nearest_and_clamps() {
        let s = sp();
        assert_eq!(selected(0.0, 5, s), 0);
        assert_eq!(selected(s * 0.49, 5, s), 0);
        assert_eq!(selected(s * 0.51, 5, s), 1);
        assert_eq!(selected(s * 99.0, 5, s), 4); // clamp to last
        assert_eq!(selected(-s * 5.0, 5, s), 0); // clamp to first
    }

    #[test]
    fn overscroll_rubber_bands_past_ends() {
        let s = sp();
        // Inside the range is identity.
        assert_eq!(clamp_overscroll(s, 5, s), s);
        // Past the start: compressed toward 0.
        assert!(clamp_overscroll(-100.0, 5, s) > -100.0);
        assert!(clamp_overscroll(-100.0, 5, s) < 0.0);
        // Past the end: compressed.
        let hi = 4.0 * s;
        assert!(clamp_overscroll(hi + 100.0, 5, s) < hi + 100.0);
        assert!(clamp_overscroll(hi + 100.0, 5, s) > hi);
    }

    #[test]
    fn fling_follows_velocity_direction() {
        let s = sp();
        // Centred on card 2, flung leftward (finger moves left => +index).
        let mid = index_scroll(2, s);
        let left_fling = fling_target(mid, -2000.0, 5, s); // negative px/s
        assert!(left_fling > 2, "leftward fling should advance index");
        let right_fling = fling_target(mid, 2000.0, 5, s);
        assert!(right_fling < 2, "rightward fling should retreat index");
    }

    #[test]
    fn every_card_has_identical_outer_size() {
        // Uniform frame: size never depends on the card's distance from centre.
        let s = sp();
        let scroll = index_scroll(2, s);
        let centred = card_transform(2, scroll, B);
        let neighbour = card_transform(3, scroll, B);
        let far = card_transform(0, scroll, B);
        for t in [centred, neighbour, far] {
            assert_eq!(t.scale, 1.0, "no per-card scaling");
            assert_eq!(t.opacity, 1.0, "no per-card fade");
            assert_eq!(t.rect.w, CARD_W_FRAC * B.w);
            assert_eq!(t.rect.h, CARD_H_FRAC * B.h);
        }
        // The centred card is still nearest (on top), just not bigger.
        assert!(centred.dist < neighbour.dist);
    }

    #[test]
    fn uniform_cards_do_not_overlap() {
        // The step must leave a positive gap between adjacent full-size frames.
        let s = sp();
        let a = card_transform(0, 0.0, B);
        let b = card_transform(1, 0.0, B);
        assert!(b.rect.x > a.rect.x + a.rect.w, "frames must not overlap");
    }

    #[test]
    fn hit_card_picks_centre_when_overlapping() {
        let s = sp();
        // Point at the screen centre lands on the centred card (index 1 here).
        let scroll = index_scroll(1, s);
        assert_eq!(hit_card(960.0, 540.0, scroll, 3, B), Some(1));
    }
}
