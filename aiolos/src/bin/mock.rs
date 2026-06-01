//! Test-only mock anemos — a helper binary used by the orchestrator integration tests. It is
//! never installed (packaging copies only aiolos + the real anemoi).
//!
//! Behaviour is driven by env vars namespaced by the MODULE NAME (argv[0]'s file name), so one
//! binary plays several roles via differently-named symlinks in the test bin dir:
//!   MOCK_<MOD>_IDS        comma list of detect ids (default "thing0")
//!   MOCK_<MOD>_IDS2       ids to switch to after SWITCH_MS (tests hotplug add/remove)
//!   MOCK_<MOD>_SWITCH_MS  ms after start to switch IDS -> IDS2
//!   MOCK_<MOD>_BEHAVIOR   ok | slow | hang | partial | error | exit   (run mode, default ok)
//!   MOCK_<MOD>_TEMP       °C this module reports (default 50)
//!   MOCK_<MOD>_SLOW_MS    apply duration for the `slow` behavior, ms (default 800)
//!   MOCK_<MOD>_WORKDIR    dir for observable side-effect marker files

use anemos::{Event, StdinReader};
use protocol::{Component, Inputs, Report, Request, Signal, Unit};
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn main() {
    anemos::install_shutdown_handlers();
    let module = module_name();
    let mode = std::env::args().nth(1).unwrap_or_else(|| "detect".into());
    match mode.as_str() {
        "detect" => detect_loop(&module),
        "info" | "collect" => info_once(&module, std::env::args().nth(2)),
        "run" => run_loop(
            &module,
            &std::env::args().nth(2).expect("run requires <ID>"),
        ),
        "restore" => {
            if let Some(d) = envk(&module, "WORKDIR") {
                let _ = std::fs::write(
                    Path::new(&d).join(format!("{module}.restored_oneshot")),
                    "x",
                );
            }
        }
        other => {
            eprintln!("mock: unknown mode {other}");
            std::process::exit(1);
        }
    }
}

/// One mock unit's entities (a `self` component + a temperature signal; optionally a `from_input`
/// component echoing the max routed temp). `temp = None` => schema only (no value).
fn mock_unit(
    id: &str,
    temp: Option<i64>,
    in_max: Option<i64>,
) -> (Unit, Vec<Component>, Vec<Signal>) {
    let comp = format!("{id}:self");
    let mut components = vec![Component::new(&comp, id).name("self").typed("mock")];
    let mut t = Signal::producer(format!("{comp}:temp"), &comp, "temperature")
        .uom("C")
        .name("temp");
    if let Some(v) = temp {
        t = t.value(json!(v));
    }
    let mut signals = vec![t];
    if let Some(m) = in_max {
        let fc = format!("{id}:from_input");
        components.push(Component::new(&fc, id).name("from_input").typed("mock"));
        signals.push(
            Signal::producer(format!("{fc}:temp"), &fc, "temperature")
                .value(json!(m))
                .uom("C")
                .name("from_input"),
        );
    }
    (
        Unit::new(id).name(format!("mock {id}")).typed("MOCK"),
        components,
        signals,
    )
}

fn info_once(module: &str, wanted: Option<String>) {
    let ids = envk(module, "IDS").unwrap_or_else(|| "thing0".into());
    let temp: i64 = envk(module, "TEMP")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    let (mut units, mut components, mut signals) = (Vec::new(), Vec::new(), Vec::new());
    let mut any = false;
    for id in ids.split(',').filter(|s| !s.is_empty()) {
        if wanted.as_deref().is_some_and(|w| w != id) {
            continue;
        }
        any = true;
        let (u, mut c, mut s) = mock_unit(id, Some(temp), None);
        units.push(u);
        components.append(&mut c);
        signals.append(&mut s);
    }
    if wanted.is_some() && !any {
        emit_line(Report::fatal("mock id not found").to_line());
    } else {
        emit_line(Report::ok(units, components, signals).to_line());
    }
}

fn detect_report(ids: &str) -> Report {
    let (mut units, mut components, mut signals) = (Vec::new(), Vec::new(), Vec::new());
    for id in ids.split(',').filter(|s| !s.is_empty()) {
        let (u, mut c, mut s) = mock_unit(id, None, None);
        units.push(u);
        components.append(&mut c);
        signals.append(&mut s);
    }
    Report::ok(units, components, signals)
}

