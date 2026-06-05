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
    Role, Signal, SinkState, Unit,
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
        self.report(temp, None, SinkState::Unknown, temp, temp, &[])
    }

    fn apply(&mut self, inputs: Option<&Inputs>, ctrl: &mut Controller) -> Report {
        let gpu_temp = match self.gpu.temperature() {
            Ok(t) => t,
            Err(e) => {
                // A failed read must not leave the GPU manual-but-unregulated: revert to firmware.
                let _ = self.gpu.restore_fans();
                return Report::error(e.to_string());
            }
        };
        let routed_temps = routed_temperature_inputs(inputs);
        let driving_temp = driving_temperature(gpu_temp, &routed_temps);
        let duty = ctrl.duty(driving_temp);
        tracing::info!(gpu = %format!("gpu{}", self.gpu.index()), uuid = %self.gpu.uuid(),
            gpu_temp, driving_temp, commanded_pct = ?duty.pct, fans = self.gpu.num_fans(), "decision: set GPU fans");
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
        self.report(
            gpu_temp,
            duty.pct,
            state,
            duty.raw,
            duty.smoothed,
            &routed_temps,
        )
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
    fn report(
        &mut self,
        gpu_temp: i32,
        commanded_pct: Option<u32>,
        state: SinkState,
        driving_raw: i32,
        driving_smoothed: i32,
        routed_temps: &[TemperatureInput],
    ) -> Report {
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
                .value(json!(gpu_temp))
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
                FanDrive {
                    gpu_temp,
                    raw: driving_raw,
                    smoothed: driving_smoothed,
                    routed_temps,
                },
                commanded_pct,
                fw,
                state,
            ));
        }
        Report::ok(units, components, signals)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TemperatureInput {
    name: String,
    value: i32,
    signal: String,
}

struct FanDrive<'a> {
    gpu_temp: i32,
    raw: i32,
    smoothed: i32,
    routed_temps: &'a [TemperatureInput],
}

fn routed_temperature_inputs(inputs: Option<&Inputs>) -> Vec<TemperatureInput> {
    let mut out = Vec::new();
    let Some(inputs) = inputs else {
        return out;
    };
    for (source, signals) in inputs {
        for signal in signals {
            if signal.role != Role::Producer || signal.kind() != Some("temperature") {
                continue;
            }
            let Some(value) = signal.value_i64() else {
                continue;
            };
            let Ok(value) = i32::try_from(value) else {
                continue;
            };
            let label = signal
                .labels
                .get("name")
                .map(String::as_str)
                .unwrap_or(signal.id.as_str());
            out.push(TemperatureInput {
                name: format!("{source}:{label}"),
                value,
                signal: signal.id.clone(),
            });
        }
    }
    out
}

fn driving_temperature(gpu_temp: i32, routed_temps: &[TemperatureInput]) -> i32 {
    routed_temps
        .iter()
        .map(|t| t.value)
        .chain(std::iter::once(gpu_temp))
        .max()
        .unwrap_or(gpu_temp)
}

