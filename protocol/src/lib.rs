//! aiolos ↔ anemos wire protocol types (v2: label-driven signal model).
//!
//! One line = one complete JSON object. Requests flow aiolos → module (the module's stdin);
//! responses flow module → aiolos (the module's stdout). stdout is protocol-only; all logs go
//! to stderr. Authoritative contract: `.agents/sow/specs/aiolos-protocol.spec.md`.
//!
//! # The data model (SOW-0018)
//! A module reports a flat stream of **signals**; the orchestrator assembles **units → components →
//! producers/sinks** from labels, user config enriches, and the UI groups by any label.
//!
//! - **`Unit`** — a piece of HARDWARE (a GPU, an SSD, the motherboard, a UPS), NOT an anemos. One
//!   physical unit reported by several anemoi merges on its `id`.
//! - **`Component`** — a sub-thing within a unit (a fan, a temperature, a CPU socket, a DIMM).
//! - **`Signal`** — the atom: a **producer** (read-only measurement) or a **sink** (controllable
//!   output). Carries a stable `id`, a value domain (`value`/`uom`/`range`), a `labels` bag, and —
//!   for sinks — typed `control` metadata.
//!
//! Every entity has a stable, system-derived `id` (the time-series key; never shown) and a `labels`
//! bag whose reserved keys are `type` (semantic kind), `name` (short user handle), and `description`
//! (long). `unit`/`component` are structural parent references; `uom` is the unit of MEASURE
//! (distinct from the `unit` entity).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

/// Current wire protocol version (the `proto` field of `hello`). v2 = the label-driven signal model.
pub const PROTO_VERSION: u32 = 2;

/// An open bag of string labels. Reserved keys understood across the system: `type` (semantic kind,
/// an open tag aiolos matches but never interprets), `name` (short unique-ish user handle, e.g.
/// `gpu0`), `description` (long, non-unique). Ordered so serialization is stable.
pub type Labels = BTreeMap<String, String>;

// ---------------------------------------------------------------------------
// Requests: aiolos → module
// ---------------------------------------------------------------------------

/// A command from aiolos to a module. Serializes as `{"cmd":"<name>", ...}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum Request {
    /// Sent to a `detect` process; expects a `Report` (schema only — signals carry no `value`).
    Detect,
    /// Sent to a `run <id>` process each heartbeat; expects a `Report` (with live signal values).
    /// `inputs` is present only when the registry wires `input=<peer>`; omitted otherwise.
    Apply {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inputs: Option<Inputs>,
    },
    /// Graceful stop; the module restores its devices and replies `{"status":"ok"}`.
    Shutdown,
}

/// Routed signals relayed from wired source instances, keyed by the source `module:id` so a consumer
/// wired to several `input=` peers can attribute each signal to its source and keys never collide.
/// aiolos relays them verbatim and uninterpreted — the consumer selects what it needs (typically
/// producers with `labels.type == "temperature"`, optionally filtered by the `module:` key prefix).
pub type Inputs = HashMap<String, Vec<Signal>>;

impl Request {
    /// Serialize to a single JSON line (no trailing newline — the caller adds `\n`).
    pub fn to_line(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }
    pub fn from_line(line: &str) -> serde_json::Result<Self> {
        serde_json::from_str(line)
    }
}

// ---------------------------------------------------------------------------
// Entities: unit → component → signal
// ---------------------------------------------------------------------------

/// A piece of hardware. Assembled by the orchestrator across all anemoi by `id`: the same physical
/// unit reported by several anemoi (e.g. the motherboard's temps and fans) merges into one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unit {
    /// Stable, system-derived identity (never shown to users).
    pub id: String,
    /// Reserved keys: `type` (e.g. `gpu`/`board`/`ssd`/`ups`), `name`, `description`; plus arbitrary.
    #[serde(default, skip_serializing_if = "Labels::is_empty")]
    pub labels: Labels,
}

