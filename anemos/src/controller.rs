//! Temperature → fan-duty controller: the single, central implementation of the curve + EMA +
//! deadband path that every anemos reuses (so the safety-critical smoothing/floor logic lives in
//! one place). A module sources its driving temperature however it likes, then calls `duty()`.

use crate::curve::{BrokenReason, ReloadOutcome};
use crate::{CurveCache, Damper};

/// Result of mapping a raw driving temperature to a fan duty.
pub struct Duty {
    /// The raw driving temperature the module supplied.
    pub raw: i32,
    /// The EMA-smoothed temperature actually fed to the curve.
    pub smoothed: i32,
    /// Commanded duty %, or `None` when there is no usable curve — the caller MUST then fall back to
    /// the device's firmware/auto control (never command 0%).
    pub pct: Option<u32>,
}

/// Owns one device's live curve cache + damper. The SDK creates one per `run` instance and passes
/// `&mut Controller` into `Device::apply`.
pub struct Controller {
    curves: CurveCache,
    damper: Damper,
}

impl Controller {
    pub fn new(curve_path: String) -> Self {
        Controller {
            curves: CurveCache::new(curve_path),
            damper: Damper::default(),
        }
    }

    /// The reason the INITIAL curve load failed, if it did. A control module fails to start on
    /// `Some` (SOW-0012 decision 2); a sensor-only module ignores it. `None` if the first load was
    /// clean. (The live keep-last-good fail-safe still applies for runtime reloads.)
    pub fn initial_curve_error(&self) -> Option<BrokenReason> {
        self.curves.initial_error()
    }

    /// Reload the curve + sensitivity (called every tick — live tuning), then EMA-smooth `raw_temp`
    /// and map it through the curve with a deadband. `pct = None` when the curve is missing/empty.
    pub fn duty(&mut self, raw_temp: i32) -> Duty {
        match self.curves.reload() {
            ReloadOutcome::Changed => {
                tracing::info!(path = %self.curves.path(), alpha = self.curves.alpha(), "config reloaded");
            }
            // SOW-0012 decision 1: complain LOUDLY every tick while the file stays broken, but keep
            // the last-good curve (runtime fail-safe — an in-progress edit must never blip the fans).
            ReloadOutcome::Broken { reason } => {
                tracing::warn!(path = %self.curves.path(), reason = reason.as_str(), "curve file broken — keeping last-good curve");
            }
            ReloadOutcome::Unchanged => {}
        }
        self.damper.set_alpha(self.curves.alpha()); // live sensitivity knob
        if self.curves.curve().is_empty() {
            self.damper.reset();
            return Duty {
                raw: raw_temp,
                smoothed: raw_temp,
                pct: None,
            };
        }
        let smoothed = self.damper.smooth(raw_temp);
        let pct = self
            .damper
            .deadband(self.curves.curve().eval(smoothed).clamp(0, 100)) as u32;
        Duty {
            raw: raw_temp,
            smoothed,
            pct: Some(pct),
        }
    }

    /// Reset the damper so control re-seeds cleanly (the SDK calls this after a failed tick, so a
    /// stale EMA/deadband isn't carried across an outage).
    pub fn reset(&mut self) {
        self.damper.reset();
    }

    pub fn path(&self) -> &str {
        self.curves.path()
    }

    pub fn curve_is_empty(&self) -> bool {
        self.curves.curve().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duty_is_none_without_a_curve_and_holds_the_floor_with_one() {
        // No curve file -> empty curve -> pct None (the anemos must fall back to firmware/auto).
        let mut c = Controller::new("/nonexistent/aiolos-ctrl-test.json".to_string());
        assert!(c.duty(50).pct.is_none(), "missing curve must yield no duty");

        // A floor curve -> a sub-floor temperature is held at 35%, never below.
        let path = std::env::temp_dir().join(format!("aiolos-ctrl-{}.json", std::process::id()));
        std::fs::write(&path, r#"{"35":35,"80":100,"sensitivity":1.0}"#).unwrap();
        let mut c2 = Controller::new(path.to_string_lossy().into_owned());
        let d = c2.duty(20); // below the floor; sensitivity 1.0 -> no smoothing lag
        assert_eq!(
            d.pct,
            Some(35),
            "a sub-floor temperature must hold the 35% floor"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn duty_keeps_last_good_curve_when_file_breaks_at_runtime() {
        // SOW-0012 decision 1: a valid curve at start, then the file breaks mid-run — the controller
        // must keep regulating on the last-good curve (NOT drop to firmware/None) and never error.
        let path =
            std::env::temp_dir().join(format!("aiolos-ctrl-break-{}.json", std::process::id()));
        std::fs::write(&path, r#"{"30":30,"80":100,"sensitivity":1.0}"#).unwrap();
        let mut c = Controller::new(path.to_string_lossy().into_owned());
        assert!(
            c.initial_curve_error().is_none(),
            "a valid initial curve is not a startup error"
        );
        assert_eq!(c.duty(80).pct, Some(100), "valid curve regulates");

        // Break the file: a control module keeps the last-good curve (pct still Some), not None.
        std::fs::write(&path, "}{ broken").unwrap();
        let d = c.duty(80);
        assert_eq!(
            d.pct,
            Some(100),
            "a broken curve at runtime keeps the last-good curve (never drops to firmware)"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn initial_curve_error_is_set_for_an_invalid_startup_curve() {
        // A control module constructs the Controller against its curve path; an unreadable file at
        // startup surfaces as initial_curve_error (run_loop turns this into a protocol `fatal`).
        let c = Controller::new("/nonexistent/aiolos-ctrl-missing.json".to_string());
        assert!(c.initial_curve_error().is_some());
    }
}
