//! ipmi-temps anemos — ASRockRack ROME2D16-2T BMC analog temperature sensors (read-only; controls
//! NO device).
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle (CLI/signals/logging/protocol/
//! restore wiring); the `ipmi` tech crate is the inband `/dev/ipmi0` transport plus the standard
//! `Get Sensor Reading` (0x04/0x2d) + `Get Sensor Reading Factors` (0x04/0x23) helpers (the SAME
//! mechanism the asrock fan module uses for tach RPM). This is a **sensor-only** anemos
//! (`ModuleInfo` curve = `None`): it reports the board/CPU/DIMM/NIC temperatures for routing (e.g.
//! into `asrock16-2t`, so a hot DIMM or NIC raises the board fans) and controls nothing — so `apply`
//! ignores the controller, and `restore`/`restore_all` are no-ops (there is nothing to hand back to
//! firmware).
//!
//! detect → one board entry (all sensors are read in this single process: the IPMI handle is opened
//!          once, and these are register-style BMC reads, not blocking admin commands).
//! run    → report every currently-readable BMC temperature (unavailable/"ns" sensors skipped).

mod sensors;

use anemos::{
    Anemos, Applied, Controller, Detected, Device, FoundEntry, Inputs, ModuleInfo, Reading,
};
use sensors::Sensors;
use serde_json::json;

fn main() -> ! {
    anemos::run(
        ModuleInfo {
            name: "ipmi-temps",
            // Sensor-only: no curve, no device control.
            curve_default_path: None,
            curve_env_filename: None,
        },
        IpmiTempsAnemos,
    )
}

struct IpmiTempsAnemos;

impl Anemos for IpmiTempsAnemos {
    fn detect(&mut self) -> Detected {
        // One instance: all BMC temp sensors are read in a single process (one `/dev/ipmi0` handle,
        // opened once). The id is board-stable; aiolos keys routed readings by `ipmi-temps:<id>`.
        Detected::ok(vec![FoundEntry {
            id: "ipmi-temps".to_string(),
            kind: "board".to_string(),
            name: "ROME2D16-2T BMC temps".to_string(),
            extra: Default::default(),
        }])
    }

    fn open(&mut self, _id: &str) -> anyhow::Result<Box<dyn Device>> {
        let mut sensors = Sensors::open()?;
        // Warm the per-sensor conversion-factor cache once here (off the apply deadline) so the
        // first tick is no heavier than the rest; any that fail are retried lazily during ticks.
        sensors.prefetch_factors();
        Ok(Box::new(BmcTemps { sensors }))
    }

    fn restore_all(&mut self) {
        // Sensor-only: nothing to restore.
    }
}

/// The board's BMC temperature reader, bound for the lifetime of the `run` instance.
struct BmcTemps {
    sensors: Sensors,
}

impl Device for BmcTemps {
    fn apply(&mut self, _inputs: Option<&Inputs>, _ctrl: &mut Controller) -> Applied {
        // Sensor-only: read the BMC temps and report them; control nothing, ignore the curve.
        let temps = self.sensors.read_temps();
        if temps.is_empty() {
            return Applied::error("no BMC temperatures readable".to_string());
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
