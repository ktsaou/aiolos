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

/// Outcome of a `reload()`: lets the caller tell a clean read (silent or info-logged) apart from a
/// present-but-broken file (warn every tick while it stays broken — SOW-0012). On `Broken` the
/// last-good curve/α are retained untouched, so an in-progress edit never blips the fans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadOutcome {
    /// Read + parsed to a usable, non-empty curve identical to the last-good one. Nothing to log.
    Unchanged,
    /// Read + parsed to a usable, non-empty curve that differs from the last-good one (apply it).
    Changed,
    /// File missing / unreadable / not valid JSON / no usable curve points. Last-good kept; the
    /// `reason` is logged as a warning by the caller (every tick the file stays broken).
    Broken { reason: BrokenReason },
}

/// Why a curve load did not yield a usable curve. A stable, log-friendly classification of the three
/// startup-fatal cases (SOW-0012 decision 1) plus the file-read error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokenReason {
    /// `fs::read_to_string` failed (missing file, permissions, …).
    Unreadable,
    /// Contents are not a JSON object (parse error, or a non-object top-level value).
    InvalidJson,
    /// Parsed fine but yields no usable temperature→duty points (empty `{}` / only non-curve keys).
    NoUsablePoints,
}

impl BrokenReason {
    /// Short human reason, embedded in the warning and (at startup) the protocol `fatal` error.
    pub fn as_str(self) -> &'static str {
        match self {
            BrokenReason::Unreadable => "file missing or unreadable",
            BrokenReason::InvalidJson => "invalid JSON (not a JSON object)",
            BrokenReason::NoUsablePoints => "no usable temperature points",
        }
    }
}

/// Holds a curve loaded from a file and re-reads it on demand. Modules call `reload()` EVERY tick.
pub struct CurveCache {
    path: String,
    curve: Curve,
    /// EMA "sensitivity" knob, read from the same file's optional `"sensitivity"` key.
    alpha: f64,
    /// Outcome of the FIRST load (in `new`). `Some(reason)` means the module came up without a
    /// usable curve — a control module must then fail to start (SOW-0012 decision 2). `None` means
    /// the initial curve loaded cleanly. Untouched by later `reload`s (those use the live outcome).
    initial: Option<BrokenReason>,
}

impl CurveCache {
    /// Create and load once. If the file is missing/invalid the curve starts empty (fail-safe) and
    /// the failure reason is recorded in `initial_error()` so a control module can fail to start
    /// (SOW-0012); a sensor-only module ignores it. A valid file appearing later is picked up by a
    /// subsequent `reload`.
    pub fn new(path: impl Into<String>) -> Self {
        let mut c = CurveCache {
            path: path.into(),
            curve: Curve::default(),
            alpha: crate::damper::DEFAULT_EMA_ALPHA,
            initial: None,
        };
        if let ReloadOutcome::Broken { reason } = c.reload() {
            c.initial = Some(reason);
        }
        c
    }

    /// The reason the FIRST load failed, if it did (control modules fail to start on `Some`). `None`
    /// if the initial curve loaded cleanly.
    pub fn initial_error(&self) -> Option<BrokenReason> {
        self.initial
    }

