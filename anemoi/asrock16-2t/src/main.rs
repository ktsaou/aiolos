//! asrock16-2t anemos — ASRockRack ROME2D16-2T board fan control via inband IPMI.
//!
//! detect → one board id. run → each `apply`: driving_temp = max(GPU temps from inputs, own CPU
//! temps via k10temp); set all 8 fans to curve(driving_temp) (uniform); report. Fail-safe: release
//! to BMC auto on shutdown/EOF, and whenever the temperature is indeterminable (user decision 9).

mod ipmi;

use crate::ipmi::Ipmi;
use protocol::{Applied, Curve, CurveCache, Found, FoundEntry, Inputs, Reading, Request, Response};
use serde_json::json;
use std::fs;
use std::io::{BufRead, Write};
use tracing::{error, info};

const CURVE_PATH_DEFAULT: &str = "/opt/aiolos/etc/asrock16-2t.curve.json";

fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let mode = std::env::args().nth(1).unwrap_or_else(|| "detect".into());
    match mode.as_str() {
        "detect" => detect_loop(),
        "run" => run_loop(&std::env::args().nth(2).expect("run requires <ID>")),
        "query" => query_mode(),
        other => {
            eprintln!("unknown mode: {other}");
            std::process::exit(1);
        }
    }
}

/// Read-only diagnostic: send only `0xda` (query duty) and print the result. Validates the IPMI
/// ioctl ABI against the live BMC with ZERO side effects (no claim, no duty change).
fn query_mode() {
    match Ipmi::open() {
        Ok(mut ipmi) => match ipmi.query_duty() {
            Ok(duty) => {
                println!("0xda OK ({} bytes): {duty:?}", duty.len());
                for (i, b) in duty.iter().take(8).enumerate() {
                    println!("  FAN{} = {}%", i + 1, b);
                }
            }
            Err(e) => {
                eprintln!("0xda query FAILED: {e}");
                std::process::exit(2);
            }
        },
        Err(e) => {
            eprintln!("open /dev/ipmi0 FAILED: {e}");
            std::process::exit(3);
        }
    }
}

// ---------------------------------------------------------------------------
// detect
// ---------------------------------------------------------------------------

fn detect_loop() {
    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    let mut line = String::new();
    loop {
        line.clear();
        match lock.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                error!(error=%e, "stdin read error");
                break;
            }
        }
        match Request::from_line(line.trim()) {
            Ok(Request::Detect) => emit(Response::Found(Found {
                found: vec![FoundEntry {
                    id: "asrock16-2t".into(),
                    kind: "board".into(),
                    name: "ROME2D16-2T".into(),
                    extra: Default::default(),
                }],
            })),
            Ok(Request::Shutdown) => {
                emit(Response::Applied(Applied::ok_empty()));
                break;
            }
            Ok(Request::Apply { .. }) => eprintln!("unexpected apply in detect mode"),
            Err(e) => eprintln!("malformed request: {e}"),
        }
    }
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

fn run_loop(_id: &str) {
    let mut curves = CurveCache::new(curve_path());
    if curves.curve().is_empty() {
        error!(path=%curves.path(), "curve missing/empty — holding fans on BMC auto until a valid curve is present");
    }

    let mut restore = FanRestore::new();
    let mut ipmi = match Ipmi::open() {
        Ok(i) => Some(i),
        Err(e) => {
            error!(error=%e, "/dev/ipmi0 open failed — running degraded (BMC keeps auto control)");
            None
        }
    };

    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    let mut line = String::new();
    loop {
        line.clear();
        match lock.read_line(&mut line) {
            Ok(0) => {
                restore.restore();
                break;
            }
            Ok(_) => {}
            Err(e) => {
                error!(error=%e, "stdin read error");
                restore.restore();
                break;
            }
        }
        match Request::from_line(line.trim()) {
            Ok(Request::Apply { inputs }) => {
                // Re-read the curve every tick (live tuning; last-good on partial writes).
                if curves.reload() {
                    info!(path=%curves.path(), "curve reloaded");
                }
                emit(Response::Applied(apply_tick(
                    &mut ipmi,
                    inputs.as_ref(),
                    curves.curve(),
                )));
            }
            Ok(Request::Shutdown) => {
                restore.restore();
                emit(Response::Applied(Applied::ok_empty()));
                break;
            }
            Ok(Request::Detect) => eprintln!("unexpected detect in run mode"),
            Err(e) => {
                eprintln!("malformed request: {e}");
                emit(Response::Applied(Applied::error(format!("malformed: {e}"))));
            }
        }
    }
}

