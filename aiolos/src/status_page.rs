//! Read-only HTTP status server for aiolos. Hand-rolled, dependency-light, no async runtime.
//!
//! Routes (all read-only; the server NEVER mutates orchestrator state):
//!   `GET /`             -> the themed single-page dashboard shell (HTML)
//!   `GET /aiolos.css`   -> embedded stylesheet
//!   `GET /aiolos.js`    -> embedded vanilla-JS app (tabs, charts, animated winds)
//!   `GET /status.json`  -> live snapshot (modules + instances + readings)
//!   `GET /history.json` -> bounded in-process time-series ring buffer
//!   `GET /curve.json?module=<m>` -> a module's temp->duty curve (read from its etc config)
//!   `GET /metrics`      -> Prometheus text-format exposition (SOW-0007)
//!   everything else     -> 404
//!
//! All HTML/CSS/JS/SVG ships embedded as `&str` consts compiled into the binary — no frameworks, no
//! external CDNs, no network requests. The dashboard polls `/status.json` + `/history.json`.
//!
//! Time-series: a bounded ring buffer lives entirely inside this module (no `AppState`/`main.rs`
//! change). A background snapshotter spawned from `serve()` reads the shared state read-only every
//! few seconds and appends a compact snapshot.

use crate::AppState;
use anyhow::Result;
use protocol::Reading;
use serde::Serialize;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::info;

/// Embedded front-end assets (compiled into the binary; no external network dependency).
const INDEX_HTML: &str = include_str!("assets/index.html");
const APP_CSS: &str = include_str!("assets/aiolos.css");
const APP_JS: &str = include_str!("assets/aiolos.js");

/// History ring-buffer sizing: one snapshot per `HISTORY_INTERVAL`, capped at `HISTORY_CAP`.
const HISTORY_CAP: usize = 720; // ~1h at a 5s cadence
const HISTORY_INTERVAL: Duration = Duration::from_secs(5);

pub fn serve(bind: &str, state: Arc<RwLock<AppState>>) -> Result<()> {
    let listener = TcpListener::bind(bind)?;
    info!(bind = %bind, "status page listening");

    // Bounded in-process time-series, owned by this module (read-only on AppState).
    let history: Arc<Mutex<History>> = Arc::new(Mutex::new(History::new(HISTORY_CAP)));
    spawn_snapshotter(Arc::clone(&state), Arc::clone(&history));

    for stream in listener.incoming() {
        match stream {
            Ok(conn) => {
                let state = Arc::clone(&state);
                let history = Arc::clone(&history);
                thread::spawn(move || {
                    let _ = handle(conn, &state, &history);
                });
            }
            Err(e) => tracing::warn!(error = %e, "status accept error"),
        }
    }
    Ok(())
}

fn handle(
    mut conn: TcpStream,
    state: &Arc<RwLock<AppState>>,
    history: &Arc<Mutex<History>>,
) -> Result<()> {
    conn.set_read_timeout(Some(Duration::from_secs(5)))?;
    // Generous write timeout: a large embedded asset (the ~27 KB JS) to a slow/remote browser must not
    // trip a short deadline mid-body (that would drop the connection and reset the resource).
    conn.set_write_timeout(Some(Duration::from_secs(30)))?;

    // Read the FULL request (headers up to the blank line), not just the first chunk. We only act on
    // the request line, but closing while unread request bytes remain in the socket makes the kernel
    // send RST instead of FIN — which a browser reports as ERR_CONNECTION_RESET on a sub-resource
    // (curl tolerates it). A GET has no body, so the blank line ends it; cap the read as a flood guard.
    let mut raw: Vec<u8> = Vec::with_capacity(2048);
    let mut buf = [0u8; 2048];
    loop {
        let n = conn.read(&mut buf)?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
        if raw.windows(4).any(|w| w == b"\r\n\r\n") || raw.len() > 32 * 1024 {
            break;
        }
    }
    if raw.is_empty() {
        return Ok(());
    }
    let req = String::from_utf8_lossy(&raw);
    let target = req
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/");
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };

    let (status, ctype, body) = match path {
        "/" => ("200 OK", "text/html; charset=utf-8", INDEX_HTML.to_string()),
        "/aiolos.css" => ("200 OK", "text/css; charset=utf-8", APP_CSS.to_string()),
        "/aiolos.js" => (
            "200 OK",
            "application/javascript; charset=utf-8",
            APP_JS.to_string(),
        ),
        "/status" | "/status.json" => ("200 OK", "application/json", render_json(state)),
        "/history" | "/history.json" => {
            ("200 OK", "application/json", render_history_json(history))
        }
        "/curve" | "/curve.json" => (
            "200 OK",
            "application/json",
            render_curve_json(module_param(query)),
        ),
        "/metrics" => (
            "200 OK",
            "text/plain; version=0.0.4; charset=utf-8",
            render_metrics(state),
        ),
        _ => (
            "404 Not Found",
            "text/plain; charset=utf-8",
            "Not Found".to_string(),
        ),
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
        body.len()
    );
    conn.write_all(response.as_bytes())?;
    Ok(())
}

