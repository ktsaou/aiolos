//! Shared temperature→duty curve and a file cache that re-reads on demand.
//!
//! Per the anemos specs, a module reads its curve **on each `apply`** (live tuning: editing the
//! JSON takes effect on the next tick, no restart). `CurveCache::reload()` does exactly that, with
//! a last-good fallback so a half-written file during an edit never disrupts fan control.

use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;

/// Temperature °C → fan %, linear-interpolated, clamped, hold-outside.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Curve {
    points: BTreeMap<i32, i32>,
}

impl Curve {
    pub fn from_json(map: &serde_json::Map<String, Value>) -> Self {
        let mut points = BTreeMap::new();
        for (k, v) in map {
            // Accept integer OR float temps and duties — operators commonly write "35.0"; parsing
            // only i64 would silently drop such a point (and a non-numeric key like "sensitivity"
            // is correctly skipped). Round floats to the nearest whole degree / percent.
            let temp = k
                .parse::<i32>()
                .ok()
                .or_else(|| k.parse::<f64>().ok().map(|f| f.round() as i32));
            let pct = v
                .as_i64()
                .map(|n| n as i32)
                .or_else(|| v.as_f64().map(|f| f.round() as i32));
            if let (Some(temp), Some(pct)) = (temp, pct) {
                points.insert(temp, pct);
            }
        }
        Curve { points }
    }

    /// True when the curve has no points (config missing/invalid) — callers must fall back to the
    /// device's firmware/auto control rather than commanding a (possibly unsafe) 0%.
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Interpolate fan % for a temperature. Clamps to the curve's ends; 0 if empty.
    pub fn eval(&self, temp_c: i32) -> i32 {
        let lo = self.points.range(..=temp_c).last();
        let hi = self.points.range(temp_c..).next();
        match (lo, hi) {
            (None, None) => 0,
            (Some((_, lo_pct)), None) => *lo_pct,
            (None, Some((_, hi_pct))) => *hi_pct,
            (Some((lo_t, lo_p)), Some((hi_t, hi_p))) => {
                if lo_t == hi_t {
                    *lo_p
                } else {
                    let t = (temp_c - lo_t) as f64 / (*hi_t - *lo_t) as f64;
                    (*lo_p as f64 + t * (*hi_p as f64 - *lo_p as f64)) as i32
                }
            }
        }
    }
}

/// Holds a curve loaded from a file and re-reads it on demand. Modules call `reload()` EVERY tick.
pub struct CurveCache {
    path: String,
    curve: Curve,
    /// EMA "sensitivity" knob, read from the same file's optional `"sensitivity"` key.
    alpha: f64,
}

impl CurveCache {
    /// Create and load once. If the file is missing/invalid the curve starts empty (fail-safe)
    /// until a valid file appears (picked up by a later `reload`).
    pub fn new(path: impl Into<String>) -> Self {
        let mut c = CurveCache {
            path: path.into(),
            curve: Curve::default(),
            alpha: crate::damper::DEFAULT_EMA_ALPHA,
        };
        c.reload();
        c
    }

    /// Re-read the file (curve + optional `"sensitivity"` α). Updates the active curve only on a
    /// successful, non-empty parse; otherwise (missing / partial write / invalid JSON / empty
    /// object) keeps the last-good values so an in-progress edit never blips the fans. Returns true
    /// iff the curve or the sensitivity changed.
    pub fn reload(&mut self) -> bool {
        let Some(map) = fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| v.as_object().cloned())
        else {
            return false;
        };
        let curve = Curve::from_json(&map);
        if curve.is_empty() {
            return false; // partial write / not yet valid — keep last good
        }
        // Optional sensitivity (EMA α); non-numeric/out-of-range falls back to the current value.
        let alpha = map
            .get("sensitivity")
            .and_then(|v| v.as_f64())
            .filter(|a| *a > 0.0 && *a <= 1.0)
            .unwrap_or(self.alpha);