    /// Re-read the file (curve + optional `"sensitivity"` α). Updates the active curve only on a
    /// successful, non-empty parse; otherwise (missing / partial write / invalid JSON / empty
    /// object) keeps the last-good values so an in-progress edit never blips the fans. The returned
    /// `ReloadOutcome` lets the caller warn on a broken file (every tick) yet stay quiet on an
    /// unchanged one.
    pub fn reload(&mut self) -> ReloadOutcome {
        let raw = match fs::read_to_string(&self.path) {
            Ok(s) => s,
            Err(_) => {
                return ReloadOutcome::Broken {
                    reason: BrokenReason::Unreadable,
                }
            }
        };
        let Some(map) = serde_json::from_str::<Value>(&raw)
            .ok()
            .and_then(|v| v.as_object().cloned())
        else {
            return ReloadOutcome::Broken {
                reason: BrokenReason::InvalidJson,
            };
        };
        let curve = Curve::from_json(&map);
        if curve.is_empty() {
            // Parsed, but no usable points (empty `{}` / only non-curve keys) — keep last good.
            return ReloadOutcome::Broken {
                reason: BrokenReason::NoUsablePoints,
            };
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
        if changed {
            ReloadOutcome::Changed
        } else {
            ReloadOutcome::Unchanged
        }
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
        // Point at a non-existent file: stays empty; reload() reports Broken(Unreadable) and the
        // initial load is recorded as failed (a control module fails to start on this).
        let mut cc = CurveCache::new("/nonexistent/aiolos-test.curve.json");
        assert!(cc.curve().is_empty());
        assert_eq!(
            cc.reload(),
            ReloadOutcome::Broken {
                reason: BrokenReason::Unreadable
            }
        );
        assert_eq!(cc.initial_error(), Some(BrokenReason::Unreadable));
    }

    #[test]
    fn reload_distinguishes_changed_unchanged_and_broken() {
        // A live edit cycle: a valid first load, an unchanged re-read, a changed re-read, then a
        // broken file (kept last-good + reported broken every read), then recovery.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("aiolos-reload-{}.json", std::process::id()));
        std::fs::write(&path, r#"{"30":30,"80":100}"#).unwrap();
        let mut cc = CurveCache::new(path.to_str().unwrap());
        assert_eq!(cc.initial_error(), None, "a valid first load is not broken");

        // Re-read the same file -> Unchanged (silent).
        assert_eq!(cc.reload(), ReloadOutcome::Unchanged);

        // Edit the curve -> Changed.
        std::fs::write(&path, r#"{"40":40,"80":100}"#).unwrap();
        assert_eq!(cc.reload(), ReloadOutcome::Changed);
        assert_eq!(cc.curve().eval(40), 40);

        // Three broken forms — each keeps the last-good curve AND reports Broken with a reason.
        let good = cc.curve().clone();

        std::fs::write(&path, r#"{"40":40,"80":"#).unwrap(); // truncated/partial write
        assert_eq!(
            cc.reload(),
            ReloadOutcome::Broken {
                reason: BrokenReason::InvalidJson
            }
        );
        assert_eq!(cc.curve(), &good, "broken JSON keeps last-good");

        std::fs::write(&path, r#"{"sensitivity":0.4}"#).unwrap(); // valid JSON, no curve points
        assert_eq!(
            cc.reload(),
            ReloadOutcome::Broken {
                reason: BrokenReason::NoUsablePoints
            }
        );
        assert_eq!(cc.curve(), &good, "no-usable-points keeps last-good");

        // It keeps reporting Broken every read while the file stays broken (warn-every-tick).
        assert!(matches!(cc.reload(), ReloadOutcome::Broken { .. }));

        // Recovery: a valid file is picked up again.
        std::fs::write(&path, r#"{"50":50,"80":100}"#).unwrap();
        assert_eq!(cc.reload(), ReloadOutcome::Changed);
        assert_eq!(cc.curve().eval(50), 50);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn initial_error_classifies_each_invalid_case() {
        let dir = std::env::temp_dir();

        // (a) missing file -> Unreadable.
        let cc = CurveCache::new("/nonexistent/aiolos-missing.curve.json");
        assert_eq!(cc.initial_error(), Some(BrokenReason::Unreadable));

        // (b) invalid JSON -> InvalidJson.
        let p_bad = dir.join(format!("aiolos-initbad-{}.json", std::process::id()));
        std::fs::write(&p_bad, "not json at all").unwrap();
        let cc = CurveCache::new(p_bad.to_str().unwrap());
        assert_eq!(cc.initial_error(), Some(BrokenReason::InvalidJson));
        let _ = std::fs::remove_file(&p_bad);

        // (c) valid JSON object but no usable points -> NoUsablePoints.
        let p_empty = dir.join(format!("aiolos-initempty-{}.json", std::process::id()));
        std::fs::write(&p_empty, r#"{"sensitivity":0.5}"#).unwrap();
        let cc = CurveCache::new(p_empty.to_str().unwrap());
        assert_eq!(cc.initial_error(), Some(BrokenReason::NoUsablePoints));
        let _ = std::fs::remove_file(&p_empty);

        // A valid curve -> no initial error.
        let p_ok = dir.join(format!("aiolos-initok-{}.json", std::process::id()));
        std::fs::write(&p_ok, r#"{"30":30,"80":100}"#).unwrap();
        let cc = CurveCache::new(p_ok.to_str().unwrap());
        assert_eq!(cc.initial_error(), None);
        let _ = std::fs::remove_file(&p_ok);
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