fn apply_tick(ipmi: &mut Option<Ipmi>, inputs: Option<&Inputs>, curve: &Curve) -> Applied {
    let gpu_temps = input_temps(inputs);
    let cpu_temps = read_cpu_temps();
    let gpu_max = gpu_temps.iter().copied().max();
    let cpu_max = cpu_temps.iter().map(|(_, t)| *t).max();
    let driving = [gpu_max, cpu_max].into_iter().flatten().max();

    let Some(ipmi) = ipmi.as_mut() else {
        return Applied::error("ipmi device unavailable");
    };

    match decide(driving, curve) {
        FanAction::ReleaseAuto => {
            // Cannot determine a temperature (or no curve): relinquish to BMC auto (decision 9).
            info!(gpu_max = ?gpu_max, cpu_max = ?cpu_max, "decision: temperature indeterminable -> release to BMC auto");
            match ipmi.release_auto() {
                Ok(()) => Applied::error("temperature indeterminable — released to BMC auto"),
                Err(e) => Applied::error(format!("temp indeterminable; release failed: {e}")),
            }
        }
        FanAction::SetDuty(pct) => {
            if let Err(e) = ipmi.set_all_fans(pct) {
                return Applied::error(format!("set fans: {e}"));
            }
            let duty = ipmi.query_duty().unwrap_or_default();
            info!(
                gpu_max = ?gpu_max,
                cpu_max = ?cpu_max,
                driving = driving.unwrap_or(-1),
                commanded_pct = pct,
                readback = ?&duty[..8.min(duty.len())],
                "decision: set all board fans",
            );

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
                json!({ "temp": driving.unwrap_or(0), "pct": pct }),
            ));
            for i in 0..8 {
                // Report the 0xda read-back duty when available (verifies the set took); else the
                // commanded value.
                let pwm = duty.get(i).map(|b| *b as i64).unwrap_or(pct as i64);
                readings.push(Reading::new(
                    "fan",
                    format!("FAN{}", i + 1),
                    json!({ "pwm": pwm }),
                ));
            }
            Applied::ok(readings)
        }
    }
}

#[derive(Debug, PartialEq)]
enum FanAction {
    SetDuty(i32),
    ReleaseAuto,
}

/// Decide the fan action. Without a valid temperature or a usable curve we cannot control safely,
/// so we release to BMC auto rather than hold manual control while blind.
fn decide(driving: Option<i32>, curve: &Curve) -> FanAction {
    match driving {
        Some(t) if !curve.is_empty() => FanAction::SetDuty(curve.eval(t)),
        _ => FanAction::ReleaseAuto,
    }
}

/// Extract every temperature reading from routed peer inputs (uninterpreted relay; we pick the
/// `temp` values here, in the consumer, as the design intends).
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

/// Read all k10temp sensors across both EPYC sockets (every `tempN_input`), labeled where possible.
/// Returns °C values; empty if k10temp is unavailable.
fn read_cpu_temps() -> Vec<(String, i32)> {
    let mut out = Vec::new();
    let Ok(dir) = fs::read_dir("/sys/class/hwmon") else {
        return out;
    };
    for entry in dir.flatten() {
        let path = entry.path();
        if fs::read_to_string(path.join("name"))
            .map(|n| n.trim() != "k10temp")
            .unwrap_or(true)
        {
            continue;
        }
        let Ok(files) = fs::read_dir(&path) else {
            continue;
        };
        for f in files.flatten() {
            let fname = f.file_name().to_string_lossy().into_owned();
            let Some(n) = fname
                .strip_prefix("temp")
                .and_then(|s| s.strip_suffix("_input"))
            else {
                continue;
            };
            if let Ok(milli) = fs::read_to_string(f.path())
                .unwrap_or_default()
                .trim()
                .parse::<i32>()
            {
                let label = fs::read_to_string(path.join(format!("temp{n}_label")))
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| format!("CPU.temp{n}"));
                out.push((label, milli / 1000));
            }
        }
    }
    out
}

fn curve_path() -> String {
    std::env::var("AIOLOS_ETC_DIR")
        .map(|d| format!("{d}/asrock16-2t.curve.json"))
        .unwrap_or_else(|_| CURVE_PATH_DEFAULT.to_string())
}

// ---------------------------------------------------------------------------
// Fail-safe restore (RAII): release BMC auto on shutdown/EOF/panic. Opens a fresh handle so it
// never depends on the (possibly poisoned) main loop state.
// ---------------------------------------------------------------------------

struct FanRestore {
    armed: bool,
}

impl FanRestore {
    fn new() -> Self {
        FanRestore { armed: true }
    }

    fn restore(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        match Ipmi::open() {
            Ok(mut i) => match i.release_auto() {
                Ok(()) => info!("released BMC auto control"),
                Err(e) => eprintln!("WARNING: BMC release failed: {e}"),
            },
            Err(e) => eprintln!("WARNING: cannot open /dev/ipmi0 to release: {e}"),
        }
    }
}

impl Drop for FanRestore {
    fn drop(&mut self) {
        if self.armed {
            if let Ok(mut i) = Ipmi::open() {
                let _ = i.release_auto();
            }
        }
    }
}

fn emit(resp: Response) {
    let line = resp.to_line().unwrap_or_else(|_| {
        r#"{"status":"error","error":"internal serialization error"}"#.to_string()
    });
    let mut out = std::io::stdout();
    let _ = out.write_all(line.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve_080() -> Curve {
        let v: serde_json::Value = serde_json::from_str(r#"{"0":0,"80":100}"#).unwrap();
        Curve::from_json(v.as_object().unwrap())
    }

    #[test]
    fn decide_release_when_no_temp() {
        assert_eq!(decide(None, &curve_080()), FanAction::ReleaseAuto);
    }

    #[test]
    fn decide_release_when_curve_empty() {
        let empty = Curve::from_json(&serde_json::Map::new());
        assert_eq!(decide(Some(50), &empty), FanAction::ReleaseAuto);
    }

    #[test]
    fn decide_setduty_interpolates() {
        assert_eq!(decide(Some(40), &curve_080()), FanAction::SetDuty(50));
        assert_eq!(decide(Some(80), &curve_080()), FanAction::SetDuty(100));
    }

    #[test]
    fn input_temps_extracts_gpu_temps() {
        let mut inputs: Inputs = std::collections::HashMap::new();
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
