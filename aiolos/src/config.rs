//! Configuration: the registry plus global tunables, parsed from `aiolos.conf`.
//!
//! File format (one directive per line; blank lines and `#` comments ignored):
//! - a `key=value` line sets a global: `tick=`, `timeout=`, `detect_every=` (seconds),
//!   `status_bind=` (host:port).
//! - any other line is a module line (`<name> [input=<peer>]`) — see `registry`.
//!
//! Paths are overridable via env for testing/packaging:
//! - `AIOLOS_CONF`    — config file path (default `/opt/aiolos/etc/aiolos.conf`)
//! - `AIOLOS_BIN_DIR` — directory holding module binaries (default `/opt/aiolos/bin`)

use crate::registry::{parse_module_line, RegistryEntry};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::Duration;
use tracing::warn;

const DEFAULT_CONF: &str = "/opt/aiolos/etc/aiolos.conf";
const DEFAULT_BIN_DIR: &str = "/opt/aiolos/bin";
const DEFAULT_STATUS_BIND: &str = "0.0.0.0:9876";
const DEFAULT_TICK_SECS: u64 = 3;
const DEFAULT_TIMEOUT_SECS: u64 = 2;
const DEFAULT_DETECT_EVERY_SECS: u64 = 10;

#[derive(Debug, Clone)]
pub struct Config {
    pub registry: Vec<RegistryEntry>,
    /// Heartbeat period.
    pub tick: Duration,
    /// Max wait for an `apply` response before the instance is killed (`< tick`).
    pub timeout: Duration,
    /// How often `detect` is re-run for hotplug reconciliation.
    pub detect_every: Duration,
    /// `host:port` the read-only status page binds to.
    pub status_bind: String,
    /// Directory holding module binaries (`<bin_dir>/<module>`).
    pub bin_dir: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            registry: Vec::new(),
            tick: Duration::from_secs(DEFAULT_TICK_SECS),
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            detect_every: Duration::from_secs(DEFAULT_DETECT_EVERY_SECS),
            status_bind: DEFAULT_STATUS_BIND.to_string(),
            bin_dir: PathBuf::from(DEFAULT_BIN_DIR),
        }
    }
}

impl Config {
    /// Load and parse the config from `AIOLOS_CONF` (or the default path).
    pub fn load() -> Result<Self> {
        let conf_path = std::env::var("AIOLOS_CONF").unwrap_or_else(|_| DEFAULT_CONF.to_string());
        let contents = std::fs::read_to_string(&conf_path)
            .with_context(|| format!("reading config {conf_path}"))?;
        let mut cfg = Self::parse(&contents)?;
        if let Ok(dir) = std::env::var("AIOLOS_BIN_DIR") {
            cfg.bin_dir = PathBuf::from(dir);
        }
        Ok(cfg)
    }

    /// Parse config text (pure; used by `load` and by tests). Does not touch env.
    pub fn parse(contents: &str) -> Result<Self> {
        let mut cfg = Config::default();
        let mut tick_secs = DEFAULT_TICK_SECS;
        let mut timeout_secs = DEFAULT_TIMEOUT_SECS;

        for raw in contents.lines() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }

            // A global is a single `key=value` token with no whitespace before the `=`.
            if is_global_line(line) {
                let (key, val) = line.split_once('=').expect("is_global_line guarantees '='");
                let (key, val) = (key.trim(), val.trim());
                match key {
                    "tick" => tick_secs = parse_secs(key, val)?,
                    "timeout" => timeout_secs = parse_secs(key, val)?,
                    "detect_every" => cfg.detect_every = Duration::from_secs(parse_secs(key, val)?),
                    "status_bind" => cfg.status_bind = val.to_string(),
                    other => warn!(key = %other, "unknown global directive ignored"),
                }
                continue;
            }

