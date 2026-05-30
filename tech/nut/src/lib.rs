//! Level-1 tech: NUT (Network UPS Tools) client via the system `upsc` binary.
//!
//! We shell out to `upsc` rather than re-implementing the upsd TCP protocol. Rationale:
//! - `upsc` is NUT's canonical read-only client; it already handles the upsd socket, SSL, and the
//!   `LIST`/`GET` framing — re-implementing that protocol would add risk for no benefit here.
//! - It keeps **no credentials in this code**: `upsc <ups>` reads public UPS *variables* (no login).
//!   The UPS identifier (`name` or `name@host[:port]`) is supplied by the *operator config* of the
//!   `nut` anemos, never hardcoded here. For a remote/authenticated upsd the operator points the id
//!   at `ups@host` and configures NUT's own `upsmon`/`upssched` out-of-band; this crate only reads.
//!
//! `upsc` writes the `key: value` variables to **stdout** (one per line) and incidental notices
//! (e.g. "Init SSL without certificate database") to **stderr** — so parsing stdout is clean. The
//! binary path is overridable via `$AIOLOS_UPSC_BIN` for off-hardware testing.
//!
//! This crate is read-only: it lists UPS names and reads variables. It controls nothing.

use std::collections::BTreeMap;
use std::process::Command;

/// Default `upsc` client binary. Override via `$AIOLOS_UPSC_BIN` (tests/dev / non-standard install).
const DEFAULT_UPSC_BIN: &str = "upsc";

fn upsc_bin() -> String {
    std::env::var("AIOLOS_UPSC_BIN").unwrap_or_else(|_| DEFAULT_UPSC_BIN.to_string())
}

/// A typed snapshot of one UPS's state, plus the full raw variable map for anything not modelled.
/// All optional fields are `None` when the driver does not expose that variable.
#[derive(Debug, Clone, PartialEq)]
pub struct UpsState {
    /// The UPS identifier this was read from (e.g. "pr3000-nova" or "ups@host").
    pub id: String,
    /// `ups.status` verbatim (space-separated flags), e.g. "OL", "OB", "OB LB", "OL CHRG". Empty if
    /// the driver did not report it (treated as unknown by the consumer).
    pub status: String,
    /// `battery.charge` percent (0–100), if reported.
    pub charge_pct: Option<i64>,
    /// `battery.runtime` — estimated seconds of runtime left on battery, if reported.
    pub runtime_s: Option<i64>,
    /// `ups.load` percent of rated load, if reported.
    pub load_pct: Option<i64>,
    /// `input.voltage` (mains volts), if reported.
    pub input_voltage: Option<f64>,
    /// Model string (`device.model`/`ups.model`), if reported.
    pub model: Option<String>,
    /// Every variable `upsc` reported, raw (so a consumer can read anything not modelled above).
    pub vars: BTreeMap<String, String>,
}

impl UpsState {
    /// Online-on-mains: the `OL` flag is present in `ups.status` AND the on-battery `OB` flag is not.
    /// NUT can briefly report both during a transition; treating `OB` as authoritative is the
    /// conservative reading (assume on-battery if the flag is set).
    pub fn on_line(&self) -> bool {
        let flags = status_flags(&self.status);
        flags.contains(&"OL") && !flags.contains(&"OB")
    }

    /// On-battery: the `OB` flag is present in `ups.status`. Authoritative over `OL` if both appear.
    pub fn on_battery(&self) -> bool {
        status_flags(&self.status).contains(&"OB")
    }

    /// Low-battery: the `LB` flag is present (NUT's "shut down now" warning level).
    pub fn low_battery(&self) -> bool {
        status_flags(&self.status).contains(&"LB")
    }
}

/// Split a NUT `ups.status` string into its space-separated flag tokens (e.g. "OL CHRG" -> [OL,CHRG]).
pub fn status_flags(status: &str) -> Vec<&str> {
    status.split_whitespace().collect()
}

/// List the UPS names known to the local upsd (`upsc -l`). Empty if `upsc` is missing, upsd is down,
/// or none are configured (the caller treats "none" as a real, declared result). Names only — no host.
pub fn list() -> Vec<String> {
    let Ok(out) = Command::new(upsc_bin()).arg("-l").output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_list(&String::from_utf8_lossy(&out.stdout))
}

