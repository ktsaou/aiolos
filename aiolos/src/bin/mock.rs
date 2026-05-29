//! Test-only mock anemos — a helper binary used by the orchestrator integration tests. It is
//! never installed (packaging copies only aiolos/nvidia/asrock16-2t).
//!
//! Behaviour is driven by env vars namespaced by the MODULE NAME (argv[0]'s file name), so one
//! binary plays several roles via differently-named symlinks in the test bin dir:
//!   MOCK_<MOD>_IDS        comma list of detect ids (default "thing0")
//!   MOCK_<MOD>_IDS2       ids to switch to after SWITCH_MS (tests hotplug add/remove)
//!   MOCK_<MOD>_SWITCH_MS  ms after start to switch IDS -> IDS2
//!   MOCK_<MOD>_BEHAVIOR   ok | hang | partial | error | exit   (run mode, default ok)
//!   MOCK_<MOD>_TEMP       °C this module reports (default 50)
//!   MOCK_<MOD>_WORKDIR    dir for observable side-effect marker files
//!
//! Side-effect markers (under WORKDIR), used by tests instead of HTTP (no port binding):
//!   <mod>-<id>.starts     appended on each run-process startup (respawn count)
//!   <mod>-<id>.applies    appended on each apply (tick count)
//!   <mod>-<id>.restored   created on graceful shutdown OR stdin EOF (fail-safe path ran)
//!   <mod>-<id>.lastinput  overwritten each apply with the max routed input temp (or -1)

use protocol::{Applied, Found, FoundEntry, Inputs, Reading, Request, Response};
use serde_json::json;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn main() {
    let module = module_name();
    let mode = std::env::args().nth(1).unwrap_or_else(|| "detect".into());
    match mode.as_str() {
        "detect" => detect_loop(&module),
        "run" => run_loop(
            &module,
            &std::env::args().nth(2).expect("run requires <ID>"),
        ),
        other => {
            eprintln!("mock: unknown mode {other}");
            std::process::exit(1);
        }
    }
}

fn detect_loop(module: &str) {
    let start = Instant::now();
    let ids1 = envk(module, "IDS").unwrap_or_else(|| "thing0".into());
    let ids2 = envk(module, "IDS2");
    let switch_ms: u64 = envk(module, "SWITCH_MS")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    let mut line = String::new();
    loop {
        line.clear();
        match lock.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        match Request::from_line(line.trim()) {
            Ok(Request::Detect) => {
                let ids = match (&ids2, switch_ms) {
                    (Some(i2), ms) if ms > 0 && start.elapsed() >= Duration::from_millis(ms) => {
                        i2.clone()
                    }
                    _ => ids1.clone(),
                };
                let found = ids
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(|id| FoundEntry {
                        id: id.to_string(),
                        kind: "MOCK".into(),
                        name: format!("mock {id}"),
                        extra: Default::default(),
                    })
                    .collect();
                emit(Response::Found(Found { found }));
            }
            Ok(Request::Shutdown) => {
                emit(Response::Applied(Applied::ok_empty()));
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

    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    let mut line = String::new();
    loop {
        line.clear();
        match lock.read_line(&mut line) {
            Ok(0) => {
                restore(module, id);
                break;
            }
            Ok(_) => {}
            Err(_) => {
                restore(module, id);
                break;
            }
        }
        match Request::from_line(line.trim()) {
            Ok(Request::Apply { inputs }) => {
                append_marker(module, id, "applies");
                let in_max = max_input_temp(inputs.as_ref());
                write_marker(module, id, "lastinput", &in_max.unwrap_or(-1).to_string());

                match behavior.as_str() {
                    "hang" => loop {
                        std::thread::sleep(Duration::from_secs(60));
                    },
                    "partial" => {
                        // Write a partial line (NO newline) then hang — exercises the orchestrator's
                        // deadline-bounded read (must kill within ~timeout, never block).
                        let mut out = std::io::stdout();
                        let _ = out.write_all(br#"{"status":"#);
                        let _ = out.flush();
                        loop {
                            std::thread::sleep(Duration::from_secs(60));
                        }
                    }
                    "error" => emit(Response::Applied(Applied::error("mock error"))),
                    "exit" => std::process::exit(0),
                    _ => {
                        let mut readings =
                            vec![Reading::new("temp", "self", json!({ "temp": temp }))];
                        if let Some(m) = in_max {
                            readings.push(Reading::new(
                                "temp",
                                "from_input",
                                json!({ "temp": m, "in_temp": m }),
                            ));
                        }
                        emit(Response::Applied(Applied::ok(readings)));
                    }
                }
            }
            Ok(Request::Shutdown) => {
                restore(module, id);
                emit(Response::Applied(Applied::ok_empty()));
                break;
            }
            Ok(Request::Detect) => eprintln!("mock run: unexpected detect"),
            Err(e) => emit(Response::Applied(Applied::error(format!("malformed: {e}")))),
        }
    }
}

fn max_input_temp(inputs: Option<&Inputs>) -> Option<i64> {
    inputs?
        .values()
        .flatten()
        .filter(|r| r.kind == "temp")
        .filter_map(|r| r.get_i64("temp"))
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

fn emit(resp: Response) {
    let line = resp
        .to_line()
        .unwrap_or_else(|_| r#"{"status":"error","error":"mock serialize"}"#.to_string());
    let mut out = std::io::stdout();
    let _ = out.write_all(line.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}
