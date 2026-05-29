//! NVML access for the nvidia anemos, via the `nvml-wrapper` crate.
//!
//! Fork-safety: NVML is not fork-safe; the orchestrator never holds it. Each `run`/`detect`
//! process initialises its own `Nvml`. Manual fan control PERSISTS after the process exits (the
//! driver does NOT auto-revert), so a `Gpu` restores firmware/default fan control in its `Drop`
//! AND on every explicit shutdown/EOF path.

use anyhow::{anyhow, Result};
use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use nvml_wrapper::Nvml;
use protocol::{Curve, Damper, Detected, FoundEntry, Reading};
use serde_json::json;
use tracing::{info, warn};

/// Detect-process GPU enumerator. Holds ONE NVML handle for the process lifetime and reuses it.
///
/// CRITICAL: NVML (`libnvidia-ml`) opens `/dev/nvidia*` file descriptors on init that are not all
/// released on shutdown. Re-initialising every detect cycle leaks fds until EMFILE. So we init once
/// and hold it. On a fault it reports an explicit `Detected{status:error,...}` (never exits, never
/// returns a bogus empty) so the supervisor reacts to a *declared* error with context; the handle
/// is dropped and lazily re-initialised on the next detect (self-recovery).
pub struct Detector {
    nvml: Option<Nvml>,
}

impl Detector {
    pub fn new() -> Self {
        let nvml = Nvml::init()
            .map_err(|e| warn!(error=%e, "NVML init failed at startup; will retry on detect"))
            .ok();
        Detector { nvml }
    }

    pub fn detect(&mut self) -> Detected {
        if self.nvml.is_none() {
            match Nvml::init() {
                Ok(n) => self.nvml = Some(n),
                Err(e) => return Detected::error(format!("NVML init failed: {e}")),
            }
        }
        match enumerate(self.nvml.as_ref().expect("just ensured Some")) {
            Ok(found) => Detected::ok(found),
            Err(e) => {
                self.nvml = None; // drop the broken handle; re-init on the next detect
                Detected::error(format!("NVML enumeration failed: {e}"))
            }
        }
    }
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

/// One-shot fail-safe: restore EVERY GPU's fans to firmware default and return. Wired into the
/// systemd unit (ExecStopPost) so manual fan control is never left persisting if aiolos died
/// without a graceful shutdown. Best-effort per fan; idempotent.
pub fn restore_all() -> Result<()> {
    let nvml = Nvml::init().map_err(|e| anyhow!("NVML init: {e}"))?;
    let count = nvml
        .device_count()
        .map_err(|e| anyhow!("device_count: {e}"))?;
    for i in 0..count {
        let Ok(mut dev) = nvml.device_by_index(i) else {
            continue;
        };
        let fans = dev.num_fans().unwrap_or(0);
        for fan in 0..fans {
            if let Err(e) = dev.set_default_fan_speed(fan) {
                warn!(index = i, fan, error=%e, "set_default_fan_speed failed");
            }
        }
    }
    info!(gpus = count, "all GPU fans restored to firmware default");
    Ok(())
}

/// Enumerate GPUs with an existing handle. `Err` = NVML fault (e.g. `device_count` failed) → the
/// caller declares `status:error`; `Ok([])` = NVML healthy but genuinely no GPUs.
fn enumerate(nvml: &Nvml) -> Result<Vec<FoundEntry>> {
    let count = nvml
        .device_count()
        .map_err(|e| anyhow!("device_count: {e}"))?;
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
    Ok(out)
}

// ---- fan-control policy (trait-based so it is unit-testable without a real GPU) ----

/// The minimal per-fan operations the apply policy needs.
pub trait FanControl {
    fn set_fan(&mut self, fan: u32, pct: u32) -> Result<()>;
    fn set_default(&mut self, fan: u32) -> Result<()>;
}

impl FanControl for nvml_wrapper::Device<'_> {
    fn set_fan(&mut self, fan: u32, pct: u32) -> Result<()> {
        self.set_fan_speed(fan, pct)
            .map_err(|e| anyhow!("set_fan_speed(fan {fan}): {e}"))
    }
    fn set_default(&mut self, fan: u32) -> Result<()> {
        self.set_default_fan_speed(fan)
            .map_err(|e| anyhow!("set_default_fan_speed(fan {fan}): {e}"))
    }
}