/// Extract `module=<value>` from a raw query string (minimal, percent-decoding the value).
fn module_param(query: &str) -> Option<String> {
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix("module="))
        .map(percent_decode)
}

/// Minimal percent-decoding for the single query parameter we accept (module names are plain).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = |c: u8| match c {
                    b'0'..=b'9' => Some(c - b'0'),
                    b'a'..=b'f' => Some(c - b'a' + 10),
                    b'A'..=b'F' => Some(c - b'A' + 10),
                    _ => None,
                };
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push(h << 4 | l);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------
// Live JSON snapshot (/status.json)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct StatusJson<'a> {
    tick: u64,
    modules: Vec<ModuleJson<'a>>,
    instances: Vec<InstanceJson<'a>>,
}

#[derive(Serialize)]
struct ModuleJson<'a> {
    module: &'a str,
    detect_status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detect_error: Option<&'a str>,
}

#[derive(Serialize)]
struct InstanceJson<'a> {
    module: &'a str,
    id: &'a str,
    name: &'a str,
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
    restart_count: u32,
    seconds_since_seen: u64,
    readings: &'a [Reading],
    stderr_tail: Vec<String>,
}

fn render_json(state: &Arc<RwLock<AppState>>) -> String {
    let s = match state.read() {
        Ok(s) => s,
        Err(_) => return r#"{"error":"state lock poisoned"}"#.to_string(),
    };
    let tick = s.tick_count;
    let mut modules: Vec<ModuleJson> = s
        .modules
        .iter()
        .map(|(name, h)| ModuleJson {
            module: name,
            detect_status: &h.detect_status,
            detect_error: h.detect_error.as_deref(),
        })
        .collect();
    modules.sort_by(|a, b| a.module.cmp(b.module));
    let mut instances: Vec<InstanceJson> = s
        .instances
        .values()
        .map(|i| InstanceJson {
            module: &i.module_name,
            id: &i.id,
            name: &i.name,
            status: &i.last_status,
            error: i.last_error.as_deref(),
            restart_count: i.restart_count,
            seconds_since_seen: i.last_seen.elapsed().as_secs(),
            // Defensive ANSI strip: stale tails captured before the SDK's `.with_ansi(false)` fix
            // could still carry escape codes; never let them reach the UI.
            readings: &i.last_readings,
            stderr_tail: tail_lines(i, 12).iter().map(|l| strip_ansi(l)).collect(),
        })
        .collect();
    instances.sort_by(|a, b| (a.module, a.id).cmp(&(b.module, b.id)));

    serde_json::to_string(&StatusJson {
        tick,
        modules,
        instances,
    })
    .unwrap_or_else(|_| "{}".to_string())
}

// ---------------------------------------------------------------------------
// Time-series ring buffer (/history.json)
// ---------------------------------------------------------------------------

/// One per-instance sample inside a history snapshot: the numeric series we chart.
#[derive(Clone, Serialize)]
struct HistInstance {
    key: String,
    module: String,
    /// max temp this instance reported (driving temp preferred if present, else max `temp` reading).
    #[serde(skip_serializing_if = "Option::is_none")]
    temp: Option<f64>,
    /// commanded/observed max fan duty % across this instance's fans (or the driving `pct`).
    #[serde(skip_serializing_if = "Option::is_none")]
    duty: Option<f64>,
    /// max fan RPM across this instance's fans.
    #[serde(skip_serializing_if = "Option::is_none")]
    rpm: Option<f64>,
    /// 1 if last_status==ok at snapshot time, else 0.
    up: u8,
}

#[derive(Clone, Serialize)]
struct HistSnap {
    /// Unix epoch milliseconds at capture.
    t: u64,
    instances: Vec<HistInstance>,
}

struct History {
    cap: usize,
    snaps: VecDeque<HistSnap>,
}

impl History {
    fn new(cap: usize) -> Self {
        History {
            cap,
            snaps: VecDeque::with_capacity(cap.min(64)),
        }
    }
    fn push(&mut self, snap: HistSnap) {
        if self.snaps.len() == self.cap {
            self.snaps.pop_front();
        }
        self.snaps.push_back(snap);
    }
}

