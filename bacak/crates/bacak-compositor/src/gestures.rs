//! Touch gesture recognition for Android-style text/image selection.
//!
//! This is the **single-finger** selection recogniser — distinct from the
//! multi-finger [`crate::input::TouchAggregator`], which classifies 3-/4-finger
//! workspace and overview swipes. The two read the same raw touch stream but
//! own different semantics, so they're kept apart: the caller feeds *this*
//! recogniser only while a sequence is single-finger.
//!
//! It turns per-slot `down` / `motion` / `up` (plus a per-frame [`tick`]) into
//! high-level [`SelectionGesture`]s:
//!
//! * **Long-press** — finger held still past [`LONG_PRESS_MS`]. Emitted from
//!   [`tick`] *while the finger is still down* (Android shows the menu mid-hold,
//!   not on release), exactly once per sequence.
//! * **Tap / double-tap / triple-tap** — quick stationary touches; the multi-tap
//!   count grows while successive taps land close together within
//!   [`MULTI_TAP_MS`]. Double = word select, triple = paragraph select upstream.
//!
//! A sequence that moves past [`SLOP`] is a drag, not a tap, and emits nothing
//! here (drag-select / handle-drag live in a later slice). Pure and fully unit
//! tested — no Smithay, no clock; the caller supplies monotonic millis.
//!
//! [`tick`]: SelectionRecognizer::tick

/// Movement past this many logical px turns a touch into a drag (cancels the
/// pending tap/long-press). Fingers are imprecise, so this is looser than the
/// 8 px the multi-finger aggregator uses.
const SLOP: f32 = 14.0;
/// Hold duration before a stationary finger becomes a long-press.
const LONG_PRESS_MS: u64 = 500;
/// Maximum gap between taps for them to count as a multi-tap.
const MULTI_TAP_MS: u64 = 320;
/// A follow-up tap further than this from the previous one starts a fresh
/// count rather than extending the multi-tap.
const MULTI_TAP_SLOP: f32 = 36.0;

/// A recognised single-finger selection gesture, carrying the touch point in
/// the same (WM-global, logical) coordinate space the caller fed in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SelectionGesture {
    /// One quick stationary touch.
    Tap { x: f32, y: f32 },
    /// Two quick stationary touches close together → select word.
    DoubleTap { x: f32, y: f32 },
    /// Three (or more) → select paragraph / line.
    TripleTap { x: f32, y: f32 },
    /// Finger held still past [`LONG_PRESS_MS`] → open the selection menu.
    LongPress { x: f32, y: f32 },
}

#[derive(Debug, Clone, Copy)]
struct Touch {
    start_x: f32,
    start_y: f32,
    start_ms: u64,
}

/// Stateful single-finger selection recogniser. See the module docs.
#[derive(Debug, Default)]
pub struct SelectionRecognizer {
    /// The live touch, if a finger is down.
    touch: Option<Touch>,
    /// Set once the live touch travels past [`SLOP`] — it's a drag now.
    dragged: bool,
    /// Set once [`tick`] has emitted the long-press for this sequence, so it
    /// fires at most once and the matching `up` stays silent.
    ///
    /// [`tick`]: SelectionRecognizer::tick
    long_pressed: bool,
    /// Last completed tap `(x, y, ms)`, for multi-tap chaining.
    last_tap: Option<(f32, f32, u64)>,
    /// Current multi-tap run length (1 = single, 2 = double, ≥3 = triple).
    tap_count: u8,
}