/// Apply `pct` to every fan (or firmware-default when `None`). If ANY set fails, restore ALL fans
/// to firmware default and return `Err` — never leave the GPU partially-manual / manual-but-frozen.
/// This is the R3 safety guarantee, isolated here so it can be unit-tested with a mock device.
pub fn apply_or_restore<D: FanControl>(dev: &mut D, num_fans: u32, pct: Option<u32>) -> Result<()> {
    for fan in 0..num_fans {
        let r = match pct {
            Some(p) => dev.set_fan(fan, p),
            None => dev.set_default(fan),
        };
        if let Err(e) = r {
            for f in 0..num_fans {
                let _ = dev.set_default(f); // hand every fan back to firmware regulation
            }
            return Err(anyhow!("{e}; restored all fans to firmware default"));
        }
    }
    Ok(())
}

/// One GPU bound by UUID, holding its own NVML handle for the instance's lifetime.
pub struct Gpu {
    nvml: Nvml,
    uuid: String,
    index: u32,
    num_fans: u32,
    damper: Damper,
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
                        damper: Damper::default(),
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
    pub fn read_and_control(&mut self, curve: &Curve, alpha: f64) -> Result<Vec<Reading>> {
        self.damper.set_alpha(alpha); // live sensitivity knob
        let idx = self.resolve_index()?;

        // Read the raw GPU temperature (this scope drops `dev` so we can touch self.damper next).
        let temp = {
            let dev = self.nvml.device_by_index(idx)?;
            dev.temperature(TemperatureSensor::Gpu)? as i32
        };

        // EMA-smooth + deadband the duty so GPU temp jitter doesn't make the fans hunt. An empty
        // curve means "no usable config" → firmware/default control (never command 0%).
        let pct: Option<u32> = if curve.is_empty() {
            self.damper.reset();
            None
        } else {
            let smoothed = self.damper.smooth(temp);
            Some(self.damper.deadband(curve.eval(smoothed).clamp(0, 100)) as u32)
        };
        info!(uuid=%self.uuid, temp, commanded_pct = ?pct, fans = self.num_fans, "decision: set GPU fans");

        let mut dev = self.nvml.device_by_index(idx)?;
        // Apply the duty; if any fan-set fails, ALL fans are reverted to firmware default before the
        // error propagates (never strand the GPU partially-manual / manual-but-frozen).
        apply_or_restore(&mut dev, self.num_fans, pct)?;

        // Report the RAW GPU temp (so the orchestrator routes the true temperature) and the
        // commanded duty; include the actual fan RPM where available.
        let mut readings = vec![Reading::new("temp", "GPU", json!({ "temp": temp }))];
        for fan in 0..self.num_fans {
            let mut f = serde_json::Map::new();
            let pwm = pct.or_else(|| dev.fan_speed(fan).ok());
            if let Some(v) = pwm {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A mock fan device for testing the `apply_or_restore` policy without a real GPU.
    struct MockDev {
        fail_set_fan: Option<u32>, // fan index whose `set_fan` fails
        set_calls: Vec<u32>,
        default_calls: Vec<u32>,
    }
    impl MockDev {
        fn new(fail_set_fan: Option<u32>) -> Self {
            MockDev {
                fail_set_fan,
                set_calls: vec![],
                default_calls: vec![],
            }
        }
    }
    impl FanControl for MockDev {
        fn set_fan(&mut self, fan: u32, _pct: u32) -> Result<()> {
            self.set_calls.push(fan);
            if self.fail_set_fan == Some(fan) {
                return Err(anyhow!("mock set_fan failure on fan {fan}"));
            }
            Ok(())
        }
        fn set_default(&mut self, fan: u32) -> Result<()> {
            self.default_calls.push(fan);
            Ok(())
        }
    }

    #[test]
    fn apply_ok_sets_all_fans_without_restore() {
        let mut d = MockDev::new(None);
        assert!(apply_or_restore(&mut d, 3, Some(80)).is_ok());
        assert_eq!(d.set_calls, vec![0, 1, 2]);
        assert!(
            d.default_calls.is_empty(),
            "no restore on a successful apply"
        );
    }

    #[test]
    fn apply_restores_all_fans_when_a_set_fails() {
        // R3: a mid-loop set failure must NOT leave the GPU partially-manual — ALL fans revert to
        // firmware default and the error propagates so the worker reports it.
        let mut d = MockDev::new(Some(1));
        assert!(apply_or_restore(&mut d, 3, Some(80)).is_err());
        assert_eq!(
            d.default_calls,
            vec![0, 1, 2],
            "every fan must be handed back to firmware default on a set failure"
        );
    }

    #[test]
    fn apply_none_means_firmware_default_for_every_fan() {
        let mut d = MockDev::new(None);
        assert!(apply_or_restore(&mut d, 2, None).is_ok());
        assert_eq!(d.default_calls, vec![0, 1]);
        assert!(
            d.set_calls.is_empty(),
            "None must never command a manual duty"
        );
    }
}
