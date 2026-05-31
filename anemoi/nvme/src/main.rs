//! nvme anemos — per-drive NVMe temperature sensor (read-only; controls NO device).
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle (CLI/signals/logging/protocol/
//! restore wiring); the `nvme` tech crate enumerates drives by serial and reads per-drive temps
//! from sysfs. This is a **sensor-only** anemos (`ModuleInfo` curve = `None`): it reports
//! temperatures for routing (e.g. into `rome2d-fans`) and controls nothing — so `apply` ignores the
//! controller, and `restore`/`restore_all` are no-ops (there is nothing to hand back to firmware).
//!
//! detect → one entry per NVMe drive (id = serial, name = model).
//! run <serial> → report that drive's per-sensor temps (Composite, Sensor 1, …).

use anemos::{
    Anemos, Applied, Component, Detected, Device, FoundEntry, Inputs, ModuleInfo, OpenMode,
    Publisher,
};
use serde_json::json;

fn main() -> ! {
    anemos::run(
        ModuleInfo {
            name: "nvme",
            // Sensor-only: no curve, no device control.
            curve_default_path: None,
            curve_env_filename: None,
        },
        NvmeAnemos,
    )
}

struct NvmeAnemos;

impl Anemos for NvmeAnemos {
    fn detect(&mut self) -> Detected {
        Detected::ok(
            nvme::enumerate()
                .into_iter()
                .map(|d| FoundEntry {
                    id: d.serial,
                    kind: "NVMe".to_string(),
                    name: d.model,
                    components: vec![
                        Component::new("drive", "NVMe drive", "ssd").with_publishers(vec![
                            Publisher::new("temp.composite", "Composite", "temperature").unit("C"),
                        ]),
                    ],
                    extra: Default::default(),
                })
                .collect(),
        )
    }

    fn open(&mut self, id: &str, _mode: OpenMode) -> anyhow::Result<Box<dyn Device>> {
        // Bind by serial. Verify presence now so a missing drive is declared fatal (the SDK retries
        // open on a long backoff) rather than limping every tick.
        if !nvme::enumerate().iter().any(|d| d.serial == id) {
            anyhow::bail!("NVMe drive not present");
        }
        Ok(Box::new(NvmeDrive {
            serial: id.to_string(),
        }))
    }

    fn restore_all(&mut self) {
        // Sensor-only: nothing to restore.
    }
}

/// One NVMe drive bound by serial. The path is re-resolved each tick so a re-enumeration (e.g. a
/// drive that dropped and returned as a different `nvmeN`) is tracked by the stable serial.
struct NvmeDrive {
    serial: String,
}

impl Device for NvmeDrive {
    fn collect(&mut self, _inputs: Option<&Inputs>) -> Applied {
        // Sensor-only: read this drive's temps and report them; control nothing, ignore the curve.
        let Some(info) = nvme::enumerate()
            .into_iter()
            .find(|d| d.serial == self.serial)
        else {
            return Applied::error("NVMe drive no longer present".to_string());
        };
        let temps = nvme::read_temps(&info.path);
        if temps.is_empty() {
            return Applied::error("no NVMe temperatures readable".to_string());
        }
        let publishers = temps
            .into_iter()
            .map(|(label, t)| {
                Publisher::new(temp_publisher_id(&label), label, "temperature")
                    .value(json!(t))
                    .unit("C")
            })
            .collect();
        Applied::ok(vec![
            Component::new("drive", info.model, "ssd").with_publishers(publishers)
        ])
    }

    fn restore(&mut self) {
        // Sensor-only: nothing to restore.
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
