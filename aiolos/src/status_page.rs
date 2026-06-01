//! Read-only HTTP status server for aiolos. Hand-rolled, dependency-light, no async runtime.
//!
//! Routes (all read-only; the server NEVER mutates orchestrator state):
//!   `GET /`             -> the themed single-page dashboard shell (HTML)
//!   `GET /aiolos.css`   -> embedded stylesheet
//!   `GET /aiolos.js`    -> embedded vanilla-JS app (tabs, charts, animated winds)
//!   `GET /status.json`  -> live snapshot (modules + instances + components)
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
use protocol::{Component, Signal};
use serde::Serialize;
use std::collections::BTreeMap;
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
struct StatusJson {
    tick: u64,
    modules: Vec<ModuleJson>,
    units: Vec<UnitJson>,
    components: Vec<Component>,
    signals: Vec<Signal>,
    instances: Vec<InstanceHealthJson>,
}

#[derive(Serialize)]
struct ModuleJson {
    module: String,
    detect_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detect_error: Option<String>,
}

/// An assembled hardware unit: labels merged across every anemos that contributes to it (e.g. the
/// motherboard's temps from `ipmi-temps` + fans from `rome2d-fans`), the source modules, and the
/// worst contributing-instance status.
#[derive(Serialize)]
struct UnitJson {
    id: String,
    labels: protocol::Labels,
    sources: Vec<String>,
    status: String,
}

/// Per-instance health only (the data is assembled into units/components/signals above).
#[derive(Serialize)]
struct InstanceHealthJson {
    module: String,
    id: String,
    name: String,
    #[serde(rename = "type")]
    unit_type: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    restart_count: u32,
    seconds_since_seen: u64,
    stderr_tail: Vec<String>,
}

/// Worse of two instance statuses (for a unit assembled from several anemoi).
fn worse_status<'a>(a: &'a str, b: &'a str) -> &'a str {
    fn rank(s: &str) -> u8 {
        match s {
            "ok" => 0,
            "starting" => 1,
            "error" => 2,
            _ => 3,
        }
    }
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
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
            module: name.clone(),
            detect_status: h.detect_status.clone(),
            detect_error: h.detect_error.clone(),
        })
        .collect();
    modules.sort_by(|a, b| a.module.cmp(&b.module));

    // Assemble across instances: merge units by id, collect components + signals, per-instance health.
    let mut units: BTreeMap<String, UnitJson> = BTreeMap::new();
    let mut comps: BTreeMap<String, Component> = BTreeMap::new();
    let mut signals: Vec<Signal> = Vec::new();
    let mut instances: Vec<InstanceHealthJson> = Vec::new();
    for i in s.instances.values() {
        for u in &i.last_units {
            let e = units.entry(u.id.clone()).or_insert_with(|| UnitJson {
                id: u.id.clone(),
                labels: protocol::Labels::new(),
                sources: Vec::new(),
                status: i.last_status.clone(),
            });
            for (k, v) in &u.labels {
                e.labels.entry(k.clone()).or_insert_with(|| v.clone());
            }
            if !e.sources.contains(&i.module_name) {
                e.sources.push(i.module_name.clone());
            }
            e.status = worse_status(&e.status, &i.last_status).to_string();
        }
        for c in &i.last_components {
            comps.entry(c.id.clone()).or_insert_with(|| c.clone());
        }
        signals.extend(i.last_signals.iter().cloned());
        instances.push(InstanceHealthJson {
            module: i.module_name.clone(),
            id: i.id.clone(),
            name: i.name.clone(),
            unit_type: i.unit_type.clone(),
            status: i.last_status.clone(),
            error: i.last_error.clone(),
            restart_count: i.restart_count,
            seconds_since_seen: i.last_seen.elapsed().as_secs(),
            stderr_tail: tail_lines(i, 12).iter().map(|l| strip_ansi(l)).collect(),
        });
    }
    let mut units: Vec<UnitJson> = units.into_values().collect();
    for u in &mut units {
        u.sources.sort();
    }
    units.sort_by(|a, b| a.id.cmp(&b.id));
    let mut components: Vec<Component> = comps.into_values().collect();
    components.sort_by(|a, b| a.id.cmp(&b.id));
    signals.sort_by(|a, b| a.id.cmp(&b.id));
    instances.sort_by(|a, b| {
        (a.module.as_str(), a.id.as_str()).cmp(&(b.module.as_str(), b.id.as_str()))
    });

    serde_json::to_string(&StatusJson {
        tick,
        modules,
        units,
        components,
        signals,
        instances,
    })
    .unwrap_or_else(|_| "{}".to_string())
}