impl SelectionRecognizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// A finger landed. Begins a fresh sequence.
    pub fn down(&mut self, x: f32, y: f32, now_ms: u64) {
        self.touch = Some(Touch { start_x: x, start_y: y, start_ms: now_ms });
        self.dragged = false;
        self.long_pressed = false;
    }

    /// The live finger moved. Crossing [`SLOP`] promotes the sequence to a drag,
    /// which suppresses both the long-press and the tap.
    pub fn motion(&mut self, x: f32, y: f32) {
        if let Some(t) = self.touch {
            if dist(t.start_x, t.start_y, x, y) > SLOP {
                self.dragged = true;
            }
        }
    }

    /// Per-frame poll. Returns [`SelectionGesture::LongPress`] the first time a
    /// still-held, un-dragged finger crosses [`LONG_PRESS_MS`]; `None`
    /// otherwise. Drive this from the render tick (it's idempotent once fired).
    /// Only the compositor's own text fields and X11 (no-native-touch) windows
    /// act on it — Wayland apps keep their own long-press.
    pub fn tick(&mut self, now_ms: u64) -> Option<SelectionGesture> {
        let t = self.touch?;
        if self.dragged || self.long_pressed {
            return None;
        }
        if now_ms.saturating_sub(t.start_ms) >= LONG_PRESS_MS {
            self.long_pressed = true;
            return Some(SelectionGesture::LongPress { x: t.start_x, y: t.start_y });
        }
        None
    }

    /// The finger lifted. Emits a tap (or double/triple) for a quick stationary
    /// touch; nothing for a drag or an already-fired long-press. The tap point
    /// is the touch's *start* — a tap barely moves, and platform `TouchUp`
    /// events carry no coordinates, so callers needn't supply one.
    pub fn up(&mut self, now_ms: u64) -> Option<SelectionGesture> {
        let t = self.touch.take()?;
        if self.long_pressed || self.dragged {
            // A long-press already spoke, or it was a drag — not a tap. Reset
            // the multi-tap chain so a later tap starts clean.
            self.last_tap = None;
            self.tap_count = 0;
            return None;
        }
        let (x, y) = (t.start_x, t.start_y);
        // Stationary, quick: it's a tap. Chain with the previous one if it was
        // recent and nearby, otherwise start a new run.
        let chained = matches!(
            self.last_tap,
            Some((lx, ly, lt))
                if now_ms.saturating_sub(lt) <= MULTI_TAP_MS
                    && dist(lx, ly, x, y) <= MULTI_TAP_SLOP
        );
        self.tap_count = if chained { self.tap_count + 1 } else { 1 };
        self.last_tap = Some((x, y, now_ms));
        Some(match self.tap_count {
            1 => SelectionGesture::Tap { x, y },
            2 => SelectionGesture::DoubleTap { x, y },
            _ => SelectionGesture::TripleTap { x, y },
        })
    }

    /// Abort the live sequence without emitting (libinput `TouchCancel`, focus
    /// loss). Leaves the multi-tap chain intact — a cancel isn't a tap, but it
    /// also shouldn't reset a legitimate in-progress double-tap timing window.
    pub fn cancel(&mut self) {
        self.touch = None;
        self.dragged = false;
        self.long_pressed = false;
    }
}

fn dist(ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let dx = ax - bx;
    let dy = ay - by;
    (dx * dx + dy * dy).sqrt()
}

// ---------------------------------------------------------------------------
// Two-finger window gesture recogniser
// ---------------------------------------------------------------------------

/// Centroid travel (logical px) that promotes a two-finger touch from a tap to
/// a window move.
const TWO_FINGER_SLOP: f32 = 12.0;
/// Longest a two-finger touch can last and still count as a tap, not a drag.
const TWO_FINGER_TAP_MS: u64 = 280;
/// Longest gap between two two-finger taps to chain them into a double-tap.
const TWO_FINGER_DOUBLE_MS: u64 = 340;

/// What a two-finger sequence produced this event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TwoFingerOut {
    /// Move the target window to its drag-start geometry **plus** this delta
    /// (the centroid's displacement since both fingers first landed).
    MoveTo { dx: f32, dy: f32 },
    /// A two-finger double-tap completed — toggle fullscreen / restore.
    DoubleTap,
}

/// Recognises two-finger window gestures: drag the two-finger centroid to move
/// a window, double-tap with two fingers to toggle fullscreen. Distinct from
/// the single-finger [`SelectionRecognizer`] and the 3-/4-finger
/// [`crate::input::TouchAggregator`]. Pure + unit-tested; the integration layer
/// ([`crate::state::BacakState`]) maps the output onto the target window.
#[derive(Debug, Default)]
pub struct TwoFingerRecognizer {
    /// Active fingers as `(slot, x, y)` in landing order.
    fingers: Vec<(i32, f32, f32)>,
    /// Centroid when both fingers first landed — the move origin.
    start_centroid: Option<(f32, f32)>,
    /// When the two-finger phase began (for tap-duration timing).
    down_ms: u64,
    /// Set once the centroid passes [`TWO_FINGER_SLOP`] — it's a move now.
    moved: bool,
    /// A third finger landed: this is a workspace/overview gesture, not ours.
    cancelled: bool,
    /// Time of the last completed two-finger tap, for double-tap chaining.
    last_tap_ms: Option<u64>,
}

impl TwoFingerRecognizer {
    pub fn new() -> Self {
        Self::default()
    }

    fn centroid(&self) -> Option<(f32, f32)> {
        if self.fingers.len() != 2 {
            return None;
        }
        let a = self.fingers[0];
        let b = self.fingers[1];
        Some(((a.1 + b.1) / 2.0, (a.2 + b.2) / 2.0))
    }

