//! nvidia anemos — per-GPU onboard fan control via NVML.
//!
//! detect → one id per GPU (by UUID). run <UUID> → each `apply`: read temp, apply the curve to the
//! GPU's onboard fans, report readings. `inputs` are ignored (nvidia uses its own GPU temperature).
//! Fail-safe: restore firmware/default fan control on shutdown/EOF (and via `Gpu::Drop` on panic).

mod nvml;

use crate::nvml::Gpu;
use protocol::{Applied, CurveCache, Event, Request, StdinReader};
use std::io::Write;
use std::time::Duration;
use tracing::{error, info};

const CURVE_PATH_DEFAULT: &str = "/opt/aiolos/etc/nvidia.curve.json";

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

    // Every long-running mode restores firmware fan control on SIGTERM/SIGINT (self-sufficient
    // shutdown — never depends on the parent killing us). The handler only sets a flag; the run
    // loop performs the restore in normal code (NVML is not safe to call from a signal handler).
    protocol::install_shutdown_handlers();

    let mode = std::env::args().nth(1).unwrap_or_else(|| "detect".into());
    match mode.as_str() {
        "detect" => detect_loop(),
        "run" => run_loop(&std::env::args().nth(2).expect("run requires <ID>")),
        // Uniform one-shot fail-safe (same verb across all anemoi, called by `aiolos restore`).
        "restore" => restore_mode(),
        other => {
            eprintln!("unknown mode: {other}");
            std::process::exit(1);
        }
    }
}

/// One-shot fail-safe: restore every GPU's fans to firmware default and exit. Invoked by
/// `aiolos restore` (which systemd's ExecStopPost calls) so NVML manual fan control is never left
/// persisting if aiolos died without a graceful shutdown (hard crash / SIGKILL). Idempotent.
fn restore_mode() {
    if let Err(e) = nvml::restore_all() {
        eprintln!("restore FAILED: {e}");
        std::process::exit(2);
    }
}

// ---------------------------------------------------------------------------
// detect
// ---------------------------------------------------------------------------

fn detect_loop() {
    // One NVML handle held for the process lifetime (no per-cycle re-init -> no fd leak). On a
    // fault the Detector reports an explicit `status:error` (it never exits or returns a bogus
    // empty); the supervisor reacts to the declared error.
    let mut detector = nvml::Detector::new();
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
            Ok(Request::Detect) => {
                let d = detector.detect();
                if let Some(err) = &d.error {
                    error!(status=%d.status.as_str(), error=%err, "detect");
                }
                emit_line(d.to_line());
            }
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

fn run_loop(id: &str) {
    let mut curves = CurveCache::new(curve_path());
    if curves.curve().is_empty() {
        error!(path=%curves.path(), "curve missing/empty — GPU fans stay on firmware default until a valid curve exists");
    }

    let mut gpu = match Gpu::open(id) {
        Ok(g) => Some(g),
        Err(e) => {
            error!(uuid=%id, error=%e, "NVML open failed — instance degraded (fans stay firmware)");
            None
        }
    };

    let mut stdin = match StdinReader::new() {
        Ok(s) => s,
        Err(e) => {
            error!(error=%e, "stdin setup failed — restoring + exiting");
            restore(&mut gpu);
            return;
        }
    };
    loop {
        let line = match stdin.next_event(STEP) {
            Event::Line(l) => l,
            // SIGTERM/SIGINT or parent gone (EOF): restore firmware fan control, then exit.
            Event::Shutdown => {
                info!("termination signal — restoring firmware fan control and exiting");
                restore(&mut gpu);
                break;
            }
            Event::Eof => {
                restore(&mut gpu);
                break;
            }
        };
        match Request::from_line(line.trim()) {
            Ok(Request::Apply { .. }) => {
                // Re-read the curve every tick (live tuning; last-good on partial writes).
                if curves.reload() {
                    info!(path=%curves.path(), "curve reloaded");
                }
                let applied = match gpu.as_mut() {
                    Some(g) => match g.read_and_control(curves.curve(), curves.alpha()) {
                        Ok(readings) => Applied::ok(readings),
                        Err(e) => {
                            // A failed tick must never leave the GPU in manual-but-unregulated state
                            // (e.g. a temp-read failure after a prior manual set). Revert to firmware
                            // so the onboard controller keeps the GPU cool until we recover, and
                            // reset the damper so control re-seeds cleanly on recovery.
                            let _ = g.restore_fans();
                            g.reset_damper();
                            Applied::error(e.to_string())
                        }
                    },
                    // Couldn't open NVML for this GPU: declare fatal so the supervisor retries on a
                    // long backoff (re-running Gpu::open) instead of limping every tick.
                    None => Applied::fatal("NVML unavailable for this GPU"),
                };
                emit_line(applied.to_line());
            }
            Ok(Request::Shutdown) => {
                restore(&mut gpu);
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
    // `gpu` drops here -> Gpu::Drop restores firmware fan control as a final safety net.
}

fn restore(gpu: &mut Option<Gpu>) {
    if let Some(g) = gpu.as_mut() {
        match g.restore_fans() {
            Ok(()) => info!("GPU fans restored to firmware default"),
            Err(e) => eprintln!("WARNING: fan restore failed: {e}"),
        }
    }
}

fn curve_path() -> String {
    std::env::var("AIOLOS_ETC_DIR")
        .map(|d| format!("{d}/nvidia.curve.json"))
        .unwrap_or_else(|_| CURVE_PATH_DEFAULT.to_string())
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
