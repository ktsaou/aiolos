//! nvidia anemos — per-GPU onboard fan control via NVML.
//!
//! detect → one id per GPU (by UUID). run <UUID> → each `apply`: read temp, apply the curve to the
//! GPU's onboard fans, report readings. `inputs` are ignored (nvidia uses its own GPU temperature).
//! Fail-safe: restore firmware/default fan control on shutdown/EOF (and via `Gpu::Drop` on panic).

mod nvml;

use crate::nvml::Gpu;
use protocol::{Applied, CurveCache, Found, Request, Response};
use std::io::{BufRead, Write};
use tracing::{error, info};

const CURVE_PATH_DEFAULT: &str = "/opt/aiolos/etc/nvidia.curve.json";

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
        other => {
            eprintln!("unknown mode: {other}");
            std::process::exit(1);
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
                found: nvml::enumerate(),
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

    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    let mut line = String::new();
    loop {
        line.clear();
        match lock.read_line(&mut line) {
            Ok(0) => {
                restore(&mut gpu);
                break;
            }
            Ok(_) => {}
            Err(e) => {
                error!(error=%e, "stdin read error");
                restore(&mut gpu);
                break;
            }
        }
        match Request::from_line(line.trim()) {
            Ok(Request::Apply { .. }) => {
                // Re-read the curve every tick (live tuning; last-good on partial writes).
                if curves.reload() {
                    info!(path=%curves.path(), "curve reloaded");
                }
                let applied = match gpu.as_mut() {
                    Some(g) => match g.read_and_control(curves.curve()) {
                        Ok(readings) => Applied::ok(readings),
                        Err(e) => Applied::error(e.to_string()),
                    },
                    None => Applied::error("NVML unavailable"),
                };
                emit(Response::Applied(applied));
            }
            Ok(Request::Shutdown) => {
                restore(&mut gpu);
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

fn emit(resp: Response) {
    let line = resp.to_line().unwrap_or_else(|_| {
        r#"{"status":"error","error":"internal serialization error"}"#.to_string()
    });
    let mut out = std::io::stdout();
    let _ = out.write_all(line.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}
