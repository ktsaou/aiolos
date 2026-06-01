//! nvme anemos — per-drive NVMe temperature sensor (read-only; controls NO device).
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle; the `nvme` tech crate enumerates
//! drives by serial and reads per-drive temps from sysfs. **Sensor-only** (`ModuleInfo` curve =
//! `None`): it reports temperatures for routing (e.g. into `rome2d-fans`) and controls nothing.
//!
//! Each drive is one **unit** (`id` = serial; short `name` = `nvmeN` from enumeration order; type
//! `ssd`) with a `drive` component carrying one temperature producer per sensor (Composite, …).

use anemos::{Anemos, Component, Device, Inputs, ModuleInfo, OpenMode, Report, Signal, Unit};
use serde_json::json;

fn main() -> ! {
    anemos::run(
        ModuleInfo {
            name: "nvme",
            curve_default_path: None,
            curve_env_filename: None,
        },
        NvmeAnemos,
    )
}

struct NvmeAnemos;

impl Anemos for NvmeAnemos {
    fn detect(&mut self) -> Report {
        let (mut units, mut components, mut signals) = (Vec::new(), Vec::new(), Vec::new());
        for (i, d) in nvme::enumerate().into_iter().enumerate() {
            let comp = format!("{}:drive", d.serial);
            units.push(drive_unit(&d.serial, i, &d.model));
            components.push(Component::new(&comp, &d.serial).name("drive").typed("ssd"));
            signals.push(
                Signal::producer(format!("{comp}:composite"), &comp, "temperature")
                    .uom("C")
                    .name("Composite"),
            );
        }
        Report::ok(units, components, signals)
    }

    fn open(&mut self, id: &str, _mode: OpenMode) -> anyhow::Result<Box<dyn Device>> {
        // Bind by serial. Verify presence now so a missing drive is declared fatal (the SDK retries
        // open on a long backoff) rather than limping every tick.
        let Some(index) = nvme::enumerate().iter().position(|d| d.serial == id) else {
            anyhow::bail!("NVMe drive not present");
        };
        Ok(Box::new(NvmeDrive {
            serial: id.to_string(),
            index,
        }))
    }

    fn restore_all(&mut self) {}
}

/// One NVMe drive bound by serial. The path is re-resolved each tick so a re-enumeration (a drive
/// that dropped and returned as a different `nvmeN`) is tracked by the stable serial.
struct NvmeDrive {
    serial: String,
    index: usize,
}

impl Device for NvmeDrive {
    fn collect(&mut self, _inputs: Option<&Inputs>) -> Report {
        let Some(info) = nvme::enumerate()
            .into_iter()
            .find(|d| d.serial == self.serial)
        else {
            return Report::error("NVMe drive no longer present".to_string());
        };
        let temps = nvme::read_temps(&info.path);
        if temps.is_empty() {
            return Report::error("no NVMe temperatures readable".to_string());
        }
        let comp = format!("{}:drive", self.serial);
        let units = vec![drive_unit(&self.serial, self.index, &info.model)];
        let components = vec![Component::new(&comp, &self.serial)
            .name("drive")
            .typed("ssd")];
        let signals = temps
            .into_iter()
            .map(|(label, t)| {
                Signal::producer(format!("{comp}:{}", slug(&label)), &comp, "temperature")
                    .value(json!(t))
                    .uom("C")
                    .name(label)
            })
            .collect();
        Report::ok(units, components, signals)
    }

    fn restore(&mut self) {}
}

fn drive_unit(serial: &str, index: usize, model: &str) -> Unit {
    Unit::new(serial)
        .name(format!("nvme{index}"))
        .description(model)
        .typed("ssd")
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