// ---------------------------------------------------------------------------
// Time-series ring buffer (/history.json)
// ---------------------------------------------------------------------------

/// One per-UNIT sample inside a history snapshot: the numeric series we chart. Keyed by the stable
/// unit id (so a chart series survives renames); `name` is the short label shown in the legend.
#[derive(Clone, Serialize)]
struct HistInstance {
    key: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    temp: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rpm: Option<f64>,
    /// 1 if every contributing instance was ok at snapshot time, else 0.
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
            let mut instances: Vec<HistInstance> = unit_aggregates(&s)
                .into_iter()
                .map(|(id, name, agg, up)| HistInstance {
                    key: id,
                    name,
                    temp: agg.temp,
                    duty: agg.duty,
                    rpm: agg.rpm,
                    up: u8::from(up),
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

/// Per-unit numeric aggregates used by the history snapshotter.
struct Agg {
    temp: Option<f64>,
    duty: Option<f64>,
    rpm: Option<f64>,
}

/// Reduce a set of signals to the headline series: representative temp (driving smoothed temp if
/// present, else the max temperature), max fan duty (driving duty or fan-duty), and max fan RPM.
fn aggregate_signals<'a>(signals: impl IntoIterator<Item = &'a Signal>) -> Agg {
    let mut max_temp: Option<f64> = None;
    let mut driving_temp: Option<f64> = None;
    let mut driving_pct: Option<f64> = None;
    let mut max_duty: Option<f64> = None;
    let mut max_rpm: Option<f64> = None;
    for sig in signals {
        let Some(v) = signal_num(sig) else { continue };
        match sig.kind() {
            Some("temperature") => max_temp = Some(max_temp.map_or(v, |m: f64| m.max(v))),
            Some("driving-temperature") => driving_temp = Some(v),
            Some("driving-duty") => driving_pct = Some(v),
            Some("fan-duty") => max_duty = Some(max_duty.map_or(v, |m: f64| m.max(v))),
            Some("fan-rpm") => max_rpm = Some(max_rpm.map_or(v, |m: f64| m.max(v))),
            _ => {}
        }
    }
    Agg {
        temp: driving_temp.or(max_temp),
        duty: driving_pct.or(max_duty),
        rpm: max_rpm,
    }
}

/// Group every instance's signals by their hardware unit (signal → component → unit), and aggregate
/// each unit's headline series. Returns `(unit_id, unit_name, agg, up)`. This is where the two
/// motherboard anemoi (`ipmi-temps` + `rome2d-fans`) fold into one `board` series.
fn unit_aggregates(s: &AppState) -> Vec<(String, String, Agg, bool)> {
    use std::collections::HashMap;
    let mut comp_unit: HashMap<&str, &str> = HashMap::new();
    let mut unit_name: HashMap<&str, String> = HashMap::new();
    let mut unit_up: HashMap<&str, bool> = HashMap::new();
    for i in s.instances.values() {
        for c in &i.last_components {
            comp_unit.insert(c.id.as_str(), c.unit.as_str());
        }
        for u in &i.last_units {
            unit_name.entry(u.id.as_str()).or_insert_with(|| {
                u.labels
                    .get("name")
                    .cloned()
                    .unwrap_or_else(|| u.id.clone())
            });
            let up = i.last_status == "ok";
            unit_up
                .entry(u.id.as_str())
                .and_modify(|e| *e = *e && up)
                .or_insert(up);
        }
    }
    let mut by_unit: HashMap<&str, Vec<&Signal>> = HashMap::new();
    for i in s.instances.values() {
        for sig in &i.last_signals {
            if let Some(u) = comp_unit.get(sig.component.as_str()) {
                by_unit.entry(u).or_default().push(sig);
            }
        }
    }
    by_unit
        .into_iter()
        .map(|(uid, sigs)| {
            let agg = aggregate_signals(sigs);
            (
                uid.to_string(),
                unit_name
                    .get(uid)
                    .cloned()
                    .unwrap_or_else(|| uid.to_string()),
                agg,
                unit_up.get(uid).copied().unwrap_or(false),
            )
        })
        .collect()
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

/// Render the live components as Prometheus text-format (version 0.0.4). Hand-rolled, no deps.
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

        for sig in &i.last_signals {
            let name = sig.labels.get("name").map(String::as_str).unwrap_or("");
            let full = format!(
                "{base},component={},signal={},label={}",
                json_str(&sig.component),
                json_str(&sig.id),
                json_str(name)
            );
            let Some(v) = signal_num(sig) else { continue };
            match sig.kind() {
                Some("temperature") => m
                    .temp
                    .push(format!("aiolos_temp_celsius{{{full}}} {}", fmt_num(v))),
                Some("fan-duty") => m
                    .duty
                    .push(format!("aiolos_fan_duty_percent{{{full}}} {}", fmt_num(v))),
                Some("fan-rpm") => m
                    .rpm
                    .push(format!("aiolos_fan_rpm{{{full}}} {}", fmt_num(v))),
                Some("driving-temperature") => m
                    .driving
                    .push(format!("aiolos_driving_celsius{{{full}}} {}", fmt_num(v))),
                Some("driving-raw-temperature") => m.driving_raw.push(format!(
                    "aiolos_driving_raw_celsius{{{full}}} {}",
                    fmt_num(v)
                )),
                Some("driving-duty") => m.driving_duty.push(format!(
                    "aiolos_driving_duty_percent{{{full}}} {}",
                    fmt_num(v)
                )),
                Some("powercap-capped") => m
                    .pc_capped
                    .push(format!("aiolos_powercap_capped{{{full}}} {}", fmt_num(v))),
                Some("power-limit") => m
                    .pc_limit
                    .push(format!("aiolos_powercap_limit_mw{{{full}}} {}", fmt_num(v))),
                Some("power-draw") => m
                    .pc_draw
                    .push(format!("aiolos_powercap_draw_mw{{{full}}} {}", fmt_num(v))),
                Some("power-on-battery") => m
                    .ps_on_battery
                    .push(format!("aiolos_power_on_battery{{{full}}} {}", fmt_num(v))),
                Some("power-runtime") => m.ps_runtime.push(format!(
                    "aiolos_power_runtime_seconds{{{full}}} {}",
                    fmt_num(v)
                )),
                Some("power-charge") => m.ps_charge.push(format!(
                    "aiolos_power_charge_percent{{{full}}} {}",
                    fmt_num(v)
                )),
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
            "Temperature component in Celsius.",
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
            "Fan tachometer component in RPM.",
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

/// Read a signal value as f64: ints/floats directly, bools as a 0/1 gauge.
fn signal_num(s: &Signal) -> Option<f64> {
    s.value.as_ref().and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_i64().map(|i| i as f64))
            .or_else(|| v.as_bool().map(|b| if b { 1.0 } else { 0.0 }))
    })
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

    use protocol::Unit;

    fn mk_instance(
        module: &str,
        id: &str,
        name: &str,
        status: &str,
        signals: Vec<Signal>,
    ) -> InstanceEntry {
        let (tx, _rx) = mpsc::channel();
        // Derive the unit + the components the signals reference so the assembly stays consistent.
        let mut comps: BTreeMap<String, Component> = BTreeMap::new();
        for s in &signals {
            comps
                .entry(s.component.clone())
                .or_insert_with(|| Component::new(s.component.clone(), id).typed("test"));
        }
        InstanceEntry {
            module_name: module.into(),
            id: id.into(),
            name: name.into(),
            unit_type: "test".into(),
            last_status: status.into(),
            last_error: None,
            last_units: vec![Unit::new(id).name(name).typed("test")],
            last_components: comps.into_values().collect(),
            last_signals: signals,
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

    fn sig(id: &str, component: &str, kind: &str, value: i64, name: &str) -> Signal {
        Signal::producer(id, component, kind)
            .value(json!(value))
            .name(name)
    }

    #[test]
    fn metrics_render_all_signal_kinds() {
        let inst = mk_instance(
            "nvidia",
            "GPU-1",
            "RTX 6000",
            "ok",
            vec![
                sig("g:gpu:temp", "g:gpu", "temperature", 63, "GPU"),
                sig("g:gpu:fan0.duty", "g:gpu", "fan-duty", 72, "fan0"),
                sig("g:gpu:fan0.rpm", "g:gpu", "fan-rpm", 2200, "fan0"),
                sig(
                    "g:gpu:driving.temp",
                    "g:gpu",
                    "driving-temperature",
                    60,
                    "driving",
                ),
                sig(
                    "g:gpu:driving.raw",
                    "g:gpu",
                    "driving-raw-temperature",
                    63,
                    "driving",
                ),
                sig("g:gpu:driving.duty", "g:gpu", "driving-duty", 80, "driving"),
            ],
        );
        let state = state_with(vec![inst], vec![("nvidia", "ok")], 42);
        let m = render_metrics(&state);

        assert!(m.contains("aiolos_tick 42"));
        let base = r#"module="nvidia",id="GPU-1",instance_name="RTX 6000""#;
        assert!(m.contains(&format!(r#"aiolos_temp_celsius{{{base},component="g:gpu",signal="g:gpu:temp",label="GPU"}} 63"#)), "{m}");
        assert!(m.contains(&format!(r#"aiolos_fan_duty_percent{{{base},component="g:gpu",signal="g:gpu:fan0.duty",label="fan0"}} 72"#)));
        assert!(m.contains(&format!(r#"aiolos_fan_rpm{{{base},component="g:gpu",signal="g:gpu:fan0.rpm",label="fan0"}} 2200"#)));
        assert!(m.contains(&format!(r#"aiolos_driving_celsius{{{base},component="g:gpu",signal="g:gpu:driving.temp",label="driving"}} 60"#)));
        assert!(m.contains(&format!(r#"aiolos_driving_raw_celsius{{{base},component="g:gpu",signal="g:gpu:driving.raw",label="driving"}} 63"#)));
        assert!(m.contains(&format!(r#"aiolos_driving_duty_percent{{{base},component="g:gpu",signal="g:gpu:driving.duty",label="driving"}} 80"#)));
        assert!(m.contains(&format!(r#"aiolos_instance_up{{{base}}} 1"#)));
        assert!(m.contains(r#"aiolos_module_detect_up{module="nvidia"} 1"#));
        assert_eq!(m.matches("# TYPE aiolos_temp_celsius gauge").count(), 1);
        assert_eq!(m.matches("# TYPE aiolos_fan_rpm gauge").count(), 1);
    }

    #[test]
    fn metrics_disambiguate_duplicate_labels() {
        // Two CPU sockets may share the display name; signal ids keep the series distinct.
        let inst = mk_instance(
            "rome2d-fans",
            "board",
            "board",
            "ok",
            vec![
                sig("board:cpu0:temp", "board:cpu0", "temperature", 50, "CPU"),
                sig("board:cpu1:temp", "board:cpu1", "temperature", 55, "CPU"),
            ],
        );
        let state = state_with(vec![inst], vec![], 1);
        let m = render_metrics(&state);
        assert!(
            m.contains(r#"signal="board:cpu0:temp",label="CPU"} 50"#),
            "{m}"
        );
        assert!(
            m.contains(r#"signal="board:cpu1:temp",label="CPU"} 55"#),
            "{m}"
        );
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
            vec![sig("sensor:c:temp", "sensor:c", "temperature", 1, "a\\b")],
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
        let signals = vec![
            sig("b:c:gpu.temp", "b:c", "temperature", 40, "GPU"),
            sig("b:c:nvme.temp", "b:c", "temperature", 55, "NVMe"),
            sig(
                "b:c:driving.temp",
                "b:c",
                "driving-temperature",
                58,
                "driving",
            ),
            sig(
                "b:c:driving.raw",
                "b:c",
                "driving-raw-temperature",
                60,
                "driving",
            ),
            sig("b:c:driving.duty", "b:c", "driving-duty", 77, "driving"),
            sig("b:c:fan0.duty", "b:c", "fan-duty", 70, "fan0"),
            sig("b:c:fan0.rpm", "b:c", "fan-rpm", 1800, "fan0"),
            sig("b:c:fan1.duty", "b:c", "fan-duty", 90, "fan1"),
            sig("b:c:fan1.rpm", "b:c", "fan-rpm", 2400, "fan1"),
        ];
        let agg = aggregate_signals(&signals);
        assert_eq!(agg.temp, Some(58.0), "driving temp preferred");
        assert_eq!(agg.duty, Some(77.0), "driving pct preferred");
        assert_eq!(agg.rpm, Some(2400.0), "max rpm");
    }

    #[test]
    fn aggregate_falls_back_to_max_temp_and_pwm() {
        let signals = vec![
            sig("b:c:a.temp", "b:c", "temperature", 30, "A"),
            sig("b:c:b.temp", "b:c", "temperature", 48, "B"),
            sig("b:c:fan0.duty", "b:c", "fan-duty", 65, "fan0"),
        ];
        let agg = aggregate_signals(&signals);
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
        assert_eq!(percent_decode("rome2d-fans"), "rome2d-fans");
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