impl Unit {
    pub fn new(id: impl Into<String>) -> Self {
        Unit {
            id: id.into(),
            labels: Labels::new(),
        }
    }
    /// Set a label (chainable).
    pub fn label(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.labels.insert(key.into(), val.into());
        self
    }
    /// Reserved-label convenience: `name`, `description`, `type`.
    pub fn name(self, v: impl Into<String>) -> Self {
        self.label("name", v)
    }
    pub fn description(self, v: impl Into<String>) -> Self {
        self.label("description", v)
    }
    pub fn typed(self, v: impl Into<String>) -> Self {
        self.label("type", v)
    }
}

/// A sub-thing within a unit (a fan, a temperature sensor group, a CPU socket, a DIMM).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Component {
    /// Stable identity (never shown). Conventionally namespaced by its unit.
    pub id: String,
    /// Parent unit id (structural).
    pub unit: String,
    /// Reserved keys: `type` (e.g. `fan`/`temperature`/`cpu`/`dimm`), `name`, `description`; plus arbitrary.
    #[serde(default, skip_serializing_if = "Labels::is_empty")]
    pub labels: Labels,
}

impl Component {
    pub fn new(id: impl Into<String>, unit: impl Into<String>) -> Self {
        Component {
            id: id.into(),
            unit: unit.into(),
            labels: Labels::new(),
        }
    }
    pub fn label(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.labels.insert(key.into(), val.into());
        self
    }
    pub fn name(self, v: impl Into<String>) -> Self {
        self.label("name", v)
    }
    pub fn description(self, v: impl Into<String>) -> Self {
        self.label("description", v)
    }
    pub fn typed(self, v: impl Into<String>) -> Self {
        self.label("type", v)
    }
}

/// Whether a signal is a read-only measurement or a controllable output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Producer,
    Sink,
}

/// Control ownership state of a sink, as the module observes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SinkState {
    /// Firmware/auto owns it (not claimed by aiolos).
    #[default]
    Released,
    /// aiolos owns it (manual control asserted).
    Claimed,
    /// Claimed, but the readback disagrees with the command (something else moved it).
    Diverged,
    /// State could not be determined this tick.
    Unknown,
}

/// One driver of a sink: which producer signal contributed, and its value (for "what drives what").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    /// The source producer signal's stable `id`.
    pub signal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uom: Option<String>,
}

impl Provenance {
    pub fn new(signal: impl Into<String>) -> Self {
        Provenance {
            signal: signal.into(),
            value: None,
            uom: None,
        }
    }
    pub fn value(mut self, value: impl Into<Value>) -> Self {
        self.value = Some(value.into());
        self
    }
    pub fn uom(mut self, uom: impl Into<String>) -> Self {
        self.uom = Some(uom.into());
        self
    }
}

/// Sink-only control metadata (typed, not labels — the control engine depends on these).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Control {
    /// True if the sink must be claimed (manual control asserted) before it can be set.
    #[serde(default)]
    pub needs_claim: bool,
    /// Current ownership state.
    #[serde(default)]
    pub state: SinkState,
    /// The value to drive on fail-safe (e.g. `"auto"`/`"default"`/`100`) — the safe direction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safe: Option<Value>,
    /// Free-form semantics of the axis (e.g. `up=more-cooling`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    /// The id of the producer signal that reads back this sink's actual value (for verification).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readback: Option<String>,
    /// What drove the current value (provenance / "what drives what").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub driven_by: Vec<Provenance>,
}

/// The atom: a producer (read-only) or sink (controllable) within a component.
///
/// `value` is omitted in schema-only `detect` output and present in live reports. Reserved labels:
/// `type` (the semantic kind / open tag), `name`, `description`. `uom` is the unit of MEASURE.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signal {
    /// Stable, system-derived identity — the time-series key (never shown).
    pub id: String,
    /// Parent component id (structural).
    pub component: String,
    /// Producer (measurement) or sink (control).
    pub role: Role,
    /// Current value (absent in pure schema/detect; present in live reports).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// Unit of measure (e.g. `C`, `%`, `rpm`, `V`, `s`, `mW`). Distinct from the `unit` entity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uom: Option<String>,
    /// Valid value domain `[min, max]` where meaningful.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<[f64; 2]>,
    /// Reserved keys `type`/`name`/`description`; plus arbitrary (vendor, zone, slot, …).
    #[serde(default, skip_serializing_if = "Labels::is_empty")]
    pub labels: Labels,
    /// Sink-only control metadata; `None`/absent for producers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<Control>,
}

