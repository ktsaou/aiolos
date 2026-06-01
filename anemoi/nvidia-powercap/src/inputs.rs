//! Reduce routed power-state signals (from `input=nut`) to one aggregate [`PowerSignal`].
//!
//! aiolos relays each source instance's signals keyed by `module:id`. We scan ALL routed inputs for
//! power-state signals (source-agnostic — any UPS sensor, not just `nut`), and fold them into the
//! worst-case signal: on battery if ANY UPS is, low-battery if ANY raised LB, and the SMALLEST
//! runtime among the on-battery UPSes (the binding constraint for the cap trigger).

use crate::policy::PowerSignal;
use anemos::{Inputs, Signal};

/// Fold all routed power-state signals into the aggregate. Absent/empty inputs -> a default
/// (not-on-battery) signal, which the policy reads as "AC present -> lift" (the safe direction).
pub fn power_signal(inputs: Option<&Inputs>) -> PowerSignal {
    let mut sig = PowerSignal::default();
    let Some(inputs) = inputs else {
        return sig;
    };
    for signals in inputs.values() {
        fold_signals(signals, &mut sig);
    }
    sig
}

/// Fold one peer instance's signals into `sig`.
fn fold_signals(signals: &[Signal], sig: &mut PowerSignal) {
    let on_batt = bool_signal(signals, "power-on-battery");
    if on_batt {
        sig.on_battery = true;
    }
    if bool_signal(signals, "power-low-battery") {
        sig.low_battery = true;
    }
    // Only on-battery UPSes constrain the runtime (a UPS on mains reports its full battery runtime,
    // which is irrelevant to the cap trigger and would mask a draining one).
    if on_batt {
        if let Some(rt) = i64_signal(signals, "power-runtime") {
            sig.min_runtime_s = Some(match sig.min_runtime_s {
                Some(cur) => cur.min(rt),
                None => rt,
            });
        }
    }
}

fn bool_signal(signals: &[Signal], kind: &str) -> bool {
    signals
        .iter()
        .find(|s| s.kind() == Some(kind))
        .and_then(|s| s.value.as_ref())
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn i64_signal(signals: &[Signal], kind: &str) -> Option<i64> {
    signals
        .iter()
        .find(|s| s.kind() == Some(kind))
        .and_then(|s| s.value_i64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anemos::Signal;
    use serde_json::json;
    use std::collections::HashMap;

    fn ups_signals(on_battery: bool, low_battery: bool, runtime: Option<i64>) -> Vec<Signal> {
        let mut v = vec![
            Signal::producer("u:power:on_battery", "u:power", "power-on-battery")
                .value(json!(on_battery)),
            Signal::producer("u:power:low_battery", "u:power", "power-low-battery")
                .value(json!(low_battery)),
        ];
        if let Some(rt) = runtime {
            v.push(
                Signal::producer("u:power:runtime", "u:power", "power-runtime").value(json!(rt)),
            );
        }
        v
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
            ups_signals(true, false, Some(420)),
        );
        let s = power_signal(Some(&inputs));
        assert!(s.on_battery);
        assert!(!s.low_battery);
        assert_eq!(s.min_runtime_s, Some(420));
    }

    #[test]
    fn aggregates_worst_case_across_multiple_upses() {
        let mut inputs: Inputs = HashMap::new();
        inputs.insert("nut:a".into(), ups_signals(false, false, Some(9000)));
        inputs.insert("nut:b".into(), ups_signals(true, true, Some(180)));
        let s = power_signal(Some(&inputs));
        assert!(s.on_battery);
        assert!(s.low_battery);
        assert_eq!(s.min_runtime_s, Some(180));
    }

    #[test]
    fn ignores_non_power_state_signals() {
        let mut inputs: Inputs = HashMap::new();
        inputs.insert(
            "nvidia:GPU-1".into(),
            vec![Signal::producer("g:t:temp", "g:t", "temperature").value(json!(63))],
        );
        let s = power_signal(Some(&inputs));
        assert!(!s.on_battery);
    }

    #[test]
    fn mains_ups_runtime_is_not_counted() {
        let mut inputs: Inputs = HashMap::new();
        inputs.insert("nut:a".into(), ups_signals(false, false, Some(120)));
        let s = power_signal(Some(&inputs));
        assert!(!s.on_battery);
        assert_eq!(s.min_runtime_s, None);
    }
}
