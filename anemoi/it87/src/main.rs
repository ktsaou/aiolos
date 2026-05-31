//! it87 anemos — consumer-board fan control via the Linux `it87` hwmon driver (sysfs PWM).
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle (CLI/signals/logging/curve+EMA/
//! protocol/restore); `hwmon` is the sysfs PWM + temperature tech. detect → one board.
//!
//! apply drives the configured PWM channels in one of two modes, decided LIVE each tick from config:
//! - **zone** (default for this host): FAN channels in the CPU zone (the Noctua CPU coolers) follow
//!   CPU temp (`coretemp`) via `it87.cpu.curve.json`; the case zone (intake + exhaust) follows
//!   `max(GPU routed from nvidia, CPU)` via `it87.case.curve.json`. Two internal `anemos::Controller`s
//!   (own EMA/deadband/sensitivity). Active only when BOTH zone curve files load a non-empty curve.
//! - **uniform** (fallback): one `it87.curve.json` over `max(GPU, CPU)` for every managed channel.
//!
//! Fail-safe: a managed channel is restored to firmware/automatic control (`pwmN_enable=2`) on
//! `shutdown`, stdin EOF, SIGTERM/SIGINT, the `restore` one-shot, AND on Drop (panic backstop). The
//! controlled (manual) state is more aggressive than firmware auto, so "module dies → firmware
//! reclaims" is the safe direction. A SIGKILL freezes the last manual duty (sysfs PWM persists, no
//! hardware watchdog) — safe because the SDK's 35% floor keeps any frozen value ≥ floor; systemd
//! `ExecStopPost: aiolos restore` is the net.

mod config;

use anemos::{
    Anemos, Applied, Controller, CurveCache, Detected, Device, FoundEntry, Inputs, ModuleInfo,
    Reading,
};
use config::It87Config;
use serde_json::json;
use std::path::PathBuf;

fn main() -> ! {
    anemos::run(
        ModuleInfo {
            name: "it87",
            curve_default_path: Some("/opt/aiolos/etc/it87.curve.json"),
            curve_env_filename: Some("it87.curve.json"),
        },
        It87,
    )
}

struct It87;

impl Anemos for It87 {
    fn detect(&mut self) -> Detected {
        let cfg = config::load();
        match hwmon::chip_path(&cfg.chip) {
            Some(_) => Detected::ok(vec![FoundEntry {
                id: "it87".to_string(),
                kind: "board".to_string(),
                name: format!("{} fans", cfg.chip),
                extra: Default::default(),
            }]),
            // The chip isn't present (driver not loaded / wrong name): a real "nothing to manage"
            // result, reported as an empty `found` (NOT an error — error means "couldn't do detect").
            None => Detected::ok(vec![]),
        }
    }

    fn open(&mut self, _id: &str) -> anyhow::Result<Box<dyn Device>> {
        let cfg = config::load();
        let dir = hwmon::chip_path(&cfg.chip)
            .ok_or_else(|| anyhow::anyhow!("hwmon chip '{}' not present", cfg.chip))?;
        Ok(Box::new(It87Device {
            dir,
            cfg,
            zones: None,
            // Start armed: the first apply switches channels to manual, so a restore is owed from
            // the outset (idempotent if no apply ever ran — restoring auto channels is a no-op).
            restore_armed: true,
        }))
    }

    fn restore_all(&mut self) {
        let cfg = config::load();
        let Some(dir) = hwmon::chip_path(&cfg.chip) else {
            // Nothing to restore if the chip is absent.
            return;
        };
        let mut failed = false;
        for ch in cfg.managed_channels() {
            if let Err(e) = hwmon::set_pwm_auto(&dir, ch) {
                eprintln!("restore FAILED (pwm{ch}): {e}");
                failed = true;
            }
        }
        if failed {
            std::process::exit(2);
        }
        tracing::info!("all managed channels restored to firmware/automatic");
    }
}

/// Per-zone controllers built lazily from the SDK controller's resolved curve path, so the zone
/// curve files sit next to the main one and honour `$AIOLOS_ETC_DIR` (mirrors asrock's zones).
struct Zones {
    cpu: Controller,
    case: Controller,
    cpu_path: String,
    case_path: String,
}

impl Zones {
    fn for_main_path(main_curve_path: &str) -> Self {
        let cpu_path = zone_path(main_curve_path, "cpu");
        let case_path = zone_path(main_curve_path, "case");
        Zones {
            cpu: Controller::new(cpu_path.clone()),
            case: Controller::new(case_path.clone()),
            cpu_path,
            case_path,
        }
    }

    /// True iff BOTH zone curve files currently load a non-empty curve (the live mode switch). A pure
    /// config read (throwaway caches) — never perturbs the persistent controllers' EMA.
    fn both_present(&self) -> bool {
        !CurveCache::new(self.cpu_path.as_str()).curve().is_empty()
            && !CurveCache::new(self.case_path.as_str()).curve().is_empty()
    }
}

/// Derive a zone curve path by inserting `.<zone>` before the `.curve.json` suffix (else append).
fn zone_path(main_curve_path: &str, zone: &str) -> String {
    match main_curve_path.strip_suffix(".curve.json") {
        Some(stem) => format!("{stem}.{zone}.curve.json"),
        None => format!("{main_curve_path}.{zone}"),
    }
}

struct It87Device {
    dir: PathBuf,
    cfg: It87Config,
    zones: Option<Zones>,
    restore_armed: bool,
}

impl Device for It87Device {
    fn apply(&mut self, inputs: Option<&Inputs>, ctrl: &mut Controller) -> Applied {
        let gpu_temps = input_temps_from(inputs, "nvidia");
        let cpu_temps = hwmon::read_temps("coretemp");
        let gpu_max = gpu_temps.iter().copied().max();
        let cpu_max = cpu_temps.iter().map(|(_, t)| *t).max();
        // Decision 2A: case fans follow max(GPU, CPU) — a desktop tower is one airflow chamber, so
        // intake/exhaust must respond to CPU heat too (unlike asrock's directed-airflow server).
        let case_raw_opt = [gpu_max, cpu_max].into_iter().flatten().max();

        let zones = self
            .zones
            .get_or_insert_with(|| Zones::for_main_path(ctrl.path()));
        let zone_mode = zones.both_present();

        // Decide the commanded duty per managed channel + the driving record for the readings.
        let (commanded, driving) = if zone_mode {
            let (Some(cpu_raw), Some(case_raw)) = (cpu_max, case_raw_opt) else {
                zones.cpu.reset();
                zones.case.reset();
                return self.release_or_error("zone mode: a zone temp is indeterminable");
            };
            let cpu_duty = zones.cpu.duty(cpu_raw);
            let case_duty = zones.case.duty(case_raw);
            let (Some(cpu_pct), Some(case_pct)) = (cpu_duty.pct, case_duty.pct) else {
                zones.cpu.reset();
                zones.case.reset();
                return self.release_or_error("zone mode: no usable curve");
            };
            tracing::info!(
                cpu_raw,
                case_raw,
                cpu_pct,
                case_pct,
                cpu_smoothed = cpu_duty.smoothed,
                case_smoothed = case_duty.smoothed,
                "decision: set board fans (zone mode)"
            );
            let commanded = self.commanded_zone(cpu_pct, case_pct);
            let driving = Reading::new(
                "driving",
                "driving",
                json!({
                    "mode": "zone",
                    "cpu_raw": cpu_raw, "cpu_temp": cpu_duty.smoothed, "cpu_pct": cpu_pct,
                    "case_raw": case_raw, "case_temp": case_duty.smoothed, "case_pct": case_pct,
                }),
            );
            (commanded, driving)
        } else {
            let Some(raw) = case_raw_opt else {
                return self.release_or_error("indeterminable temp");
            };
            let duty = ctrl.duty(raw);
            let Some(pct) = duty.pct else {
                return self.release_or_error("no usable curve");
            };
            tracing::info!(gpu_max = ?gpu_max, cpu_max = ?cpu_max, raw_driving = raw,
                smoothed = duty.smoothed, commanded_pct = pct, "decision: set all board fans (uniform)");
            let commanded: Vec<(u8, u32)> = self
                .cfg
                .managed_channels()
                .into_iter()
                .map(|ch| (ch, pct))
                .collect();
            let driving = Reading::new(
                "driving",
                "driving",
                json!({ "mode": "uniform", "temp": duty.smoothed, "raw": raw, "pct": pct }),
            );
            (commanded, driving)
        };

        // Command the duties. A channel is put under manual control and set in one call; on the first
        // failure, revert EVERYTHING to firmware and report the fault (never hold manual-but-stale).
        for &(ch, pct) in &commanded {
            if let Err(e) = hwmon::set_pwm_duty(&self.dir, ch, pct) {
                let _ = self.restore_to_auto();
                return Applied::error(format!("set pwm{ch}: {e}"));
            }
        }

        // Build readings: GPU temp (routed), CPU temps, the driving record, and per-channel pwm+rpm.
        let mut readings = Vec::new();
        if let Some(g) = gpu_max {
            readings.push(Reading::new("temp", "GPU", json!({ "temp": g })));
        }
        for (label, t) in &cpu_temps {
            readings.push(Reading::new("temp", label.clone(), json!({ "temp": t })));
        }
        readings.push(driving);
        for &(ch, pct) in &commanded {
            let mut f = serde_json::Map::new();
            f.insert("pwm".to_string(), json!(pct));
            if let Some(rpm) = hwmon::read_fan_rpm(&self.dir, ch) {
                f.insert("rpm".to_string(), json!(rpm));
            }
            readings.push(Reading::new("fan", format!("fan{ch}"), json!(f)));
        }
        Applied::ok(readings)
    }

    fn restore(&mut self) {
        if !self.restore_armed {
            return;
        }
        match self.restore_to_auto() {
            Ok(()) => {
                tracing::info!("managed channels restored to firmware/automatic");
                self.restore_armed = false;
            }
            Err(e) => eprintln!("WARNING: pwm restore failed (will retry on drop): {e}"),
        }
    }
}

impl It87Device {
    /// Commanded `(channel, pct)` in zone mode: CPU-zone channels take `cpu_pct`, all other managed
    /// channels take `case_pct`.
    fn commanded_zone(&self, cpu_pct: u32, case_pct: u32) -> Vec<(u8, u32)> {
        self.cfg
            .managed_channels()
            .into_iter()
            .map(|ch| {
                let pct = if self.cfg.cpu_channels.contains(&ch) {
                    cpu_pct
                } else {
                    case_pct
                };
                (ch, pct)
            })
            .collect()
    }

    /// Set every managed channel back to firmware/automatic control. Returns the first error (after
    /// attempting ALL channels, so one stuck channel never strands the others on manual).
    fn restore_to_auto(&self) -> anyhow::Result<()> {
        let mut first_err = None;
        for ch in self.cfg.managed_channels() {
            if let Err(e) = hwmon::set_pwm_auto(&self.dir, ch) {
                first_err.get_or_insert(anyhow::anyhow!("pwm{ch}: {e}"));
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Release all channels to firmware/automatic and report the tick as an `error` — the safe
    /// fallback when no duty can be determined (no temp / no usable curve). Mirrors asrock.
    fn release_or_error(&self, why: &str) -> Applied {
        match self.restore_to_auto() {
            Ok(()) => Applied::error(format!("{why} — released to firmware/automatic")),
            Err(e) => Applied::error(format!("{why}; release failed: {e}")),
        }
    }
}

impl Drop for It87Device {
    fn drop(&mut self) {
        // Final fail-safe: restore firmware control on any path that skipped `restore` (panic unwind,
        // early exit). sysfs PWM persists after exit, so this matters.
        if self.restore_armed {
            let _ = self.restore_to_auto();
        }
    }
}

/// Extract temperature readings only from inputs whose SOURCE MODULE is `src` (keys are `module:id`;
/// module names cannot contain `:` — enforced by the registry — so the `module:` prefix is
/// unambiguous). Mirrors asrock's helper.
fn input_temps_from(inputs: Option<&Inputs>, src: &str) -> Vec<i32> {
    let mut v = Vec::new();
    if let Some(inputs) = inputs {
        let prefix = format!("{src}:");
        for (key, readings) in inputs {
            if key.starts_with(&prefix) {
                for r in readings {
                    if r.kind == "temp" {
                        if let Some(t) = r.get_i64("temp") {
                            v.push(t as i32);
                        }
                    }
                }
            }
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_path_inserts_before_suffix() {
        assert_eq!(
            zone_path("/opt/aiolos/etc/it87.curve.json", "cpu"),
            "/opt/aiolos/etc/it87.cpu.curve.json"
        );
        assert_eq!(
            zone_path("/tmp/etc/it87.curve.json", "case"),
            "/tmp/etc/it87.case.curve.json"
        );
        assert_eq!(zone_path("/weird/path", "cpu"), "/weird/path.cpu");
    }

    #[test]
    fn commanded_zone_splits_cpu_and_case() {
        let dev = It87Device {
            dir: PathBuf::from("/nonexistent"),
            cfg: It87Config {
                chip: "it8689".into(),
                cpu_channels: vec![1],
                case_channels: vec![3, 4],
            },
            zones: None,
            restore_armed: true,
        };
        // FAN1 = cpu duty (30); FAN3/FAN4 = case duty (75); sorted by channel.
        assert_eq!(dev.commanded_zone(30, 75), vec![(1, 30), (3, 75), (4, 75)]);
    }

    #[test]
    fn input_temps_from_partitions_by_source_module() {
        use std::collections::HashMap;
        let mut inputs: Inputs = HashMap::new();
        inputs.insert(
            "nvidia:GPU-1".into(),
            vec![Reading::new("temp", "GPU", json!({"temp": 63}))],
        );
        inputs.insert(
            "nvme:SER-A".into(),
            vec![Reading::new("temp", "Composite", json!({"temp": 44}))],
        );
        assert_eq!(input_temps_from(Some(&inputs), "nvidia"), vec![63]);
        // A short source name must not match a longer module (the `:` guards it).
        assert!(input_temps_from(Some(&inputs), "nv").is_empty());
        assert!(input_temps_from(None, "nvidia").is_empty());
    }
}
