//! Touch-to-photon latency compensation via short-horizon input extrapolation.
//!
//! Predicted points are for rendering only — never written to permanent
//! stroke history. Callers must call [`TouchPredictor::predict`] fresh each
//! frame and discard the result once drawn.

use glam::Vec2;

/// A real sample older than this means the pointer stopped: no prediction.
const STALE_SECS: f64 = 0.04;
/// Shortest time span a velocity is measured over.
const MIN_VELOCITY_DT: f64 = 0.004;
/// Upper bound on how far ahead of the last real sample the tail reaches
/// (canvas px) — a few pixels hide latency; more reads as a stray line.
const MAX_PREDICT_DIST: f32 = 12.0;

/// A single timestamped input sample (mouse or touch position).
#[derive(Clone, Copy, Debug)]
pub struct TouchSample {
    pub position: Vec2,
    /// Seconds, monotonic (e.g. `Instant::elapsed().as_secs_f64()`).
    pub timestamp: f64,
}

/// Estimates where the pointer will be `lookahead_ms` in the future using
/// velocity + acceleration extrapolation (a truncated Taylor series), with
/// damping and a reversal guard to avoid overshoot on sudden stops/turns.
pub struct TouchPredictor {
    history: [Option<TouchSample>; 3],
    lookahead_ms: f32,
    /// 0.0 = no prediction, 1.0 = full extrapolation.
    damping: f32,
    /// Below this speed (px/s) we don't bother extrapolating — avoids jitter
    /// on a near-stationary pointer.
    velocity_threshold: f32,
}

impl TouchPredictor {
    pub fn new(lookahead_ms: f32) -> Self {
        Self {
            history: [None; 3],
            lookahead_ms: lookahead_ms.clamp(0.0, 50.0),
            damping: 0.85,
            velocity_threshold: 15.0,
        }
    }

    pub fn with_damping(mut self, damping: f32) -> Self {
        self.damping = damping.clamp(0.0, 1.0);
        self
    }

    pub fn with_velocity_threshold(mut self, threshold: f32) -> Self {
        self.velocity_threshold = threshold.max(0.0);
        self
    }

    /// Feed a new real sample (in chronological order).
    pub fn push_sample(&mut self, sample: TouchSample) {
        self.history[0] = self.history[1];
        self.history[1] = self.history[2];
        self.history[2] = Some(sample);
    }

    /// Clears history — call on stroke start / pointer-up.
    pub fn reset(&mut self) {
        self.history = [None; 3];
    }

    /// Returns the last real sample position, if any.
    pub fn last_position(&self) -> Option<Vec2> {
        self.history[2].map(|s| s.position)
    }

    /// Produce a short run of extrapolated points for the current frame
    /// (1-3 points depending on available history), most-recent-real-sample
    /// excluded. Empty if there isn't enough history yet, or if the last
    /// real sample is older than [`STALE_SECS`] at `now` — a finger that
    /// paused must not keep a tail pointing where it was heading.
    pub fn predict(&self, now: f64) -> Vec<Vec2> {
        let (Some(p1), Some(p2)) = (self.history[1], self.history[2]) else {
            return Vec::new();
        };
        if now - p2.timestamp > STALE_SECS {
            return Vec::new();
        }

        // Digitizers often deliver several samples in one burst (same
        // millisecond); a velocity over such a tiny dt is wildly inflated.
        // Measure over the oldest sample we have when the last gap is short.
        let base = match self.history[0] {
            Some(p0) if p2.timestamp - p1.timestamp < MIN_VELOCITY_DT => p0,
            _ => p1,
        };
        let dt = p2.timestamp - base.timestamp;
        if dt < MIN_VELOCITY_DT {
            return Vec::new();
        }
        let velocity = (p2.position - base.position) / dt as f32;

        if velocity.length() < self.velocity_threshold {
            return Vec::new();
        }

        // Velocity only: the acceleration term flung the tail far past the
        // pen on every sudden stop or turn (letter ends, corners).
        let lookahead_s = self.lookahead_ms / 1000.0;
        let steps = 3usize;
        let mut out = Vec::with_capacity(steps);
        for i in 1..=steps {
            let t = lookahead_s * (i as f32 / steps as f32);
            let offset = (velocity * t * self.damping).clamp_length_max(MAX_PREDICT_DIST);
            out.push(p2.position + offset);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fed(samples: &[(f32, f64)]) -> TouchPredictor {
        let mut p = TouchPredictor::new(16.0);
        for &(x, t) in samples {
            p.push_sample(TouchSample { position: Vec2::new(x, 0.0), timestamp: t });
        }
        p
    }

    #[test]
    fn paused_finger_has_no_tail() {
        let p = fed(&[(0.0, 0.0), (10.0, 0.01), (20.0, 0.02)]);
        assert!(!p.predict(0.025).is_empty());
        assert!(p.predict(0.2).is_empty());
    }

    #[test]
    fn burst_samples_do_not_fling() {
        // Two samples 0.1 ms apart would read as 30 000 px/s.
        let p = fed(&[(0.0, 0.0), (17.0, 0.0200), (20.0, 0.0201)]);
        for q in p.predict(0.021) {
            assert!(q.x - 20.0 <= MAX_PREDICT_DIST + 0.01, "{q:?}");
        }
    }

    #[test]
    fn tail_is_bounded() {
        let p = fed(&[(0.0, 0.0), (400.0, 0.008), (800.0, 0.016)]);
        for q in p.predict(0.017) {
            assert!(q.x - 800.0 <= MAX_PREDICT_DIST + 0.01);
        }
    }
}
