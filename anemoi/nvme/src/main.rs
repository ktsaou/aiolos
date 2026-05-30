//! nvme anemos — per-drive NVMe temperature sensor (read-only; controls NO device).
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle (CLI/signals/logging/protocol/
//! restore wiring); the `nvme` tech crate enumerates drives by serial and reads per-drive temps
//! from sysfs. This is a **sensor-only** anemos (`ModuleInfo` curve = `None`): it reports
//! temperatures for routing (e.g. into `asrock16-2t`) and controls nothing — so `apply` ignores the
//! controller, and `restore`/`restore_all` are no-ops (there is nothing to hand back to firmware).
//!
//! detect → one entry per NVMe drive (id = serial, name = model).
//! run <serial> → report that drive's per-sensor temps (Composite, Sensor 1, …).

use anemos::{
    Anemos, Applied, Controller, Detected, Device, FoundEntry, Inputs, ModuleInfo, Reading,
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
                    extra: Default::default(),
                })
                .collect(),
        )
    }

    fn open(&mut self, id: &str) -> anyhow::Result<Box<dyn Device>> {
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
    fn apply(&mut self, _inputs: Option<&Inputs>, _ctrl: &mut Controller) -> Applied {
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
        let readings = temps
            .into_iter()
            .map(|(label, t)| Reading::new("temp", label, json!({ "temp": t })))
            .collect();
        Applied::ok(readings)
    }

    fn restore(&mut self) {
        // Sensor-only: nothing to restore.
    }
}