/// Background thread: periodically read the shared state (read lock only) and append a compact
/// snapshot to the ring buffer. Never writes to `AppState`; never panics the orchestrator.
fn spawn_snapshotter(state: Arc<RwLock<AppState>>, history: Arc<Mutex<History>>) {
    thread::spawn(move || loop {
        thread::sleep(HISTORY_INTERVAL);
        let snap = {
            let Ok(s) = state.read() else { continue };
            // Don't record empty pre-detect state as a data point.
            if s.instances.is_empty() {
                continue;
            }
            let mut instances: Vec<HistInstance> = s
                .instances
                .values()
                .map(|i| {
                    let agg = aggregate_readings(&i.last_readings);
                    HistInstance {
                        key: format!("{}:{}", i.module_name, i.id),
                        module: i.module_name.clone(),
                        temp: agg.temp,
                        duty: agg.duty,
                        rpm: agg.rpm,
                        up: u8::from(i.last_status == "ok"),
                    }
                })
                .collect();
            instances.sort_by(|a, b| a.key.cmp(&b.key));
            HistSnap {
                t: now_millis(),
                instances,
            }
        };
        if let Ok(mut h) = history.lock() {
            h.push(snap);
        }
    });
}

/// Per-instance numeric aggregates used both by the history snapshotter and the live KPIs.
struct Agg {
    temp: Option<f64>,
    duty: Option<f64>,
    rpm: Option<f64>,
}

/// Reduce a reading list to the headline series: representative temp (driving smoothed temp if the
/// instance reports a `driving` reading, else the max `temp` reading), max fan duty (driving `pct`
/// if present, else max fan `pwm`), and max fan `rpm`.
fn aggregate_readings(readings: &[Reading]) -> Agg {
    let mut max_temp: Option<f64> = None;
    let mut driving_temp: Option<f64> = None;
    let mut driving_pct: Option<f64> = None;
    let mut max_pwm: Option<f64> = None;
    let mut max_rpm: Option<f64> = None;

    for r in readings {
        match r.kind.as_str() {
            "temp" => {
                if let Some(t) = num(r, "temp") {
                    max_temp = Some(max_temp.map_or(t, |m: f64| m.max(t)));
                }
            }
            "driving" => {
                driving_temp = num(r, "temp").or(driving_temp);
                driving_pct = num(r, "pct").or(driving_pct);
            }
            "fan" => {
                if let Some(p) = num(r, "pwm") {
                    max_pwm = Some(max_pwm.map_or(p, |m: f64| m.max(p)));
                }
                if let Some(rp) = num(r, "rpm") {
                    max_rpm = Some(max_rpm.map_or(rp, |m: f64| m.max(rp)));
                }
            }
            _ => {}
        }
    }
    Agg {
        temp: driving_temp.or(max_temp),
        duty: driving_pct.or(max_pwm),
        rpm: max_rpm,
    }
}

fn render_history_json(history: &Arc<Mutex<History>>) -> String {
    let Ok(h) = history.lock() else {
        return r#"{"snaps":[]}"#.to_string();
    };
    let snaps: Vec<&HistSnap> = h.snaps.iter().collect();
    #[derive(Serialize)]
    struct Out<'a> {
        snaps: Vec<&'a HistSnap>,
    }
    serde_json::to_string(&Out { snaps }).unwrap_or_else(|_| r#"{"snaps":[]}"#.to_string())
}

// ---------------------------------------------------------------------------
// Curve config (/curve.json?module=<m>)
// ---------------------------------------------------------------------------

