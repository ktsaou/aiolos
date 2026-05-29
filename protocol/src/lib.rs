//! aiolos ↔ anemos wire protocol types.
//!
//! One line = one complete JSON object. Requests flow aiolos → module (the module's stdin);
//! responses flow module → aiolos (the module's stdout). stdout is protocol-only; all logs go
//! to stderr. Authoritative contract: `.agents/sow/specs/aiolos-protocol.spec.md`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;

mod curve;
pub use curve::{Curve, CurveCache};

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

/// Readings relayed from another module's instances, keyed by that peer's `id`.
///
/// Each peer instance reports a *list* of readings (temp, fan, …); aiolos relays the whole list
/// verbatim and uninterpreted (it never picks "the temperature" — the consumer decides). This
/// mirrors the protocol spec's normative text ("the most recent readings … keyed by their id").
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

/// Any line a module may emit on stdout. `hello` is the only unsolicited line (optional, at
/// startup); `found` answers `detect`; `applied` answers `apply`/`shutdown`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Response {
    Hello(Hello),
    Found(Found),
    Applied(Applied),
}

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Found {
    pub found: Vec<FoundEntry>,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Error,
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

impl Response {
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
    fn found_response_round_trip() {
        let line = r#"{"found":[{"id":"GPU-uuid-1234","type":"GPU","name":"NVIDIA RTX 6000"}]}"#;
        let resp = Response::from_line(line).unwrap();
        assert!(matches!(resp, Response::Found(_)));
        assert_eq!(resp.to_line().unwrap(), line);
    }

    #[test]
    fn ok_response_round_trip() {
        let line = r#"{"status":"ok","readings":[{"type":"temp","label":"GPU","temp":63}]}"#;
        let resp = Response::from_line(line).unwrap();
        assert!(matches!(resp, Response::Applied(_)));
        assert_eq!(resp.to_line().unwrap(), line);
    }

    #[test]
    fn error_response_round_trip() {
        let line = r#"{"status":"error","error":"gpu lost"}"#;
        let resp = Response::from_line(line).unwrap();
        assert!(matches!(resp, Response::Applied(_)));
        assert_eq!(resp.to_line().unwrap(), line);
    }

    #[test]
    fn hello_response_parses_and_is_distinct() {
        let line = r#"{"hello":{"proto":1,"name":"nvidia","modes":["detect","run"]}}"#;
        let resp = Response::from_line(line).unwrap();
        let Response::Hello(h) = &resp else {
            panic!("expected Hello, got {resp:?}");
        };
        assert_eq!(h.hello.proto, PROTO_VERSION);
        assert_eq!(resp.to_line().unwrap(), line);
    }

    #[test]
    fn malformed_line_is_error_not_panic() {
        assert!(Request::from_line("not json").is_err());
        assert!(Response::from_line("{").is_err());
        // An object that matches no response variant is rejected.
        assert!(Response::from_line(r#"{"unexpected":true}"#).is_err());
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
