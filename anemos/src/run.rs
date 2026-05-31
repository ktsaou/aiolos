//! The anemos lifecycle driver. A module's `main()` is just:
//! `anemos::run(anemos::ModuleInfo { .. }, MyAnemos::new())`.
//!
//! `run` parses argv (`detect` / `info` / `collect` / `run <id>` / `restore` / optional extras),
//! initialises logging, installs SIGTERM/SIGINT handlers, and drives the matching loop over the
//! one-line-JSON protocol. Every `run`-mode exit path (shutdown request, stdin EOF, signal) restores
//! the device; `info`/`collect` opens in read-only observe mode and never restores on drop.

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
    /// Bind one detected id. `mode` is `Control` for normal `run <id>` and `Observe` for read-only
    /// collection (the `info` one-shot); observe mode MUST NOT claim/set/release hardware.
    /// `Err` => the SDK declares `fatal` (supervisor retries on a long backoff in run mode);
    /// `Ok` => a live device handle.
    fn open(&mut self, id: &str, mode: OpenMode) -> anyhow::Result<Box<dyn Device>>;
    /// One-shot: restore EVERY device this module manages to firmware/auto-safe (for `aiolos restore`).
    fn restore_all(&mut self);
}

/// How an anemos opens a device handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    /// Read-only observation. No claim/set/release and no restore-on-drop side effects.
    Observe,
    /// Normal controller operation. The device may be claimed/set and must restore on exit.
    Control,
}

/// One bound device. The SDK ticks it and guarantees a restore on shutdown/EOF/signal.
pub trait Device {
    /// Read-only snapshot: collect sensors/readbacks and return live components without changing the
    /// device state. This backs `<module> info` and can also be reused by sensor-only `apply`.
    fn collect(&mut self, inputs: Option<&Inputs>) -> Applied;

    /// One tick: read sensors, compute a duty via `ctrl.duty(raw_temp)`, drive the device, return
    /// components. Declare faults via `Applied::error`/`fatal`; on error, restore the device first.
    fn apply(&mut self, inputs: Option<&Inputs>, _ctrl: &mut Controller) -> Applied {
        self.collect(inputs)
    }

    /// Fail-safe: restore this device to firmware/auto-safe (called on shutdown/EOF/signal).
    fn restore(&mut self);
}

/// Static identity + curve-config location for an anemos.
pub struct ModuleInfo {
    /// Module name, e.g. "nvidia" (logging only).
    pub name: &'static str,
    /// Absolute default curve path, e.g. "/opt/aiolos/etc/nvidia.curve.json". `None` marks a
    /// **sensor-only** module: it controls no device, needs no curve, and its `apply` ignores the
    /// controller (e.g. `nvme`, which only reports temperatures for routing).
    pub curve_default_path: Option<&'static str>,
    /// Curve filename under `$AIOLOS_ETC_DIR` when set (tests/dev), e.g. "nvidia.curve.json".
    /// `None` for a sensor-only module.
    pub curve_env_filename: Option<&'static str>,
}

/// An optional extra one-shot subcommand a module registers (e.g. rome2d-fans `query`); receives the
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
            if stdin_is_tty() {
                emit_detected(&anemos.detect());
            } else {
                detect_loop(&mut anemos);
            }
            0
        }
        "info" => {
            let id = args.get(2).map(String::as_str);
            info_once(&mut anemos, id)
        }
        "collect" => {
            let id = args.get(2).map(String::as_str);
            info_once(&mut anemos, id)
        }
        "schema" => {
            emit_detected(&anemos.detect());
            0
        }
        "run" => {
            let Some(id) = args.get(2) else {
                eprintln!("run requires <ID>");
                std::process::exit(1);
            };
            run_loop(&info, &mut anemos, id)
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
                emit_detected(&d);
            }
            Ok(Request::Shutdown) => {
                emit_applied(&Applied::ok_empty());
                break;
            }
            Ok(Request::Apply { .. }) => eprintln!("unexpected apply in detect mode"),
            Err(e) => eprintln!("malformed request: {e}"),
        }
    }
}