/// Read a module's temp->duty curve from its etc config (same convention as the anemos SDK:
/// `$AIOLOS_ETC_DIR/<module>.curve.json` else `/opt/aiolos/etc/<module>.curve.json`). Read-only;
/// this touches CONFIG only (never AppState/main.rs). Returns sorted `[temp,pct]` points + α.
fn render_curve_json(module: Option<String>) -> String {
    let Some(module) = module else {
        return r#"{"error":"missing module"}"#.to_string();
    };
    // Guard against path traversal: module names never contain a path separator (and the registry
    // forbids `:`); accept only a plain file-name token.
    if module.is_empty() || module.contains(['/', '\\', ':', '.']) {
        return r#"{"error":"invalid module"}"#.to_string();
    }
    let path = curve_path(&module);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return format!(
            r#"{{"module":{},"available":false,"points":[],"path":{}}}"#,
            json_str(&module),
            json_str(&path)
        );
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return format!(
            r#"{{"module":{},"available":false,"points":[],"path":{}}}"#,
            json_str(&module),
            json_str(&path)
        );
    };

    let mut points: Vec<(i64, i64)> = Vec::new();
    let mut sensitivity: Option<f64> = None;
    if let Some(map) = value.as_object() {
        for (k, v) in map {
            if k == "sensitivity" {
                sensitivity = v.as_f64();
                continue;
            }
            let temp = k
                .parse::<i64>()
                .ok()
                .or_else(|| k.parse::<f64>().ok().map(|f| f.round() as i64));
            let pct = v.as_i64().or_else(|| v.as_f64().map(|f| f.round() as i64));
            if let (Some(t), Some(p)) = (temp, pct) {
                points.push((t, p));
            }
        }
    }
    points.sort_by_key(|(t, _)| *t);

    let pts: String = points
        .iter()
        .map(|(t, p)| format!("[{t},{p}]"))
        .collect::<Vec<_>>()
        .join(",");
    let sens = sensitivity
        .map(|a| a.to_string())
        .unwrap_or_else(|| "null".to_string());
    format!(
        r#"{{"module":{},"available":true,"points":[{}],"sensitivity":{}}}"#,
        json_str(&module),
        pts,
        sens
    )
}

fn curve_path(module: &str) -> String {
    match std::env::var("AIOLOS_ETC_DIR") {
        Ok(dir) => format!("{dir}/{module}.curve.json"),
        Err(_) => format!("/opt/aiolos/etc/{module}.curve.json"),
    }
}

// ---------------------------------------------------------------------------
// Prometheus exposition (/metrics) — SOW-0007
// ---------------------------------------------------------------------------

