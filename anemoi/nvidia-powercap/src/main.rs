//! nvidia-powercap anemos — react to utility-power loss by capping GPU power via NVML (label-driven
//! signal model, SOW-0018).
//!
//! A **curve-less CONTROL** anemos: it reacts to `power` signals routed from `nut` (`input=nut`) and
//! caps/lifts each GPU's power-management limit. Reports each GPU as a unit (`id` = UUID, same as
//! `nvidia`, so they merge) with a `power` component carrying the cap state + a `power-limit` sink.
//!
//! The cap/lift/restore control path is UNCHANGED from v1 — this is a reporting refactor.

mod inputs;
mod policy;

use anemos::{
    Anemos, Component, Control, Controller, Device, Inputs, ModuleInfo, OpenMode, Provenance,
    Report, Signal, SinkState, Unit,
};
use inputs::power_signal;
use nvml::{Detector, Gpu};
use policy::{decide, CapReason, Decision, Policy};
use serde_json::json;
use std::io::Write;

fn main() -> ! {
    anemos::run(
        ModuleInfo {
            name: "nvidia-powercap",
            curve_default_path: None,
            curve_env_filename: None,
        },
        NvidiaPowercap {
            detector: Detector::new(),
        },
    )
}

struct NvidiaPowercap {
    detector: Detector,
}

impl Anemos for NvidiaPowercap {
    fn detect(&mut self) -> Report {
        match self.detector.enumerate() {
            Ok(gpus) => {
                let (mut units, mut components, mut signals) = (Vec::new(), Vec::new(), Vec::new());
                for g in gpus {
                    let comp = format!("{}:power", g.uuid);
                    units.push(gpu_unit(&g.uuid, g.index, &g.name));
                    components.push(Component::new(&comp, &g.uuid).name("power").typed("power"));
                    signals.push(
                        Signal::sink(format!("{comp}:power_limit"), &comp, "power-limit")
                            .uom("mW")
                            .name("power limit")
                            .control(Control {
                                needs_claim: false,
                                safe: Some(json!("default")),
                                ..Default::default()
                            }),
                    );
                }
                Report::ok(units, components, signals)
            }
            Err(e) => Report::error(format!("NVML enumeration failed: {e}")),
        }
    }

    fn open(&mut self, id: &str, mode: OpenMode) -> anyhow::Result<Box<dyn Device>> {
        let mut gpu = Gpu::open(id)?.without_fan_restore_on_drop();
        let limits = gpu
            .power_limits()
            .map_err(|e| anyhow::anyhow!("GPU power limits unreadable (cannot power-cap): {e}"))?;
        let already_capped = limits.current_mw < limits.default_mw;
        let control = mode == OpenMode::Control;
        tracing::info!(
            uuid = %gpu.uuid(), default_mw = limits.default_mw, current_mw = limits.current_mw,
            min_mw = limits.min_mw, max_mw = limits.max_mw, already_capped, control,
            "opened GPU for power-cap; recorded firmware default limit"
        );
        Ok(Box::new(GpuCap {
            gpu,
            default_mw: limits.default_mw,
            min_mw: limits.min_mw,
            policy: Policy::load(),
            capped: already_capped,
            applied_cap_mw: (control && already_capped).then_some(AppliedCap {
                requested_mw: limits.current_mw,
                actual_mw: limits.current_mw,
            }),
            restore_armed: control && already_capped,
        }))
    }

    fn restore_all(&mut self) {
        if let Err(e) = nvml::restore_all_power() {
            eprintln!("restore FAILED: {e}");
            std::process::exit(2);
        }
    }
}

struct GpuCap {
    gpu: Gpu,
    default_mw: u32,
    min_mw: u32,
    policy: Policy,
    capped: bool,
    applied_cap_mw: Option<AppliedCap>,
    restore_armed: bool,
}

#[derive(Debug, Clone, Copy)]
struct AppliedCap {
    requested_mw: u32,
    actual_mw: u32,
}

impl Device for GpuCap {
    fn collect(&mut self, _inputs: Option<&Inputs>) -> Report {
        let limits = match self.gpu.power_limits() {
            Ok(limits) => limits,
            Err(e) => return Report::error(e.to_string()),
        };
        self.default_mw = limits.default_mw;
        self.min_mw = limits.min_mw;
        self.capped = limits.current_mw < limits.default_mw;
        self.report_observed(limits.current_mw)
    }

    fn apply(&mut self, inputs: Option<&Inputs>, _ctrl: &mut Controller) -> Report {
        self.policy = Policy::load();
        let sig = power_signal(inputs);
        let decision = decide(&self.policy, &sig);

        let result = match decision {
            Decision::Cap(reason) => {
                let target = self.policy.cap_target_mw(self.default_mw);
                self.apply_cap(target, reason)
            }
            Decision::Lift => self.apply_lift(),
        };
        let commanded_mw = match result {
            Ok(mw) => mw,
            Err(e) => {
                self.restore();
                return Report::error(e.to_string());
            }
        };
        self.report_control(&sig, &decision, commanded_mw)
    }

    fn restore(&mut self) {
        if !self.restore_armed {
            return;
        }
        match self.gpu.restore_power() {
            Ok(()) => {
                tracing::info!(uuid = %self.gpu.uuid(), default_mw = self.default_mw,
                    "GPU power limit restored to firmware default");
                self.capped = false;
                self.applied_cap_mw = None;
                self.restore_armed = false;
            }
            Err(e) => eprintln!("WARNING: power-limit restore failed (will retry on drop): {e}"),
        }
    }
}