impl Signal {
    /// A read-only measurement. `kind` is the semantic `type` label (e.g. `temperature`).
    pub fn producer(
        id: impl Into<String>,
        component: impl Into<String>,
        kind: impl Into<String>,
    ) -> Self {
        let mut labels = Labels::new();
        labels.insert("type".to_string(), kind.into());
        Signal {
            id: id.into(),
            component: component.into(),
            role: Role::Producer,
            value: None,
            uom: None,
            range: None,
            labels,
            control: None,
        }
    }

    /// A controllable output. `kind` is the semantic `type` label (e.g. `fan-duty`). Starts with an
    /// empty (default) `Control`; refine it with `.control(..)` or the `with_*` helpers.
    pub fn sink(
        id: impl Into<String>,
        component: impl Into<String>,
        kind: impl Into<String>,
    ) -> Self {
        let mut labels = Labels::new();
        labels.insert("type".to_string(), kind.into());
        Signal {
            id: id.into(),
            component: component.into(),
            role: Role::Sink,
            value: None,
            uom: None,
            range: None,
            labels,
            control: Some(Control::default()),
        }
    }

    pub fn value(mut self, value: impl Into<Value>) -> Self {
        self.value = Some(value.into());
        self
    }
    pub fn uom(mut self, uom: impl Into<String>) -> Self {
        self.uom = Some(uom.into());
        self
    }
    pub fn range(mut self, min: f64, max: f64) -> Self {
        self.range = Some([min, max]);
        self
    }
    pub fn label(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.labels.insert(key.into(), val.into());
        self
    }
    pub fn name(self, v: impl Into<String>) -> Self {
        self.label("name", v)
    }
    pub fn description(self, v: impl Into<String>) -> Self {
        self.label("description", v)
    }
    /// Replace the sink's control metadata (no-op semantics on a producer, but allowed).
    pub fn control(mut self, control: Control) -> Self {
        self.control = Some(control);
        self
    }
    /// The semantic `type` label, if set.
    pub fn kind(&self) -> Option<&str> {
        self.labels.get("type").map(String::as_str)
    }
    /// Read `value` as i64 (handles ints and whole floats).
    pub fn value_i64(&self) -> Option<i64> {
        self.value
            .as_ref()
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f.round() as i64)))
    }
    pub fn value_f64(&self) -> Option<f64> {
        self.value.as_ref().and_then(Value::as_f64)
    }
}

// ---------------------------------------------------------------------------
// Responses: module → aiolos
// ---------------------------------------------------------------------------

/// Outcome a module declares on every `detect`/report (the supervisor reacts EXPLICITLY — never
/// inferring faults from empty data, exit, or silence):
/// - `ok`     — did the job; `units`/`components`/`signals` authoritative (empty is real). An
///   accompanying `error` is a non-fatal warning ("done, with errors").
/// - `error`  — transient: could NOT do the job this time (NOT "no devices"). Keep instances, retry.
/// - `fatal`  — cannot work on this host. Long-backoff retry; surfaced. Never inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Ok,
    Error,
    Fatal,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Error => "error",
            Status::Fatal => "fatal",
        }
    }
}