/// Build a GPU fan's `duty` sink. When commanded (apply) it is CLAIMED and carries the
/// `driving` record (CI-checked); otherwise (read-only info) it reports the firmware duty with
/// no decision. Pure (no NVML) so the sink contract is unit-testable.
fn duty_sink(
    fcomp: &str,
    short: &str,
    temp_sig: &str,
    drive: FanDrive<'_>,
    commanded_pct: Option<u32>,
    firmware_pct: Option<u32>,
    state: SinkState,
) -> Signal {
    let mut driven_by = vec![Provenance::new(short)
        .value(json!(drive.gpu_temp))
        .uom("C")
        .signal(temp_sig)];
    driven_by.extend(drive.routed_temps.iter().map(|t| {
        Provenance::new(&t.name)
            .value(json!(t.value))
            .uom("C")
            .signal(&t.signal)
    }));
    let mut control = Control {
        needs_claim: true,
        state,
        safe: Some(json!("auto")),
        direction: Some("up=more-cooling".into()),
        readback: Some(format!("{fcomp}:rpm")),
        driven_by,
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
                .raw(drive.raw as f64)
                .input(drive.smoothed as f64)
                .uom("C")
                .output(pct as f64)
                .how(if drive.routed_temps.is_empty() {
                    "self→curve"
                } else {
                    "max(self,routed)→curve"
                }),
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
            FanDrive {
                gpu_temp: 64,
                raw: 64,
                smoothed: 64,
                routed_temps: &[],
            },
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
        assert_eq!(d.how.as_deref(), Some("self→curve"));
    }

    #[test]
    fn read_only_gpu_fan_sink_is_exempt() {
        // Read-only info: unknown state, firmware duty, no decision — exempt from the contract.
        let s = duty_sink(
            "GPU-x:fan0",
            "gpu0",
            "t",
            FanDrive {
                gpu_temp: 50,
                raw: 50,
                smoothed: 50,
                routed_temps: &[],
            },
            None,
            Some(40),
            SinkState::Unknown,
        );
        assert!(Report::ok(vec![], vec![], vec![s])
            .sink_contract_violations()
            .is_empty());
    }

    fn temp_signal(id: &str, value: i64) -> Signal {
        Signal::producer(id, "component", "temperature")
            .value(json!(value))
            .uom("C")
            .name("temp")
    }

    #[test]
    fn routed_temperature_inputs_extract_only_temperature_producers() {
        use std::collections::HashMap;

        let mut inputs: Inputs = HashMap::new();
        inputs.insert(
            "it87:board".into(),
            vec![
                temp_signal("board:cpu:temp", 74),
                temp_signal("board:impossible:temp", i64::from(i32::MAX) + 1),
                Signal::producer("board:fan:rpm", "board:fan", "fan-rpm").value(json!(1200)),
                Signal::sink("board:fan:duty", "board:fan", "fan-duty").value(json!(80)),
            ],
        );

        let got = routed_temperature_inputs(Some(&inputs));
        assert_eq!(
            got,
            vec![TemperatureInput {
                name: "it87:board:temp".into(),
                value: 74,
                signal: "board:cpu:temp".into(),
            }]
        );
    }

    #[test]
    fn driving_temperature_uses_gpu_temp_when_inputs_absent_or_lower() {
        assert_eq!(driving_temperature(62, &[]), 62);
        assert_eq!(
            driving_temperature(
                62,
                &[TemperatureInput {
                    name: "it87:board:cpu".into(),
                    value: 55,
                    signal: "board:cpu:temp".into(),
                }]
            ),
            62
        );
    }

    #[test]
    fn driving_temperature_uses_higher_routed_temperature() {
        assert_eq!(
            driving_temperature(
                45,
                &[
                    TemperatureInput {
                        name: "it87:board:cpu".into(),
                        value: 81,
                        signal: "board:cpu:temp".into(),
                    },
                    TemperatureInput {
                        name: "hwmon-temps:board:vrm".into(),
                        value: 66,
                        signal: "board:board:vrm".into(),
                    },
                ]
            ),
            81
        );
    }

    #[test]
    fn commanded_gpu_fan_sink_reports_routed_driving_source() {
        let routed = vec![TemperatureInput {
            name: "it87:board:cpu".into(),
            value: 82,
            signal: "board:cpu:temp".into(),
        }];
        let s = duty_sink(
            "GPU-x:fan0",
            "gpu0",
            "GPU-x:temperature:temp",
            FanDrive {
                gpu_temp: 45,
                raw: 82,
                smoothed: 80,
                routed_temps: &routed,
            },
            Some(90),
            None,
            SinkState::Claimed,
        );
        let control = s.control.as_ref().unwrap();
        assert_eq!(control.driven_by.len(), 2);
        assert_eq!(control.driven_by[1].name, "it87:board:cpu");
        let d = control.driving.as_ref().unwrap();
        assert_eq!(d.raw, Some(82.0));
        assert_eq!(d.input, Some(80.0));
        assert_eq!(d.output, Some(90.0));
        assert_eq!(d.how.as_deref(), Some("max(self,routed)→curve"));
    }
}