/// Drive one `run <id>` instance. Returns the process exit code: `0` on a clean stop, non-zero when
/// a control module could not start because its curve was invalid (SOW-0012 decision 2).
fn run_loop<A: Anemos>(info: &ModuleInfo, anemos: &mut A, id: &str) -> i32 {
    // A sensor-only module (no curve configured) controls no device and ignores the controller in
    // `apply`. A CONTROL module (curve configured) MUST have a usable curve to start: if the initial
    // load failed (missing / invalid JSON / no usable points) it must NOT regulate — it surfaces a
    // protocol `fatal` (so the reason shows on the status page) and exits non-zero, leaving the
    // device under firmware/auto control (SOW-0012). aiolos respawns it with capped exponential
    // backoff. The controller is constructed regardless (cheap) so the `Device::apply(ctrl)`
    // contract is uniform.
    let curve = curve_path(info);
    let curve_configured = curve.is_some();
    let mut ctrl = Controller::new(curve.unwrap_or_default());
    if curve_configured {
        if let Some(reason) = ctrl.initial_curve_error() {
            error!(module = info.name, path=%ctrl.path(), reason = reason.as_str(), "invalid curve at startup — refusing to regulate; device stays on firmware/auto");
            // Never open the device. Surface the reason as a protocol `fatal` and exit non-zero.
            return startup_curve_fatal_loop(info, &format!("curve invalid: {}", reason.as_str()));
        }
    }

    let mut dev: Option<Box<dyn Device>> = match anemos.open(id, OpenMode::Control) {
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
            return 0; // device restored; this is a clean fail-safe exit, not a startup failure
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
                emit_applied(&applied);
            }
            Ok(Request::Shutdown) => {
                if let Some(d) = dev.as_mut() {
                    d.restore();
                }
                emit_applied(&Applied::ok_empty());
                break;
            }
            Ok(Request::Detect) => eprintln!("unexpected detect in run mode"),
            Err(e) => {
                eprintln!("malformed request: {e}");
                emit_applied(&Applied::error(format!("malformed: {e}")));
            }
        }
    }
    // `dev` drops here -> the concrete device type (or its fields, e.g. an NVML/IPMI handle with a
    // restoring Drop) is the final restore net on panic/early-exit. A `Device` impl SHOULD ensure
    // its underlying resource restores on drop (the shipped nvidia/rome2d-fans devices do, via the tech
    // handle's Drop).
    0
}

/// A control module whose curve was invalid at startup: it never opened the device (so there is
/// nothing to restore — the device stays on firmware/auto). It keeps the protocol half-duplex
/// contract — answering each `apply` with a structured `fatal` so the reason reaches the status page
/// — then exits non-zero so aiolos respawns it on the capped exponential backoff. A graceful
/// `shutdown` ends it cleanly (exit 0); EOF/signal (parent gone) ends it with the same non-zero code
/// (it never started regulating). Returns the process exit code.
fn startup_curve_fatal_loop(info: &ModuleInfo, reason: &str) -> i32 {
    let mut stdin = match StdinReader::new() {
        Ok(s) => s,
        // Can't even read stdin: nothing to restore (device never opened); exit non-zero.
        Err(e) => {
            error!(module = info.name, error=%e, "stdin setup failed");
            return 1;
        }
    };
    loop {
        match stdin.next_event(STEP) {
            Event::Line(line) => match Request::from_line(line.trim()) {
                // First (and any) apply: declare fatal with the reason, then exit non-zero. The
                // supervisor records the declared-fatal and respawns on the long (capped) backoff.
                Ok(Request::Apply { .. }) => {
                    emit_applied(&Applied::fatal(format!("startup: {reason}")));
                    return 1;
                }
                // aiolos asked us to stop: nothing was regulated, so this is a clean stop.
                Ok(Request::Shutdown) => {
                    emit_applied(&Applied::ok_empty());
                    return 0;
                }
                Ok(Request::Detect) => eprintln!("unexpected detect in run mode"),
                Err(e) => eprintln!("malformed request: {e}"),
            },
            // Parent gone or signalled before any apply: never regulated; nothing to restore.
            Event::Shutdown | Event::Eof => return 1,
        }
    }
}

/// Resolve the curve path, or `None` for a sensor-only module (no curve configured). The env
/// override (`$AIOLOS_ETC_DIR/<filename>`) applies only when both the dir and a filename exist.
fn curve_path(info: &ModuleInfo) -> Option<String> {
    let default = info.curve_default_path?;
    let resolved = match (std::env::var("AIOLOS_ETC_DIR"), info.curve_env_filename) {
        (Ok(dir), Some(filename)) => format!("{dir}/{filename}"),
        _ => default.to_string(),
    };
    Some(resolved)
}

