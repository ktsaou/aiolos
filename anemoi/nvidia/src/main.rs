//! nvidia anemos — per-GPU onboard fan control via NVML (label-driven signal model, SOW-0018).
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle (CLI dispatch, signals, logging,
//! curve+EMA, the protocol stdio loops, restore-on-shutdown/EOF/signal); `nvml` owns NVML access.
//! This module supplies just detect / open / report / restore.
//!
//! A GPU is one **unit** (`id` = its UUID; short `name` = `gpuN` from the NVML index). It has a
//! `temperature` component and one component per fan (an `rpm` producer + a `duty` sink). The whole
//! GPU's fans move together (one NVML set), so each fan's duty reports the same commanded value,
//! driven by this GPU's own temperature.

use anemos::{
    Anemos, Component, Control, Controller, Device, Driving, Inputs, OpenMode, Provenance, Report,
    Signal, SinkState, Unit,
};
use nvml::{Detector, Gpu};
use serde_json::json;

fn main() -> ! {
    anemos::run(
        anemos::ModuleInfo {
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
    fn detect(&mut self) -> Report {
        match self.detector.enumerate() {
            Ok(gpus) => {
                let (mut units, mut components, mut signals) = (Vec::new(), Vec::new(), Vec::new());
                for g in gpus {
                    let (mut u, mut c, mut s) = gpu_schema(&g.uuid, g.index, &g.name, g.num_fans);
                    units.append(&mut u);
                    components.append(&mut c);
                    signals.append(&mut s);
                }
                Report::ok(units, components, signals)
            }
            Err(e) => Report::error(format!("NVML enumeration failed: {e}")),
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
    fn collect(&mut self, _inputs: Option<&Inputs>) -> Report {
        let temp = match self.gpu.temperature() {
            Ok(t) => t,
            Err(e) => return Report::error(e.to_string()),
        };
        self.report(temp, None, SinkState::Unknown)
    }

    fn apply(&mut self, _inputs: Option<&Inputs>, ctrl: &mut Controller) -> Report {
        // nvidia ignores routed inputs — it uses its own GPU temperature.
        let temp = match self.gpu.temperature() {
            Ok(t) => t,
            Err(e) => {
                // A failed read must not leave the GPU manual-but-unregulated: revert to firmware.
                let _ = self.gpu.restore_fans();
                return Report::error(e.to_string());
            }
        };
        let duty = ctrl.duty(temp);
        tracing::info!(gpu = %format!("gpu{}", self.gpu.index()), uuid = %self.gpu.uuid(),
            temp, commanded_pct = ?duty.pct, fans = self.gpu.num_fans(), "decision: set GPU fans");
        let set = match duty.pct {
            Some(p) => self.gpu.set_all_fans(p),
            None => self.gpu.set_all_default(), // empty curve -> firmware/default control
        };
        if let Err(e) = set {
            let _ = self.gpu.restore_fans();
            return Report::error(e.to_string());
        }
        let state = if duty.pct.is_some() {
            SinkState::Claimed
        } else {
            SinkState::Released
        };
        self.report(temp, duty.pct, state)
    }

    fn restore(&mut self) {
        match self.gpu.restore_fans() {
            Ok(()) => tracing::info!("GPU fans restored to firmware default"),
            Err(e) => eprintln!("WARNING: fan restore failed: {e}"),
        }
    }
}

impl GpuDevice {
    /// Live report for THIS GPU (one unit + its components + signals, with values).
    fn report(&mut self, temp: i32, commanded_pct: Option<u32>, state: SinkState) -> Report {
        let uid = self.gpu.uuid().to_string();
        let short = format!("gpu{}", self.gpu.index());
        let temp_sig = format!("{uid}:temperature:temp");
        let units = vec![gpu_unit(&uid, &short, self.gpu.name())];

        let tcomp = format!("{uid}:temperature");
        let mut components = vec![Component::new(&tcomp, &uid)
            .name("temperature")
            .typed("temperature")];
        let mut signals = vec![
            Signal::producer(format!("{tcomp}:temp"), &tcomp, "temperature")
                .value(json!(temp))
                .uom("C")
                .name("temperature"),
        ];

        for fan in 0..self.gpu.num_fans() {
            let fcomp = format!("{uid}:fan{fan}");
            components.push(
                Component::new(&fcomp, &uid)
                    .name(format!("fan{fan}"))
                    .typed("fan"),
            );
            if let Some(rpm) = self.gpu.fan_rpm(fan) {
                signals.push(
                    Signal::producer(format!("{fcomp}:rpm"), &fcomp, "fan-rpm")
                        .value(json!(rpm))
                        .uom("rpm")
                        .name("rpm"),
                );
            }
            let fw = self.gpu.fan_speed(fan);
            signals.push(duty_sink(
                &fcomp,
                &short,
                &temp_sig,
                temp,
                commanded_pct,
                fw,
                state,
            ));
        }
        Report::ok(units, components, signals)
    }
}

/// Build a GPU fan's `duty` sink. Driven by THIS GPU's own temperature; when commanded (apply) it is
/// CLAIMED and carries the `driving` record (CI-checked); otherwise (read-only info) it reports the
/// firmware duty with no decision. Pure (no NVML) so the sink contract is unit-testable.
fn duty_sink(
    fcomp: &str,
    short: &str,
    temp_sig: &str,
    temp: i32,
    commanded_pct: Option<u32>,
    firmware_pct: Option<u32>,
    state: SinkState,
) -> Signal {
    let mut control = Control {
        needs_claim: true,
        state,
        safe: Some(json!("auto")),
        direction: Some("up=more-cooling".into()),
        readback: Some(format!("{fcomp}:rpm")),
        driven_by: vec![Provenance::new(short)
            .value(json!(temp))
            .uom("C")
            .signal(temp_sig)],
        driving: None,
    };
    let mut sink = Signal::sink(format!("{fcomp}:duty"), fcomp, "fan-duty")
        .uom("%")
        .range(0.0, 100.0)
        .name("duty");
    if let Some(pct) = commanded_pct {
        sink = sink.value(json!(pct));
        control.driving = Some(
            Driving::new()
                .kind("temperature")
                .raw(temp as f64)
                .input(temp as f64)
                .uom("C")
                .output(pct as f64)
                .how("curve"),
        );
    } else if let Some(fw) = firmware_pct {
        sink = sink.value(json!(fw));
    }
    sink.control(control)
}

fn gpu_unit(uuid: &str, short: &str, product: &str) -> Unit {
    Unit::new(uuid)
        .name(short)
        .description(product)
        .typed("gpu")
        .label("vendor", "NVIDIA")
}

/// Schema-only (no values) twin used by `detect`: (units, components, signals) for one GPU.
fn gpu_schema(
    uuid: &str,
    index: u32,
    product: &str,
    num_fans: u32,
) -> (Vec<Unit>, Vec<Component>, Vec<Signal>) {
    let short = format!("gpu{index}");
    let tcomp = format!("{uuid}:temperature");
    let mut components = vec![Component::new(&tcomp, uuid)
        .name("temperature")
        .typed("temperature")];
    let mut signals = vec![
        Signal::producer(format!("{tcomp}:temp"), &tcomp, "temperature")
            .uom("C")
            .name("temperature"),
    ];
    for fan in 0..num_fans {
        let fcomp = format!("{uuid}:fan{fan}");
        components.push(
            Component::new(&fcomp, uuid)
                .name(format!("fan{fan}"))
                .typed("fan"),
        );
        signals.push(
            Signal::producer(format!("{fcomp}:rpm"), &fcomp, "fan-rpm")
                .uom("rpm")
                .name("rpm"),
        );
        signals.push(
            Signal::sink(format!("{fcomp}:duty"), &fcomp, "fan-duty")
                .uom("%")
                .range(0.0, 100.0)
                .name("duty")
                .control(Control {
                    needs_claim: true,
                    safe: Some(json!("auto")),
                    direction: Some("up=more-cooling".into()),
                    readback: Some(format!("{fcomp}:rpm")),
                    ..Default::default()
                }),
        );
    }
    (vec![gpu_unit(uuid, &short, product)], components, signals)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commanded_gpu_fan_sink_satisfies_the_driving_contract() {
        // A claimed (commanded) GPU fan sink must carry driving (input temp + output duty).
        let s = duty_sink(
            "GPU-x:fan0",
            "gpu0",
            "GPU-x:temperature:temp",
            64,
            Some(37),
            None,
            SinkState::Claimed,
        );
        let r = Report::ok(vec![], vec![], vec![s]);
        assert!(
            r.sink_contract_violations().is_empty(),
            "a commanded GPU fan must report driving"
        );
        let d = r.signals[0]
            .control
            .as_ref()
            .unwrap()
            .driving
            .as_ref()
            .unwrap();
        assert_eq!(d.input, Some(64.0));
        assert_eq!(d.output, Some(37.0));
        assert_eq!(d.kind.as_deref(), Some("temperature"));
    }

    #[test]
    fn read_only_gpu_fan_sink_is_exempt() {
        // Read-only info: unknown state, firmware duty, no decision — exempt from the contract.
        let s = duty_sink(
            "GPU-x:fan0",
            "gpu0",
            "t",
            50,
            None,
            Some(40),
            SinkState::Unknown,
        );
        assert!(Report::ok(vec![], vec![], vec![s])
            .sink_contract_violations()
            .is_empty());
    }
}
