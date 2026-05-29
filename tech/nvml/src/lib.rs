//! Level-1 tech: NVML GPU access via the `nvml-wrapper` crate.
//!
//! Pure device technology — enumerate GPUs, read temperature, set/restore per-fan duty. No curve,
//! EMA, protocol, or readings concepts (those belong to the SDK + the anemos). Manual fan control
//! PERSISTS after a process exits (the driver does NOT auto-revert), so a `Gpu` restores firmware
//! fan control in its `Drop`.

use anyhow::{anyhow, Result};
use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use nvml_wrapper::Nvml;
use tracing::{info, warn};

/// A discovered GPU (raw info; the anemos maps this into the protocol's `FoundEntry`).
pub struct GpuInfo {
    pub uuid: String,
    pub name: String,
    pub num_fans: u32,
}

/// Detect-process enumerator: holds ONE NVML handle for the process lifetime and reuses it.
///
/// CRITICAL: NVML opens `/dev/nvidia*` fds on init that are not all released on shutdown;
/// re-initialising every cycle leaks fds until EMFILE. So it inits once (lazily) and holds it; on a
/// fault the handle is dropped and re-initialised on the next call (self-recovery).
#[derive(Default)]
pub struct Detector {
    nvml: Option<Nvml>,
}

impl Detector {
    pub fn new() -> Self {
        Detector { nvml: None }
    }

    /// Enumerate GPUs. `Err` = NVML fault (the anemos declares `error`); `Ok([])` = NVML healthy but
    /// genuinely no GPUs.
    pub fn enumerate(&mut self) -> Result<Vec<GpuInfo>> {
        if self.nvml.is_none() {
            self.nvml = Some(Nvml::init().map_err(|e| anyhow!("NVML init: {e}"))?);
        }
        match list(self.nvml.as_ref().expect("just ensured Some")) {
            Ok(v) => Ok(v),
            Err(e) => {
                self.nvml = None; // drop the broken handle; re-init on the next enumerate
                Err(e)
            }
        }
    }
}

fn list(nvml: &Nvml) -> Result<Vec<GpuInfo>> {
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
        let num_fans = dev.num_fans().unwrap_or(0);
        out.push(GpuInfo {
            uuid,
            name,
            num_fans,
        });
    }
    Ok(out)
}

/// One-shot: restore EVERY GPU's fans to firmware default. Best-effort per fan; idempotent.
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

// ---- per-fan apply policy (trait-based so it is unit-testable without a real GPU) ----

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

/// Apply `pct` to every fan (or firmware-default when `None`). If ANY set fails, restore ALL fans to
/// firmware default and return `Err` — never leave the GPU partially-manual / manual-but-frozen.
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

/// One GPU bound by UUID (stable across index renumbering), holding its own NVML handle.
pub struct Gpu {
    nvml: Nvml,
    uuid: String,
    index: u32,
    num_fans: u32,
}

impl Gpu {
    /// Initialise NVML and resolve the device by UUID.
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

    pub fn uuid(&self) -> &str {
        &self.uuid
    }

    pub fn num_fans(&self) -> u32 {
        self.num_fans
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
        for i in 0..count {
            let matches = self
                .nvml
                .device_by_index(i)
                .ok()
                .and_then(|d| d.uuid().ok())
                .map(|u| u == self.uuid)
                .unwrap_or(false);
            if matches {
                self.index = i;
                return Ok(i);
            }
        }
        Err(anyhow!("GPU {} not present", self.uuid))
    }

    /// Read the GPU temperature (°C).
    pub fn temperature(&mut self) -> Result<i32> {
        let idx = self.resolve_index()?;
        let dev = self.nvml.device_by_index(idx)?;
        Ok(dev.temperature(TemperatureSensor::Gpu)? as i32)
    }

    /// Set ALL fans to `pct`. On any per-fan failure, revert ALL fans to firmware default + `Err`.
    pub fn set_all_fans(&mut self, pct: u32) -> Result<()> {
        let idx = self.resolve_index()?;
        let mut dev = self.nvml.device_by_index(idx)?;
        apply_or_restore(&mut dev, self.num_fans, Some(pct))
    }

    /// Hand ALL fans back to firmware/default control (best-effort across the set).
    pub fn set_all_default(&mut self) -> Result<()> {
        let idx = self.resolve_index()?;
        let mut dev = self.nvml.device_by_index(idx)?;
        apply_or_restore(&mut dev, self.num_fans, None)
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

    /// Current fan duty % for `fan`, if readable (for readings).
    pub fn fan_speed(&mut self, fan: u32) -> Option<u32> {
        let idx = self.resolve_index().ok()?;
        let dev = self.nvml.device_by_index(idx).ok()?;
        dev.fan_speed(fan).ok()
    }

    /// Current fan RPM for `fan`, if readable (for readings).
    pub fn fan_rpm(&mut self, fan: u32) -> Option<u32> {
        let idx = self.resolve_index().ok()?;
        let dev = self.nvml.device_by_index(idx).ok()?;
        dev.fan_speed_rpm(fan).ok()
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        // Safety net: restore firmware fan control on ANY drop (normal exit or panic unwind), since
        // NVML manual control would otherwise persist after we're gone.
        match self.restore_fans() {
            Ok(()) => info!(uuid=%self.uuid, "fans restored to firmware default on drop"),
            Err(e) => warn!(uuid=%self.uuid, error=%e,
                "fan restore on drop FAILED — GPU may be left in manual (`aiolos restore` is the net)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mock fan device for testing the `apply_or_restore` policy without a real GPU.
    struct MockDev {
        fail_set_fan: Option<u32>,
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
