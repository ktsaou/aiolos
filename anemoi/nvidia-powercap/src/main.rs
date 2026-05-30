//! nvidia-powercap anemos — react to utility-power loss by capping GPU power via NVML.
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle (CLI/signals/logging/protocol/
//! restore wiring); the `nvml` tech crate owns NVML access (incl. power-limit get/set/restore).
//!
//! This is a **curve-less CONTROL** anemos: `ModuleInfo` curve = `None` (no temperature curve — its
//! decision is driven by routed power state, not a curve), but it DOES control a device. It reacts
//! to `power-state` readings routed from the `nut` sensor (`input=nut`): when the policy's trigger
//! fires (an on-battery UPS whose runtime/charge is critically low — see `policy.rs`) it CAPS each
//! GPU's power-management limit; on AC restore (or no trigger) it LIFTS the cap back to the firmware
//! default. The policy is conservative by default (monitor + log; cap only on low runtime) so a
//! brief blip never throttles a running job.
//!
//! Fail-safe (exactly like `nvidia` restores fans): the ORIGINAL/default power limit recorded at
//! `open` is restored on `shutdown`, stdin EOF, SIGTERM/SIGINT, the `restore` one-shot, AND on Drop
//! (panic backstop). A failed control tick also restores the limit. NVML power limits PERSIST after
//! the process exits, so restore is mandatory; `aiolos restore` (systemd ExecStopPost) is the net
//! after a SIGKILL.
//!
//! detect → one entry per GPU (id = UUID, like `nvidia`).
//! run <UUID> → each tick, decide cap/lift from routed power-state and apply it.

mod inputs;
mod policy;

use anemos::{
    Anemos, Applied, Controller, Detected, Device, FoundEntry, Inputs, ModuleInfo, Reading,
};
use inputs::power_signal;
use nvml::{Detector, Gpu};
use policy::{decide, CapReason, Decision, Policy};
use serde_json::json;

fn main() -> ! {
    anemos::run(
        ModuleInfo {
            name: "nvidia-powercap",
            // Curve-less control: no temperature curve. The reaction is driven by routed power
            // state, so `apply` ignores the SDK controller (None means the SDK skips the curve
            // checks; this module still controls the device).
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
    fn detect(&mut self) -> Detected {
        match self.detector.enumerate() {
            Ok(gpus) => Detected::ok(
                gpus.into_iter()
                    .map(|g| FoundEntry {
                        id: g.uuid,
                        kind: "GPU".to_string(),
                        name: g.name,
                        extra: Default::default(),
                    })
                    .collect(),
            ),
            Err(e) => Detected::error(format!("NVML enumeration failed: {e}")),
        }
    }

    fn open(&mut self, id: &str) -> anyhow::Result<Box<dyn Device>> {
        // Opt out of the Gpu's fan-restore-on-drop: this module never touches fans, and our own
        // Drop/restore handles the power limit instead.
        let mut gpu = Gpu::open(id)?.without_fan_restore_on_drop();
        // Record the ORIGINAL power envelope NOW (at open) so restore always targets the true
        // firmware default even if a later read fails. A GPU that cannot report a power limit is
        // not power-cappable -> fatal (the SDK retries open on a long backoff); we never half-manage.
        let limits = gpu
            .power_limits()
            .map_err(|e| anyhow::anyhow!("GPU power limits unreadable (cannot power-cap): {e}"))?;
        tracing::info!(
            uuid = %gpu.uuid(),
            default_mw = limits.default_mw,
            min_mw = limits.min_mw,
            max_mw = limits.max_mw,
            "opened GPU for power-cap; recorded firmware default limit"
        );
        Ok(Box::new(GpuCap {
            gpu,
            default_mw: limits.default_mw,
            min_mw: limits.min_mw,
            policy: Policy::load(),
            capped: false,
            applied_cap_mw: None,
            // Nothing owed at open: we never take control until a trigger actually caps. `apply_cap`
            // arms this; `restore` is then a clean no-op until we have capped at least once.
            restore_armed: false,
        }))
    }

    fn restore_all(&mut self) {
        if let Err(e) = nvml::restore_all_power() {
            eprintln!("restore FAILED: {e}");
            std::process::exit(2);
        }
    }
}

/// One GPU under power-cap management. Holds the recorded firmware default (the restore target) and
/// whether a cap is currently applied. `restore_armed` stays set until a restore succeeds (so a
/// failed restore is retried by Drop), mirroring the asrock board's release-arming.
struct GpuCap {
    gpu: Gpu,
    /// Firmware default power limit (mW), recorded at open — the value `restore` targets.
    default_mw: u32,
    /// Device-accepted minimum limit (mW), recorded at open (for the readings/logging only; the
    /// tech crate re-clamps on every set).
    min_mw: u32,
    policy: Policy,
    /// Whether a cap is currently applied (so a tick only issues NVML when the state changes).
    capped: bool,
    /// The cap currently applied, if any — so a tick re-issues NVML only when the requested target
    /// changes (a live `cap_pct` edit), not every tick.
    applied_cap_mw: Option<AppliedCap>,
    /// Whether the power limit still needs restoring (set when we cap, cleared once restored).
    restore_armed: bool,
}

/// A cap currently in effect: the value we requested (the dedupe key) and the value the device
/// actually applied after its `[min,max]` clamp (what we report).
#[derive(Debug, Clone, Copy)]
struct AppliedCap {
    requested_mw: u32,
    actual_mw: u32,
}

impl Device for GpuCap {
    fn apply(&mut self, inputs: Option<&Inputs>, _ctrl: &mut Controller) -> Applied {
        // Reload the policy each tick (live tuning, like the curve modules reload their curve).
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
                // A control failure must not leave the GPU stuck capped: restore to firmware default
                // and report the fault (the SDK also resets after a non-Ok tick).
                self.restore();
                return Applied::error(e.to_string());
            }
        };

        Applied::ok(self.readings(&sig, &decision, commanded_mw))
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
    /// Apply (or maintain) the cap. Issues NVML only when the effective cap CHANGES — on the
    /// transition into the capped state, or when a live `cap_pct` edit moves the target — so an
    /// unchanged limit is never re-commanded every tick. Returns the limit in effect (mW).
    fn apply_cap(&mut self, target_mw: u32, reason: CapReason) -> anyhow::Result<u32> {
        // Dedupe on the REQUESTED target so an unchanged request is never re-issued. Report the
        // previously-applied (clamped) limit without another NVML write.
        if let Some(applied) = self.applied_cap_mw {
            if applied.requested_mw == target_mw {
                return Ok(applied.actual_mw);
            }
        }
        let actual = self.gpu.set_power_limit(target_mw)?;
        let transition = !self.capped;
        self.capped = true;
        self.applied_cap_mw = Some(AppliedCap {
            requested_mw: target_mw,
            actual_mw: actual,
        });
        self.restore_armed = true; // a cap is in effect -> restore is owed
        if transition {
            tracing::warn!(
                uuid = %self.gpu.uuid(), reason = reason.as_str(), target_mw,
                actual_mw = actual, default_mw = self.default_mw,
                "CAPPING GPU power (utility power event)"
            );
        } else {
            tracing::info!(uuid = %self.gpu.uuid(), reason = reason.as_str(),
                actual_mw = actual, "adjusted GPU power cap (policy change)");
        }
        Ok(actual)
    }

    /// Lift any cap, restoring the firmware default. Issues NVML only on the transition out of the
    /// capped state. Returns the limit in effect (mW).
    fn apply_lift(&mut self) -> anyhow::Result<u32> {
        if !self.capped {
            return Ok(self.default_mw); // never capped (or already lifted) -> nothing to do
        }
        self.gpu.restore_power()?;
        self.capped = false;
        self.applied_cap_mw = None;
        self.restore_armed = false; // back at firmware default -> nothing owed
        tracing::info!(uuid = %self.gpu.uuid(), default_mw = self.default_mw,
            "lifted GPU power cap (utility power restored / trigger cleared)");
        Ok(self.default_mw)
    }

    /// Build this tick's readings: one `powercap` record carrying the control state (capped?, the
    /// effective/default/min limits, current draw, the decision reason) plus an echo of the
    /// aggregate power signal (on_battery / runtime) for the status page.
    fn readings(
        &mut self,
        sig: &policy::PowerSignal,
        decision: &Decision,
        limit_mw: u32,
    ) -> Vec<Reading> {
        let mut f = serde_json::Map::new();
        f.insert("capped".to_string(), json!(self.capped));
        f.insert("limit_mw".to_string(), json!(limit_mw));
        f.insert("default_mw".to_string(), json!(self.default_mw));
        f.insert("min_mw".to_string(), json!(self.min_mw));
        if let Some(draw) = self.gpu.power_usage() {
            f.insert("draw_mw".to_string(), json!(draw));
        }
        f.insert(
            "reason".to_string(),
            json!(match decision {
                Decision::Cap(r) => r.as_str(),
                Decision::Lift => "none",
            }),
        );
        f.insert("on_battery".to_string(), json!(sig.on_battery));
        if let Some(rt) = sig.min_runtime_s {
            f.insert("runtime_s".to_string(), json!(rt));
        }
        vec![Reading::new(
            "powercap",
            "GPU",
            serde_json::Value::Object(f),
        )]
    }
}

impl Drop for GpuCap {
    fn drop(&mut self) {
        // Final fail-safe: if a cap is still owed (a restore never succeeded), restore on drop — the
        // backstop for panic unwinding or any path that skipped `restore`. NVML power limits persist
        // after exit, so this matters.
        if self.restore_armed {
            if let Err(e) = self.gpu.restore_power() {
                eprintln!(
                    "WARNING: power-limit restore on drop FAILED — GPU may stay capped (`aiolos restore` is the net): {e}"
                );
            }
        }
    }
}
