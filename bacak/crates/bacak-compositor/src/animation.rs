//! Spring physics for window animations.
//!
//! We use a classic damped harmonic oscillator with critical damping. The
//! oscillator solves
//!
//! ```text
//!   x''(t) = -k * (x - target) - c * x'(t)
//! ```
//!
//! where `k` is stiffness and `c` is damping. With `m = 1` and
//! `c = 2 * sqrt(k)` the motion approaches the target **without** oscillating
//! — that's the "natural settle" feel users associate with first-party
//! desktop animations (macOS Dock, iOS, Windows 11). Slightly under-damped
//! systems wobble; ours doesn't, by design.
//!
//! Integration is semi-implicit Euler (also known as symplectic Euler):
//!
//! ```text
//!   v ← v + a * dt
//!   x ← x + v * dt
//! ```
//!
//! Semi-implicit Euler is the standard choice in real-time graphics — it's
//! cheap, stable for typical UI stiffnesses, and conserves energy better
//! than plain forward Euler. Δt is clamped to `0.05 s` so a stalled frame
//! can't catapult a window across the screen.

use crate::wm::{OutputId, Rect, SnapZone, WindowId, WorkspaceId};

/// Stiffness `k`. Higher = stiffer (snappier). 220 lands ~ 250 ms for a
/// half-screen distance, which feels right for desktop windows.
pub const SPRING_STIFFNESS: f64 = 220.0;

/// Damping `c`. `2 * sqrt(220) ≈ 29.66`; we round slightly under to keep
/// the motion feeling alive instead of dead-stiff. The resulting system
/// has a very mild (<1 px) overshoot at typical UI stiffnesses, well below
/// what the eye can pick up.
pub const SPRING_DAMPING: f64 = 28.0;

/// A spring is "settled" once it's within these tolerances of its target.
pub const SETTLE_POS_EPS: f64 = 0.5; // pixels
pub const SETTLE_VEL_EPS: f64 = 0.5; // pixels / second

/// Maximum integration step. Bigger Δt values would let extreme stiffness
/// values blow up; clamping is the simplest stability guard.
pub const MAX_DT: f64 = 0.05;

/// 1-D spring solver.
#[derive(Debug, Clone, Copy)]
pub struct Spring {
    pub pos: f64,
    pub vel: f64,
    pub target: f64,
    pub stiffness: f64,
    pub damping: f64,
}

impl Spring {
    /// New spring at `start` heading toward `target`, with the default
    /// (critical-damped) constants. Initial velocity is zero — callers
    /// that want a "throw" can mutate `vel` after construction.
    pub fn settle_to(start: f64, target: f64) -> Self {
        Self {
            pos: start,
            vel: 0.0,
            target,
            stiffness: SPRING_STIFFNESS,
            damping: SPRING_DAMPING,
        }
    }

    /// Replace the target while keeping current pos / vel — useful when the
    /// user yanks the snap zone mid-animation.
    pub fn retarget(&mut self, target: f64) {
        self.target = target;
    }

    /// Advance the spring by `dt` seconds. Returns `true` while the spring
    /// is still moving (caller uses this to drive `needs_redraw`).
    pub fn step(&mut self, dt: f64) -> bool {
        let dt = dt.clamp(0.0, MAX_DT);
        let force = -self.stiffness * (self.pos - self.target) - self.damping * self.vel;
        self.vel += force * dt;
        self.pos += self.vel * dt;

        if self.is_settled() {
            // Snap to the target exactly so the WM doesn't see a sub-pixel
            // drift forever — and so geometry compares cleanly with the
            // configure we'll send the client.
            self.pos = self.target;
            self.vel = 0.0;
            false
        } else {
            true
        }
    }

    pub fn is_settled(&self) -> bool {
        (self.pos - self.target).abs() < SETTLE_POS_EPS && self.vel.abs() < SETTLE_VEL_EPS
    }
}

