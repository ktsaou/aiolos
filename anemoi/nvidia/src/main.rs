//! nvidia anemos — per-GPU onboard fan control via NVML.
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle (CLI dispatch, signals, logging,
//! curve+EMA, the protocol stdio loops, and the restore-on-shutdown/EOF/signal wiring); `nvml` owns
//! NVML access. This module supplies just detect / open / apply / restore.

use anemos::{
    Anemos, Applied, Component, Controller, Detected, Device, FoundEntry, Inputs, ModuleInfo,
    OpenMode, Publisher, Sink, SinkState,
};
use nvml::{Detector, Gpu};
use serde_json::json;

fn main() -> ! {
    anemos::run(
        ModuleInfo {
            name: "nvidia",
            curve_default_path: Some("/opt/aiolos/etc/nvidia.curve.json"),
            curve_env_filename: Some("nvidia.curve.json"),
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
                            components: vec![gpu_schema_component("gpu", "GPU", g.num_fans)],
                            extra,
                        }
                    })
                    .collect(),
            ),
            Err(e) => Detected::error(format!("NVML enumeration failed: {e}")),
        }
    }

    fn open(&mut self, id: &str, mode: OpenMode) -> anyhow::Result<Box<dyn Device>> {
        let gpu = match mode {
            // Read-only info must not run the fan-restore Drop backstop: this process never claimed
            // fans, so dropping it must not hand control back or perturb an existing controller.
            OpenMode::Observe => Gpu::open(id)?.without_fan_restore_on_drop(),
            OpenMode::Control => Gpu::open(id)?,
        };
        Ok(Box::new(GpuDevice { gpu }))
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
    fn collect(&mut self, _inputs: Option<&Inputs>) -> Applied {
        let temp = match self.gpu.temperature() {
            Ok(t) => t,
            Err(e) => return Applied::error(e.to_string()),
        };
        Applied::ok(vec![self.component(temp, None, SinkState::Unknown)])
    }

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

        Applied::ok(vec![self.component(
            temp,
            duty.pct,
            if duty.pct.is_some() {
                SinkState::Claimed
            } else {
                SinkState::Released
            },
        )])
    }

    fn restore(&mut self) {
        match self.gpu.restore_fans() {
            Ok(()) => tracing::info!("GPU fans restored to firmware default"),
            Err(e) => eprintln!("WARNING: fan restore failed: {e}"),
        }
    }
}

impl GpuDevice {
    fn component(&mut self, temp: i32, commanded_pct: Option<u32>, state: SinkState) -> Component {
        // Report the RAW GPU temp (the orchestrator routes the true temperature), fan duty/RPM
        // readbacks where available, and the sink state/target when this process commanded one.
        let mut publishers = vec![Publisher::new("temp", "Temperature", "temperature")
            .value(json!(temp))
            .unit("C")];
        for fan in 0..self.gpu.num_fans() {
            if let Some(pwm) = commanded_pct.or_else(|| self.gpu.fan_speed(fan)) {
                publishers.push(
                    Publisher::new(
                        format!("fan{fan}.duty"),
                        format!("fan{fan} duty"),
                        "fan-duty",
                    )
                    .value(json!(pwm))
                    .unit("%")
                    .range(0.0, 100.0),
                );
            }
            if let Some(rpm) = self.gpu.fan_rpm(fan) {
                publishers.push(
                    Publisher::new(format!("fan{fan}.rpm"), format!("fan{fan} RPM"), "fan-rpm")
                        .value(json!(rpm))
                        .unit("rpm"),
                );
            }
        }
        let mut sink = Sink::new("fans", "GPU fans", "fan-duty")
            .range(0.0, 100.0)
            .unit("%")
            .safe(json!("auto"))
            .needs_claim(true)
            .state(state)
            .direction("up=more-cooling")
            .driven_by(vec![anemos::DrivenBy::new(format!(
                "nvidia:{}",
                self.gpu.uuid()
            ))
            .publisher("gpu/temp")
            .value(json!(temp))
            .unit("C")]);
        if let Some(pct) = commanded_pct {
            sink = sink.value(json!(pct));
        }
        Component::new("gpu", self.gpu.uuid().to_string(), "gpu")
            .with_publishers(publishers)
            .with_sinks(vec![sink])
    }
}

fn gpu_schema_component(id: &str, label: &str, fans: u32) -> Component {
    let mut publishers = vec![Publisher::new("temp", "Temperature", "temperature").unit("C")];
    for fan in 0..fans {
        publishers.push(
            Publisher::new(
                format!("fan{fan}.duty"),
                format!("fan{fan} duty"),
                "fan-duty",
            )
            .unit("%")
            .range(0.0, 100.0),
        );
        publishers.push(
            Publisher::new(format!("fan{fan}.rpm"), format!("fan{fan} RPM"), "fan-rpm").unit("rpm"),
        );
    }
    Component::new(id, label, "gpu")
        .with_publishers(publishers)
        .with_sinks(vec![Sink::new("fans", "GPU fans", "fan-duty")
            .range(0.0, 100.0)
            .unit("%")
            .safe(json!("auto"))
            .needs_claim(true)
            .direction("up=more-cooling")])
}