/// Render the live readings as Prometheus text-format (version 0.0.4). Hand-rolled, no deps.
fn render_metrics(state: &Arc<RwLock<AppState>>) -> String {
    let s = match state.read() {
        Ok(s) => s,
        Err(_) => return "# aiolos: state lock poisoned\n".to_string(),
    };

    let mut out = String::with_capacity(4096);
    let mut m = MetricBuf::default();

    // Orchestrator heartbeat.
    out.push_str("# HELP aiolos_tick The orchestrator heartbeat tick counter.\n");
    out.push_str("# TYPE aiolos_tick gauge\n");
    out.push_str(&format!("aiolos_tick {}\n\n", s.tick_count));

    // Per-module detect health.
    let mut modules: Vec<_> = s.modules.iter().collect();
    modules.sort_by(|a, b| a.0.cmp(b.0));
    for (name, h) in &modules {
        m.detect_up.push(format!(
            "aiolos_module_detect_up{{module={}}} {}",
            json_str(name),
            u8::from(h.detect_status == "ok")
        ));
    }

    // Per-instance series. Sort for stable output.
    let mut instances: Vec<_> = s.instances.values().collect();
    instances.sort_by(|a, b| (&a.module_name, &a.id).cmp(&(&b.module_name, &b.id)));

    for i in &instances {
        let base = format!(
            "module={},id={},instance_name={}",
            json_str(&i.module_name),
            json_str(&i.id),
            json_str(&i.name)
        );
        m.up.push(format!(
            "aiolos_instance_up{{{base}}} {}",
            u8::from(i.last_status == "ok")
        ));
        m.restarts.push(format!(
            "aiolos_instance_restarts_total{{{base}}} {}",
            i.restart_count
        ));
        m.stale.push(format!(
            "aiolos_instance_seconds_since_seen{{{base}}} {}",
            i.last_seen.elapsed().as_secs()
        ));

        // Disambiguate duplicate (kind,label) within an instance (e.g. two "CPU" sockets) by
        // suffixing _2, _3, … to the label value so each series stays unique.
        let mut seen: std::collections::HashMap<(&str, String), u32> =
            std::collections::HashMap::new();
        for r in &i.last_readings {
            let mut label = r.label.clone();
            let count = seen.entry((r.kind.as_str(), r.label.clone())).or_insert(0);
            *count += 1;
            if *count > 1 {
                label = format!("{label}_{count}");
            }
            let full = format!("{base},label={}", json_str(&label));
            match r.kind.as_str() {
                "temp" => {
                    if let Some(t) = num(r, "temp") {
                        m.temp
                            .push(format!("aiolos_temp_celsius{{{full}}} {}", fmt_num(t)));
                    }
                }
                "fan" => {
                    if let Some(p) = num(r, "pwm") {
                        m.duty
                            .push(format!("aiolos_fan_duty_percent{{{full}}} {}", fmt_num(p)));
                    }
                    if let Some(rp) = num(r, "rpm") {
                        m.rpm
                            .push(format!("aiolos_fan_rpm{{{full}}} {}", fmt_num(rp)));
                    }
                }
                "driving" => {
                    if let Some(t) = num(r, "temp") {
                        m.driving
                            .push(format!("aiolos_driving_celsius{{{full}}} {}", fmt_num(t)));
                    }
                    if let Some(t) = num(r, "raw") {
                        m.driving_raw.push(format!(
                            "aiolos_driving_raw_celsius{{{full}}} {}",
                            fmt_num(t)
                        ));
                    }
                    if let Some(p) = num(r, "pct") {
                        m.driving_duty.push(format!(
                            "aiolos_driving_duty_percent{{{full}}} {}",
                            fmt_num(p)
                        ));
                    }
                }
                // SOW-0009 power readings: `powercap` = nvidia-powercap control state; `power-state`
                // = nut UPS state. Booleans are exported as 0/1 gauges via `bnum`.
                "powercap" => {
                    if let Some(c) = bnum(r, "capped") {
                        m.pc_capped
                            .push(format!("aiolos_powercap_capped{{{full}}} {}", fmt_num(c)));
                    }
                    if let Some(v) = num(r, "limit_mw") {
                        m.pc_limit
                            .push(format!("aiolos_powercap_limit_mw{{{full}}} {}", fmt_num(v)));
                    }
                    if let Some(v) = num(r, "draw_mw") {
                        m.pc_draw
                            .push(format!("aiolos_powercap_draw_mw{{{full}}} {}", fmt_num(v)));
                    }
                }
                "power-state" => {
                    if let Some(b) = bnum(r, "on_battery") {
                        m.ps_on_battery
                            .push(format!("aiolos_power_on_battery{{{full}}} {}", fmt_num(b)));
                    }
                    if let Some(v) = num(r, "runtime_s") {
                        m.ps_runtime.push(format!(
                            "aiolos_power_runtime_seconds{{{full}}} {}",
                            fmt_num(v)
                        ));
                    }
                    if let Some(v) = num(r, "charge") {
                        m.ps_charge.push(format!(
                            "aiolos_power_charge_percent{{{full}}} {}",
                            fmt_num(v)
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    m.write(&mut out);
    out
}

/// Groups metric lines by name so each `# HELP`/`# TYPE` header is emitted once (Prometheus requires
/// all samples of a metric family to be grouped together).
#[derive(Default)]
struct MetricBuf {
    temp: Vec<String>,
    duty: Vec<String>,
    rpm: Vec<String>,
    driving: Vec<String>,
    driving_raw: Vec<String>,
    driving_duty: Vec<String>,
    up: Vec<String>,
    restarts: Vec<String>,
    stale: Vec<String>,
    detect_up: Vec<String>,
    // SOW-0009 power series.
    pc_capped: Vec<String>,
    pc_limit: Vec<String>,
    pc_draw: Vec<String>,
    ps_on_battery: Vec<String>,
    ps_runtime: Vec<String>,
    ps_charge: Vec<String>,
}

impl MetricBuf {
    fn write(&self, out: &mut String) {
        emit(
            out,
            "aiolos_temp_celsius",
            "gauge",
            "Temperature reading in Celsius.",
            &self.temp,
        );
        emit(
            out,
            "aiolos_fan_duty_percent",
            "gauge",
            "Commanded/observed fan duty in percent.",
            &self.duty,
        );
        emit(
            out,
            "aiolos_fan_rpm",
            "gauge",
            "Fan tachometer reading in RPM.",
            &self.rpm,
        );
        emit(
            out,
            "aiolos_driving_celsius",
            "gauge",
            "Smoothed driving temperature in Celsius.",
            &self.driving,
        );
        emit(
            out,
            "aiolos_driving_raw_celsius",
            "gauge",
            "Raw (unsmoothed) driving temperature in Celsius.",
            &self.driving_raw,
        );
        emit(
            out,
            "aiolos_driving_duty_percent",
            "gauge",
            "Commanded duty for the driving temperature in percent.",
            &self.driving_duty,
        );
        emit(
            out,
            "aiolos_instance_up",
            "gauge",
            "1 if the instance's last tick was ok, else 0.",
            &self.up,
        );
        emit(
            out,
            "aiolos_instance_restarts_total",
            "counter",
            "Number of times the instance has been restarted.",
            &self.restarts,
        );
        emit(
            out,
            "aiolos_instance_seconds_since_seen",
            "gauge",
            "Seconds since the instance last reported (staleness).",
            &self.stale,
        );
        emit(
            out,
            "aiolos_module_detect_up",
            "gauge",
            "1 if the module's last detect was ok, else 0.",
            &self.detect_up,
        );
        emit(
            out,
            "aiolos_powercap_capped",
            "gauge",
            "1 if aiolos is currently capping this GPU's power limit, else 0.",
            &self.pc_capped,
        );
        emit(
            out,
            "aiolos_powercap_limit_mw",
            "gauge",
            "Effective GPU power limit in milliwatts.",
            &self.pc_limit,
        );
        emit(
            out,
            "aiolos_powercap_draw_mw",
            "gauge",
            "Current GPU power draw in milliwatts.",
            &self.pc_draw,
        );
        emit(
            out,
            "aiolos_power_on_battery",
            "gauge",
            "1 if this UPS is on battery (utility power lost), else 0.",
            &self.ps_on_battery,
        );
        emit(
            out,
            "aiolos_power_runtime_seconds",
            "gauge",
            "Estimated UPS runtime remaining in seconds.",
            &self.ps_runtime,
        );
        emit(
            out,
            "aiolos_power_charge_percent",
            "gauge",
            "UPS battery charge percent.",
            &self.ps_charge,
        );
    }
}

fn emit(out: &mut String, name: &str, kind: &str, help: &str, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} {kind}\n"));
    for l in lines {
        out.push_str(l);
        out.push('\n');
    }
    out.push('\n');
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Read a numeric reading field as f64 (ints or floats).
fn num(r: &Reading, key: &str) -> Option<f64> {
    r.fields.get(key).and_then(|v| v.as_f64())
}

/// Read a JSON boolean field as a Prometheus 0/1 gauge value (`num` only handles JSON numbers, and
/// `as_f64()` returns `None` for a bool).
fn bnum(r: &Reading, key: &str) -> Option<f64> {
    r.fields
        .get(key)
        .and_then(|v| v.as_bool())
        .map(|b| if b { 1.0 } else { 0.0 })
}

/// Format an f64 for Prometheus: drop the trailing `.0` for whole numbers, else plain decimal.
fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// JSON-quote a string (for Prometheus label VALUES and small hand-built JSON snippets). Escapes
/// `\`, `"`, and control chars; strips ANSI first so escape codes never reach the output.
fn json_str(s: &str) -> String {
    let stripped = strip_ansi(s);
    let mut out = String::with_capacity(stripped.len() + 2);
    out.push('"');
    for c in stripped.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Strip ANSI/VT escape sequences (CSI `\x1b[...m` etc. and bare control chars) defensively, so no
/// terminal control codes ever reach the UI or metrics — belt-and-suspenders alongside the SDK's
/// `.with_ansi(false)`.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // ESC: skip an optional intermediate then the terminator of a CSI/OSC/etc. sequence.
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    // CSI: parameter/intermediate bytes 0x20-0x3f, final byte 0x40-0x7e.
                    for f in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&f) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    // OSC: terminated by BEL or ESC\.
                    while let Some(&f) = chars.peek() {
                        if f == '\u{07}' {
                            chars.next();
                            break;
                        }
                        if f == '\u{1b}' {
                            chars.next();
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                        chars.next();
                    }
                }
                _ => {
                    // Lone ESC or a two-byte escape — drop ESC and the next byte if present.
                    chars.next();
                }
            }
        } else if c == '\u{7f}' || ((c as u32) < 0x20 && c != '\n' && c != '\t') {
            // Drop other control characters (keep newline/tab for log readability).
        } else {
            out.push(c);
        }
    }
    out
}

fn tail_lines(entry: &crate::InstanceEntry, n: usize) -> Vec<String> {
    entry
        .stderr_tail
        .lock()
        .map(|t| t.iter().rev().take(n).rev().cloned().collect())
        .unwrap_or_default()
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InstanceEntry, ModuleHealth};
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::mpsc;
    use std::time::Instant;

    fn mk_instance(
        module: &str,
        id: &str,
        name: &str,
        status: &str,
        readings: Vec<Reading>,
    ) -> InstanceEntry {
        let (tx, _rx) = mpsc::channel();
        InstanceEntry {
            module_name: module.into(),
            id: id.into(),
            name: name.into(),
            last_status: status.into(),
            last_error: None,
            last_readings: readings,
            restart_count: 0,
            last_seen: Instant::now(),
            cmd_tx: tx,
            stderr_tail: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    fn state_with(
        instances: Vec<InstanceEntry>,
        modules: Vec<(&str, &str)>,
        tick: u64,
    ) -> Arc<RwLock<AppState>> {
        let mut s = AppState {
            tick_count: tick,
            ..Default::default()
        };
        for i in instances {
            s.instances.insert(format!("{}:{}", i.module_name, i.id), i);
        }
        for (name, st) in modules {
            s.modules.insert(
                name.to_string(),
                ModuleHealth {
                    detect_status: st.to_string(),
                    detect_error: None,
                },
            );
        }
        Arc::new(RwLock::new(s))
    }

    #[test]
    fn metrics_render_all_reading_kinds() {
        let inst = mk_instance(
            "nvidia",
            "GPU-1",
            "RTX 6000",
            "ok",
            vec![
                Reading::new("temp", "GPU", json!({"temp": 63})),
                Reading::new("fan", "fan0", json!({"pwm": 72, "rpm": 2200})),
                Reading::new(
                    "driving",
                    "driving",
                    json!({"temp": 60, "raw": 63, "pct": 80}),
                ),
            ],
        );
        let state = state_with(vec![inst], vec![("nvidia", "ok")], 42);
        let m = render_metrics(&state);

        assert!(m.contains("aiolos_tick 42"));
        assert!(m.contains(r#"aiolos_temp_celsius{module="nvidia",id="GPU-1",instance_name="RTX 6000",label="GPU"} 63"#));
        assert!(m.contains(r#"aiolos_fan_duty_percent{module="nvidia",id="GPU-1",instance_name="RTX 6000",label="fan0"} 72"#));
        assert!(m.contains(r#"aiolos_fan_rpm{module="nvidia",id="GPU-1",instance_name="RTX 6000",label="fan0"} 2200"#));
        assert!(m.contains(r#"aiolos_driving_celsius{module="nvidia",id="GPU-1",instance_name="RTX 6000",label="driving"} 60"#));
        assert!(m.contains(r#"aiolos_driving_raw_celsius{module="nvidia",id="GPU-1",instance_name="RTX 6000",label="driving"} 63"#));
        assert!(m.contains(r#"aiolos_driving_duty_percent{module="nvidia",id="GPU-1",instance_name="RTX 6000",label="driving"} 80"#));
        assert!(m.contains(
            r#"aiolos_instance_up{module="nvidia",id="GPU-1",instance_name="RTX 6000"} 1"#
        ));
        assert!(m.contains(r#"aiolos_module_detect_up{module="nvidia"} 1"#));
        // Each family header appears exactly once.
        assert_eq!(m.matches("# TYPE aiolos_temp_celsius gauge").count(), 1);
        assert_eq!(m.matches("# TYPE aiolos_fan_rpm gauge").count(), 1);
    }

    #[test]
    fn metrics_disambiguate_duplicate_labels() {
        // Two CPU sockets both labelled "CPU" must become "CPU" and "CPU_2" (unique series).
        let inst = mk_instance(
            "asrock16-2t",
            "board",
            "board",
            "ok",
            vec![
                Reading::new("temp", "CPU", json!({"temp": 50})),
                Reading::new("temp", "CPU", json!({"temp": 55})),
            ],
        );
        let state = state_with(vec![inst], vec![], 1);
        let m = render_metrics(&state);
        assert!(m.contains(r#"label="CPU"} 50"#), "{m}");
        assert!(m.contains(r#"label="CPU_2"} 55"#), "{m}");
    }

    #[test]
    fn metrics_down_when_not_ok() {
        let inst = mk_instance("nvme", "SER-A", "Samsung", "error", vec![]);
        let state = state_with(vec![inst], vec![("nvme", "error")], 5);
        let m = render_metrics(&state);
        assert!(
            m.contains(r#"aiolos_instance_up{module="nvme",id="SER-A",instance_name="Samsung"} 0"#)
        );
        assert!(m.contains(r#"aiolos_module_detect_up{module="nvme"} 0"#));
    }

    #[test]
    fn metrics_escape_label_values() {
        let inst = mk_instance(
            "m",
            "id",
            "na\"me",
            "ok",
            vec![Reading::new("temp", "a\\b", json!({"temp": 1}))],
        );
        let state = state_with(vec![inst], vec![], 1);
        let m = render_metrics(&state);
        assert!(m.contains(r#"instance_name="na\"me""#), "{m}");
        assert!(m.contains(r#"label="a\\b""#), "{m}");
    }

    #[test]
    fn fmt_num_drops_trailing_zero() {
        assert_eq!(fmt_num(63.0), "63");
        assert_eq!(fmt_num(2200.0), "2200");
        assert_eq!(fmt_num(63.5), "63.5");
    }

    #[test]
    fn aggregate_prefers_driving_and_takes_maxima() {
        let agg = aggregate_readings(&[
            Reading::new("temp", "GPU", json!({"temp": 40})),
            Reading::new("temp", "NVMe", json!({"temp": 55})),
            Reading::new(
                "driving",
                "driving",
                json!({"temp": 58, "raw": 60, "pct": 77}),
            ),
            Reading::new("fan", "fan0", json!({"pwm": 70, "rpm": 1800})),
            Reading::new("fan", "fan1", json!({"pwm": 90, "rpm": 2400})),
        ]);
        assert_eq!(agg.temp, Some(58.0), "driving temp preferred");
        assert_eq!(agg.duty, Some(77.0), "driving pct preferred");
        assert_eq!(agg.rpm, Some(2400.0), "max rpm");
    }

    #[test]
    fn aggregate_falls_back_to_max_temp_and_pwm() {
        let agg = aggregate_readings(&[
            Reading::new("temp", "A", json!({"temp": 30})),
            Reading::new("temp", "B", json!({"temp": 48})),
            Reading::new("fan", "fan0", json!({"pwm": 65})),
        ]);
        assert_eq!(agg.temp, Some(48.0));
        assert_eq!(agg.duty, Some(65.0));
        assert_eq!(agg.rpm, None);
    }

    #[test]
    fn strip_ansi_removes_color_codes() {
        assert_eq!(strip_ansi("\u{1b}[2mDEBUG\u{1b}[0m hello"), "DEBUG hello");
        assert_eq!(strip_ansi("\u{1b}[32mgreen\u{1b}[39m"), "green");
        assert_eq!(strip_ansi("plain"), "plain");
        // Keeps newlines/tabs.
        assert_eq!(strip_ansi("a\nb\tc"), "a\nb\tc");
    }

    #[test]
    fn percent_decode_basic() {
        assert_eq!(percent_decode("asrock16-2t"), "asrock16-2t");
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("a+b"), "a b");
    }

    #[test]
    fn module_param_extracts_value() {
        assert_eq!(module_param("module=nvidia"), Some("nvidia".to_string()));
        assert_eq!(
            module_param("x=1&module=nvme&y=2"),
            Some("nvme".to_string())
        );
        assert_eq!(module_param("nope=1"), None);
    }

    #[test]
    fn curve_json_rejects_path_traversal() {
        assert!(render_curve_json(Some("../etc/passwd".into())).contains("invalid module"));
        assert!(render_curve_json(Some("a/b".into())).contains("invalid module"));
        assert!(render_curve_json(Some("a.b".into())).contains("invalid module"));
        assert!(render_curve_json(None).contains("missing module"));
    }

    #[test]
    fn curve_json_reads_points() {
        let dir = std::env::temp_dir().join(format!("aiolos-curve-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("testmod.curve.json"),
            r#"{"30":30,"80":100,"sensitivity":0.5}"#,
        )
        .unwrap();
        std::env::set_var("AIOLOS_ETC_DIR", &dir);
        let out = render_curve_json(Some("testmod".into()));
        std::env::remove_var("AIOLOS_ETC_DIR");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.contains(r#""available":true"#), "{out}");
        assert!(out.contains("[30,30]"), "{out}");
        assert!(out.contains("[80,100]"), "{out}");
        assert!(out.contains(r#""sensitivity":0.5"#), "{out}");
    }

    #[test]
    fn history_ring_buffer_is_bounded() {
        let mut h = History::new(3);
        for t in 0..5u64 {
            h.push(HistSnap {
                t,
                instances: vec![],
            });
        }
        assert_eq!(h.snaps.len(), 3);
        assert_eq!(h.snaps.front().unwrap().t, 2);
        assert_eq!(h.snaps.back().unwrap().t, 4);
    }

    #[test]
    fn status_json_strips_ansi_in_stderr_tail() {
        let inst = mk_instance("m", "i", "n", "ok", vec![]);
        {
            let mut t = inst.stderr_tail.lock().unwrap();
            t.push_back("\u{1b}[2mDEBUG\u{1b}[0m line".to_string());
        }
        let state = state_with(vec![inst], vec![], 1);
        let j = render_json(&state);
        assert!(j.contains("DEBUG line"), "{j}");
        assert!(!j.contains("\u{1b}"), "no escape char in output");
    }
}
