//! asrock16-2t anemos — ASRockRack ROME2D16-2T board fan control via inband IPMI.
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle (CLI/signals/logging/curve+EMA/
//! protocol/restore); `ipmi` is the IPMI transport; `board` is the board's OEM fan commands; `hwmon`
//! reads CPU temps. detect → one board.
//!
//! apply (SOW-0010) drives the 8 fans in one of two modes, decided live each tick from the config:
//! - **uniform** (default, back-compatible): one `curve(max(all routed inputs, CPU))` for all 8 fans
//!   — the SDK-provided controller reading `asrock16-2t.curve.json`.
//! - **per-zone**: when BOTH optional zone curve files load a non-empty curve, drive FAN1/2 (the
//!   Noctua CPU coolers) from CPU temps via `asrock16-2t.cpu.curve.json`, and FAN3–8 (case fans)
//!   from `max(all routed inputs = GPU+NVMe+…)` via `asrock16-2t.case.curve.json`. Two internal
//!   `anemos::Controller`s (own EMA/deadband/sensitivity), commanded via the per-fan `0xd6` path.
//!
//! Fan-fault detection (SOW-0008): a fan commanded above a duty threshold that reads ≈0 RPM for
//! several consecutive ticks (after a spin-up grace) is flagged (`"fault":true` reading + warn), and
//! the surviving fans in its zone are boosted to 100% for more airflow.
//!
//! restore → release to BMC auto. A `query` subcommand reads the live duty (read-only diagnostic).

mod board;
mod fault;
mod zones;

use anemos::{
    Anemos, Applied, Controller, Detected, Device, ExtraCmd, FoundEntry, Inputs, ModuleInfo,
    Reading,
};
use board::Board;
use fault::FanFaultTracker;
use serde_json::json;
use std::collections::HashMap;
use zones::ZoneControllers;

fn main() -> ! {
    let mut extra: HashMap<&'static str, ExtraCmd> = HashMap::new();
    extra.insert("query", Box::new(|_args| query_mode()));
    anemos::run_with(
        ModuleInfo {
            name: "asrock16-2t",
            curve_default_path: Some("/opt/aiolos/etc/asrock16-2t.curve.json"),
            curve_env_filename: Some("asrock16-2t.curve.json"),
        },
        Asrock,
        extra,
    )
}

struct Asrock;

impl Anemos for Asrock {
    fn detect(&mut self) -> Detected {
        Detected::ok(vec![FoundEntry {
            id: "asrock16-2t".to_string(),
            kind: "board".to_string(),
            name: "ROME2D16-2T".to_string(),
            extra: Default::default(),
        }])
    }

    fn open(&mut self, _id: &str) -> anyhow::Result<Box<dyn Device>> {
        let mut board = Board::open()?;
        // Warm the per-fan tach conversion-factor cache once here (off the apply deadline) so the
        // first tick is no heavier than the rest; any that fail are retried lazily during ticks.
        board.prefetch_fan_factors();
        Ok(Box::new(AsrockDevice {
            board,
            restore_armed: true,
            // Lazily built on the first apply, once the SDK controller reveals the curve path
            // (so the zone curve files sit next to the main one and honour `$AIOLOS_ETC_DIR`).
            zones: None,
            faults: FanFaultTracker::new(),
        }))
    }

    fn restore_all(&mut self) {
        match (|| -> anyhow::Result<()> { Board::open()?.release_auto() })() {
            Ok(()) => tracing::info!("fans released to BMC auto"),
            Err(e) => {
                eprintln!("restore FAILED: {e}");
                std::process::exit(2);
            }
        }
    }
}

struct AsrockDevice {
    board: Board,
    restore_armed: bool,
    /// Per-zone controllers (CPU coolers vs case fans), built lazily on the first apply from the
    /// SDK controller's curve path. Present only when both zone curve files exist; otherwise the
    /// uniform SDK controller drives all 8 fans.
    zones: Option<ZoneControllers>,
    /// Per-fan stall detector (commanded-above-threshold vs measured RPM, with grace + hysteresis).
    faults: FanFaultTracker,
}

impl Device for AsrockDevice {
    fn apply(&mut self, inputs: Option<&Inputs>, ctrl: &mut Controller) -> Applied {
        // Routed temps arrive keyed by `module:id`; partition by source so GPU and NVMe are
        // labelled distinctly in the readings. The uniform driving max uses ALL routed temps (robust
        // if more sources are wired later) plus the local CPU sensors. The per-zone path (SOW-0010)
        // splits these: CPU coolers (FAN1/2) by CPU temp; case fans (FAN3–8) by the routed-input max.
        let gpu_temps = input_temps_from(inputs, "nvidia");
        let nvme_temps = input_temps_from(inputs, "nvme");
        let cpu_temps = hwmon::read_temps("k10temp");
        let gpu_max = gpu_temps.iter().copied().max();
        let nvme_max = nvme_temps.iter().copied().max();
        let cpu_max = cpu_temps.iter().map(|(_, t)| *t).max();
        let input_max = input_temps(inputs).into_iter().max(); // GPU+NVMe+any routed source
        let raw_driving = [input_max, cpu_max].into_iter().flatten().max();

        // Build (lazily, once) the per-zone controllers next to the main curve file. The SDK
        // controller's path reveals the etc dir + env override; we never touch the SDK controller's
        // EMA when in zone mode.
        let zones = self
            .zones
            .get_or_insert_with(|| ZoneControllers::for_main_path(ctrl.path()));

        // Decide the mode LIVE each tick (so dropping in / removing the zone files takes effect on
        // the next tick): zone mode iff BOTH zone curve files load a non-empty curve. This is a pure
        // config read; it does not perturb any controller's EMA.
        let zone_mode = zones.both_present();

        // Compensation set from PRIOR ticks: zones with a confirmed-faulted fan get their surviving
        // fans boosted this tick. Computed before the duty math so it can override the base duties.
        let confirmed = self.faults.confirmed();

        let outcome = if zone_mode {
            // Per-zone: each zone needs a present temp AND (guaranteed non-empty) curve; if either
            // zone is blind we cannot split safely, so release the WHOLE board (one 0xd6 sets all 8
            // fans — we cannot release one zone and hold the other). Mirrors the uniform fail-safe.
            let (Some(cpu_raw), Some(case_raw)) = (cpu_max, input_max) else {
                self.reset_zone_dampers();
                return release_or_error(
                    &mut self.board,
                    "zone mode: a zone temp is indeterminable",
                );
            };
            let cpu_duty = zones.cpu.duty(cpu_raw);
            let case_duty = zones.case.duty(case_raw);
            let (Some(cpu_pct), Some(case_pct)) = (cpu_duty.pct, case_duty.pct) else {
                self.reset_zone_dampers();
                return release_or_error(&mut self.board, "zone mode: no usable curve");
            };
            let base = zones::per_fan_duties(cpu_pct, case_pct);
            let commanded = fault::compensate(base, &confirmed);
            tracing::info!(
                cpu_raw,
                case_raw,
                cpu_smoothed = cpu_duty.smoothed,
                case_smoothed = case_duty.smoothed,
                cpu_pct,
                case_pct,
                ?commanded,
                ?confirmed,
                "decision: set board fans (zone mode)"
            );
            if let Err(e) = self.board.set_fans_per_fan(&commanded.map(|p| p as i32)) {
                return Applied::error(format!("set fans: {e}"));
            }
            ApplyOutcome::zone(
                commanded,
                cpu_raw,
                case_raw,
                cpu_duty.smoothed,
                case_duty.smoothed,
            )
        } else {
            // Uniform (default, back-compatible): one curve over max(all inputs, CPU), all 8 fans.
            let Some(raw) = raw_driving else {
                return release_or_error(&mut self.board, "indeterminable temp");
            };
            let duty = ctrl.duty(raw);
            let Some(pct) = duty.pct else {
                return release_or_error(&mut self.board, "no usable curve");
            };
            let base = [pct; 8];
            let commanded = fault::compensate(base, &confirmed);
            tracing::info!(gpu_max = ?gpu_max, nvme_max = ?nvme_max, cpu_max = ?cpu_max,
                raw_driving = raw, smoothed_driving = duty.smoothed,
                commanded_pct = pct, ?commanded, ?confirmed,
                "decision: set all board fans (uniform)");
            // Uniform with no active compensation -> the dedicated uniform set; a fault boost makes
            // the duties non-uniform, so use the per-fan path in that case.
            let set = if commanded == base {
                self.board.set_all_fans(pct as i32)
            } else {
                self.board.set_fans_per_fan(&commanded.map(|p| p as i32))
            };
            if let Err(e) = set {
                return Applied::error(format!("set fans: {e}"));
            }
            ApplyOutcome::uniform(commanded, raw, duty.smoothed, pct)
        };

        let mut readings = Vec::new();
        if let Some(g) = gpu_max {
            readings.push(Reading::new("temp", "GPU", json!({ "temp": g })));
        }
        if let Some(n) = nvme_max {
            readings.push(Reading::new("temp", "NVMe", json!({ "temp": n })));
        }
        for (label, t) in &cpu_temps {
            readings.push(Reading::new("temp", label.clone(), json!({ "temp": t })));
        }
        readings.push(outcome.driving_reading());

        // Observability, read AFTER the control decision (and under a short timeout) so a sensor
        // hiccup never affects cooling or fails the tick: the true per-fan duty (0xda readback) and
        // each fan's tachometer RPM. pwm falls back to the commanded duty if the readback is
        // unavailable; rpm is omitted if the tach is unreadable.
        let (duty_readback, fan_rpms) = self.board.read_fan_status();
        // Update the per-fan stall detector with what we COMMANDED this tick and what each tach now
        // reads, producing the confirmed-fault set used for compensation next tick.
        let commanded = outcome.commanded();
        let rpms: [Option<i32>; 8] = std::array::from_fn(|i| fan_rpms.get(i).and_then(|(_, r)| *r));
        let now_faulted = self.faults.update(&commanded, &rpms);
        for (i, (label, rpm)) in fan_rpms.into_iter().enumerate() {
            let pwm = duty_readback
                .as_ref()
                .and_then(|d| d.get(i))
                .map(|&b| b as i64)
                .unwrap_or(commanded[i] as i64);
            let mut f = serde_json::Map::new();
            f.insert("pwm".to_string(), json!(pwm));
            if let Some(r) = rpm {
                f.insert("rpm".to_string(), json!(r));
            }
            if now_faulted[i] {
                f.insert("fault".to_string(), json!(true));
                tracing::warn!(fan = %label, commanded = commanded[i], rpm = ?rpm,
                    "FAN FAULT: commanded above threshold but reads ~0 RPM (stalled/failed fan)");
            }
            readings.push(Reading::new("fan", label, json!(f)));
        }
        Applied::ok(readings)
    }

    fn restore(&mut self) {
        if !self.restore_armed {
            return;
        }
        // Open a FRESH handle (independent of the main one, which may be wedged) and release; disarm
        // ONLY on success so a failed release is retried by `Drop`. [R8]
        let result = (|| -> anyhow::Result<()> { Board::open()?.release_auto() })();
        match &result {
            Ok(()) => tracing::info!("released BMC auto control"),
            Err(e) => eprintln!("WARNING: BMC release failed (will retry on drop): {e}"),
        }
        self.restore_armed = still_armed_after(result.is_ok());
    }
}

impl AsrockDevice {
    /// Reset both zone dampers when we abandon a zone-mode tick (release path), mirroring the SDK's
    /// `ctrl.reset()` on a non-Ok tick so the per-zone EMA/deadband re-seed cleanly on recovery.
    fn reset_zone_dampers(&mut self) {
        if let Some(z) = self.zones.as_mut() {
            z.cpu.reset();
            z.case.reset();
        }
    }
}

impl Drop for AsrockDevice {
    fn drop(&mut self) {
        if self.restore_armed {
            if let Ok(mut b) = Board::open() {
                let _ = b.release_auto();
            }
        }
    }
}

/// What the apply tick decided, carried to the readings stage: the 8 commanded duties plus the
/// `driving` record (uniform: one curve; zone: the two zones' raw/smoothed temps).
enum ApplyOutcome {
    Uniform {
        commanded: [u32; 8],
        raw: i32,
        smoothed: i32,
        pct: u32,
    },
    Zone {
        commanded: [u32; 8],
        cpu_raw: i32,
        case_raw: i32,
        cpu_smoothed: i32,
        case_smoothed: i32,
    },
}

impl ApplyOutcome {
    fn uniform(commanded: [u32; 8], raw: i32, smoothed: i32, pct: u32) -> Self {
        ApplyOutcome::Uniform {
            commanded,
            raw,
            smoothed,
            pct,
        }
    }
    fn zone(
        commanded: [u32; 8],
        cpu_raw: i32,
        case_raw: i32,
        cpu_smoothed: i32,
        case_smoothed: i32,
    ) -> Self {
        ApplyOutcome::Zone {
            commanded,
            cpu_raw,
            case_raw,
            cpu_smoothed,
            case_smoothed,
        }
    }
    fn commanded(&self) -> [u32; 8] {
        match self {
            ApplyOutcome::Uniform { commanded, .. } | ApplyOutcome::Zone { commanded, .. } => {
                *commanded
            }
        }
    }
    /// The `driving` reading describing this tick's control decision (mode-specific fields).
    fn driving_reading(&self) -> Reading {
        match self {
            ApplyOutcome::Uniform {
                raw, smoothed, pct, ..
            } => Reading::new(
                "driving",
                "driving",
                json!({ "mode": "uniform", "temp": smoothed, "raw": raw, "pct": pct }),
            ),
            ApplyOutcome::Zone {
                cpu_raw,
                case_raw,
                cpu_smoothed,
                case_smoothed,
                commanded,
            } => Reading::new(
                "driving",
                "driving",
                json!({
                    "mode": "zone",
                    "cpu_raw": cpu_raw, "cpu_temp": cpu_smoothed, "cpu_pct": commanded[0],
                    "case_raw": case_raw, "case_temp": case_smoothed, "case_pct": commanded[2],
                }),
            ),
        }
    }
}

/// Release the board to BMC auto and report it as an `error` apply (the safe fallback when we cannot
/// determine a duty); used by both the no-temp and empty-curve paths.
fn release_or_error(board: &mut Board, why: &str) -> Applied {
    match board.release_auto() {
        Ok(()) => Applied::error(format!("{why} — released to BMC auto")),
        Err(e) => Applied::error(format!("{why}; release failed: {e}")),
    }
}

/// New `armed` state after a restore attempt: stays armed until a release SUCCEEDS, so a failed
/// release is retried later by `Drop`. [R8]
fn still_armed_after(released_ok: bool) -> bool {
    !released_ok
}

/// Read-only diagnostic: send only `0xda` (query duty) and print the result. Returns an exit code.
fn query_mode() -> i32 {
    match Board::open() {
        Ok(mut b) => match b.query_duty() {
            Ok(duty) => {
                println!("0xda OK ({} bytes): {duty:?}", duty.len());
                for (i, d) in duty.iter().take(8).enumerate() {
                    println!("  FAN{} = {}%", i + 1, d);
                }
                0
            }
            Err(e) => {
                eprintln!("0xda query FAILED: {e}");
                2
            }
        },
        Err(e) => {
            eprintln!("open /dev/ipmi0 FAILED: {e}");
            3
        }
    }
}

/// Extract every temperature reading from ALL routed peer inputs, source-agnostic (used for the
/// driving max, which must reflect every routed source). `inputs` are keyed `module:id`.
fn input_temps(inputs: Option<&Inputs>) -> Vec<i32> {
    let mut v = Vec::new();
    if let Some(inputs) = inputs {
        for readings in inputs.values() {
            push_temps(readings, &mut v);
        }
    }
    v
}

/// Extract temperature readings only from inputs whose SOURCE MODULE is `src` (keys are
/// `module:id`; module names cannot contain `:` — enforced by the registry — so the `module:`
/// prefix is unambiguous). Used to label GPU vs NVMe distinctly in the readings.
fn input_temps_from(inputs: Option<&Inputs>, src: &str) -> Vec<i32> {
    let mut v = Vec::new();
    if let Some(inputs) = inputs {
        let prefix = format!("{src}:");
        for (key, readings) in inputs {
            if key.starts_with(&prefix) {
                push_temps(readings, &mut v);
            }
        }
    }
    v
}

/// Append the `temp` value of every `type:temp` reading to `out`.
fn push_temps(readings: &[Reading], out: &mut Vec<i32>) {
    for r in readings {
        if r.kind == "temp" {
            if let Some(t) = r.get_i64("temp") {
                out.push(t as i32);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fan_restore_stays_armed_until_release_succeeds() {
        assert!(
            still_armed_after(false),
            "a failed release must stay armed for a Drop retry"
        );
        assert!(!still_armed_after(true), "a successful release must disarm");
    }

    #[test]
    fn input_temps_extracts_all_temps_source_agnostic() {
        let mut inputs: Inputs = HashMap::new();
        inputs.insert(
            "nvidia:GPU-1".into(),
            vec![
                Reading::new("temp", "GPU", json!({"temp": 63})),
                Reading::new("fan", "fan0", json!({"pwm": 70})),
            ],
        );
        inputs.insert(
            "nvme:SER-A".into(),
            vec![Reading::new("temp", "Composite", json!({"temp": 44}))],
        );
        let mut temps = input_temps(Some(&inputs));
        temps.sort();
        assert_eq!(temps, vec![44, 63], "driving max sees every routed source");
    }

    #[test]
    fn input_temps_from_partitions_by_source_module() {
        let mut inputs: Inputs = HashMap::new();
        inputs.insert(
            "nvidia:GPU-1".into(),
            vec![Reading::new("temp", "GPU", json!({"temp": 63}))],
        );
        inputs.insert(
            "nvidia:GPU-2".into(),
            vec![Reading::new("temp", "GPU", json!({"temp": 71}))],
        );
        inputs.insert(
            "nvme:SER-A".into(),
            vec![
                Reading::new("temp", "Composite", json!({"temp": 40})),
                Reading::new("temp", "Sensor 2", json!({"temp": 44})),
            ],
        );

        let mut gpu = input_temps_from(Some(&inputs), "nvidia");
        gpu.sort();
        assert_eq!(gpu, vec![63, 71]);

        let mut nv = input_temps_from(Some(&inputs), "nvme");
        nv.sort();
        assert_eq!(nv, vec![40, 44]);

        // A short source name must not match a longer module (the `:` guards it); unknown -> empty.
        assert!(input_temps_from(Some(&inputs), "nv").is_empty());
        assert!(input_temps_from(Some(&inputs), "other").is_empty());

        // No routed inputs at all -> empty, never a panic (both helpers).
        assert!(input_temps_from(None, "nvidia").is_empty());
        assert!(input_temps(None).is_empty());
    }

    #[test]
    fn uniform_outcome_reports_one_curve_decision() {
        let o = ApplyOutcome::uniform([60; 8], 70, 68, 60);
        assert_eq!(o.commanded(), [60; 8]);
        let r = o.driving_reading();
        assert_eq!(r.kind, "driving");
        assert_eq!(r.fields.get("mode").unwrap(), "uniform");
        assert_eq!(r.get_i64("pct"), Some(60));
        assert_eq!(r.get_i64("raw"), Some(70));
    }

    #[test]
    fn zone_outcome_reports_both_zones_from_commanded() {
        // FAN1/2 = cpu duty (30), FAN3-8 = case duty (75); the driving record reads them back from
        // the commanded array so a fault boost would be visible there too.
        let commanded = zones::per_fan_duties(30, 75);
        let o = ApplyOutcome::zone(commanded, 55, 72, 54, 70);
        assert_eq!(o.commanded(), [30, 30, 75, 75, 75, 75, 75, 75]);
        let r = o.driving_reading();
        assert_eq!(r.fields.get("mode").unwrap(), "zone");
        assert_eq!(r.get_i64("cpu_pct"), Some(30));
        assert_eq!(r.get_i64("case_pct"), Some(75));
        assert_eq!(r.get_i64("cpu_raw"), Some(55));
        assert_eq!(r.get_i64("case_raw"), Some(72));
    }

    #[test]
    fn zone_outcome_driving_reflects_a_case_fan_boost() {
        // A confirmed case-fan fault boosts the surviving case fans to 100; the driving record's
        // case_pct (read from commanded[2]) follows.
        let base = zones::per_fan_duties(30, 60);
        let mut confirmed = [false; 8];
        confirmed[4] = true; // a case fan down -> siblings (incl. FAN3=idx2) boosted
        let commanded = fault::compensate(base, &confirmed);
        let o = ApplyOutcome::zone(commanded, 50, 65, 50, 64);
        assert_eq!(o.driving_reading().get_i64("case_pct"), Some(100));
        assert_eq!(
            o.driving_reading().get_i64("cpu_pct"),
            Some(30),
            "CPU zone unaffected"
        );
    }
}