fn detect_loop(module: &str) {
    let start = Instant::now();
    let ids1 = envk(module, "IDS").unwrap_or_else(|| "thing0".into());
    let ids2 = envk(module, "IDS2");
    let switch_ms: u64 = envk(module, "SWITCH_MS")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let after = envk(module, "AFTER").unwrap_or_else(|| "ok".into());

    let mut stdin = match StdinReader::new() {
        Ok(s) => s,
        Err(_) => return,
    };
    while let Event::Line(line) = stdin.next_event(Duration::from_millis(100)) {
        match Request::from_line(line.trim()) {
            Ok(Request::Detect) => {
                let switched = switch_ms > 0 && start.elapsed() >= Duration::from_millis(switch_ms);
                let d = if switched && after == "error" {
                    Report::error("mock detect error")
                } else if switched && after == "fatal" {
                    Report::fatal("mock detect fatal")
                } else {
                    let ids = match (&ids2, switched) {
                        (Some(i2), true) => i2.clone(),
                        _ => ids1.clone(),
                    };
                    detect_report(&ids)
                };
                emit_line(d.to_line());
            }
            Ok(Request::Shutdown) => {
                emit_line(Report::ok_empty().to_line());
                break;
            }
            _ => eprintln!("mock detect: unexpected request"),
        }
    }
}

fn run_loop(module: &str, id: &str) {
    append_marker(module, id, "starts");
    let behavior = envk(module, "BEHAVIOR").unwrap_or_else(|| "ok".into());
    let temp: i64 = envk(module, "TEMP")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);

    let mut stdin = match StdinReader::new() {
        Ok(s) => s,
        Err(_) => {
            restore(module, id);
            return;
        }
    };
    let ok_report = |in_max: Option<i64>| {
        let (u, c, s) = mock_unit(id, Some(temp), in_max);
        Report::ok(vec![u], c, s)
    };
    loop {
        let line = match stdin.next_event(Duration::from_millis(100)) {
            Event::Line(l) => l,
            Event::Shutdown => {
                append_marker(module, id, "signaled");
                restore(module, id);
                break;
            }
            Event::Eof => {
                restore(module, id);
                break;
            }
        };
        match Request::from_line(line.trim()) {
            Ok(Request::Apply { inputs }) => {
                append_marker(module, id, "applies");
                let in_max = max_input_temp(inputs.as_ref());
                write_marker(module, id, "lastinput", &in_max.unwrap_or(-1).to_string());

                match behavior.as_str() {
                    "slow" => {
                        let slow_ms: u64 = envk(module, "SLOW_MS")
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(800);
                        std::thread::sleep(Duration::from_millis(slow_ms));
                        emit_line(ok_report(in_max).to_line());
                    }
                    "hang" => loop {
                        std::thread::sleep(Duration::from_secs(60));
                    },
                    "partial" => {
                        let mut out = std::io::stdout();
                        let _ = out.write_all(br#"{"status":"#);
                        let _ = out.flush();
                        loop {
                            std::thread::sleep(Duration::from_secs(60));
                        }
                    }
                    "error" => emit_line(Report::error("mock error").to_line()),
                    "fatal" => emit_line(Report::fatal("mock fatal").to_line()),
                    "exit" => std::process::exit(0),
                    _ => emit_line(ok_report(in_max).to_line()),
                }
            }
            Ok(Request::Shutdown) => {
                restore(module, id);
                emit_line(Report::ok_empty().to_line());
                break;
            }
            Ok(Request::Detect) => eprintln!("mock run: unexpected detect"),
            Err(e) => emit_line(Report::error(format!("malformed: {e}")).to_line()),
        }
    }
}

fn max_input_temp(inputs: Option<&Inputs>) -> Option<i64> {
    inputs?
        .values()
        .flatten()
        .filter(|s| s.kind() == Some("temperature"))
        .filter_map(Signal::value_i64)
        .max()
}

fn restore(module: &str, id: &str) {
    append_marker(module, id, "restored");
}

// ---- env + markers ---------------------------------------------------------

fn module_name() -> String {
    std::env::args()
        .next()
        .and_then(|a| {
            PathBuf::from(a)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "mock".into())
}

fn envk(module: &str, key: &str) -> Option<String> {
    let norm: String = module
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    std::env::var(format!("MOCK_{norm}_{key}")).ok()
}

fn marker_path(module: &str, id: &str, suffix: &str) -> Option<PathBuf> {
    envk(module, "WORKDIR").map(|d| Path::new(&d).join(format!("{module}-{id}.{suffix}")))
}

fn append_marker(module: &str, id: &str, suffix: &str) {
    if let Some(p) = marker_path(module, id, suffix) {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
        {
            let _ = f.write_all(b"x");
        }
    }
}

fn write_marker(module: &str, id: &str, suffix: &str, content: &str) {
    if let Some(p) = marker_path(module, id, suffix) {
        let _ = std::fs::write(p, content);
    }
}

fn emit_line(line: serde_json::Result<String>) {
    let line =
        line.unwrap_or_else(|_| r#"{"status":"error","error":"mock serialize"}"#.to_string());
    let mut out = std::io::stdout();
    let _ = out.write_all(line.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}
