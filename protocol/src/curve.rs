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
            if let (Ok(temp), Some(pct)) = (k.parse::<i32>(), v.as_i64()) {
                points.insert(temp, pct as i32);
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
}

impl CurveCache {
    /// Create and load once. If the file is missing/invalid the curve starts empty (fail-safe)
    /// until a valid file appears (picked up by a later `reload`).
    pub fn new(path: impl Into<String>) -> Self {
        let mut c = CurveCache {
            path: path.into(),
            curve: Curve::default(),
        };
        c.reload();
        c
    }

    /// Re-read the file. Updates the active curve only on a successful, non-empty parse; otherwise
    /// (missing / partial write / invalid JSON / empty object) keeps the last-good curve so an
    /// in-progress edit never blips the fans. Returns true iff the active curve changed.
    pub fn reload(&mut self) -> bool {
        let parsed = fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| v.as_object().cloned())
            .map(|m| Curve::from_json(&m));
        if let Some(c) = parsed {
            if !c.is_empty() && c != self.curve {
                self.curve = c;
                return true;
            }
        }
        false
    }

    pub fn curve(&self) -> &Curve {
        &self.curve
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
}
