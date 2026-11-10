//! Online guessing bound for the 6-digit pairing PIN.
//!
//! The server's PIN is typed in once by the operator and stays the same for
//! the whole session, so without a bound a LAN peer could simply try all
//! ~900 000 values. After every [`MAX_WRONG`] wrong PINs pairing is paused —
//! 30 s, doubling each time, at most 15 min — and requests arriving during the
//! pause are refused without being compared. Worst case for an attacker:
//! 5 guesses per 15 minutes. A successful pairing clears the guard.

use std::time::{Duration, Instant};

pub const MAX_WRONG: u32 = 5;
pub const PAUSE_FIRST: Duration = Duration::from_secs(30);
pub const PAUSE_MAX: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Default)]
pub struct PairGuard {
    wrong: u32,
    strikes: u32,
    until: Option<Instant>,
}

impl PairGuard {
    /// `Some(remaining)` while pairing is paused.
    pub fn paused(&self, now: Instant) -> Option<Duration> {
        self.until.and_then(|t| t.checked_duration_since(now)).filter(|d| !d.is_zero())
    }

    /// Records a wrong PIN; returns the pause length when this one starts a pause.
    pub fn wrong(&mut self, now: Instant) -> Option<Duration> {
        self.wrong += 1;
        if self.wrong < MAX_WRONG {
            return None;
        }
        self.wrong = 0;
        self.strikes = self.strikes.saturating_add(1);
        let factor = 1u32.checked_shl(self.strikes - 1).unwrap_or(u32::MAX);
        let pause = PAUSE_FIRST.saturating_mul(factor).min(PAUSE_MAX);
        self.until = Some(now + pause);
        Some(pause)
    }

    pub fn success(&mut self) {
        *self = PairGuard::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pauses_after_five_and_doubles_up_to_the_cap() {
        let mut g = PairGuard::default();
        let t = Instant::now();
        let mut pauses = Vec::new();
        for _ in 0..8 {
            for i in 0..MAX_WRONG {
                assert!(g.paused(t).is_none() || i == 0);
                let p = g.wrong(t);
                assert_eq!(p.is_some(), i == MAX_WRONG - 1);
                if let Some(p) = p {
                    pauses.push(p.as_secs());
                }
            }
            assert!(g.paused(t).is_some());
            g.until = None; // the pause elapses
        }
        assert_eq!(pauses, vec![30, 60, 120, 240, 480, 900, 900, 900]);
    }

    #[test]
    fn pause_ends_and_success_resets() {
        let mut g = PairGuard::default();
        let t = Instant::now();
        for _ in 0..MAX_WRONG {
            g.wrong(t);
        }
        assert!(g.paused(t + Duration::from_secs(29)).is_some());
        assert!(g.paused(t + Duration::from_secs(30)).is_none());
        g.success();
        assert!(g.paused(t).is_none());
        for _ in 0..MAX_WRONG - 1 {
            assert!(g.wrong(t).is_none());
        }
        assert_eq!(g.wrong(t), Some(PAUSE_FIRST), "success must reset the doubling");
    }
}