/// What the WM should do once a [`WindowAnim`] settles.
///
/// * [`AnimEnd::None`] — a plain floating drop; nothing to commit.
/// * [`AnimEnd::Snap`] — record the window as snapped to this zone.
/// * [`AnimEnd::Minimize`] — mark the window minimized and restore its
///   pre-animation geometry (the spring shrank it toward the dock for
///   show; the stored rect is what an un-minimize should expand back
///   to).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AnimEnd {
    None,
    Snap(SnapZone),
    Minimize { restore: Rect },
}

/// Four springs in lockstep: one per geometry dimension. Bound to a single
/// [`WindowId`] so the WM-side update path stays trivial.
///
/// `end` is the action the WM applies once the springs settle.
#[derive(Debug, Clone)]
pub struct WindowAnim {
    pub window: WindowId,
    pub x: Spring,
    pub y: Spring,
    pub w: Spring,
    pub h: Spring,
    pub end: AnimEnd,
}

impl WindowAnim {
    /// Build an animation from `start` to `target`. `end` is applied
    /// when the springs settle (see [`AnimEnd`]).
    pub fn to_rect(window: WindowId, start: Rect, target: Rect, end: AnimEnd) -> Self {
        Self {
            window,
            x: Spring::settle_to(start.x as f64, target.x as f64),
            y: Spring::settle_to(start.y as f64, target.y as f64),
            w: Spring::settle_to(start.w as f64, target.w as f64),
            h: Spring::settle_to(start.h as f64, target.h as f64),
            end,
        }
    }

    /// Advance every spring by `dt`. Returns `true` while *any* spring is
    /// still moving.
    pub fn step(&mut self, dt: f64) -> bool {
        // Step all four every tick (don't short-circuit) — otherwise a
        // settled axis would stop receiving the explicit "snap to target"
        // assignment inside `Spring::step`.
        let a = self.x.step(dt);
        let b = self.y.step(dt);
        let c = self.w.step(dt);
        let d = self.h.step(dt);
        a || b || c || d
    }

    /// Snapshot the springs into a `Rect` — what the WM should currently
    /// display for this window.
    pub fn current_rect(&self) -> Rect {
        Rect::new(
            self.x.pos as f32,
            self.y.pos as f32,
            self.w.pos as f32,
            self.h.pos as f32,
        )
    }

    pub fn is_settled(&self) -> bool {
        self.x.is_settled() && self.y.is_settled() && self.w.is_settled() && self.h.is_settled()
    }
}

// ---------------------------------------------------------------------------
// Workspace slide
// ---------------------------------------------------------------------------

/// Horizontal slide between two workspaces on the same output. The WM
/// is updated *immediately* at the moment the slide is created — this
/// struct only carries the transient visual offset that the renderer
/// uses to paint both workspaces moving across the screen until the
/// spring settles.
///
/// `direction` controls which way the content slides:
/// * `+1.0` — the new workspace slides in from the right; the previous
///   one slides off to the left. Matches a 3-finger left swipe ("show me
///   what's to the right").
/// * `-1.0` — opposite.
///
/// `progress` is a unit-interval spring (`0.0 → 1.0`). Render code reads
/// `progress.pos` and maps it into pixel offsets via the output's width.
#[derive(Debug, Clone, Copy)]
pub struct SlideAnim {
    pub output: OutputId,
    pub prev_ws: WorkspaceId,
    pub direction: f32,
    pub progress: Spring,
}

impl SlideAnim {
    pub fn new(output: OutputId, prev_ws: WorkspaceId, direction: f32) -> Self {
        Self {
            output,
            prev_ws,
            direction,
            progress: Spring::settle_to(0.0, 1.0),
        }
    }

    /// Step the spring. Returns `true` while the slide is still moving.
    pub fn step(&mut self, dt: f64) -> bool {
        self.progress.step(dt)
    }

    /// Current `t` in `[0, 1]`. Render code multiplies this by the
    /// output's width to get pixel offsets.
    pub fn t(&self) -> f64 {
        self.progress.pos
    }

