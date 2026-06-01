//! ipmi-temps anemos — ASRockRack ROME2D16-2T BMC analog temperature sensors (read-only; controls
//! NO device).
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle; the `ipmi` tech crate is the
//! inband `/dev/ipmi0` transport plus the standard `Get Sensor Reading` helpers. **Sensor-only**
//! (`ModuleInfo` curve = `None`): it reports the board/CPU/DIMM/NIC temperatures for routing (e.g.
//! into `rome2d-fans`) and controls nothing.
//!
//! It reports the **motherboard unit** (`id` = `board`, so it merges with `rome2d-fans`'s fans into
//! one unit), split into real **components** — `cpu0`/`cpu1`, `dimms`, `lan`, `board` — each carrying
//! its temperature producers (a hot DIMM or NIC is its own component, not buried in one blob).

mod sensors;

use anemos::{Anemos, Component, Device, Inputs, ModuleInfo, OpenMode, Report, Signal, Unit};
use sensors::Sensors;
use serde_json::json;
use std::collections::BTreeSet;

/// The stable id of the physical motherboard unit (shared with `rome2d-fans` so they merge).
const BOARD_ID: &str = "board";
const BOARD_DESC: &str = "ASRockRack ROME2D16-2T";

fn main() -> ! {
    anemos::run(
        ModuleInfo {
            name: "ipmi-temps",
            curve_default_path: None,
            curve_env_filename: None,
        },
        IpmiTempsAnemos,
    )
}

struct IpmiTempsAnemos;

impl Anemos for IpmiTempsAnemos {
    fn detect(&mut self) -> Report {
        // One unit (the motherboard); its components/signals are discovered live in `collect` (the
        // populated sensor set is only known after reading). `units[].id` is what aiolos spawns on.
        Report::ok(vec![board_unit()], Vec::new(), Vec::new())
    }

    fn open(&mut self, _id: &str, _mode: OpenMode) -> anyhow::Result<Box<dyn Device>> {
        let mut sensors = Sensors::open()?;
        // Warm the per-sensor conversion-factor cache once here (off the apply deadline).
        sensors.prefetch_factors();
        Ok(Box::new(BmcTemps { sensors }))
    }

    fn restore_all(&mut self) {}
}

struct BmcTemps {
    sensors: Sensors,
}

impl Device for BmcTemps {
    fn collect(&mut self, _inputs: Option<&Inputs>) -> Report {
        let temps = self.sensors.read_temps();
        if temps.is_empty() {
            return Report::error("no BMC temperatures readable".to_string());
        }
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut components = Vec::new();
        let mut signals = Vec::new();
        for (label, t) in temps {
            let (suffix, cname, ctype) = component_for(label);
            let cid = format!("{BOARD_ID}:{suffix}");
            if seen.insert(cid.clone()) {
                components.push(Component::new(&cid, BOARD_ID).name(cname).typed(ctype));
            }
            signals.push(
                Signal::producer(format!("{cid}:{}", slug(label)), &cid, "temperature")
                    .value(json!(t))
                    .uom("C")
                    .name(label),
            );
        }
        Report::ok(vec![board_unit()], components, signals)
    }

    fn restore(&mut self) {}
}

fn board_unit() -> Unit {
    Unit::new(BOARD_ID)
        .name("board")
        .description(BOARD_DESC)
        .typed("board")
}

/// Map a BMC sensor label to its component (suffix, display name, type). CPUs split per socket; DIMM
/// temps group into `dimms`; the NIC into `lan`; everything else (MB1/2, CARD_SIDE, …) into `board`.
fn component_for(label: &str) -> (String, String, &'static str) {
    let up = label.to_ascii_uppercase();
    if let Some(rest) = up.strip_prefix("CPU") {
        let n: u32 = rest.trim().parse().unwrap_or(1);
        let idx = n.saturating_sub(1);
        (format!("cpu{idx}"), format!("cpu{idx}"), "cpu")
    } else if up.contains("DDR") || up.contains("DIMM") {
        ("dimms".into(), "dimms".into(), "dimm")
    } else if up.contains("LAN") || up.contains("NIC") {
        ("lan".into(), "lan".into(), "nic")
    } else {
        ("board".into(), "board".into(), "board")
    }
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