    /// The current two-finger centroid, if exactly two fingers are down. The
    /// integration uses it to pick the target window when the phase begins.
    pub fn live_centroid(&self) -> Option<(f32, f32)> {
        self.centroid()
    }

    /// `true` while a genuine two-finger gesture is active (exactly two fingers,
    /// not escalated to a third).
    pub fn is_active(&self) -> bool {
        self.fingers.len() == 2 && !self.cancelled
    }

    /// A finger landed. The two-finger phase starts when the second lands; a
    /// third cancels it (it belongs to the workspace/overview aggregator).
    pub fn down(&mut self, slot: i32, x: f32, y: f32, now_ms: u64) {
        if let Some(f) = self.fingers.iter_mut().find(|f| f.0 == slot) {
            f.1 = x;
            f.2 = y;
        } else {
            self.fingers.push((slot, x, y));
        }
        match self.fingers.len() {
            2 => {
                self.start_centroid = self.centroid();
                self.down_ms = now_ms;
                self.moved = false;
                self.cancelled = false;
            }
            n if n > 2 => self.cancelled = true,
            _ => {}
        }
    }

    /// A finger moved. Returns [`TwoFingerOut::MoveTo`] once the centroid has
    /// travelled past the slop, then on every subsequent move.
    pub fn motion(&mut self, slot: i32, x: f32, y: f32) -> Option<TwoFingerOut> {
        if let Some(f) = self.fingers.iter_mut().find(|f| f.0 == slot) {
            f.1 = x;
            f.2 = y;
        }
        if self.cancelled {
            return None;
        }
        let (cx, cy) = self.centroid()?;
        let (sx, sy) = self.start_centroid?;
        let (dx, dy) = (cx - sx, cy - sy);
        if !self.moved && (dx * dx + dy * dy).sqrt() > TWO_FINGER_SLOP {
            self.moved = true;
        }
        self.moved.then_some(TwoFingerOut::MoveTo { dx, dy })
    }

    /// A finger lifted. Returns [`TwoFingerOut::DoubleTap`] when this completes a
    /// second stationary two-finger tap shortly after the first.
    pub fn up(&mut self, slot: i32, now_ms: u64) -> Option<TwoFingerOut> {
        let was_two = self.fingers.len() == 2;
        self.fingers.retain(|f| f.0 != slot);

        let mut out = None;
        // A two-finger tap = both fingers landed and one lifts again quickly,
        // without the centroid having moved. Only the 2→1 transition counts.
        if was_two
            && !self.cancelled
            && !self.moved
            && now_ms.saturating_sub(self.down_ms) <= TWO_FINGER_TAP_MS
        {
            match self.last_tap_ms {
                Some(prev) if now_ms.saturating_sub(prev) <= TWO_FINGER_DOUBLE_MS => {
                    out = Some(TwoFingerOut::DoubleTap);
                    self.last_tap_ms = None;
                }
                _ => self.last_tap_ms = Some(now_ms),
            }
        }
        if self.fingers.len() < 2 {
            self.start_centroid = None;
            self.moved = false;
            self.cancelled = false;
        }
        out
    }

