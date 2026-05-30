//! Per-fan stall/failure detection (SOW-0008) + surviving-sibling compensation.
//!
//! A fan is *faulted* when, for several consecutive ticks, it is commanded above a duty threshold
//! yet its tachometer reads ≈0 RPM — i.e. it is being driven but not spinning (stalled/failed). A
//! spin-up grace and N-consecutive-tick hysteresis avoid false positives while a fan ramps up or a
//! tach read momentarily glitches. Detection is module-local: asrock owns both the commanded duty
//! and the matching tach, so no orchestrator change is needed.
//!
//! A `None` (unreadable) RPM is **not** treated as a fault (a sensor read failing ≠ a dead fan); it
//! holds the per-fan state rather than confirming or clearing it. A present RPM above the threshold
//! clears the fault immediately.
//!
//! On a *confirmed* fault, the surviving (non-faulted) fans in the same zone are boosted to 100% on
//! subsequent ticks (more airflow is always safe). The faulted fan keeps its normal commanded duty
//! (we never command 0). Surfacing is a `"fault":true` reading field + a `tracing::warn!`; richer
//! delivery (webhook / Netdata alarm) is a documented follow-on, not implemented here.

use crate::zones::{self, Zone};

/// A fan must be commanded at least this duty (%) before "≈0 RPM" counts as a stall: below it a
/// fan may legitimately be near its minimum / the firmware floor. 20% sits well under the board's
/// 30% floor yet above any plausible idle, so a driven fan that should clearly be spinning.
const FAULT_MIN_DUTY: u32 = 20;
/// RPM at or below this counts as "not spinning". A healthy case fan idles ~800–1600 RPM and even
/// the slow Noctua CPU coolers run several hundred, so 100 is a safe "stopped" ceiling.
const FAULT_RPM_MAX: i32 = 100;
/// Ticks a fan must be commanded ≥ `FAULT_MIN_DUTY` before its RPM is judged (spin-up grace).
const SPINUP_GRACE_TICKS: u32 = 2;
/// Consecutive qualifying ticks (driven + ≈0 RPM, past the grace) before a fault is *confirmed*.
const FAULT_TICKS: u32 = 3;

/// Per-fan detection state.
#[derive(Debug, Clone, Copy, Default)]
struct FanState {
    /// Consecutive ticks commanded ≥ `FAULT_MIN_DUTY` (drives the spin-up grace).
    above_streak: u32,
    /// Consecutive qualifying fault ticks (past the grace).
    fault_streak: u32,
    /// True once `fault_streak` has reached `FAULT_TICKS`.
    confirmed: bool,
}

/// Tracks all 8 fans' stall state across ticks.
pub struct FanFaultTracker {
    fans: [FanState; 8],
}

impl FanFaultTracker {
    pub fn new() -> Self {
        FanFaultTracker {
            fans: [FanState::default(); 8],
        }
    }

    /// The currently *confirmed*-faulted fans (state from prior ticks). Read at the start of a tick
    /// to drive sibling compensation, before `update` folds in this tick's reading.
    pub fn confirmed(&self) -> [bool; 8] {
        std::array::from_fn(|i| self.fans[i].confirmed)
    }

    /// Fold this tick's `(commanded duty, measured RPM)` per fan into the detector and return the
    /// confirmed-fault flags AFTER the update (used to annotate the readings this tick).
    pub fn update(&mut self, commanded: &[u32; 8], rpms: &[Option<i32>; 8]) -> [bool; 8] {
        for i in 0..8 {
            self.fans[i] = step(self.fans[i], commanded[i], rpms[i]);
        }
        self.confirmed()
    }
}

impl Default for FanFaultTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Pure per-fan state transition for one tick (unit-testable without hardware).
fn step(mut s: FanState, commanded: u32, rpm: Option<i32>) -> FanState {
    if commanded < FAULT_MIN_DUTY {
        // Not meaningfully driven this tick — can't judge; clear everything (a later genuine stall
        // re-earns the grace + streak from scratch).
        return FanState::default();
    }
    s.above_streak = s.above_streak.saturating_add(1);
    if s.above_streak <= SPINUP_GRACE_TICKS {
        // Still in the spin-up window: do not accrue or confirm a fault yet.
        s.fault_streak = 0;
        s.confirmed = false;
        return s;
    }
    match rpm {
        // Driven but not spinning: accrue toward a confirmed fault.
        Some(r) if r <= FAULT_RPM_MAX => {
            s.fault_streak = s.fault_streak.saturating_add(1);
            if s.fault_streak >= FAULT_TICKS {
                s.confirmed = true;
            }
        }
        // Spinning fine: clear immediately.
        Some(_) => {
            s.fault_streak = 0;
            s.confirmed = false;
        }
        // Tach unreadable this tick: hold state (neither confirm nor clear).
        None => {}
    }
    s
}

