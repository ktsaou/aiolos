//! NVML access for the nvidia anemos, via the `nvml-wrapper` crate.
//!
//! Fork-safety: NVML is not fork-safe; the orchestrator never holds it. Each `run`/`detect`
//! process initialises its own `Nvml`. Manual fan control PERSISTS after the process exits (the
//! driver does NOT auto-revert), so a `Gpu` restores firmware/default fan control in its `Drop`
//! AND on every explicit shutdown/EOF path.

use anyhow::{anyhow, Result};
use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use nvml_wrapper::Nvml;
use protocol::{Curve, FoundEntry, Reading};
use serde_json::json;
use tracing::{info, warn};

/// Enumerate all GPUs (one `detect` reply). Inits its own NVML and drops it on return.
pub fn enumerate() -> Vec<FoundEntry> {
    let nvml = match Nvml::init() {
        Ok(n) => n,
        Err(e) => {
            warn!(error=%e, "NVML init failed during detect");
            return Vec::new();
        }
    };
    let count = nvml.device_count().unwrap_or(0);
    let mut out = Vec::new();
    for i in 0..count {
        let Ok(dev) = nvml.device_by_index(i) else {
            continue;
        };
        let uuid = match dev.uuid() {
            Ok(u) => u,
            Err(e) => {
                warn!(index = i, error=%e, "GPU UUID read failed; skipping");
                continue;
            }
        };
        let name = dev.name().unwrap_or_else(|_| "NVIDIA GPU".to_string());
        let fans = dev.num_fans().unwrap_or(0);
        let mut extra = serde_json::Map::new();
        extra.insert("fans".to_string(), json!(fans));
        out.push(FoundEntry {
            id: uuid,
            kind: "GPU".to_string(),
            name,
            extra,
        });
    }
    out
}

/// One GPU bound by UUID, holding its own NVML handle for the instance's lifetime.
pub struct Gpu {
    nvml: Nvml,
    uuid: String,
    index: u32,
    num_fans: u32,
}

impl Gpu {
    /// Initialise NVML and resolve the device by UUID (stable across index renumbering).
    pub fn open(uuid: &str) -> Result<Self> {
        let nvml = Nvml::init().map_err(|e| anyhow!("NVML init: {e}"))?;
        let count = nvml
            .device_count()
            .map_err(|e| anyhow!("device_count: {e}"))?;
        for i in 0..count {
            if let Ok(dev) = nvml.device_by_index(i) {
                if dev.uuid().map(|u| u == uuid).unwrap_or(false) {
                    let num_fans = dev.num_fans().unwrap_or(0);
                    return Ok(Gpu {
                        nvml,
                        uuid: uuid.to_string(),
                        index: i,
                        num_fans,
                    });
                }
            }
        }
        Err(anyhow!("GPU {uuid} not found among {count} device(s)"))
    }

    /// Re-resolve the device index by UUID (handles renumbering); updates the cached index.
    fn resolve_index(&mut self) -> Result<u32> {
        let cached_ok = self
            .nvml
            .device_by_index(self.index)
            .ok()
            .and_then(|d| d.uuid().ok())
            .map(|u| u == self.uuid)
            .unwrap_or(false);
        if cached_ok {
            return Ok(self.index);
        }
        let count = self.nvml.device_count()?;
        let mut found = None;
        for i in 0..count {
            let matches = self
                .nvml
                .device_by_index(i)
                .ok()
                .and_then(|d| d.uuid().ok())
                .map(|u| u == self.uuid)
                .unwrap_or(false);
            if matches {
                found = Some(i);
                break;
            }
        }
        match found {
            Some(i) => {
                self.index = i;
                Ok(i)
            }
            None => Err(anyhow!("GPU {} not present", self.uuid)),
        }
    }

    /// Read temperature, apply the curve to the onboard fans, and report readings. An empty curve
    /// means "no usable config" → fall back to firmware/default fan control (never command 0%).
    pub fn read_and_control(&mut self, curve: &Curve) -> Result<Vec<Reading>> {
        let idx = self.resolve_index()?;
        let mut dev = self.nvml.device_by_index(idx)?;
        let temp = dev.temperature(TemperatureSensor::Gpu)? as i32;

        let pct = if curve.is_empty() {
            None
        } else {
            Some(curve.eval(temp).clamp(0, 100) as u32)
        };
        info!(uuid=%self.uuid, temp, commanded_pct = ?pct, fans = self.num_fans, "decision: set GPU fans");

        for fan in 0..self.num_fans {
            match pct {
                Some(p) => dev
                    .set_fan_speed(fan, p)
                    .map_err(|e| anyhow!("set_fan_speed(fan {fan}): {e}"))?,
                None => {
                    let _ = dev.set_default_fan_speed(fan);
                }
            }
        }

        let mut readings = vec![Reading::new("temp", "GPU", json!({ "temp": temp }))];
        for fan in 0..self.num_fans {
            let mut f = serde_json::Map::new();
            let got = dev.fan_speed(fan).ok().or(pct);
            if let Some(v) = got {
                f.insert("pwm".to_string(), json!(v));
            }
            if let Ok(rpm) = dev.fan_speed_rpm(fan) {
                f.insert("rpm".to_string(), json!(rpm));
            }
            readings.push(Reading::new("fan", format!("fan{fan}"), json!(f)));
        }
        Ok(readings)
    }

    /// Restore every fan to firmware/default control (the fail-safe). Best-effort.
    pub fn restore_fans(&mut self) -> Result<()> {
        let idx = self.resolve_index()?;
        let mut dev = self.nvml.device_by_index(idx)?;
        for fan in 0..self.num_fans {
            if let Err(e) = dev.set_default_fan_speed(fan) {
                warn!(uuid=%self.uuid, fan, error=%e, "set_default_fan_speed failed");
            }
        }
        Ok(())
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        // Safety net: restore firmware fan control on ANY drop (normal exit or panic unwind),
        // since NVML manual control would otherwise persist after we're gone.
        if self.restore_fans().is_ok() {
            info!(uuid=%self.uuid, "fans restored to firmware default on drop");
        }
    }
}