    /// Drop all per-gesture state (libinput `TouchCancel`).
    pub fn cancel(&mut self) {
        self.fingers.clear();
        self.start_centroid = None;
        self.moved = false;
        self.cancelled = false;
        self.last_tap_ms = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stationary_hold_emits_long_press_once_mid_hold() {
        let mut r = SelectionRecognizer::new();
        r.down(100.0, 100.0, 0);
        assert_eq!(r.tick(400), None, "not yet past the threshold");
        assert_eq!(
            r.tick(500),
            Some(SelectionGesture::LongPress { x: 100.0, y: 100.0 }),
            "fires at the threshold"
        );
        assert_eq!(r.tick(900), None, "only once per sequence");
        // The matching release must stay silent (the long-press already spoke).
        assert_eq!(r.up(950), None);
    }

    #[test]
    fn movement_past_slop_cancels_long_press() {
        let mut r = SelectionRecognizer::new();
        r.down(100.0, 100.0, 0);
        r.motion(100.0, 120.0); // 20 px > SLOP
        assert_eq!(r.tick(600), None, "a drag never long-presses");
        assert_eq!(r.up(650), None, "a drag is not a tap");
    }

    #[test]
    fn quick_stationary_touch_is_a_tap() {
        let mut r = SelectionRecognizer::new();
        r.down(50.0, 50.0, 0);
        assert_eq!(r.up(80), Some(SelectionGesture::Tap { x: 50.0, y: 50.0 }));
    }

    #[test]
    fn two_close_quick_taps_make_a_double_tap() {
        let mut r = SelectionRecognizer::new();
        r.down(50.0, 50.0, 0);
        assert_eq!(r.up(60), Some(SelectionGesture::Tap { x: 50.0, y: 50.0 }));
        r.down(52.0, 51.0, 200);
        assert_eq!(
            r.up(240),
            Some(SelectionGesture::DoubleTap { x: 52.0, y: 51.0 })
        );
    }

    #[test]
    fn three_close_quick_taps_make_a_triple_tap() {
        let mut r = SelectionRecognizer::new();
        r.down(50.0, 50.0, 0);
        r.up(40);
        r.down(50.0, 50.0, 150);
        r.up(190);
        r.down(50.0, 50.0, 300);
        assert_eq!(
            r.up(340),
            Some(SelectionGesture::TripleTap { x: 50.0, y: 50.0 })
        );
    }

    #[test]
    fn slow_second_tap_starts_a_new_single_tap() {
        let mut r = SelectionRecognizer::new();
        r.down(50.0, 50.0, 0);
        r.up(40);
        // Gap exceeds MULTI_TAP_MS → not a double.
        r.down(50.0, 50.0, 500);
        assert_eq!(r.up(540), Some(SelectionGesture::Tap { x: 50.0, y: 50.0 }));
    }

    #[test]
    fn far_second_tap_starts_a_new_single_tap() {
        let mut r = SelectionRecognizer::new();
        r.down(50.0, 50.0, 0);
        r.up(40);
        // Within time but far away → not a double.
        r.down(400.0, 400.0, 200);
        assert_eq!(
            r.up(240),
            Some(SelectionGesture::Tap { x: 400.0, y: 400.0 })
        );
    }

    #[test]
    fn cancel_drops_the_live_touch() {
        let mut r = SelectionRecognizer::new();
        r.down(50.0, 50.0, 0);
        r.cancel();
        assert_eq!(r.tick(600), None);
        assert_eq!(r.up(650), None);
    }

    // --- two-finger recogniser ------------------------------------------

    #[test]
    fn two_finger_drag_moves_by_centroid_delta() {
        let mut r = TwoFingerRecognizer::new();
        r.down(0, 100.0, 100.0, 0);
        r.down(1, 200.0, 100.0, 5); // centroid (150,100)
        // Small jitter under slop → no move yet.
        assert_eq!(r.motion(0, 104.0, 100.0), None);
        // Both fingers shift right by 60 → centroid +30 in x, past slop.
        assert_eq!(r.motion(0, 160.0, 100.0), Some(TwoFingerOut::MoveTo { dx: 30.0, dy: 0.0 }));
        let _ = r.motion(1, 260.0, 100.0); // centroid (210,100) → dx 60
        assert_eq!(r.motion(1, 260.0, 100.0), Some(TwoFingerOut::MoveTo { dx: 60.0, dy: 0.0 }));
        assert!(r.is_active());
    }

    #[test]
    fn two_finger_double_tap_emits_once() {
        let mut r = TwoFingerRecognizer::new();
        // First stationary two-finger tap.
        r.down(0, 100.0, 100.0, 0);
        r.down(1, 200.0, 100.0, 0);
        assert_eq!(r.up(0, 50), None); // first tap registered
        assert_eq!(r.up(1, 55), None);
        // Second tap shortly after → double-tap.
        r.down(0, 100.0, 100.0, 100);
        r.down(1, 200.0, 100.0, 100);
        assert_eq!(r.up(0, 150), Some(TwoFingerOut::DoubleTap));
        assert_eq!(r.up(1, 155), None);
    }

    #[test]
    fn two_finger_drag_is_not_a_tap() {
        let mut r = TwoFingerRecognizer::new();
        r.down(0, 100.0, 100.0, 0);
        r.down(1, 200.0, 100.0, 0);
        let _ = r.motion(0, 200.0, 100.0); // moved well past slop
        assert_eq!(r.up(0, 50), None); // a move never counts as a tap
        let _ = r.up(1, 55);
    }

    #[test]
    fn third_finger_cancels_two_finger() {
        let mut r = TwoFingerRecognizer::new();
        r.down(0, 100.0, 100.0, 0);
        r.down(1, 200.0, 100.0, 0);
        r.down(2, 300.0, 100.0, 0); // third finger → workspace/overview territory
        assert!(!r.is_active());
        assert_eq!(r.motion(0, 400.0, 100.0), None); // no window move
    }
}
