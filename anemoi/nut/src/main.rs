//! nut anemos — UPS / utility-power state sensor via NUT (read-only; controls NO device).
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle (CLI/signals/logging/protocol/
//! restore wiring); the `nut` tech crate shells out to `upsc` to list UPS names and read variables.
//! This is a **sensor-only** anemos (`ModuleInfo` curve = `None`): it reports each UPS's
//! utility-power state as a new reading type `power-state`, routed (e.g. `input=nut`) into a reactor
//! such as `gpu-powercap`. It controls nothing — so `apply` ignores the controller and
//! `restore`/`restore_all` are no-ops.
//!
//! Which UPS(es) to monitor comes from **operator config** `nut.conf` (one id per line; `#`
//! comments) at `$AIOLOS_ETC_DIR/nut.conf` else `/opt/aiolos/etc/nut.conf`. If that file is absent
//! (or lists nothing) the module auto-discovers via `upsc -l` (local upsd). No credentials live in
//! committed artifacts: `upsc` reads public UPS variables (no login); a remote/authenticated upsd is
//! reached by configuring the id as `ups@host` in the operator's `nut.conf`.
//!
//! detect → one entry per configured/discovered UPS id (id = the upsc name).
//! run <id> → report that UPS's `power-state` reading.

mod config;

use anemos::{
    Anemos, Applied, Controller, Detected, Device, FoundEntry, Inputs, ModuleInfo, Reading,
};
use serde_json::json;

fn main() -> ! {
    anemos::run(
        ModuleInfo {
            name: "nut",
            // Sensor-only: no curve, no device control.
            curve_default_path: None,
            curve_env_filename: None,
        },
        NutAnemos,
    )
}

struct NutAnemos;

impl Anemos for NutAnemos {
    fn detect(&mut self) -> Detected {
        // Operator config decides the UPS set; fall back to local upsd discovery when unconfigured.
        let ids = config::ups_ids();
        Detected::ok(
            ids.into_iter()
                .map(|id| FoundEntry {
                    id: id.clone(),
                    kind: "UPS".to_string(),
                    name: id,
                    extra: Default::default(),
                })
                .collect(),
        )
    }

    fn open(&mut self, id: &str) -> anyhow::Result<Box<dyn Device>> {
        // Bind by UPS id. Do NOT fail open if the UPS is momentarily unreadable: a UPS that upsd
        // cannot reach right now is a transient condition the per-tick `apply` reports as `error`
        // (the orchestrator keeps the instance and retries), not a fatal that withdraws the module.
        Ok(Box::new(UpsSensor { id: id.to_string() }))
    }

    fn restore_all(&mut self) {
        // Sensor-only: nothing to restore.
    }
}

/// One UPS bound by its upsc id for the lifetime of the `run` instance.
struct UpsSensor {
    id: String,
}

impl Device for UpsSensor {
    fn apply(&mut self, _inputs: Option<&Inputs>, _ctrl: &mut Controller) -> Applied {
        // Sensor-only: read this UPS's state and report it as a `power-state` reading.
        match nut::read(&self.id) {
            Ok(s) => Applied::ok(vec![power_state_reading(&s)]),
            Err(e) => Applied::error(e),
        }
    }

    fn restore(&mut self) {
        // Sensor-only: nothing to restore.
    }
}

/// Build the `power-state` reading for one UPS. The booleans (`online`/`on_battery`/`low_battery`)
/// are the decision-ready signals a reactor keys off; the raw `status` flags and the numeric fields
/// are included for the status page and for richer policies. Numeric fields are omitted when the
/// driver does not report them (never emitted as null-ish placeholders).
fn power_state_reading(s: &nut::UpsState) -> Reading {
    let mut f = serde_json::Map::new();
    f.insert("status".to_string(), json!(s.status));
    f.insert("online".to_string(), json!(s.on_line()));
    f.insert("on_battery".to_string(), json!(s.on_battery()));
    f.insert("low_battery".to_string(), json!(s.low_battery()));
    if let Some(c) = s.charge_pct {
        f.insert("charge".to_string(), json!(c));
    }
    if let Some(r) = s.runtime_s {
        f.insert("runtime_s".to_string(), json!(r));
    }
    if let Some(l) = s.load_pct {
        f.insert("load_pct".to_string(), json!(l));
    }
    if let Some(v) = s.input_voltage {
        f.insert("input_voltage".to_string(), json!(v));
    }
    if let Some(m) = &s.model {
        f.insert("model".to_string(), json!(m));
    }
    Reading::new("power-state", s.id.clone(), serde_json::Value::Object(f))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn state(id: &str, status: &str) -> nut::UpsState {
        nut::UpsState {
            id: id.to_string(),
            status: status.to_string(),
            charge_pct: Some(100),
            runtime_s: Some(697),
            load_pct: Some(36),
            input_voltage: Some(219.0),
            model: Some("PR3000ERT2U".to_string()),
            vars: BTreeMap::new(),
        }
    }

    #[test]
    fn power_state_reading_carries_decision_signals_and_metrics() {
        let r = power_state_reading(&state("pr3000-nova", "OL"));
        assert_eq!(r.kind, "power-state");
        assert_eq!(r.label, "pr3000-nova");
        assert_eq!(r.fields.get("status").unwrap(), "OL");
        assert_eq!(r.fields.get("online").unwrap(), true);
        assert_eq!(r.fields.get("on_battery").unwrap(), false);
        assert_eq!(r.fields.get("low_battery").unwrap(), false);
        assert_eq!(r.get_i64("charge"), Some(100));
        assert_eq!(r.get_i64("runtime_s"), Some(697));
        assert_eq!(r.get_i64("load_pct"), Some(36));
    }

    #[test]
    fn power_state_reading_reflects_on_battery() {
        let r = power_state_reading(&state("ups0", "OB DISCHRG"));
        assert_eq!(r.fields.get("online").unwrap(), false);
        assert_eq!(r.fields.get("on_battery").unwrap(), true);
    }

    #[test]
    fn power_state_reading_omits_unreported_numeric_fields() {
        let mut s = state("ups0", "OB LB");
        s.charge_pct = None;
        s.runtime_s = None;
        s.input_voltage = None;
        s.model = None;
        let r = power_state_reading(&s);
        assert!(!r.fields.contains_key("charge"));
        assert!(!r.fields.contains_key("runtime_s"));
        assert!(!r.fields.contains_key("input_voltage"));
        assert!(!r.fields.contains_key("model"));
        // The boolean signals are always present.
        assert_eq!(r.fields.get("low_battery").unwrap(), true);
    }
}
