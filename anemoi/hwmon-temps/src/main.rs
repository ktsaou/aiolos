//! hwmon-temps anemos — generic Linux sysfs temperature sensor (read-only; controls NO device).
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle (CLI/signals/logging/protocol/
//! restore wiring); the `hwmon` tech crate reads labelled temperatures from any chip by name. This
//! is the BMC-less workstation analog of `ipmi-temps`: it reports board/VRM, DIMM, NIC (and any
//! other configured) temperatures for the status page (and for routing, if ever wired), and controls
//! nothing — so `apply` ignores the controller, and `restore`/`restore_all` are no-ops.
//!
//! detect → one entry ("hwmon"): all configured chips are read in this single process.
//! run    → report every readable temperature from the configured chips, with labels that
//!          disambiguate multiple chips sharing a `name` (e.g. four `spd5118` DIMMs).

mod config;

use anemos::{
    Anemos, Applied, Component, Detected, Device, FoundEntry, Inputs, ModuleInfo, OpenMode,
    Publisher,
};
use hwmon::ChipTemps;
use serde_json::json;

fn main() -> ! {
    anemos::run(
        ModuleInfo {
            name: "hwmon-temps",
            // Sensor-only: no curve, no device control.
            curve_default_path: None,
            curve_env_filename: None,
        },
        HwmonTempsAnemos,
    )
}

struct HwmonTempsAnemos;

impl Anemos for HwmonTempsAnemos {
    fn detect(&mut self) -> Detected {
        // One instance reads all configured chips (cheap sysfs reads, one process).
        Detected::ok(vec![FoundEntry {
            id: "hwmon".to_string(),
            kind: "board".to_string(),
            name: "hwmon sysfs temps".to_string(),
            components: vec![Component::new("hwmon", "hwmon sysfs temps", "board")],
            extra: Default::default(),
        }])
    }

    fn open(&mut self, _id: &str, _mode: OpenMode) -> anyhow::Result<Box<dyn Device>> {
        Ok(Box::new(HwmonTemps))
    }

    fn restore_all(&mut self) {
        // Sensor-only: nothing to restore.
    }
}

struct HwmonTemps;

impl Device for HwmonTemps {
    fn collect(&mut self, _inputs: Option<&Inputs>) -> Applied {
        // Sensor-only: read the configured chips' temps and report them; control nothing.
        let chips = config::chips();
        let components = build_components(&hwmon::read_chip_temps(&chips));
        if components.is_empty() {
            return Applied::error(format!(
                "no temperatures readable from configured chips: {}",
                chips.join(", ")
            ));
        }
        Applied::ok(components)
    }

    fn restore(&mut self) {
        // Sensor-only: nothing to restore.
    }
}

/// Turn per-chip temperature groups into temperature publishers with unambiguous labels:
/// - a chip name with ONE instance → `chip` (single sensor) or `chip.<sensor>` (multiple sensors);
/// - a chip name with MULTIPLE instances → `chip@<instance>` / `chip@<instance>.<sensor>`,
///   where `<instance>` is the chip's stable device discriminator (e.g. an i2c address).
fn build_components(groups: &[ChipTemps]) -> Vec<Component> {
    // Count instances per chip name so we only add the `@instance` discriminator where it's needed.
    let mut instances_per_chip = std::collections::HashMap::<&str, usize>::new();
    for g in groups {
        *instances_per_chip.entry(g.chip.as_str()).or_insert(0) += 1;
    }

    let mut publishers = Vec::new();
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
        for (sensor, temp) in &g.temps {
            let label = if multi_sensor {
                format!("{base}.{sensor}")
            } else {
                base.clone()
            };
            publishers.push(
                Publisher::new(temp_publisher_id(&label), label, "temperature")
                    .value(json!(temp))
                    .unit("C"),
            );
        }
    }
    if publishers.is_empty() {
        Vec::new()
    } else {
        vec![Component::new("hwmon", "hwmon sysfs temps", "board").with_publishers(publishers)]
    }
}

fn temp_publisher_id(label: &str) -> String {
    format!(
        "temp.{}",
        label
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            })
            .collect::<String>()
            .trim_matches('_')
    )
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

    fn labels(cs: &[Component]) -> Vec<(String, i64)> {
        cs.iter()
            .flat_map(|c| c.publishers.iter())
            .map(|p| (p.label.clone(), p.value_i64().unwrap()))
            .collect()
    }

    #[test]
    fn single_instance_single_sensor_uses_bare_chip_name() {
        let g = vec![chip("nvme", "0000:02:00.0", &[("Composite", 37)])];
        assert_eq!(
            labels(&build_components(&g)),
            vec![("nvme".to_string(), 37)]
        );
    }

    #[test]
    fn single_instance_multiple_sensors_appends_sensor() {
        let g = vec![chip(
            "gigabyte_wmi",
            "virt",
            &[("temp1", 31), ("temp2", 40)],
        )];
        assert_eq!(
            labels(&build_components(&g)),
            vec![
                ("gigabyte_wmi.temp1".to_string(), 31),
                ("gigabyte_wmi.temp2".to_string(), 40)
            ]
        );
    }

    #[test]
    fn multiple_instances_disambiguate_by_device() {
        // Four DDR5 DIMMs, each a single-sensor `spd5118` at a distinct i2c address.
        let g = vec![
            chip("spd5118", "11-0050", &[("temp1", 36)]),
            chip("spd5118", "11-0051", &[("temp1", 32)]),
            chip("spd5118", "11-0052", &[("temp1", 35)]),
            chip("spd5118", "11-0053", &[("temp1", 36)]),
        ];
        assert_eq!(
            labels(&build_components(&g)),
            vec![
                ("spd5118@11-0050".to_string(), 36),
                ("spd5118@11-0051".to_string(), 32),
                ("spd5118@11-0052".to_string(), 35),
                ("spd5118@11-0053".to_string(), 36),
            ]
        );
    }

    #[test]
    fn mixed_names_only_disambiguate_the_duplicated_one() {
        let g = vec![
            chip("gigabyte_wmi", "virt", &[("temp1", 31)]),
            chip("r8169", "0000:06:00", &[("temp1", 38)]),
            chip("r8169", "0000:07:00", &[("temp1", 32)]),
        ];
        // gigabyte_wmi is single-instance single-sensor -> bare; r8169 has two -> @instance.
        assert_eq!(
            labels(&build_components(&g)),
            vec![
                ("gigabyte_wmi".to_string(), 31),
                ("r8169@0000:06:00".to_string(), 38),
                ("r8169@0000:07:00".to_string(), 32),
            ]
        );
    }
}
