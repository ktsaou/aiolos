//! asrock16-2t anemos — ASRockRack ROME2D16-2T board fan control via inband IPMI.
//!
//! detect → one board id. run → each `apply`: driving_temp = max(GPU temps from inputs, own CPU
//! temps via k10temp); set all 8 fans to curve(driving_temp) (uniform); report. Fail-safe: release
//! to BMC auto on shutdown/EOF, and whenever the temperature is indeterminable (user decision 9).

mod ipmi;

use crate::ipmi::Ipmi;
use protocol::{
    Applied, Curve, CurveCache, Damper, Detected, Event, FoundEntry, Inputs, Reading, Request,
    StdinReader,
};
use serde_json::json;
use std::fs;
use std::io::Write;
use std::time::Duration;
use tracing::{error, info};

const CURVE_PATH_DEFAULT: &str = "/opt/aiolos/etc/asrock16-2t.curve.json";

/// Poll step for the signal-aware stdin reader: a termination signal is noticed within ~this long.
const STEP: Duration = Duration::from_millis(200);

fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    // Every long-running mode restores the board to BMC auto on SIGTERM/SIGINT (self-sufficient
    // shutdown — never depends on the parent killing us). The handler only sets a flag; the run
    // loop performs the release in normal code.
    protocol::install_shutdown_handlers();

    let mode = std::env::args().nth(1).unwrap_or_else(|| "detect".into());
    match mode.as_str() {
        "detect" => detect_loop(),
        "run" => run_loop(&std::env::args().nth(2).expect("run requires <ID>")),
        "query" => query_mode(),
        // Uniform one-shot fail-safe (same verb across all anemoi, called by `aiolos restore`).
        "restore" => restore_mode(),
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

/// One-shot fail-safe: hand the board fans back to BMC auto and exit. Invoked by `aiolos restore`
/// (which systemd's ExecStopPost calls) so the fans are never stranded in manual if aiolos died
/// without a graceful shutdown (hard crash / SIGKILL). Idempotent — safe to run when already auto.
fn restore_mode() {
    match Ipmi::open() {
        Ok(mut ipmi) => match ipmi.release_auto() {
            Ok(()) => info!("fans released to BMC auto"),
            Err(e) => {
                eprintln!("release FAILED: {e}");
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
    // detect holds no device, so a signal/EOF just exits cleanly (nothing to restore).
    let mut stdin = match StdinReader::new() {
        Ok(s) => s,
        Err(e) => {
            error!(error=%e, "stdin setup failed");
            return;
        }
    };
    // A signal/EOF ends `next_event` with a non-`Line` event -> the while-let exits the loop.
    while let Event::Line(line) = stdin.next_event(STEP) {
        match Request::from_line(line.trim()) {
            Ok(Request::Detect) => emit_line(
                Detected::ok(vec![FoundEntry {
                    id: "asrock16-2t".into(),
                    kind: "board".into(),
                    name: "ROME2D16-2T".into(),
                    extra: Default::default(),
                }])
                .to_line(),
            ),
            Ok(Request::Shutdown) => {
                emit_line(Applied::ok_empty().to_line());
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
    let mut damper = Damper::default();
    let mut ipmi = match Ipmi::open() {
        Ok(i) => Some(i),
        Err(e) => {
            error!(error=%e, "/dev/ipmi0 open failed — running degraded (BMC keeps auto control)");
            None
        }
    };

    let mut stdin = match StdinReader::new() {
        Ok(s) => s,
        Err(e) => {
            error!(error=%e, "stdin setup failed — restoring + exiting");
            restore.restore();
            return;
        }
    };
    loop {
        let line = match stdin.next_event(STEP) {
            Event::Line(l) => l,
            // SIGTERM/SIGINT or parent gone (EOF): release to BMC auto, then exit. Self-sufficient.
            Event::Shutdown => {
                info!("termination signal — releasing to BMC auto and exiting");
                restore.restore();
                break;
            }
            Event::Eof => {
                restore.restore();
                break;
            }
        };
        match Request::from_line(line.trim()) {
            Ok(Request::Apply { inputs }) => {
                // Re-read the curve + sensitivity every tick (live tuning; last-good on partial writes).
                if curves.reload() {
                    info!(path=%curves.path(), alpha = curves.alpha(), "config reloaded");
                }
                damper.set_alpha(curves.alpha()); // live sensitivity knob
                emit_line(
                    apply_tick(&mut ipmi, inputs.as_ref(), curves.curve(), &mut damper).to_line(),
                );
            }
            Ok(Request::Shutdown) => {
                restore.restore();
                emit_line(Applied::ok_empty().to_line());
                break;
            }
            Ok(Request::Detect) => eprintln!("unexpected detect in run mode"),
            Err(e) => {
                eprintln!("malformed request: {e}");
                emit_line(Applied::error(format!("malformed: {e}")).to_line());
            }
        }
    }
}

fn apply_tick(
    ipmi: &mut Option<Ipmi>,
    inputs: Option<&Inputs>,
    curve: &Curve,
    damper: &mut Damper,
) -> Applied {
    let gpu_temps = input_temps(inputs);
    let cpu_temps = read_cpu_temps();
    let gpu_max = gpu_temps.iter().copied().max();
    let cpu_max = cpu_temps.iter().map(|(_, t)| *t).max();
    let raw_driving = [gpu_max, cpu_max].into_iter().flatten().max();

    let Some(ipmi) = ipmi.as_mut() else {
        // Can't open /dev/ipmi0 at all → declare fatal; supervisor retries on a long backoff.
        return Applied::fatal("/dev/ipmi0 unavailable");
    };

    // Without a valid temperature or a usable curve we cannot control safely: release to BMC auto
    // rather than hold manual control while blind (decision 9). Reset the damper so control
    // re-seeds cleanly when temperature returns.
    if !should_control(raw_driving, curve) {
        damper.reset();
        info!(gpu_max = ?gpu_max, cpu_max = ?cpu_max, curve_empty = curve.is_empty(),
              "decision: cannot control (no temp or empty curve) -> release to BMC auto");
        return match ipmi.release_auto() {
            Ok(()) => Applied::error("indeterminable temp/curve — released to BMC auto"),
            Err(e) => Applied::error(format!("release failed: {e}")),
        };
    }
    let raw = raw_driving.expect("should_control guarantees Some");

    // EMA-smooth the driving temp, evaluate the curve, then deadband the duty so sensor jitter
    // (noisy CPU Tctl, bursty GPU temp) doesn't make the fans hunt.
    let smoothed = damper.smooth(raw);
    let target = curve.eval(smoothed).clamp(0, 100);
    let pct = damper.deadband(target);

    if let Err(e) = ipmi.set_all_fans(pct) {
        return Applied::error(format!("set fans: {e}"));
    }

    info!(
        gpu_max = ?gpu_max,
        cpu_max = ?cpu_max,
        raw_driving = raw,
        smoothed_driving = smoothed,
        commanded_pct = pct,
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
        json!({ "temp": smoothed, "raw": raw, "pct": pct }),
    ));
    for i in 0..8 {
        // Report the commanded duty (authoritative). The immediate 0xda readback is one BMC cycle
        // stale, so it is no longer used here (see SOW-0001 decision 12).
        readings.push(Reading::new(
            "fan",
            format!("FAN{}", i + 1),
            json!({ "pwm": pct }),
        ));
    }
    Applied::ok(readings)
}

/// True when we can control safely (a valid driving temp AND a usable curve); false -> release.
fn should_control(driving: Option<i32>, curve: &Curve) -> bool {
    driving.is_some() && !curve.is_empty()
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
        let result = (|| -> anyhow::Result<()> { Ipmi::open()?.release_auto() })();
        match &result {
            Ok(()) => info!("released BMC auto control"),
            Err(e) => eprintln!("WARNING: BMC release failed (will retry on drop): {e}"),
        }
        // R8: disarm ONLY on a successful release, so a failed release is retried by `Drop`.
        self.armed = still_armed_after(result.is_ok());
    }
}

/// New `armed` state after a restore attempt: stays armed (true) until a release SUCCEEDS, so a
/// failed release is retried later by `Drop` — we never give up trying to hand the fans back to the
/// BMC. [R8]
fn still_armed_after(released_ok: bool) -> bool {
    !released_ok
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

fn emit_line(line: serde_json::Result<String>) {
    let line = line.unwrap_or_else(|_| {
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
    fn release_when_no_temp() {
        assert!(!should_control(None, &curve_080()));
    }

    #[test]
    fn release_when_curve_empty() {
        let empty = Curve::from_json(&serde_json::Map::new());
        assert!(!should_control(Some(50), &empty));
    }

    #[test]
    fn control_when_temp_and_curve_present() {
        assert!(should_control(Some(40), &curve_080()));
        assert!(should_control(Some(80), &curve_080()));
    }

    #[test]
    fn fan_restore_stays_armed_until_release_succeeds() {
        // R8: a failed release must keep the guard armed so Drop retries; success disarms.
        assert!(
            still_armed_after(false),
            "a failed release must stay armed for a Drop retry"
        );
        assert!(
            !still_armed_after(true),
            "a successful release must disarm (no redundant retry)"
        );
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
