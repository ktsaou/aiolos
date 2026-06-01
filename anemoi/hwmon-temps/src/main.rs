//! hwmon-temps anemos — generic Linux sysfs temperature sensor (read-only; controls NO device).
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle; the `hwmon` tech crate reads
//! labelled temperatures from any chip by name. The BMC-less workstation analog of `ipmi-temps`: it
//! reports the **board** unit (`id` = `board`, shared with `it87` on that host so they merge), split
//! into `board`/`dimms`/`lan` components, and controls nothing.

mod config;

use anemos::{Anemos, Component, Device, Inputs, ModuleInfo, OpenMode, Report, Signal, Unit};
use hwmon::ChipTemps;
use serde_json::json;
use std::collections::{BTreeSet, HashMap};

const BOARD_ID: &str = "board";

fn main() -> ! {
    anemos::run(
        ModuleInfo {
            name: "hwmon-temps",
            curve_default_path: None,
            curve_env_filename: None,
        },
        HwmonTempsAnemos,
    )
}

struct HwmonTempsAnemos;

impl Anemos for HwmonTempsAnemos {
    fn detect(&mut self) -> Report {
        Report::ok(vec![board_unit()], Vec::new(), Vec::new())
    }

    fn open(&mut self, _id: &str, _mode: OpenMode) -> anyhow::Result<Box<dyn Device>> {
        Ok(Box::new(HwmonTemps))
    }

    fn restore_all(&mut self) {}
}

struct HwmonTemps;

impl Device for HwmonTemps {
    fn collect(&mut self, _inputs: Option<&Inputs>) -> Report {
        let chips = config::chips();
        let (components, signals) = build(&hwmon::read_chip_temps(&chips));
        if signals.is_empty() {
            return Report::error(format!(
                "no temperatures readable from configured chips: {}",
                chips.join(", ")
            ));
        }
        Report::ok(vec![board_unit()], components, signals)
    }

    fn restore(&mut self) {}
}

fn board_unit() -> Unit {
    Unit::new(BOARD_ID)
        .name("board")
        .description("workstation board")
        .typed("board")
}

/// Map a chip name to its component (suffix, type): DIMM SPD hubs → `dimms`, NIC → `lan`, the rest
/// (board/VRM super-I/O, WMI) → `board`.
fn component_for(chip: &str) -> (&'static str, &'static str) {
    let c = chip.to_ascii_lowercase();
    if c.contains("spd5118") || c.contains("dimm") {
        ("dimms", "dimm")
    } else if c.contains("r8169") || c.contains("igc") || c.contains("lan") || c.contains("nic") {
        ("lan", "nic")
    } else {
        ("board", "board")
    }
}

/// Build the board's components + temperature producer signals, with unambiguous labels (a chip name
/// with multiple instances gets `@instance`; multiple sensors get `.sensor`).
fn build(groups: &[ChipTemps]) -> (Vec<Component>, Vec<Signal>) {
    let mut instances_per_chip = HashMap::<&str, usize>::new();
    for g in groups {
        *instances_per_chip.entry(g.chip.as_str()).or_insert(0) += 1;
    }
    let mut seen = BTreeSet::new();
    let mut components = Vec::new();
    let mut signals = Vec::new();
    for g in groups {
        let multi_instance = instances_per_chip
            .get(g.chip.as_str())
            .copied()
            .unwrap_or(1)
            > 1;
        let base = if multi_instance {
            format!("{}@{}", g.chip, g.instance)
        } else {
            g.chip.clone()
        };
        let multi_sensor = g.temps.len() > 1;
        let (suffix, ctype) = component_for(&g.chip);
        let cid = format!("{BOARD_ID}:{suffix}");
        if seen.insert(cid.clone()) {
            components.push(Component::new(&cid, BOARD_ID).name(suffix).typed(ctype));
        }
        for (sensor, temp) in &g.temps {
            let label = if multi_sensor {
                format!("{base}.{sensor}")
            } else {
                base.clone()
            };
            signals.push(
                Signal::producer(format!("{cid}:{}", slug(&label)), &cid, "temperature")
                    .value(json!(temp))
                    .uom("C")
                    .name(label),
            );
        }
    }
    (components, signals)
}

fn slug(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chip(name: &str, instance: &str, temps: &[(&str, i32)]) -> ChipTemps {
        ChipTemps {
            chip: name.to_string(),
            instance: instance.to_string(),
            temps: temps.iter().map(|(l, t)| (l.to_string(), *t)).collect(),
        }
    }

    /// (signal display name, value, component id) for each produced signal.
    fn rows(groups: &[ChipTemps]) -> Vec<(String, i64, String)> {
        let (_c, s) = build(groups);
        s.iter()
            .map(|sig| {
                (
                    sig.labels.get("name").cloned().unwrap_or_default(),
                    sig.value_i64().unwrap(),
                    sig.component.clone(),
                )
            })
            .collect()
    }

    #[test]
    fn single_instance_single_sensor_uses_bare_chip_name() {
        let r = rows(&[chip("gigabyte_wmi", "virt", &[("temp1", 31)])]);
        assert_eq!(
            r,
            vec![("gigabyte_wmi".to_string(), 31, "board:board".to_string())]
        );
    }

    #[test]
    fn single_instance_multiple_sensors_appends_sensor() {
        let r = rows(&[chip(
            "gigabyte_wmi",
            "virt",
            &[("temp1", 31), ("temp2", 40)],
        )]);
        assert_eq!(
            r,
            vec![
                (
                    "gigabyte_wmi.temp1".to_string(),
                    31,
                    "board:board".to_string()
                ),
                (
                    "gigabyte_wmi.temp2".to_string(),
                    40,
                    "board:board".to_string()
                ),
            ]
        );
    }

    #[test]
    fn dimms_route_to_the_dimms_component_disambiguated_by_device() {
        let r = rows(&[
            chip("spd5118", "11-0050", &[("temp1", 36)]),
            chip("spd5118", "11-0051", &[("temp1", 32)]),
        ]);
        assert_eq!(
            r,
            vec![
                ("spd5118@11-0050".to_string(), 36, "board:dimms".to_string()),
                ("spd5118@11-0051".to_string(), 32, "board:dimms".to_string()),
            ]
        );
    }

    #[test]
    fn nic_routes_to_lan_component() {
        let r = rows(&[chip("r8169", "0000:06:00", &[("temp1", 38)])]);
        assert_eq!(r, vec![("r8169".to_string(), 38, "board:lan".to_string())]);
    }
}