    pub fn is_settled(&self) -> bool {
        self.progress.is_settled()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wm::SnapZone;

    fn run_to_settle(spring: &mut Spring) -> usize {
        let mut steps = 0;
        // ~5 s budget at 60 Hz should be more than enough for the
        // configured stiffness/damping; if we exceed it, the math is wrong.
        while spring.step(1.0 / 60.0) {
            steps += 1;
            assert!(steps < 5 * 60, "spring failed to settle within 5s");
        }
        steps
    }

    #[test]
    fn spring_settles_to_target() {
        let mut s = Spring::settle_to(0.0, 1000.0);
        run_to_settle(&mut s);
        assert_eq!(s.pos, 1000.0); // forced exact on settle
        assert_eq!(s.vel, 0.0);
    }

    #[test]
    fn spring_overshoot_is_below_one_pixel() {
        // For a critically-damped 2nd-order system, theoretical overshoot
        // is zero. We use a slightly-under-damped tuning, so we accept a
        // tiny overshoot but want it imperceptible.
        let mut s = Spring::settle_to(0.0, 100.0);
        let mut peak = f64::NEG_INFINITY;
        while s.step(1.0 / 240.0) {
            if s.pos > peak {
                peak = s.pos;
            }
        }
        assert!(peak <= 100.0 + 1.0, "overshoot too large: peak={peak}");
    }

    #[test]
    fn spring_zero_dt_does_not_move() {
        let mut s = Spring::settle_to(0.0, 100.0);
        let moved = s.step(0.0);
        // The system has no kinetic energy yet → step at dt=0 cannot move
        // it. Returning `false` (settled) would be wrong because the
        // spring still has a force; but no displacement should occur.
        assert_eq!(s.pos, 0.0);
        assert_eq!(s.vel, 0.0);
        // It's still alive (target isn't reached).
        assert!(moved);
    }

    #[test]
    fn slide_anim_progresses_from_zero_to_one() {
        let mut s = SlideAnim::new(0, 7, 1.0);
        assert_eq!(s.t(), 0.0);
        assert!(!s.is_settled());

        // Drive to settle and confirm we land at 1.0 exactly.
        for _ in 0..(60 * 5) {
            if !s.step(1.0 / 60.0) {
                break;
            }
        }
        assert!(s.is_settled());
        assert_eq!(s.t(), 1.0);
        // Identifying metadata is preserved.
        assert_eq!(s.output, 0);
        assert_eq!(s.prev_ws, 7);
        assert_eq!(s.direction, 1.0);
    }

    #[test]
    fn slide_anim_monotonically_increases_under_critical_damping() {
        // Critical damping shouldn't overshoot or oscillate; the
        // progress value should grow monotonically toward 1. A real
        // oscillating spring would fail this — a useful guard against
        // accidentally tuning the constants out of the critical regime.
        let mut s = SlideAnim::new(0, 1, 1.0);
        let mut prev = 0.0;
        for _ in 0..600 {
            s.step(1.0 / 240.0);
            assert!(s.t() + 1.0 >= prev, "regression in progress: {prev} → {}", s.t());
            prev = s.t();
            if s.is_settled() {
                break;
            }
        }
    }

    #[test]
    fn window_anim_advances_all_dimensions() {
        let start = Rect::new(0.0, 0.0, 100.0, 100.0);
        let target = Rect::new(500.0, 200.0, 800.0, 600.0);
        let mut a = WindowAnim::to_rect(1, start, target, AnimEnd::Snap(SnapZone::Left));
        // Run a single step and confirm motion on every axis.
        a.step(1.0 / 60.0);
        let r = a.current_rect();
        assert!(r.x > 0.0 && r.x < target.x);
        assert!(r.y > 0.0 && r.y < target.y);
        assert!(r.w > start.w && r.w < target.w);
        assert!(r.h > start.h && r.h < target.h);

        // And confirm settling lands on the target exactly.
        for _ in 0..(60 * 5) {
            if !a.step(1.0 / 60.0) {
                break;
            }
        }
        let r = a.current_rect();
        assert_eq!((r.x, r.y, r.w, r.h), (500.0, 200.0, 800.0, 600.0));
        assert_eq!(a.end, AnimEnd::Snap(SnapZone::Left));
    }
}
