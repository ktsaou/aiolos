//! Reduce routed `power-state` readings (from `input=nut`) to one aggregate [`PowerSignal`].
//!
//! aiolos relays each source instance's full readings array keyed by `module:id`. We scan ALL routed
//! inputs for `type:"power-state"` records (source-agnostic — any UPS sensor, not just `nut`), and
//! fold them into the worst-case signal: on battery if ANY UPS is, low-battery if ANY raised LB, and
//! the SMALLEST runtime among the on-battery UPSes (the binding constraint for the cap trigger).

use crate::policy::PowerSignal;
use anemos::{Inputs, Reading};

/// Fold all routed `power-state` readings into the aggregate signal. Absent/empty inputs -> a
/// default (not-on-battery) signal, which the policy reads as "AC present -> lift" (the safe
/// direction: no power-state input must never cause a spurious cap).
pub fn power_signal(inputs: Option<&Inputs>) -> PowerSignal {
    let mut sig = PowerSignal::default();
    let Some(inputs) = inputs else {
        return sig;
    };
    for readings in inputs.values() {
        fold_readings(readings, &mut sig);
    }
    sig
}

/// Fold one peer instance's readings into `sig` (only `type:"power-state"` records are considered).
fn fold_readings(readings: &[Reading], sig: &mut PowerSignal) {
    for r in readings {
        if r.kind != "power-state" {
            continue;
        }
        let on_batt = bool_field(r, "on_battery");
        if on_batt {
            sig.on_battery = true;
        }
        if bool_field(r, "low_battery") {
            sig.low_battery = true;
        }
        // Only on-battery UPSes constrain the runtime (a UPS on mains reports its full battery
        // runtime, which is irrelevant to the cap trigger and would mask a draining one).
        if on_batt {
            if let Some(rt) = r.get_i64("runtime_s") {
                sig.min_runtime_s = Some(match sig.min_runtime_s {
                    Some(cur) => cur.min(rt),
                    None => rt,
                });
            }
        }
    }
}

/// Read a JSON boolean field, defaulting to `false` if absent or not a bool.
fn bool_field(r: &Reading, key: &str) -> bool {
    r.fields.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn ps(label: &str, on_battery: bool, low_battery: bool, runtime: Option<i64>) -> Reading {
        let mut f = serde_json::Map::new();
        f.insert("on_battery".into(), json!(on_battery));
        f.insert("low_battery".into(), json!(low_battery));
        if let Some(rt) = runtime {
            f.insert("runtime_s".into(), json!(rt));
        }
        Reading::new("power-state", label, serde_json::Value::Object(f))
    }

    #[test]
    fn no_inputs_is_not_on_battery() {
        let s = power_signal(None);
        assert!(!s.on_battery);
        assert!(!s.low_battery);
        assert_eq!(s.min_runtime_s, None);
    }

    #[test]
    fn single_ups_on_battery_folds_through() {
        let mut inputs: Inputs = HashMap::new();
        inputs.insert(
            "nut:pr3000-nova".into(),
            vec![ps("pr3000-nova", true, false, Some(420))],
        );
        let s = power_signal(Some(&inputs));
        assert!(s.on_battery);
        assert!(!s.low_battery);
        assert_eq!(s.min_runtime_s, Some(420));
    }

    #[test]
    fn aggregates_worst_case_across_multiple_upses() {
        let mut inputs: Inputs = HashMap::new();
        // One UPS healthy on mains (its big runtime must NOT mask the draining one).
        inputs.insert("nut:a".into(), vec![ps("a", false, false, Some(9000))]);
        // One UPS on battery, draining, with LB raised.
        inputs.insert("nut:b".into(), vec![ps("b", true, true, Some(180))]);
        let s = power_signal(Some(&inputs));
        assert!(s.on_battery, "any UPS on battery -> aggregate on battery");
        assert!(s.low_battery, "any LB -> aggregate LB");
        assert_eq!(
            s.min_runtime_s,
            Some(180),
            "min runtime among on-battery UPSes binds; the mains UPS runtime is ignored"
        );
    }

    #[test]
    fn ignores_non_power_state_readings() {
        let mut inputs: Inputs = HashMap::new();
        inputs.insert(
            "nvidia:GPU-1".into(),
            vec![Reading::new("temp", "GPU", json!({"temp": 63}))],
        );
        let s = power_signal(Some(&inputs));
        assert!(
            !s.on_battery,
            "a temp reading must not look like a power event"
        );
    }

    #[test]
    fn mains_ups_runtime_is_not_counted() {
        // A UPS on mains reporting a runtime estimate must not set min_runtime_s (only on-battery
        // UPSes constrain the trigger).
        let mut inputs: Inputs = HashMap::new();
        inputs.insert("nut:a".into(), vec![ps("a", false, false, Some(120))]);
        let s = power_signal(Some(&inputs));
        assert!(!s.on_battery);
        assert_eq!(s.min_runtime_s, None);
    }
}