/// The response to BOTH `detect` and `apply` — one shape. `detect` emits schema only (signals carry
/// no `value`); a live report includes values. `units`/`components`/`signals` are meaningful only
/// when `status == ok`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    #[serde(default)]
    pub status: Status,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub units: Vec<Unit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<Component>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<Signal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Report {
    pub fn ok(units: Vec<Unit>, components: Vec<Component>, signals: Vec<Signal>) -> Self {
        Report {
            status: Status::Ok,
            units,
            components,
            signals,
            error: None,
        }
    }
    /// `ok` with a non-fatal warning ("done, with errors").
    pub fn ok_warn(
        units: Vec<Unit>,
        components: Vec<Component>,
        signals: Vec<Signal>,
        msg: impl Into<String>,
    ) -> Self {
        Report {
            status: Status::Ok,
            units,
            components,
            signals,
            error: Some(msg.into()),
        }
    }
    /// An `ok` report carrying no entities (e.g. a control tick that reports nothing new).
    pub fn ok_empty() -> Self {
        Report {
            status: Status::Ok,
            units: Vec::new(),
            components: Vec::new(),
            signals: Vec::new(),
            error: None,
        }
    }
    pub fn error(msg: impl Into<String>) -> Self {
        Report {
            status: Status::Error,
            units: Vec::new(),
            components: Vec::new(),
            signals: Vec::new(),
            error: Some(msg.into()),
        }
    }
    pub fn fatal(msg: impl Into<String>) -> Self {
        Report {
            status: Status::Fatal,
            units: Vec::new(),
            components: Vec::new(),
            signals: Vec::new(),
            error: Some(msg.into()),
        }
    }
    pub fn to_line(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }
    pub fn from_line(line: &str) -> serde_json::Result<Self> {
        serde_json::from_str(line)
    }
}

/// Optional one-line greeting a module may emit once at startup (the only unsolicited line).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub hello: HelloBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloBody {
    pub proto: u32,
    pub name: String,
    pub modes: Vec<String>,
}

impl Hello {
    pub fn to_line(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }
    pub fn from_line(line: &str) -> serde_json::Result<Self> {
        serde_json::from_str(line)
    }
}

