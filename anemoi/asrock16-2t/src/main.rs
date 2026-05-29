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
            curve_default_path: "/opt/aiolos/etc/asrock16-2t.curve.json",
            curve_env_filename: "asrock16-2t.curve.json",
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
        let gpu_temps = input_temps(inputs);
        let cpu_temps = hwmon::read_temps("k10temp");
        let gpu_max = gpu_temps.iter().copied().max();
        let cpu_max = cpu_temps.iter().map(|(_, t)| *t).max();
        let raw_driving = [gpu_max, cpu_max].into_iter().flatten().max();

        // Without a valid temperature OR a usable curve we cannot control safely: release to BMC auto
        // rather than hold manual control while blind (decision 9). The SDK resets the controller.
        let Some(raw) = raw_driving else {
            return release_or_error(&mut self.board, "indeterminable temp");
        };
        let duty = ctrl.duty(raw);
        let Some(pct) = duty.pct else {
            return release_or_error(&mut self.board, "no usable curve");
        };

        if let Err(e) = self.board.set_all_fans(pct as i32) {
            return Applied::error(format!("set fans: {e}"));
        }

        let mut readings = Vec::new();
        if let Some(g) = gpu_max {
            readings.push(Reading::new("temp", "GPU", json!({ "temp": g })));
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

/// Extract every temperature reading from routed peer inputs (uninterpreted relay; we pick the
/// `temp` values here, in the consumer).
fn input_temps(inputs: Option<&Inputs>) -> Vec<i32> {
    let mut v = Vec::new();
    if let Some(inputs) = inputs {
        for readings in inputs.values() {
            for r in readings {
                if r.kind == "temp" {
                    if let Some(t) = r.get_i64("temp") {
                        v.push(t as i32);
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
    fn fan_restore_stays_armed_until_release_succeeds() {
        assert!(
            still_armed_after(false),
            "a failed release must stay armed for a Drop retry"
        );
        assert!(!still_armed_after(true), "a successful release must disarm");
    }

    #[test]
    fn input_temps_extracts_gpu_temps() {
        let mut inputs: Inputs = HashMap::new();
        inputs.insert(
            "GPU-1".into(),
            vec![
                Reading::new("temp", "GPU", json!({"temp": 63})),
                Reading::new("fan", "fan0", json!({"pwm": 70})),
            ],
        );
        inputs.insert(
            "GPU-2".into(),
            vec![Reading::new("temp", "GPU", json!({"temp": 71}))],
        );
        let mut temps = input_temps(Some(&inputs));
        temps.sort();
        assert_eq!(temps, vec![63, 71]);
    }
}