/// Apply surviving-sibling compensation: for any zone containing a confirmed-faulted fan, boost the
/// non-faulted fans in that zone to 100%. The faulted fan keeps its base duty. Pure/testable.
pub fn compensate(base: [u32; 8], confirmed: &[bool; 8]) -> [u32; 8] {
    let cpu_fault = zones::CPU_ZONE.iter().any(|&i| confirmed[i]);
    let case_fault = zones::CASE_ZONE.iter().any(|&i| confirmed[i]);
    let mut out = base;
    for (i, o) in out.iter_mut().enumerate() {
        if confirmed[i] {
            continue; // a dead fan keeps its base duty; we can't help it by commanding more
        }
        let boost = match zones::zone_of(i) {
            Zone::Cpu => cpu_fault,
            Zone::Case => case_fault,
        };
        if boost {
            *o = 100;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a fan: commanded high, RPM stays ≈0. It must NOT fault during the grace, then confirm
    /// only after `FAULT_TICKS` further qualifying ticks.
    #[test]
    fn confirms_only_after_grace_plus_hysteresis() {
        let mut t = FanFaultTracker::new();
        let mut commanded = [50u32; 8];
        commanded[3] = 50; // FAN4 (a case fan) under test
        let mut rpms = [Some(1500); 8];
        rpms[3] = Some(0); // FAN4 stalled

        // Grace ticks: no fault.
        for _ in 0..SPINUP_GRACE_TICKS {
            let f = t.update(&commanded, &rpms);
            assert!(!f[3], "must not fault during spin-up grace");
        }
        // Now accrue; faults only on the FAULT_TICKS-th qualifying tick.
        for n in 1..FAULT_TICKS {
            let f = t.update(&commanded, &rpms);
            assert!(
                !f[3],
                "must not fault before {FAULT_TICKS} qualifying ticks (n={n})"
            );
        }
        let f = t.update(&commanded, &rpms);
        assert!(
            f[3],
            "must confirm after grace + {FAULT_TICKS} qualifying ticks"
        );
        // Healthy peers never fault.
        assert!(f.iter().enumerate().all(|(i, &x)| i == 3 || !x));
    }

    /// A healthy RPM clears a brewing fault immediately (no latch before confirmation).
    #[test]
    fn recovers_when_rpm_returns() {
        let mut t = FanFaultTracker::new();
        let commanded = [50u32; 8];
        let stalled = [Some(0); 8];
        let healthy = [Some(1500); 8];
        // Past grace, one stalled tick (not yet confirmed)...
        for _ in 0..SPINUP_GRACE_TICKS + 1 {
            t.update(&commanded, &stalled);
        }
        // ...then RPM returns -> cleared.
        let f = t.update(&commanded, &healthy);
        assert!(
            f.iter().all(|&x| !x),
            "a returning RPM must clear the fault"
        );
    }

    /// Below the duty threshold, ≈0 RPM is expected and must never fault (also resets state).
    #[test]
    fn low_duty_never_faults() {
        let mut t = FanFaultTracker::new();
        let commanded = [FAULT_MIN_DUTY - 1; 8];
        let rpms = [Some(0); 8];
        for _ in 0..(SPINUP_GRACE_TICKS + FAULT_TICKS + 5) {
            let f = t.update(&commanded, &rpms);
            assert!(
                f.iter().all(|&x| !x),
                "a lightly-commanded fan must not fault at 0 RPM"
            );
        }
    }

    /// An unreadable tach (None) neither confirms nor clears a fault.
    #[test]
    fn unreadable_rpm_holds_state() {
        let mut t = FanFaultTracker::new();
        let commanded = [50u32; 8];
        // Build up to a confirmed fault on FAN0.
        let mut stalled = [Some(1500); 8];
        stalled[0] = Some(0);
        for _ in 0..(SPINUP_GRACE_TICKS + FAULT_TICKS) {
            t.update(&commanded, &stalled);
        }
        assert!(t.confirmed()[0], "precondition: FAN1 confirmed faulted");
        // An unreadable tach must hold the confirmed fault, not clear it.
        let mut unreadable = [Some(1500); 8];
        unreadable[0] = None;
        let f = t.update(&commanded, &unreadable);
        assert!(f[0], "an unreadable tach must not clear a confirmed fault");
    }

    #[test]
    fn compensate_boosts_surviving_siblings_in_the_same_zone() {
        // FAN4 (case zone) confirmed faulted -> the other case fans go to 100, CPU zone untouched.
        let base = [30, 30, 60, 60, 60, 60, 60, 60];
        let mut confirmed = [false; 8];
        confirmed[3] = true; // FAN4
        let out = compensate(base, &confirmed);
        assert_eq!(out[0], 30, "CPU zone unaffected by a case-fan fault");
        assert_eq!(out[1], 30);
        assert_eq!(
            out[3], 60,
            "the dead fan keeps its base duty (can't help it)"
        );
        for &i in &[2usize, 4, 5, 6, 7] {
            assert_eq!(out[i], 100, "surviving case fan FAN{} boosted", i + 1);
        }
    }

    #[test]
    fn compensate_cpu_fault_boosts_only_cpu_siblings() {
        let base = [30, 30, 60, 60, 60, 60, 60, 60];
        let mut confirmed = [false; 8];
        confirmed[0] = true; // FAN1 (CPU cooler) dead
        let out = compensate(base, &confirmed);
        assert_eq!(out[0], 30, "dead CPU fan keeps base");
        assert_eq!(out[1], 100, "surviving CPU cooler boosted");
        for &i in &[2usize, 3, 4, 5, 6, 7] {
            assert_eq!(out[i], 60, "case fans untouched by a CPU-zone fault");
        }
    }

    #[test]
    fn compensate_noop_without_faults() {
        let base = [30, 30, 60, 60, 60, 60, 60, 60];
        assert_eq!(compensate(base, &[false; 8]), base);
    }
}
