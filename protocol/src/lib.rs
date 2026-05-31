//! aiolos ↔ anemos wire protocol types.
//!
//! One line = one complete JSON object. Requests flow aiolos → module (the module's stdin);
//! responses flow module → aiolos (the module's stdout). stdout is protocol-only; all logs go
//! to stderr. Authoritative contract: `.agents/sow/specs/aiolos-protocol.spec.md`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;

// Wire types only. The module-side SDK (signal-aware stdin, curve, EMA, the run() driver and the
// Anemos/Device traits) lives in the `anemos` crate; the orchestrator depends only on these types.

/// Current wire protocol version (the `proto` field of `hello`).
pub const PROTO_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Requests: aiolos → module
// ---------------------------------------------------------------------------

/// A command from aiolos to a module. Serializes as `{"cmd":"<name>", ...}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum Request {
    /// Sent to a `detect` process; expects a `Found` response.
    Detect,
    /// Sent to a `run <id>` process each heartbeat; expects an `Applied` response.
    /// `inputs` is present only when the registry wires `input=<peer>`; omitted otherwise.
    Apply {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inputs: Option<Inputs>,
    },
    /// Graceful stop; the module restores its device and replies `{"status":"ok"}`.
    Shutdown,
}

/// Component reports relayed from source modules' instances, keyed by `module:id` (the source module
/// name and the peer instance id), so a consumer wired to multiple `input=` sources can attribute each
/// publisher to its source module and keys never collide across sources.
///
/// Each peer instance reports a *list* of components; aiolos relays the whole list verbatim and
/// uninterpreted (it never picks "the temperature" — the consumer decides, optionally filtering by
/// the `module:` key prefix and publisher `kind`). Mirrors the protocol spec's normative text.
pub type Inputs = HashMap<String, Vec<Component>>;

// ---------------------------------------------------------------------------
// Component reports
// ---------------------------------------------------------------------------

/// One device/entity reported by an anemos. This is the stable SOW-0014-ready schema:
/// publishers are measurements/readbacks; sinks are outputs this component can drive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Component {
    pub id: String,
    pub label: String,
    #[serde(rename = "class")]
    pub class: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publishers: Vec<Publisher>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sinks: Vec<Sink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Extra descriptive fields for forward compatibility. Empty by default.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl Component {
    pub fn new(id: impl Into<String>, label: impl Into<String>, class: impl Into<String>) -> Self {
        Component {
            id: id.into(),
            label: label.into(),
            class: class.into(),
            publishers: Vec::new(),
            sinks: Vec::new(),
            icon: None,
            extra: Map::new(),
        }
    }

    pub fn with_publishers(mut self, publishers: Vec<Publisher>) -> Self {
        self.publishers = publishers;
        self
    }

    pub fn with_sinks(mut self, sinks: Vec<Sink>) -> Self {
        self.sinks = sinks;
        self
    }
}

