//! Touch-to-photon latency compensation via short-horizon input extrapolation.
//!
//! Predicted points are for rendering only — never written to permanent
//! stroke history. Callers must call [`TouchPredictor::predict`] fresh each
//! frame and discard the result once drawn.

use glam::Vec2;

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
    /// excluded. Empty if there isn't enough history yet.
    pub fn predict(&self) -> Vec<Vec2> {
        let (Some(p1), Some(p2)) = (self.history[1], self.history[2]) else {
            return Vec::new();
        };

        let dt1 = (p2.timestamp - p1.timestamp).max(1.0 / 1000.0);
        let velocity = (p2.position - p1.position) / dt1 as f32;

        if velocity.length() < self.velocity_threshold {
            return Vec::new();
        }

        let acceleration = if let Some(p0) = self.history[0] {
            let dt0 = (p1.timestamp - p0.timestamp).max(1.0 / 1000.0);
            let velocity_prev = (p1.position - p0.position) / dt0 as f32;

            // Reversal guard: if direction flips sharply, the pointer likely
            // just changed course — don't trust acceleration to fling it.
            if velocity.length() > 0.0
                && velocity_prev.length() > 0.0
                && velocity.dot(velocity_prev) < 0.0
            {
                Vec2::ZERO
            } else {
                let dt_avg = ((dt0 + dt1) * 0.5).max(1.0 / 1000.0) as f32;
                (velocity - velocity_prev) / dt_avg
            }
        } else {
            Vec2::ZERO
        };

        let lookahead_s = self.lookahead_ms / 1000.0;
        let steps = 3usize;
        let mut out = Vec::with_capacity(steps);

        for i in 1..=steps {
            let dt = lookahead_s * (i as f32 / steps as f32);
            let raw_offset = velocity * dt + 0.5 * acceleration * dt * dt;
            let damped_offset = raw_offset * self.damping;
            out.push(p2.position + damped_offset);
        }

        out
    }
}