fn info_once<A: Anemos>(anemos: &mut A, id: Option<&str>) -> i32 {
    let mut detected = anemos.detect();
    if detected.status != Status::Ok {
        emit_detected(&detected);
        return 1;
    }

    if let Some(wanted) = id {
        detected.found.retain(|f| f.id == wanted);
        if detected.found.is_empty() {
            emit_detected(&Detected::fatal(format!("id not found: {wanted}")));
            return 1;
        }
    }

    let mut warnings = Vec::new();
    for found in &mut detected.found {
        match anemos.open(&found.id, OpenMode::Observe) {
            Ok(mut dev) => {
                let applied = dev.collect(None);
                if applied.status == Status::Ok {
                    if let Some(components) = applied.components {
                        found.components = components;
                    }
                    if let Some(err) = applied.error {
                        warnings.push(format!("{}: {err}", found.id));
                    }
                } else {
                    warnings.push(format!(
                        "{}: {}",
                        found.id,
                        applied
                            .error
                            .unwrap_or_else(|| format!("collect {}", applied.status.as_str()))
                    ));
                }
            }
            Err(e) => warnings.push(format!("{}: open for collect: {e}", found.id)),
        }
    }

    if !warnings.is_empty() {
        detected.error = Some(warnings.join("; "));
    }
    emit_detected(&detected);
    if warnings.is_empty() {
        0
    } else {
        1
    }
}

fn emit_detected(detected: &Detected) {
    emit_serialized(detected.to_line());
}

fn emit_applied(applied: &Applied) {
    emit_serialized(applied.to_line());
}

fn emit_serialized(line: serde_json::Result<String>) {
    let line = line.unwrap_or_else(|_| {
        r#"{"status":"error","error":"internal serialization error"}"#.to_string()
    });
    let mut out = std::io::stdout();
    let _ = out.write_all(line.as_bytes());
    if !line.ends_with('\n') {
        let _ = out.write_all(b"\n");
    }
    let _ = out.flush();
}

fn stdin_is_tty() -> bool {
    // SAFETY: `isatty` reads only the process stdin fd and has no memory safety preconditions.
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

fn init_logging() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        // An anemos's stderr is always captured by aiolos (status page + journal), never a user
        // terminal — emit plain text so no ANSI colour escapes (`\x1b[2m` …) leak into the captured
        // tail. (tracing's default auto-detect can still enable them here; force them off.)
        .with_ansi(false)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_path_is_none_for_a_sensor_only_module() {
        let info = ModuleInfo {
            name: "nvme",
            curve_default_path: None,
            curve_env_filename: None,
        };
        assert_eq!(
            curve_path(&info),
            None,
            "a sensor-only module (curve = None) has no curve path"
        );
    }

    #[test]
    fn curve_path_resolves_for_a_curved_module() {
        let info = ModuleInfo {
            name: "nvidia",
            curve_default_path: Some("/opt/aiolos/etc/nvidia.curve.json"),
            curve_env_filename: Some("nvidia.curve.json"),
        };
        // Resolves to the default path, or `$AIOLOS_ETC_DIR/<filename>` if that env is set — either
        // way it ends with the curve filename.
        let p = curve_path(&info).expect("a curved module resolves a path");
        assert!(p.ends_with("nvidia.curve.json"), "got {p}");
    }

    #[test]
    fn a_control_module_with_an_invalid_curve_detects_a_startup_error() {
        // SOW-0012 decision 2: a control module (curve configured) whose curve cannot load must NOT
        // regulate. run_loop gates on `curve_configured && ctrl.initial_curve_error().is_some()`;
        // verify both halves: the module IS curve-configured, and an unreadable curve is flagged.
        let info = ModuleInfo {
            name: "nvidia",
            curve_default_path: Some("/nonexistent/aiolos-startup-fatal.curve.json"),
            curve_env_filename: None,
        };
        let path = curve_path(&info).expect("control module has a curve path");
        let ctrl = Controller::new(path);
        assert!(
            ctrl.initial_curve_error().is_some(),
            "an invalid startup curve must be flagged so run_loop fails to start"
        );
    }

    #[test]
    fn a_sensor_only_module_is_exempt_from_the_startup_curve_check() {
        // A sensor-only module (curve = None) has no curve path, so `curve_configured` is false and
        // run_loop never enters the startup-fatal branch — it regulates nothing and is unaffected.
        let info = ModuleInfo {
            name: "nvme",
            curve_default_path: None,
            curve_env_filename: None,
        };
        assert!(
            curve_path(&info).is_none(),
            "a sensor-only module is never subject to the startup curve check"
        );
    }

    #[test]
    fn startup_fatal_applied_line_is_well_formed_protocol() {
        // The line a startup-invalid control module emits on `apply` must be valid protocol JSON
        // with status fatal and the curve reason (so it surfaces on the status page).
        let line = Applied::fatal("startup: curve invalid: file missing or unreadable")
            .to_line()
            .unwrap();
        let parsed = Applied::from_line(&line).unwrap();
        assert_eq!(parsed.status, Status::Fatal);
        assert!(parsed.error.as_deref().unwrap().contains("curve"));
        // stdout protocol-only: exactly one JSON object, no embedded newline.
        assert!(!line.contains('\n'));
    }
}
