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

/// Readings relayed from source modules' instances, keyed by `module:id` (the source module name
/// and the peer instance id), so a consumer wired to multiple `input=` sources can attribute each
/// reading to its source module and keys never collide across sources.
///
/// Each peer instance reports a *list* of readings (temp, fan, …); aiolos relays the whole list
/// verbatim and uninterpreted (it never picks "the temperature" — the consumer decides, optionally
/// filtering by the `module:` key prefix). Mirrors the protocol spec's normative text.
pub type Inputs = HashMap<String, Vec<Reading>>;

// ---------------------------------------------------------------------------
// Readings
// ---------------------------------------------------------------------------

/// One measurement/actuation record a module reports. `kind` (`temp`/`fan`/…) and `label` are
/// required; all other numeric/string fields live in `fields` (flattened onto the JSON object).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    #[serde(rename = "type")]
    pub kind: String,
    pub label: String,
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

impl Reading {
    /// Build a reading. `fields` should be a JSON object (e.g. `json!({"temp":63})`); a non-object
    /// is treated as empty.
    pub fn new(kind: impl Into<String>, label: impl Into<String>, fields: Value) -> Self {
        Reading {
            kind: kind.into(),
            label: label.into(),
            fields: match fields {
                Value::Object(m) => m,
                _ => Map::new(),
            },
        }
    }

    /// Read a numeric field as i64 (handles ints and whole floats).
    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.fields
            .get(key)
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f.round() as i64)))
    }
}

// ---------------------------------------------------------------------------
// Responses: module → aiolos
// ---------------------------------------------------------------------------

/// Outcome a module declares on every `detect`/`apply` (and the supervisor reacts to EXPLICITLY —
/// it never infers faults from empty data, exits, or silence):
/// - `ok`     — the module did its job; `found`/`readings` are authoritative (empty is real). An
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
    /// Extra descriptive fields (surfaced on the status page). Empty by default.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}

/// Response to `apply` (and `shutdown`). `readings` is meaningful only when `status == ok`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Applied {
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readings: Option<Vec<Reading>>,
}

impl Applied {
    pub fn ok(readings: Vec<Reading>) -> Self {
        Applied {
            status: Status::Ok,
            error: None,
            readings: Some(readings),
        }
    }

    pub fn ok_empty() -> Self {
        Applied {
            status: Status::Ok,
            error: None,
            readings: None,
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Applied {
            status: Status::Error,
            error: Some(msg.into()),
            readings: None,
        }
    }

    pub fn fatal(msg: impl Into<String>) -> Self {
        Applied {
            status: Status::Fatal,
            error: Some(msg.into()),
            readings: None,
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
        let line = r#"{"cmd":"apply","inputs":{"gpu0":[{"type":"temp","label":"GPU","temp":63}]}}"#;
        let req = Request::from_line(line).unwrap();
        let Request::Apply {
            inputs: Some(inputs),
        } = &req
        else {
            panic!("expected Apply with inputs");
        };
        let gpu0 = inputs.get("gpu0").unwrap();
        assert_eq!(gpu0[0].get_i64("temp"), Some(63));
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
            r#"{"status":"ok","readings":[{"type":"temp","label":"GPU","temp":63}]}"#,
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
    fn reading_helper_and_extra_fields() {
        let r = Reading::new("fan", "fan0", json!({"pwm": 72, "rpm": 2200}));
        assert_eq!(r.get_i64("pwm"), Some(72));
        assert_eq!(r.get_i64("rpm"), Some(2200));
        // Non-object fields degrade to empty rather than corrupting the stream.
        let empty = Reading::new("temp", "x", json!(5));
        assert!(empty.fields.is_empty());
    }
}