            cfg.registry.push(parse_module_line(line));
        }

        // Invariant: 0 < timeout < tick. Clamp forgivingly rather than refusing to boot.
        if tick_secs == 0 {
            warn!("tick=0 invalid; using default {DEFAULT_TICK_SECS}s");
            tick_secs = DEFAULT_TICK_SECS;
        }
        if tick_secs < 2 {
            // timeout is integer seconds, so tick must be >= 2 to leave room for timeout < tick.
            warn!(
                tick = tick_secs,
                "tick too small for a sub-second timeout; using 2s"
            );
            tick_secs = 2;
        }
        if timeout_secs == 0 || timeout_secs >= tick_secs {
            let clamped = tick_secs.div_ceil(2); // < tick, and >= 1 for tick >= 1
            warn!(
                tick = tick_secs,
                requested_timeout = timeout_secs,
                clamped_timeout = clamped,
                "timeout must be in (0, tick); clamping"
            );
            timeout_secs = clamped;
        }
        cfg.tick = Duration::from_secs(tick_secs);
        cfg.timeout = Duration::from_secs(timeout_secs);

        Ok(cfg)
    }
}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(i) => &line[..i],
        None => line,
    }
}

/// True when the first whitespace-delimited token contains `=` (i.e. a `key=value` global),
/// as opposed to a module line whose first token is a bare module name.
fn is_global_line(line: &str) -> bool {
    match line.split_whitespace().next() {
        Some(first) => first.contains('='),
        None => false,
    }
}

fn parse_secs(key: &str, val: &str) -> Result<u64> {
    val.parse::<u64>()
        .with_context(|| format!("global '{key}' must be a non-negative integer, got '{val}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_empty() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.tick, Duration::from_secs(3));
        assert_eq!(c.timeout, Duration::from_secs(2));
        assert_eq!(c.detect_every, Duration::from_secs(10));
        assert_eq!(c.status_bind, "0.0.0.0:9876");
        assert!(c.registry.is_empty());
    }

    #[test]
    fn parses_globals_and_modules() {
        let text = "\
# aiolos config
tick=5
timeout=3
detect_every=20
status_bind=127.0.0.1:9000

nvidia
asrock16-2t  input=nvidia   # board fans follow GPU temps
";
        let c = Config::parse(text).unwrap();
        assert_eq!(c.tick, Duration::from_secs(5));
        assert_eq!(c.timeout, Duration::from_secs(3));
        assert_eq!(c.detect_every, Duration::from_secs(20));
        assert_eq!(c.status_bind, "127.0.0.1:9000");
        assert_eq!(c.registry.len(), 2);
        assert_eq!(c.registry[0].module_name, "nvidia");
        assert_eq!(c.registry[1].input.as_deref(), Some("nvidia"));
    }

    #[test]
    fn timeout_clamped_below_tick() {
        let c = Config::parse("tick=3\ntimeout=9").unwrap();
        assert!(
            c.timeout < c.tick,
            "timeout {:?} !< tick {:?}",
            c.timeout,
            c.tick
        );
        assert_eq!(c.timeout, Duration::from_secs(2));
    }

    #[test]
    fn zero_timeout_clamped() {
        let c = Config::parse("tick=4\ntimeout=0").unwrap();
        assert_eq!(c.timeout, Duration::from_secs(2));
    }

    #[test]
    fn bad_global_value_errors() {
        assert!(Config::parse("tick=abc").is_err());
    }

    #[test]
    fn tick_floored_to_two_with_valid_timeout() {
        let c = Config::parse("tick=1\ntimeout=1").unwrap();
        assert!(c.tick >= Duration::from_secs(2));
        assert!(c.timeout < c.tick);
    }

    #[test]
    fn module_name_with_dash_is_not_global() {
        // 'asrock16-2t' contains no '=', so it is a module line, not a global.
        let c = Config::parse("asrock16-2t").unwrap();
        assert_eq!(c.registry.len(), 1);
        assert_eq!(c.registry[0].module_name, "asrock16-2t");
    }
}
