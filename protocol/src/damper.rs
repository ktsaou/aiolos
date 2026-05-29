//! Fan-output damping against sensor noise.
//!
//! Raw temperature sensors (notably AMD `Tctl` from k10temp) jitter several °C between reads. Fed
//! straight through a steep temp→duty curve, that jitter makes fans audibly hunt ("wave"). A
//! `Damper` removes the hunting two ways:
//! - **EMA**: exponentially smooth the driving temperature over several ticks.
//! - **Deadband**: hold the last commanded duty until the target moves by at least `deadband` %.
//!
//! Both modules own one `Damper` per controlled device.

/// EMA weight for the newest sample. Smaller = smoother/slower. 0.5 ≈ ~95% of a step in ~5 ticks.
pub const DEFAULT_EMA_ALPHA: f64 = 0.5;
/// Don't re-command duty unless the curve target moves at least this many percentage points.
pub const DEFAULT_DEADBAND_PCT: i32 = 3;

pub struct Damper {
    alpha: f64,
    deadband: i32,
    smoothed: Option<f64>,
    applied: Option<i32>,
}

impl Damper {
    pub fn new(alpha: f64, deadband: i32) -> Self {
        Damper {
            alpha: alpha.clamp(0.01, 1.0),
            deadband: deadband.max(0),
            smoothed: None,
            applied: None,
        }
    }

    /// Live-update the EMA weight (the user "sensitivity" knob): higher = more responsive to each
    /// reading, lower = smoother / less sensitive to noisy spikes. Does NOT reset the running
    /// average, so tuning it mid-run doesn't blip the fans.
    pub fn set_alpha(&mut self, alpha: f64) {
        self.alpha = alpha.clamp(0.01, 1.0);
    }

    /// Current EMA weight.
    pub fn alpha(&self) -> f64 {
        self.alpha
    }

    /// EMA-smooth a raw driving temperature; returns the smoothed temp (rounded) to feed the curve.
    /// The first sample seeds the average (no startup lag from 0).
    pub fn smooth(&mut self, raw_temp: i32) -> i32 {
        let s = match self.smoothed {
            Some(prev) => self.alpha * raw_temp as f64 + (1.0 - self.alpha) * prev,
            None => raw_temp as f64,
        };
        self.smoothed = Some(s);
        s.round() as i32
    }

    /// Apply the deadband to a target duty. **Asymmetric for safety:** a duty *increase* is applied
    /// immediately (never delay a needed ramp-up), while a small *decrease* (within the deadband) is
    /// held to stop hunting. A decrease of at least `deadband` is applied. Returns the duty to
    /// command (re-sending a held value every tick is harmless and keeps control fresh).
    pub fn deadband(&mut self, target: i32) -> i32 {
        let out = match self.applied {
            // Hold ONLY small decreases (anti-hunt). Increases and large decreases pass through.
            Some(a) if target < a && (a - target) < self.deadband => a,
            _ => target,
        };
        self.applied = Some(out);
        out
    }

    /// Reset state (e.g. after releasing control), so the next sample re-seeds the EMA.
    pub fn reset(&mut self) {
        self.smoothed = None;
        self.applied = None;
    }
}

impl Default for Damper {
    fn default() -> Self {
        Damper::new(DEFAULT_EMA_ALPHA, DEFAULT_DEADBAND_PCT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ema_seeds_on_first_sample_then_smooths() {
        let mut d = Damper::new(0.5, 0);
        assert_eq!(d.smooth(40), 40); // first sample seeds (no lag from 0)
        assert_eq!(d.smooth(60), 50); // 0.5*60 + 0.5*40
        assert_eq!(d.smooth(60), 55); // 0.5*60 + 0.5*50
    }

    #[test]
    fn ema_damps_jitter() {
        // Alternating 40/50 noise converges toward the mean, not bouncing full-swing.
        let mut d = Damper::new(0.5, 0);
        d.smooth(45);
        let a = d.smooth(40);
        let b = d.smooth(50);
        let c = d.smooth(40);
        // Swings shrink vs the raw ±5 input.
        assert!((a - b).abs() <= 5 && (b - c).abs() <= 6);
    }

    #[test]
    fn deadband_asymmetric_increases_immediate_decreases_held() {
        let mut d = Damper::new(1.0, 5);
        assert_eq!(d.deadband(50), 50); // first apply
        assert_eq!(d.deadband(52), 52); // +2 increase -> applied IMMEDIATELY (safety, no up-lag)
        assert_eq!(d.deadband(60), 60); // +8 increase -> applied
        assert_eq!(d.deadband(58), 60); // -2 decrease (<5) -> held
        assert_eq!(d.deadband(56), 60); // -4 decrease (<5) -> held
        assert_eq!(d.deadband(55), 55); // -5 decrease (>=5) -> applied
        assert_eq!(d.deadband(57), 57); // +2 increase -> applied immediately even though tiny
    }

    #[test]
    fn deadband_never_holds_an_increase() {
        // Property: the deadband must never return less than the requested duty when it rises.
        let mut d = Damper::new(1.0, 10);
        let mut applied = d.deadband(40);
        for target in [41, 45, 50, 42, 80, 81] {
            let out = d.deadband(target);
            if target >= applied {
                assert_eq!(out, target, "an increase to {target} was held at {out}");
            }
            applied = out;
        }
    }

    #[test]
    fn deadband_zero_passes_through() {
        let mut d = Damper::new(1.0, 0);
        assert_eq!(d.deadband(50), 50);
        assert_eq!(d.deadband(51), 51);
    }

    #[test]
    fn set_alpha_tunes_sensitivity_without_reset() {
        let mut d = Damper::new(0.5, 0);
        d.smooth(40); // seed
                      // alpha=1.0 -> fully responsive (returns raw), without resetting the average.
        d.set_alpha(1.0);
        assert_eq!(d.smooth(80), 80);
        // alpha clamps into (0,1].
        d.set_alpha(5.0);
        assert!(d.alpha() <= 1.0);
        d.set_alpha(0.0);
        assert!(d.alpha() >= 0.01);
    }

    #[test]
    fn low_alpha_strongly_rejects_a_single_spike() {
        // A lone bad reading must not swing the smoothed value much when sensitivity is low.
        let mut d = Damper::new(0.2, 0);
        for _ in 0..10 {
            d.smooth(45);
        }
        let after_spike = d.smooth(120); // one wild spike (Δ=75); α=0.2 → moves ~15, to ~60
        assert!(
            after_spike <= 61,
            "single spike moved smoothed to {after_spike}, far more than α·Δ (alpha too hot?)"
        );
    }
}
