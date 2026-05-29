//! nvidia anemos — per-GPU onboard fan control via NVML.
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle (CLI dispatch, signals, logging,
//! curve+EMA, the protocol stdio loops, and the restore-on-shutdown/EOF/signal wiring); `nvml` owns
//! NVML access. This module supplies just detect / open / apply / restore.

use anemos::{
    Anemos, Applied, Controller, Detected, Device, FoundEntry, Inputs, ModuleInfo, Reading,
};
use nvml::{Detector, Gpu};
use serde_json::json;

fn main() -> ! {
    anemos::run(
        ModuleInfo {
            name: "nvidia",
            curve_default_path: "/opt/aiolos/etc/nvidia.curve.json",
            curve_env_filename: "nvidia.curve.json",
        },
        Nvidia {
            detector: Detector::new(),
        },
    )
}

struct Nvidia {
    detector: Detector,
}

impl Anemos for Nvidia {
    fn detect(&mut self) -> Detected {
        match self.detector.enumerate() {
            Ok(gpus) => Detected::ok(
                gpus.into_iter()
                    .map(|g| {
                        let mut extra = serde_json::Map::new();
                        extra.insert("fans".to_string(), json!(g.num_fans));
                        FoundEntry {
                            id: g.uuid,
                            kind: "GPU".to_string(),
                            name: g.name,
                            extra,
                        }
                    })
                    .collect(),
            ),
            Err(e) => Detected::error(format!("NVML enumeration failed: {e}")),
        }
    }

    fn open(&mut self, id: &str) -> anyhow::Result<Box<dyn Device>> {
        Ok(Box::new(GpuDevice {
            gpu: Gpu::open(id)?,
        }))
    }

    fn restore_all(&mut self) {
        if let Err(e) = nvml::restore_all() {
            eprintln!("restore FAILED: {e}");
            std::process::exit(2);
        }
    }
}

struct GpuDevice {
    gpu: Gpu,
}

impl Device for GpuDevice {
    fn apply(&mut self, _inputs: Option<&Inputs>, ctrl: &mut Controller) -> Applied {
        // nvidia ignores routed inputs — it uses its own GPU temperature.
        let temp = match self.gpu.temperature() {
            Ok(t) => t,
            Err(e) => {
                // A failed read must not leave the GPU manual-but-unregulated: revert to firmware.
                let _ = self.gpu.restore_fans();
                return Applied::error(e.to_string());
            }
        };
        let duty = ctrl.duty(temp);
        tracing::info!(uuid = %self.gpu.uuid(), temp, commanded_pct = ?duty.pct,
            fans = self.gpu.num_fans(), "decision: set GPU fans");
        let set = match duty.pct {
            Some(p) => self.gpu.set_all_fans(p),
            None => self.gpu.set_all_default(), // empty curve -> firmware/default control
        };
        if let Err(e) = set {
            let _ = self.gpu.restore_fans();
            return Applied::error(e.to_string());
        }

        // Report the RAW GPU temp (the orchestrator routes the true temperature) + the commanded
        // duty + actual RPM where available.
        let mut readings = vec![Reading::new("temp", "GPU", json!({ "temp": temp }))];
        for fan in 0..self.gpu.num_fans() {
            let mut f = serde_json::Map::new();
            if let Some(pwm) = duty.pct.or_else(|| self.gpu.fan_speed(fan)) {
                f.insert("pwm".to_string(), json!(pwm));
            }
            if let Some(rpm) = self.gpu.fan_rpm(fan) {
                f.insert("rpm".to_string(), json!(rpm));
            }
            readings.push(Reading::new("fan", format!("fan{fan}"), json!(f)));
        }
        Applied::ok(readings)
    }

    fn restore(&mut self) {
        match self.gpu.restore_fans() {
            Ok(()) => tracing::info!("GPU fans restored to firmware default"),
            Err(e) => eprintln!("WARNING: fan restore failed: {e}"),
        }
    }
}