impl GpuCap {
    fn apply_cap(&mut self, target_mw: u32, reason: CapReason) -> anyhow::Result<u32> {
        if let Some(applied) = self.applied_cap_mw {
            if applied.requested_mw == target_mw {
                return Ok(applied.actual_mw);
            }
        }
        self.restore_armed = true;
        let actual = self.gpu.set_power_limit(target_mw)?;
        let transition = !self.capped;
        self.capped = true;
        self.applied_cap_mw = Some(AppliedCap {
            requested_mw: target_mw,
            actual_mw: actual,
        });
        if transition {
            tracing::warn!(uuid = %self.gpu.uuid(), reason = reason.as_str(), target_mw,
                actual_mw = actual, default_mw = self.default_mw,
                "CAPPING GPU power (utility power event)");
        } else {
            tracing::info!(uuid = %self.gpu.uuid(), reason = reason.as_str(),
                actual_mw = actual, "adjusted GPU power cap (policy change)");
        }
        Ok(actual)
    }

    fn apply_lift(&mut self) -> anyhow::Result<u32> {
        if !self.capped {
            return Ok(self.default_mw);
        }
        self.gpu.restore_power()?;
        self.capped = false;
        self.applied_cap_mw = None;
        self.restore_armed = false;
        tracing::info!(uuid = %self.gpu.uuid(), default_mw = self.default_mw,
            "lifted GPU power cap (utility power restored / trigger cleared)");
        Ok(self.default_mw)
    }

    /// This tick's report: the GPU unit, a `power` component carrying the cap state, and the
    /// `power-limit` sink. Routed UPS state is not re-published; it appears as sink `driven_by`.
    fn report_control(
        &mut self,
        sig: &policy::PowerSignal,
        decision: &Decision,
        limit_mw: u32,
    ) -> Report {
        let mut driven_by = vec![Provenance::new("nut:on_battery").value(json!(sig.on_battery))];
        if let Some(rt) = sig.min_runtime_s {
            driven_by.push(Provenance::new("nut:runtime").value(json!(rt)).uom("s"));
        }
        self.report_with(
            limit_mw,
            match decision {
                Decision::Cap(r) => r.as_str(),
                Decision::Lift => "none",
            },
            if self.capped {
                SinkState::Claimed
            } else {
                SinkState::Released
            },
            driven_by,
        )
    }

    fn report_observed(&mut self, limit_mw: u32) -> Report {
        let state = if self.restore_armed {
            if self.capped {
                SinkState::Claimed
            } else {
                SinkState::Released
            }
        } else if self.capped {
            SinkState::Unknown
        } else {
            SinkState::Released
        };
        self.report_with(limit_mw, "observed", state, Vec::new())
    }

    fn report_with(
        &mut self,
        limit_mw: u32,
        reason: &str,
        state: SinkState,
        driven_by: Vec<Provenance>,
    ) -> Report {
        let uid = self.gpu.uuid().to_string();
        let comp = format!("{uid}:power");
        let units = vec![gpu_unit(&uid, self.gpu.index(), self.gpu.name())];
        let components = vec![Component::new(&comp, &uid).name("power").typed("power")];
        let mut signals = vec![
            Signal::producer(format!("{comp}:capped"), &comp, "powercap-capped")
                .value(json!(self.capped))
                .name("capped"),
            Signal::producer(format!("{comp}:limit"), &comp, "power-limit")
                .value(json!(limit_mw))
                .uom("mW")
                .name("power limit"),
            Signal::producer(
                format!("{comp}:default_limit"),
                &comp,
                "power-limit-default",
            )
            .value(json!(self.default_mw))
            .uom("mW")
            .name("default limit"),
            Signal::producer(format!("{comp}:min_limit"), &comp, "power-limit-min")
                .value(json!(self.min_mw))
                .uom("mW")
                .name("min limit"),
            Signal::producer(format!("{comp}:reason"), &comp, "powercap-reason")
                .value(json!(reason))
                .name("reason"),
        ];
        if let Some(draw) = self.gpu.power_usage() {
            signals.push(
                Signal::producer(format!("{comp}:draw"), &comp, "power-draw")
                    .value(json!(draw))
                    .uom("mW")
                    .name("draw"),
            );
        }
        signals.push(
            Signal::sink(format!("{comp}:power_limit"), &comp, "power-limit")
                .value(json!(limit_mw))
                .uom("mW")
                .name("power limit")
                .control(Control {
                    needs_claim: false,
                    state,
                    safe: Some(json!("default")),
                    driven_by,
                    ..Default::default()
                }),
        );
        Report::ok(units, components, signals)
    }
}

fn gpu_unit(uuid: &str, index: u32, product: &str) -> Unit {
    Unit::new(uuid)
        .name(format!("gpu{index}"))
        .description(product)
        .typed("gpu")
        .label("vendor", "NVIDIA")
}

impl Drop for GpuCap {
    fn drop(&mut self) {
        if self.restore_armed && self.gpu.restore_power().is_err() {
            let _ = std::io::stderr().write_all(
                b"WARNING: nvidia-powercap restore-on-drop FAILED - GPU may stay capped; `aiolos restore` is the net\n",
            );
        }
    }
}