        let changed = curve != self.curve || (alpha - self.alpha).abs() > f64::EPSILON;
        self.curve = curve;
        self.alpha = alpha;
        changed
    }

    pub fn curve(&self) -> &Curve {
        &self.curve
    }

    /// Current EMA "sensitivity" (α) from the config (default if unset).
    pub fn alpha(&self) -> f64 {
        self.alpha
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linear() -> Curve {
        let v: Value = serde_json::from_str(r#"{"0":0,"80":100}"#).unwrap();
        Curve::from_json(v.as_object().unwrap())
    }

    #[test]
    fn eval_linear_and_clamps() {
        let c = linear();
        assert_eq!(c.eval(0), 0);
        assert_eq!(c.eval(40), 50);
        assert_eq!(c.eval(80), 100);
        assert_eq!(c.eval(-10), 0); // clamp below
        assert_eq!(c.eval(100), 100); // clamp above
    }

    #[test]
    fn empty_curve() {
        assert!(Curve::default().is_empty());
        assert_eq!(Curve::default().eval(50), 0);
    }

    #[test]
    fn cache_reload_keeps_last_good_on_bad_read() {
        // Point at a non-existent file: stays empty, reload() is false.
        let mut cc = CurveCache::new("/nonexistent/aiolos-test.curve.json");
        assert!(cc.curve().is_empty());
        assert!(!cc.reload());
    }

    fn floor_curve() -> Curve {
        // The production default: floor 35%, ceiling 100%, linear between.
        let v: Value = serde_json::from_str(r#"{"35":35,"80":100,"sensitivity":0.4}"#).unwrap();
        Curve::from_json(v.as_object().unwrap())
    }

    #[test]
    fn floor_curve_never_below_35_or_above_100() {
        let c = floor_curve();
        // SAFETY: no temperature — including absurd/wrong-low readings — ever yields < 35%.
        for t in -100..200 {
            let p = c.eval(t);
            assert!((35..=100).contains(&p), "eval({t}) = {p} escaped [35,100]");
        }
        assert_eq!(c.eval(35), 35);
        assert_eq!(c.eval(80), 100);
        assert!(c.eval(57) > 35 && c.eval(57) < 100); // interpolates between
    }

    #[test]
    fn sensitivity_key_is_not_a_curve_point() {
        // "sensitivity" must not be parsed as a temperature point.
        let c = floor_curve();
        assert_eq!(c.points.len(), 2);
    }

    #[test]
    fn from_json_accepts_float_temps_and_duties() {
        // Operators commonly write "35.0"; such points must NOT be silently dropped (they were when
        // parsing only i64). "sensitivity" (a float) is still excluded — its key isn't numeric.
        let v: Value =
            serde_json::from_str(r#"{"35.0":35.0,"80":100.0,"sensitivity":0.5}"#).unwrap();
        let c = Curve::from_json(v.as_object().unwrap());
        assert_eq!(c.points.len(), 2, "float temp/duty points must parse");
        assert_eq!(c.eval(35), 35);
        assert_eq!(c.eval(80), 100);
    }

    #[test]
    fn cache_parses_sensitivity_and_curve() {
        // Write a temp config with a sensitivity, confirm both curve and alpha load.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("aiolos-curvecache-{}.json", std::process::id()));
        std::fs::write(&path, r#"{"35":35,"80":100,"sensitivity":0.3}"#).unwrap();
        let cc = CurveCache::new(path.to_str().unwrap());
        assert!(!cc.curve().is_empty());
        assert_eq!(cc.curve().eval(35), 35);
        assert!((cc.alpha() - 0.3).abs() < 1e-9);
        // Flat curve with no sensitivity -> default alpha.
        std::fs::write(&path, r#"{"35":35,"80":100}"#).unwrap();
        let cc2 = CurveCache::new(path.to_str().unwrap());
        assert!((cc2.alpha() - crate::damper::DEFAULT_EMA_ALPHA).abs() < 1e-9);
        let _ = std::fs::remove_file(&path);
    }
}
