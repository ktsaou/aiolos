//! asrock16-2t anemos — ASRockRack ROME2D16-2T board fan control via inband IPMI.
//!
//! Level-3: device logic ONLY. The `anemos` SDK owns the lifecycle (CLI/signals/logging/curve+EMA/
//! protocol/restore); `ipmi` is the IPMI transport; `board` is the board's OEM fan commands; `hwmon`
//! reads CPU temps. detect → one board. apply → drive 8 fans to curve(max(GPU inputs, CPU temps)).
//! restore → release to BMC auto. A `query` subcommand reads the live duty (read-only diagnostic).

mod board;

use anemos::{
    Anemos, Applied, Controller, Detected, Device, ExtraCmd, FoundEntry, Inputs, ModuleInfo,
    Reading,
};
use board::Board;
use serde_json::json;
use std::collections::HashMap;

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
        Ok(Box::new(AsrockDevice {
            board: Board::open()?,
            restore_armed: true,
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
}

impl Device for AsrockDevice {
    fn apply(&mut self, inputs: Option<&Inputs>, ctrl: &mut Controller) -> Applied {
        // Routed temps arrive keyed by `module:id`; partition by source so GPU and NVMe are
        // labelled distinctly in the readings. The driving max uses ALL routed temps (robust if
        // more sources are wired later) plus the local CPU sensors.
        let gpu_temps = input_temps_from(inputs, "nvidia");
        let nvme_temps = input_temps_from(inputs, "nvme");
        let cpu_temps = hwmon::read_temps("k10temp");
        let gpu_max = gpu_temps.iter().copied().max();
        let nvme_max = nvme_temps.iter().copied().max();
        let cpu_max = cpu_temps.iter().map(|(_, t)| *t).max();
        let input_max = input_temps(inputs).into_iter().max();
        let raw_driving = [input_max, cpu_max].into_iter().flatten().max();

        // Without a valid temperature OR a usable curve we cannot control safely: release to BMC auto
        // rather than hold manual control while blind (decision 9). The SDK resets the controller.
        let Some(raw) = raw_driving else {
            return release_or_error(&mut self.board, "indeterminable temp");
        };
        let duty = ctrl.duty(raw);
        let Some(pct) = duty.pct else {
            return release_or_error(&mut self.board, "no usable curve");
        };
        tracing::info!(gpu_max = ?gpu_max, nvme_max = ?nvme_max, cpu_max = ?cpu_max,
            raw_driving = raw, smoothed_driving = duty.smoothed, commanded_pct = pct,
            "decision: set all board fans");

        if let Err(e) = self.board.set_all_fans(pct as i32) {
            return Applied::error(format!("set fans: {e}"));
        }

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
        readings.push(Reading::new(
            "driving",
            "driving",
            json!({ "temp": duty.smoothed, "raw": raw, "pct": pct }),
        ));
        for i in 0..8 {
            readings.push(Reading::new(
                "fan",
                format!("FAN{}", i + 1),
                json!({ "pwm": pct }),
            ));
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

impl Drop for AsrockDevice {
    fn drop(&mut self) {
        if self.restore_armed {
            if let Ok(mut b) = Board::open() {
                let _ = b.release_auto();
            }
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
}