/// One normalised scalar stream published by a component. `value` is omitted in schema-only detect
/// output and present in live `apply` reports/status surfaces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Publisher {
    pub id: String,
    pub label: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<[f64; 2]>,
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl Publisher {
    pub fn new(id: impl Into<String>, label: impl Into<String>, kind: impl Into<String>) -> Self {
        Publisher {
            id: id.into(),
            label: label.into(),
            kind: kind.into(),
            value: None,
            unit: None,
            range: None,
            extra: Map::new(),
        }
    }

    pub fn value(mut self, value: impl Into<Value>) -> Self {
        self.value = Some(value.into());
        self
    }

    pub fn unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    pub fn range(mut self, min: f64, max: f64) -> Self {
        self.range = Some([min, max]);
        self
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SinkState {
    #[default]
    Released,
    Claimed,
    Diverged,
    Unknown,
}

/// One output a component can drive. In SOW-0017 modules still compute/report `value` locally; SOW-0014
/// reuses this detect/report schema and changes apply so aiolos commands sink targets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sink {
    pub id: String,
    pub label: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safe: Option<Value>,
    #[serde(default)]
    pub needs_claim: bool,
    #[serde(default)]
    pub state: SinkState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub driven_by: Vec<DrivenBy>,
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

impl Sink {
    pub fn new(id: impl Into<String>, label: impl Into<String>, kind: impl Into<String>) -> Self {
        Sink {
            id: id.into(),
            label: label.into(),
            kind: kind.into(),
            range: None,
            unit: None,
            value: None,
            readback: None,
            safe: None,
            needs_claim: false,
            state: SinkState::Released,
            direction: None,
            driven_by: Vec::new(),
            extra: Map::new(),
        }
    }

    pub fn range(mut self, min: f64, max: f64) -> Self {
        self.range = Some([min, max]);
        self
    }

    pub fn unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    pub fn value(mut self, value: impl Into<Value>) -> Self {
        self.value = Some(value.into());
        self
    }

    pub fn readback(mut self, publisher_id: impl Into<String>) -> Self {
        self.readback = Some(publisher_id.into());
        self
    }

    pub fn safe(mut self, safe: impl Into<Value>) -> Self {
        self.safe = Some(safe.into());
        self
    }

    pub fn needs_claim(mut self, needs_claim: bool) -> Self {
        self.needs_claim = needs_claim;
        self
    }

    pub fn state(mut self, state: SinkState) -> Self {
        self.state = state;
        self
    }

    pub fn direction(mut self, direction: impl Into<String>) -> Self {
        self.direction = Some(direction.into());
        self
    }

    pub fn driven_by(mut self, driven_by: Vec<DrivenBy>) -> Self {
        self.driven_by = driven_by;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DrivenBy {
    pub from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

impl DrivenBy {
    pub fn new(from: impl Into<String>) -> Self {
        DrivenBy {
            from: from.into(),
            publisher: None,
            value: None,
            unit: None,
        }
    }

    pub fn publisher(mut self, publisher: impl Into<String>) -> Self {
        self.publisher = Some(publisher.into());
        self
    }

    pub fn value(mut self, value: impl Into<Value>) -> Self {
        self.value = Some(value.into());
        self
    }

    pub fn unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }
}

// ---------------------------------------------------------------------------
// Responses: module → aiolos
// ---------------------------------------------------------------------------

/// Outcome a module declares on every `detect`/`apply` (and the supervisor reacts to EXPLICITLY —
/// it never infers faults from empty data, exits, or silence):
/// - `ok`     — the module did its job; `found`/`components` are authoritative (empty is real). An
///   accompanying `error` is a non-fatal warning ("done, with errors").
/// - `error`  — transient: it could NOT do its job this time (NOT "no devices"). Keep going, retry.
/// - `fatal`  — it cannot work on this host (wrong hw, missing capability). Retried only on a long
///   backoff; surfaced/alerted. Never inferred — the module says so.
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

/// Response to `detect`. `found` is meaningful only when `status == ok`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Detected {
    #[serde(default)]
    pub status: Status,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub found: Vec<FoundEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Detected {
    pub fn ok(found: Vec<FoundEntry>) -> Self {
        Detected {
            status: Status::Ok,
            found,
            error: None,
        }
    }
    /// `ok` with a non-fatal warning ("done, with errors").
    pub fn ok_warn(found: Vec<FoundEntry>, msg: impl Into<String>) -> Self {
        Detected {
            status: Status::Ok,
            found,
            error: Some(msg.into()),
        }
    }
    pub fn error(msg: impl Into<String>) -> Self {
        Detected {
            status: Status::Error,
            found: Vec::new(),
            error: Some(msg.into()),
        }
    }
    pub fn fatal(msg: impl Into<String>) -> Self {
        Detected {
            status: Status::Fatal,
            found: Vec::new(),
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FoundEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<Component>,
    /// Extra descriptive fields (surfaced on the status page). Empty by default.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

/// Response to `apply` (and `shutdown`). `components` is meaningful only when `status == ok`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Applied {
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub components: Option<Vec<Component>>,
}

impl Applied {
    pub fn ok(components: Vec<Component>) -> Self {
        Applied {
            status: Status::Ok,
            error: None,
            components: Some(components),
        }
    }

    pub fn ok_empty() -> Self {
        Applied {
            status: Status::Ok,
            error: None,
            components: None,
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Applied {
            status: Status::Error,
            error: Some(msg.into()),
            components: None,
        }
    }

    pub fn fatal(msg: impl Into<String>) -> Self {
        Applied {
            status: Status::Fatal,
            error: Some(msg.into()),
            components: None,
        }
    }

    pub fn to_line(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }
    pub fn from_line(line: &str) -> serde_json::Result<Self> {
        serde_json::from_str(line)
    }
}

// ---------------------------------------------------------------------------
// Line (de)serialization
// ---------------------------------------------------------------------------

impl Request {
    /// Serialize to a single JSON line (no trailing newline — the caller adds `\n`).
    pub fn to_line(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }
    pub fn from_line(line: &str) -> serde_json::Result<Self> {
        serde_json::from_str(line)
    }
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
        // Absent inputs MUST NOT serialize as "inputs":null (spec: absent or {}).
        let req = Request::Apply { inputs: None };
        assert_eq!(req.to_line().unwrap(), r#"{"cmd":"apply"}"#);
        // And it parses back identically.
        assert_eq!(Request::from_line(r#"{"cmd":"apply"}"#).unwrap(), req);
    }

    #[test]
    fn apply_with_inputs_round_trip() {
        let line = r#"{"cmd":"apply","inputs":{"gpu0":[{"id":"gpu0","label":"GPU 0","class":"gpu","publishers":[{"id":"temp","label":"Temperature","kind":"temperature","value":63,"unit":"C"}]}]}}"#;
        let req = Request::from_line(line).unwrap();
        let Request::Apply {
            inputs: Some(inputs),
        } = &req
        else {
            panic!("expected Apply with inputs");
        };
        let gpu0 = inputs.get("gpu0").unwrap();
        assert_eq!(gpu0[0].publishers[0].value_i64(), Some(63));
        assert_eq!(req.to_line().unwrap(), line);
    }

    #[test]
    fn detect_ok_round_trip() {
        let line = r#"{"status":"ok","found":[{"id":"GPU-uuid-1234","type":"GPU","name":"NVIDIA RTX 6000"}]}"#;
        let d = Detected::from_line(line).unwrap();
        assert_eq!(d.status, Status::Ok);
        assert_eq!(d.found.len(), 1);
        assert_eq!(d.to_line().unwrap(), line);
    }

    #[test]
    fn detect_status_defaults_ok_for_legacy_found() {
        // A bare `{"found":[...]}` (no status) is accepted as ok (back-compat / lenient).
        let d = Detected::from_line(r#"{"found":[]}"#).unwrap();
        assert_eq!(d.status, Status::Ok);
        assert!(d.found.is_empty());
    }

    #[test]
    fn detect_error_and_fatal() {
        let e = Detected::error("NVML init failed");
        assert_eq!(e.status, Status::Error);
        assert_eq!(
            e.to_line().unwrap(),
            r#"{"status":"error","error":"NVML init failed"}"#
        );
        let f = Detected::fatal("no /dev/ipmi0");
        assert_eq!(f.status, Status::Fatal);
        assert_eq!(Detected::from_line(&f.to_line().unwrap()).unwrap(), f);
    }

    #[test]
    fn apply_ok_error_fatal_round_trip() {
        for line in [
            r#"{"status":"ok","components":[{"id":"gpu0","label":"GPU 0","class":"gpu","publishers":[{"id":"temp","label":"Temperature","kind":"temperature","value":63,"unit":"C"}]}]}"#,
            r#"{"status":"error","error":"gpu lost"}"#,
            r#"{"status":"fatal","error":"device unsupported"}"#,
        ] {
            let a = Applied::from_line(line).unwrap();
            assert_eq!(a.to_line().unwrap(), line);
        }
    }

    #[test]
    fn hello_detection_is_distinct() {
        let hello = r#"{"hello":{"proto":1,"name":"nvidia","modes":["detect","run"]}}"#;
        assert!(is_hello(hello));
        // Real responses are NOT mistaken for hello.
        assert!(!is_hello(r#"{"status":"ok","found":[]}"#));
        assert!(!is_hello(r#"{"status":"error","error":"x"}"#));
        let h = Hello::from_line(hello).unwrap();
        assert_eq!(h.hello.proto, PROTO_VERSION);
    }

    #[test]
    fn malformed_line_is_error_not_panic() {
        assert!(Request::from_line("not json").is_err());
        assert!(Applied::from_line("{").is_err());
        assert!(Detected::from_line("{").is_err());
    }

    #[test]
    fn publisher_sink_helpers_round_trip() {
        let p = Publisher::new("fan0.rpm", "fan0", "fan-rpm")
            .value(json!(2200))
            .unit("rpm");
        assert_eq!(p.value_i64(), Some(2200));
        let s = Sink::new("fans", "fans", "fan-duty")
            .range(0.0, 100.0)
            .unit("%")
            .value(json!(72))
            .safe(json!("auto"))
            .needs_claim(true)
            .state(SinkState::Claimed)
            .readback("fan0.rpm")
            .direction("up=more-cooling")
            .driven_by(vec![DrivenBy::new("nvidia:GPU-1")
                .publisher("gpu.temp")
                .value(json!(63))
                .unit("C")]);
        let c = Component::new("gpu0", "GPU 0", "gpu")
            .with_publishers(vec![p])
            .with_sinks(vec![s]);
        let line = serde_json::to_string(&c).unwrap();
        let back: Component = serde_json::from_str(&line).unwrap();
        assert_eq!(back, c);
    }
}