/// True if a line is an optional `hello` (so the orchestrator can skip it before the real reply).
pub fn is_hello(line: &str) -> bool {
    serde_json::from_str::<Hello>(line).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detect_request_round_trip() {
        let line = r#"{"cmd":"detect"}"#;
        assert_eq!(Request::from_line(line).unwrap().to_line().unwrap(), line);
    }

    #[test]
    fn shutdown_request_round_trip() {
        let line = r#"{"cmd":"shutdown"}"#;
        assert_eq!(Request::from_line(line).unwrap().to_line().unwrap(), line);
    }

    #[test]
    fn apply_without_inputs_omits_field() {
        let req = Request::Apply { inputs: None };
        assert_eq!(req.to_line().unwrap(), r#"{"cmd":"apply"}"#);
        assert_eq!(Request::from_line(r#"{"cmd":"apply"}"#).unwrap(), req);
    }

    #[test]
    fn apply_with_inputs_round_trip() {
        // A routed producer signal, keyed by the source module:id.
        let line = r#"{"cmd":"apply","inputs":{"nvidia:GPU-1":[{"id":"nvidia:GPU-1:temperature:temp","component":"nvidia:GPU-1:temperature","role":"producer","value":63,"uom":"C","labels":{"type":"temperature"}}]}}"#;
        let req = Request::from_line(line).unwrap();
        let Request::Apply {
            inputs: Some(inputs),
        } = &req
        else {
            panic!("expected Apply with inputs");
        };
        let gpu = inputs.get("nvidia:GPU-1").unwrap();
        assert_eq!(gpu[0].value_i64(), Some(63));
        assert_eq!(gpu[0].kind(), Some("temperature"));
        assert_eq!(req.to_line().unwrap(), line);
    }

    #[test]
    fn detect_report_is_schema_only() {
        // detect: signals carry no value; labels present.
        let report = Report::ok(
            vec![Unit::new("nvml:GPU-1")
                .name("gpu0")
                .description("NVIDIA RTX PRO 6000")
                .typed("gpu")],
            vec![Component::new("nvml:GPU-1:fan0", "nvml:GPU-1")
                .name("fan0")
                .typed("fan")],
            vec![
                Signal::producer("nvml:GPU-1:fan0:rpm", "nvml:GPU-1:fan0", "fan-rpm").uom("rpm"),
                Signal::sink("nvml:GPU-1:fan0:duty", "nvml:GPU-1:fan0", "fan-duty")
                    .uom("%")
                    .range(0.0, 100.0),
            ],
        );
        let line = report.to_line().unwrap();
        assert!(
            !line.contains("\"value\""),
            "detect must omit values: {line}"
        );
        let back = Report::from_line(&line).unwrap();
        assert_eq!(back, report);
        // labels survive; type readable.
        assert_eq!(back.units[0].labels.get("name").unwrap(), "gpu0");
        assert_eq!(back.signals[1].kind(), Some("fan-duty"));
    }

    #[test]
    fn live_report_round_trip_with_sink_control() {
        let sink = Signal::sink("nvml:GPU-1:fan0:duty", "nvml:GPU-1:fan0", "fan-duty")
            .value(json!(32))
            .uom("%")
            .range(0.0, 100.0)
            .name("duty")
            .control(Control {
                needs_claim: true,
                state: SinkState::Claimed,
                safe: Some(json!("auto")),
                direction: Some("up=more-cooling".into()),
                readback: Some("nvml:GPU-1:fan0:rpm".into()),
                driven_by: vec![Provenance::new("nvml:GPU-1:temperature:temp")
                    .value(json!(27))
                    .uom("C")],
            });
        let report = Report::ok(
            vec![Unit::new("nvml:GPU-1").name("gpu0")],
            vec![Component::new("nvml:GPU-1:fan0", "nvml:GPU-1")
                .name("fan0")
                .typed("fan")],
            vec![
                Signal::producer("nvml:GPU-1:fan0:rpm", "nvml:GPU-1:fan0", "fan-rpm")
                    .value(json!(1247))
                    .uom("rpm"),
                sink,
            ],
        );
        let line = report.to_line().unwrap();
        let back = Report::from_line(&line).unwrap();
        assert_eq!(back, report);
        let s = &back.signals[1];
        assert_eq!(s.role, Role::Sink);
        assert_eq!(s.value_i64(), Some(32));
        let ctrl = s.control.as_ref().unwrap();
        assert_eq!(ctrl.state, SinkState::Claimed);
        assert!(ctrl.needs_claim);
        assert_eq!(ctrl.driven_by[0].signal, "nvml:GPU-1:temperature:temp");
    }

    #[test]
    fn producer_has_no_control_field_on_wire() {
        let p = Signal::producer("u:c:temp", "u:c", "temperature")
            .value(json!(40))
            .uom("C");
        let line = serde_json::to_string(&p).unwrap();
        assert!(
            !line.contains("control"),
            "producer must omit control: {line}"
        );
        assert!(!line.contains("range"), "unset range omitted: {line}");
    }

    #[test]
    fn report_status_defaults_ok_for_legacy() {
        let r = Report::from_line(r#"{"signals":[]}"#).unwrap();
        assert_eq!(r.status, Status::Ok);
        assert!(r.signals.is_empty());
    }

    #[test]
    fn report_error_and_fatal() {
        let e = Report::error("NVML init failed");
        assert_eq!(e.status, Status::Error);
        assert_eq!(
            e.to_line().unwrap(),
            r#"{"status":"error","error":"NVML init failed"}"#
        );
        let f = Report::fatal("no /dev/ipmi0");
        assert_eq!(f.status, Status::Fatal);
        assert_eq!(Report::from_line(&f.to_line().unwrap()).unwrap(), f);
    }

    #[test]
    fn hello_detection_is_distinct() {
        let hello = r#"{"hello":{"proto":2,"name":"nvidia","modes":["detect","run"]}}"#;
        assert!(is_hello(hello));
        assert!(!is_hello(r#"{"status":"ok","signals":[]}"#));
        assert!(!is_hello(r#"{"status":"error","error":"x"}"#));
        let h = Hello::from_line(hello).unwrap();
        assert_eq!(h.hello.proto, PROTO_VERSION);
    }

    #[test]
    fn malformed_line_is_error_not_panic() {
        assert!(Request::from_line("not json").is_err());
        assert!(Report::from_line("{").is_err());
    }

    #[test]
    fn labels_serialize_in_stable_order() {
        // BTreeMap → keys sorted, so the wire line is deterministic.
        let u = Unit::new("x").typed("gpu").name("gpu0").description("d");
        assert_eq!(
            serde_json::to_string(&u).unwrap(),
            r#"{"id":"x","labels":{"description":"d","name":"gpu0","type":"gpu"}}"#
        );
    }
}
