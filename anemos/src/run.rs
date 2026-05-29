//! The anemos lifecycle driver. A module's `main()` is just:
//! `anemos::run(anemos::ModuleInfo { .. }, MyAnemos::new())`.
//!
//! `run` parses argv (`detect` / `run <id>` / `restore` / optional extras), initialises logging,
//! installs SIGTERM/SIGINT handlers, and drives the matching loop over the one-line-JSON protocol.
//! Every `run`-mode exit path (shutdown request, stdin EOF, signal) restores the device.

use crate::stdio::{Event, StdinReader};
use crate::Controller;
use protocol::{Applied, Detected, Inputs, Request, Status};
use std::collections::HashMap;
use std::io::Write;
use std::time::Duration;
use tracing::{error, info};

/// Poll step for the signal-aware stdin reader: a termination signal is noticed within ~this long.
const STEP: Duration = Duration::from_millis(200);

/// The device-agnostic surface each anemos implements. The SDK owns the lifecycle and calls this;
/// faults MUST be declared explicitly (`Detected`/`Applied` `error`/`fatal`), never via exit/empty.
pub trait Anemos {
    /// Report the IDs this module currently manages (answers `detect`).
    fn detect(&mut self) -> Detected;
    /// Bind one detected id for `run <id>`. `Err` => the SDK declares `fatal` (supervisor retries on
    /// a long backoff); `Ok` => a live device the SDK will tick.
    fn open(&mut self, id: &str) -> anyhow::Result<Box<dyn Device>>;
    /// One-shot: restore EVERY device this module manages to firmware/auto-safe (for `aiolos restore`).
    fn restore_all(&mut self);
}

/// One bound device. The SDK ticks it and guarantees a restore on shutdown/EOF/signal.
pub trait Device {
    /// One tick: read sensors, compute a duty via `ctrl.duty(raw_temp)`, drive the device, return
    /// readings. Declare faults via `Applied::error`/`fatal`; on error, restore the device first.
    fn apply(&mut self, inputs: Option<&Inputs>, ctrl: &mut Controller) -> Applied;
    /// Fail-safe: restore this device to firmware/auto-safe (called on shutdown/EOF/signal).
    fn restore(&mut self);
}

/// Static identity + curve-config location for an anemos.
pub struct ModuleInfo {
    /// Module name, e.g. "nvidia" (logging only).
    pub name: &'static str,
    /// Absolute default curve path, e.g. "/opt/aiolos/etc/nvidia.curve.json".
    pub curve_default_path: &'static str,
    /// Curve filename under `$AIOLOS_ETC_DIR` when set (tests/dev), e.g. "nvidia.curve.json".
    pub curve_env_filename: &'static str,
}

/// An optional extra one-shot subcommand a module registers (e.g. asrock `query`); receives the
/// argv tail and returns a process exit code.
pub type ExtraCmd = Box<dyn FnOnce(&[String]) -> i32>;

/// Drive the whole lifecycle with no extra subcommands. Never returns.
pub fn run<A: Anemos>(info: ModuleInfo, anemos: A) -> ! {
    run_with(info, anemos, HashMap::new())
}

/// Drive the whole lifecycle, allowing extra one-shot subcommands. Never returns.
pub fn run_with<A: Anemos>(
    info: ModuleInfo,
    mut anemos: A,
    mut extra: HashMap<&'static str, ExtraCmd>,
) -> ! {
    init_logging();
    // Self-sufficient shutdown: the run loop restores the device on SIGTERM/SIGINT itself.
    crate::stdio::install_shutdown_handlers();

    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("detect");
    let code = match mode {
        "detect" => {
            detect_loop(&mut anemos);
            0
        }
        "run" => {
            let Some(id) = args.get(2) else {
                eprintln!("run requires <ID>");
                std::process::exit(1);
            };
            run_loop(&info, &mut anemos, id);
            0
        }
        "restore" => {
            anemos.restore_all();
            0
        }
        other => match extra.remove(other) {
            Some(cmd) => cmd(args.get(2..).unwrap_or(&[])),
            None => {
                eprintln!("unknown mode: {other}");
                1
            }
        },
    };
    std::process::exit(code);
}

fn detect_loop<A: Anemos>(anemos: &mut A) {
    // detect holds no device, so a signal/EOF just exits cleanly (nothing to restore).
    let mut stdin = match StdinReader::new() {
        Ok(s) => s,
        Err(e) => {
            error!(error=%e, "stdin setup failed");
            return;
        }
    };
    while let Event::Line(line) = stdin.next_event(STEP) {
        match Request::from_line(line.trim()) {
            Ok(Request::Detect) => {
                let d = anemos.detect();
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

fn run_loop<A: Anemos>(info: &ModuleInfo, anemos: &mut A, id: &str) {
    let mut ctrl = Controller::new(curve_path(info));
    if ctrl.curve_is_empty() {
        error!(module = info.name, path=%ctrl.path(), "curve missing/empty — device stays on firmware/auto until a valid curve exists");
    }

    let mut dev: Option<Box<dyn Device>> = match anemos.open(id) {
        Ok(d) => Some(d),
        Err(e) => {
            error!(module = info.name, id=%id, error=%e, "open failed — instance degraded (device stays on firmware/auto)");
            None
        }
    };

    let mut stdin = match StdinReader::new() {
        Ok(s) => s,
        Err(e) => {
            error!(error=%e, "stdin setup failed — restoring + exiting");
            if let Some(d) = dev.as_mut() {
                d.restore();
            }
            return;
        }
    };
    loop {
        let line = match stdin.next_event(STEP) {
            Event::Line(l) => l,
            // SIGTERM/SIGINT or parent gone (EOF): restore the device, then exit. Self-sufficient.
            Event::Shutdown => {
                info!(
                    module = info.name,
                    "termination signal — restoring device and exiting"
                );
                if let Some(d) = dev.as_mut() {
                    d.restore();
                }
                break;
            }
            Event::Eof => {
                if let Some(d) = dev.as_mut() {
                    d.restore();
                }
                break;
            }
        };
        match Request::from_line(line.trim()) {
            Ok(Request::Apply { inputs }) => {
                let applied = match dev.as_mut() {
                    Some(d) => {
                        let a = d.apply(inputs.as_ref(), &mut ctrl);
                        // A failed tick reverts to firmware (the Device does that); also reset the
                        // controller so EMA/deadband re-seed cleanly on recovery.
                        if a.status != Status::Ok {
                            ctrl.reset();
                        }
                        a
                    }
                    // Couldn't open the device: declare fatal so the supervisor retries on a long
                    // backoff (re-running open) instead of limping every tick.
                    None => Applied::fatal("device unavailable for this id"),
                };
                emit_line(applied.to_line());
            }
            Ok(Request::Shutdown) => {
                if let Some(d) = dev.as_mut() {
                    d.restore();
                }
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
    // `dev` drops here -> the concrete device type (or its fields, e.g. an NVML/IPMI handle with a
    // restoring Drop) is the final restore net on panic/early-exit. A `Device` impl SHOULD ensure
    // its underlying resource restores on drop (the shipped nvidia/asrock devices do, via the tech
    // handle's Drop).
}

fn curve_path(info: &ModuleInfo) -> String {
    std::env::var("AIOLOS_ETC_DIR")
        .map(|d| format!("{d}/{}", info.curve_env_filename))
        .unwrap_or_else(|_| info.curve_default_path.to_string())
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

fn init_logging() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
}
