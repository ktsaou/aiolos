//! nut anemos — UPS / utility-power state sensor via NUT (read-only; controls NO device).
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle; the `nut` tech crate shells out
//! to `upsc`. **Sensor-only** (`ModuleInfo` curve = `None`): it reports each UPS's utility-power
//! state, routed (e.g. `input=nut`) into a reactor such as `nvidia-powercap`.
//!
//! Each UPS is one **unit** (`id` = upsc name; short `name` = `upsN`; type `ups`) with a `power`
//! component carrying the state producers (`power-online`/`power-charge`/…). Which UPS(es) to
//! monitor comes from operator config `nut.conf`; no credentials live in committed artifacts.

mod config;

use anemos::{Anemos, Component, Device, Inputs, ModuleInfo, OpenMode, Report, Signal, Unit};
use serde_json::json;

fn main() -> ! {
    anemos::run(
        ModuleInfo {
            name: "nut",
            curve_default_path: None,
            curve_env_filename: None,
        },
        NutAnemos,
    )
}

struct NutAnemos;

impl Anemos for NutAnemos {
    fn detect(&mut self) -> Report {
        let (mut units, mut components, mut signals) = (Vec::new(), Vec::new(), Vec::new());
        for (i, id) in config::ups_ids().into_iter().enumerate() {
            let comp = format!("{id}:power");
            units.push(ups_unit(&id, i, &id));
            components.push(Component::new(&comp, &id).name("power").typed("power"));
            signals.push(
                Signal::producer(format!("{comp}:online"), &comp, "power-online").name("online"),
            );
        }
        Report::ok(units, components, signals)
    }

    fn open(&mut self, id: &str, _mode: OpenMode) -> anyhow::Result<Box<dyn Device>> {
        // Do NOT fail open if the UPS is momentarily unreadable: a transient unreachable UPS is
        // reported by `apply` as `error` (the orchestrator keeps the instance and retries).
        let index = config::ups_ids().iter().position(|u| u == id).unwrap_or(0);
        Ok(Box::new(UpsSensor {
            id: id.to_string(),
            index,
        }))
    }

    fn restore_all(&mut self) {}
}

struct UpsSensor {
    id: String,
    index: usize,
}

impl Device for UpsSensor {
    fn collect(&mut self, _inputs: Option<&Inputs>) -> Report {
        match nut::read(&self.id) {
            Ok(s) => ups_report(&s, self.index),
            Err(e) => Report::error(e),
        }
    }

    fn restore(&mut self) {}
}

fn ups_unit(id: &str, index: usize, description: &str) -> Unit {
    Unit::new(id)
        .name(format!("ups{index}"))
        .description(description)
        .typed("ups")
}

/// Build the live report for one UPS: its unit, a `power` component, and the state producers. The
/// booleans (`online`/`on_battery`/`low_battery`) are decision-ready signals a reactor keys off; the
/// raw `status` flags + numeric fields enrich the status page. Numeric fields are omitted when the
/// driver does not report them (never null-ish placeholders).
fn ups_report(s: &nut::UpsState, index: usize) -> Report {
    let uid = s.id.clone();
    let comp = format!("{uid}:power");
    let desc = s.model.clone().unwrap_or_else(|| uid.clone());
    let units = vec![ups_unit(&uid, index, &desc)];
    let components = vec![Component::new(&comp, &uid).name("power").typed("power")];
    let mut signals = vec![
        Signal::producer(format!("{comp}:status"), &comp, "power-status")
            .value(json!(s.status))
            .name("status"),
        Signal::producer(format!("{comp}:online"), &comp, "power-online")
            .value(json!(s.on_line()))
            .name("online"),
        Signal::producer(format!("{comp}:on_battery"), &comp, "power-on-battery")
            .value(json!(s.on_battery()))
            .name("on battery"),
        Signal::producer(format!("{comp}:low_battery"), &comp, "power-low-battery")
            .value(json!(s.low_battery()))
            .name("low battery"),
    ];
    if let Some(c) = s.charge_pct {
        signals.push(
            Signal::producer(format!("{comp}:charge"), &comp, "power-charge")
                .value(json!(c))
                .uom("%")
                .range(0.0, 100.0)
                .name("charge"),
        );
    }
    if let Some(r) = s.runtime_s {
        signals.push(
            Signal::producer(format!("{comp}:runtime"), &comp, "power-runtime")
                .value(json!(r))
                .uom("s")
                .name("runtime"),
        );
    }
    if let Some(l) = s.load_pct {
        signals.push(
            Signal::producer(format!("{comp}:load"), &comp, "power-load")
                .value(json!(l))
                .uom("%")
                .range(0.0, 100.0)
                .name("load"),
        );
    }
    if let Some(v) = s.input_voltage {
        signals.push(
            Signal::producer(format!("{comp}:input_voltage"), &comp, "power-voltage")
                .value(json!(v))
                .uom("V")
                .name("input voltage"),
        );
    }
    if let Some(m) = &s.model {
        signals.push(
            Signal::producer(format!("{comp}:model"), &comp, "power-model")
                .value(json!(m))
                .name("model"),
        );
    }
    Report::ok(units, components, signals)
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

    fn sig<'a>(r: &'a Report, suffix: &str) -> Option<&'a Signal> {
        r.signals.iter().find(|s| s.id.ends_with(suffix))
    }

    #[test]
    fn ups_report_carries_decision_signals_and_metrics() {
        let r = ups_report(&state("pr3000-nova", "OL"), 0);
        assert_eq!(
            r.units[0].labels.get("type").map(String::as_str),
            Some("ups")
        );
        assert_eq!(
            r.units[0].labels.get("name").map(String::as_str),
            Some("ups0")
        );
        assert_eq!(
            sig(&r, ":status").and_then(|s| s.value.clone()),
            Some(json!("OL"))
        );
        assert_eq!(
            sig(&r, ":online").and_then(|s| s.value.clone()),
            Some(json!(true))
        );
        assert_eq!(
            sig(&r, ":on_battery").and_then(|s| s.value.clone()),
            Some(json!(false))
        );
        assert_eq!(sig(&r, ":charge").and_then(|s| s.value_i64()), Some(100));
        assert_eq!(sig(&r, ":runtime").and_then(|s| s.value_i64()), Some(697));
        assert_eq!(sig(&r, ":load").and_then(|s| s.value_i64()), Some(36));
    }

    #[test]
    fn ups_report_reflects_on_battery() {
        let r = ups_report(&state("ups0", "OB DISCHRG"), 1);
        assert_eq!(
            sig(&r, ":online").and_then(|s| s.value.clone()),
            Some(json!(false))
        );
        assert_eq!(
            sig(&r, ":on_battery").and_then(|s| s.value.clone()),
            Some(json!(true))
        );
    }

    #[test]
    fn ups_report_omits_unreported_numeric_fields() {
        let mut s = state("ups0", "OB LB");
        s.charge_pct = None;
        s.runtime_s = None;
        s.input_voltage = None;
        s.model = None;
        let r = ups_report(&s, 0);
        assert!(sig(&r, ":charge").is_none());
        assert!(sig(&r, ":runtime").is_none());
        assert!(sig(&r, ":input_voltage").is_none());
        assert!(sig(&r, ":model").is_none());
        assert_eq!(
            sig(&r, ":low_battery").and_then(|s| s.value.clone()),
            Some(json!(true))
        );
    }
}
