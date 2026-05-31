//! nut anemos — UPS / utility-power state sensor via NUT (read-only; controls NO device).
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle (CLI/signals/logging/protocol/
//! restore wiring); the `nut` tech crate shells out to `upsc` to list UPS names and read variables.
//! This is a **sensor-only** anemos (`ModuleInfo` curve = `None`): it reports each UPS's
//! utility-power state as a new reading type `power-state`, routed (e.g. `input=nut`) into a reactor
//! such as `nvidia-powercap`. It controls nothing — so `apply` ignores the controller and
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
    Anemos, Applied, Component, Detected, Device, FoundEntry, Inputs, ModuleInfo, OpenMode,
    Publisher,
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
                    components: vec![Component::new("ups", "UPS", "power")],
                    extra: Default::default(),
                })
                .collect(),
        )
    }

    fn open(&mut self, id: &str, _mode: OpenMode) -> anyhow::Result<Box<dyn Device>> {
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
    fn collect(&mut self, _inputs: Option<&Inputs>) -> Applied {
        // Sensor-only: read this UPS's state and report it as a `power-state` reading.
        match nut::read(&self.id) {
            Ok(s) => Applied::ok(vec![power_state_component(&s)]),
            Err(e) => Applied::error(e),
        }
    }

    fn restore(&mut self) {
        // Sensor-only: nothing to restore.
    }
}

/// Build the power-state component for one UPS. The booleans (`online`/`on_battery`/`low_battery`)
/// are the decision-ready signals a reactor keys off; the raw `status` flags and the numeric fields
/// are included for the status page and for richer policies. Numeric fields are omitted when the
/// driver does not report them (never emitted as null-ish placeholders).
fn power_state_component(s: &nut::UpsState) -> Component {
    let mut publishers = vec![
        Publisher::new("status", "Status", "power-status").value(json!(s.status)),
        Publisher::new("online", "Online", "power-online").value(json!(s.on_line())),
        Publisher::new("on_battery", "On battery", "power-on-battery").value(json!(s.on_battery())),
        Publisher::new("low_battery", "Low battery", "power-low-battery")
            .value(json!(s.low_battery())),
    ];
    if let Some(c) = s.charge_pct {
        publishers.push(
            Publisher::new("charge", "Charge", "power-charge")
                .value(json!(c))
                .unit("%")
                .range(0.0, 100.0),
        );
    }
    if let Some(r) = s.runtime_s {
        publishers.push(
            Publisher::new("runtime", "Runtime", "power-runtime")
                .value(json!(r))
                .unit("s"),
        );
    }
    if let Some(l) = s.load_pct {
        publishers.push(
            Publisher::new("load", "Load", "power-load")
                .value(json!(l))
                .unit("%")
                .range(0.0, 100.0),
        );
    }
    if let Some(v) = s.input_voltage {
        publishers.push(
            Publisher::new("input_voltage", "Input voltage", "power-voltage")
                .value(json!(v))
                .unit("V"),
        );
    }
    if let Some(m) = &s.model {
        publishers.push(Publisher::new("model", "Model", "power-model").value(json!(m)));
    }
    Component::new("ups", s.id.clone(), "power").with_publishers(publishers)
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
        let c = power_state_component(&state("pr3000-nova", "OL"));
        assert_eq!(c.class, "power");
        assert_eq!(c.label, "pr3000-nova");
        assert_eq!(pub_value(&c, "status"), Some(json!("OL")));
        assert_eq!(pub_value(&c, "online"), Some(json!(true)));
        assert_eq!(pub_value(&c, "on_battery"), Some(json!(false)));
        assert_eq!(pub_value(&c, "low_battery"), Some(json!(false)));
        assert_eq!(pub_i64(&c, "charge"), Some(100));
        assert_eq!(pub_i64(&c, "runtime"), Some(697));
        assert_eq!(pub_i64(&c, "load"), Some(36));
    }

    #[test]
    fn power_state_reading_reflects_on_battery() {
        let c = power_state_component(&state("ups0", "OB DISCHRG"));
        assert_eq!(pub_value(&c, "online"), Some(json!(false)));
        assert_eq!(pub_value(&c, "on_battery"), Some(json!(true)));
    }

    #[test]
    fn power_state_reading_omits_unreported_numeric_fields() {
        let mut s = state("ups0", "OB LB");
        s.charge_pct = None;
        s.runtime_s = None;
        s.input_voltage = None;
        s.model = None;
        let c = power_state_component(&s);
        assert!(pub_value(&c, "charge").is_none());
        assert!(pub_value(&c, "runtime").is_none());
        assert!(pub_value(&c, "input_voltage").is_none());
        assert!(pub_value(&c, "model").is_none());
        // The boolean signals are always present.
        assert_eq!(pub_value(&c, "low_battery"), Some(json!(true)));
    }

    fn pub_value(c: &Component, id: &str) -> Option<serde_json::Value> {
        c.publishers
            .iter()
            .find(|p| p.id == id)
            .and_then(|p| p.value.clone())
    }

    fn pub_i64(c: &Component, id: &str) -> Option<i64> {
        c.publishers
            .iter()
            .find(|p| p.id == id)
            .and_then(|p| p.value_i64())
    }
}