/// Parse `upsc -l` stdout (one UPS name per line) into a trimmed, non-empty list. Pure (testable).
pub fn parse_list(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Read one UPS's variables via `upsc <id>` and parse them into a [`UpsState`].
///
/// `Err` carries a human reason (binary missing, upsd unreachable, or `upsc` exited non-zero) — the
/// anemos surfaces that as a transient `status:error`, never a crash. `id` is the operator-supplied
/// UPS identifier (`name` or `name@host[:port]`); this function passes it through unmodified.
pub fn read(id: &str) -> Result<UpsState, String> {
    let out = Command::new(upsc_bin())
        .arg(id)
        .output()
        .map_err(|e| format!("running upsc: {e}"))?;
    if !out.status.success() {
        // upsc prints the reason (e.g. "Error: Connection failure") to stderr.
        let err = String::from_utf8_lossy(&out.stderr);
        let reason = err.lines().last().unwrap_or("").trim();
        return Err(format!(
            "upsc {id} failed{}",
            if reason.is_empty() {
                String::new()
            } else {
                format!(": {reason}")
            }
        ));
    }
    Ok(parse_vars(id, &String::from_utf8_lossy(&out.stdout)))
}

/// Parse `upsc <ups>` stdout (`key: value` lines) into a [`UpsState`]. Pure (testable). Unknown or
/// unparseable numeric fields are simply left `None`; everything still lands in `vars`.
pub fn parse_vars(id: &str, stdout: &str) -> UpsState {
    let mut vars = BTreeMap::new();
    for line in stdout.lines() {
        // NUT variables are `key: value`; the value may itself contain ": " (rare), so split once.
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim();
            let val = v.trim();
            if !key.is_empty() {
                vars.insert(key.to_string(), val.to_string());
            }
        }
    }
    let num_i = |k: &str| vars.get(k).and_then(|v| v.parse::<i64>().ok());
    let num_f = |k: &str| vars.get(k).and_then(|v| v.parse::<f64>().ok());
    UpsState {
        id: id.to_string(),
        status: vars.get("ups.status").cloned().unwrap_or_default(),
        charge_pct: num_i("battery.charge"),
        runtime_s: num_i("battery.runtime"),
        load_pct: num_i("ups.load"),
        input_voltage: num_f("input.voltage"),
        model: vars
            .get("device.model")
            .or_else(|| vars.get("ups.model"))
            .cloned(),
        vars,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
battery.charge: 100
battery.runtime: 697
battery.type: PbAcid
device.model: PR3000ERT2U
input.voltage: 219.0
ups.load: 36
ups.status: OL
";

    #[test]
    fn parses_typed_fields_and_keeps_raw_vars() {
        let s = parse_vars("pr3000-nova", SAMPLE);
        assert_eq!(s.id, "pr3000-nova");
        assert_eq!(s.status, "OL");
        assert_eq!(s.charge_pct, Some(100));
        assert_eq!(s.runtime_s, Some(697));
        assert_eq!(s.load_pct, Some(36));
        assert_eq!(s.input_voltage, Some(219.0));
        assert_eq!(s.model.as_deref(), Some("PR3000ERT2U"));
        // Raw map retains everything (incl. fields we don't model).
        assert_eq!(
            s.vars.get("battery.type").map(String::as_str),
            Some("PbAcid")
        );
        assert!(s.on_line());
        assert!(!s.on_battery());
        assert!(!s.low_battery());
    }

    #[test]
    fn missing_fields_become_none_not_a_panic() {
        let s = parse_vars("ups0", "ups.status: OB DISCHRG\nfoo: bar\n");
        assert_eq!(s.status, "OB DISCHRG");
        assert_eq!(s.charge_pct, None);
        assert_eq!(s.runtime_s, None);
        assert_eq!(s.load_pct, None);
        assert_eq!(s.input_voltage, None);
        assert_eq!(s.model, None);
        assert!(s.on_battery());
        assert!(!s.on_line());
    }

    #[test]
    fn status_flags_and_transition_is_conservative() {
        // A clean online state.
        assert!(parse_vars("u", "ups.status: OL\n").on_line());
        // Charging while online is still online.
        let chrg = parse_vars("u", "ups.status: OL CHRG\n");
        assert!(chrg.on_line());
        assert!(!chrg.on_battery());
        // Both flags during a transition -> OB wins (assume on-battery; do not claim on_line).
        let both = parse_vars("u", "ups.status: OL OB\n");
        assert!(both.on_battery());
        assert!(!both.on_line());
        // Low-battery flag detected.
        assert!(parse_vars("u", "ups.status: OB LB\n").low_battery());
        // Empty/unknown status -> neither.
        let none = parse_vars("u", "battery.charge: 50\n");
        assert!(!none.on_line());
        assert!(!none.on_battery());
    }

    #[test]
    fn value_containing_a_colon_is_preserved() {
        // `split_once(':')` keeps the remainder intact (value may contain a colon).
        let s = parse_vars("u", "ups.test.result: Done: passed\n");
        assert_eq!(
            s.vars.get("ups.test.result").map(String::as_str),
            Some("Done: passed")
        );
    }

    #[test]
    fn parse_list_trims_and_drops_blanks() {
        assert_eq!(
            parse_list("pr3000-nova\n  ups2  \n\n"),
            vec!["pr3000-nova".to_string(), "ups2".to_string()]
        );
        assert!(parse_list("").is_empty());
    }
}
