//! Configuration: the registry plus global tunables, parsed from `aiolos.conf`.
//!
//! File format (one directive per line; blank lines and `#` comments ignored):
//! - a `key=value` line sets a global: `base_tick=` (the non-blocking scheduler wake period —
//!   bare number is milliseconds, e.g. `100`; `100ms`/`1s` also accepted), `detect_every=`
//!   (seconds), `max_backoff=` (seconds), `status_bind=` (host:port).
//! - any other line is a module line (`<name> [input=<peer> ...] [every=<dur>] [timeout=<dur>]`) —
//!   see `registry`. A module name MUST NOT contain `:` (the blackboard/routing key is `module:id`);
//!   such a line is rejected. `every=`/`timeout=` are per-anemos schedule overrides (bare number is
//!   seconds, e.g. `every=1` ≡ `every=1s`; `500ms` also accepted). `every` is the anemos's own
//!   cadence; `timeout` bounds one `apply`.
//!
//! Scheduler model (SOW-0013): aiolos wakes every `base_tick` and does only non-blocking work —
//! dispatch an `apply` to any instance that is idle and due (`now - last_dispatch >= every`), reap
//! whatever results workers posted asynchronously. At most one `apply` is in flight per instance, so
//! a slow/hung anemos is DELAYED (re-dispatched when free), never queued and never replayed; only a
//! `timeout` breach kills+restores it. `timeout` may exceed `every`/`base_tick`. `every` is floored
//! to `base_tick` (it can never be finer than the wake granularity).
//!
//! Paths are overridable via env for testing/packaging:
//! - `AIOLOS_CONF`    — config file path (default `/opt/aiolos/etc/aiolos.conf`)
//! - `AIOLOS_BIN_DIR` — directory holding module binaries (default `/opt/aiolos/bin`)

use crate::registry::{parse_module_line, RegistryEntry};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tracing::warn;

const DEFAULT_CONF: &str = "/opt/aiolos/etc/aiolos.conf";
const DEFAULT_BIN_DIR: &str = "/opt/aiolos/bin";
const DEFAULT_STATUS_BIND: &str = "0.0.0.0:9876";
/// Non-blocking scheduler wake period: the loop wakes this often to dispatch due+idle instances and
/// reap async results. Default 100 ms (fast reaction); `every` can never be finer than this.
const DEFAULT_BASE_TICK_MS: u64 = 100;
/// A `base_tick` below this would spin the loop pointlessly; clamp up.
const MIN_BASE_TICK_MS: u64 = 10;
/// Per-anemos default cadence (how often its `apply` runs) when no `every=` directive is given.
const DEFAULT_EVERY_MS: u64 = 1_000;
/// Per-anemos default `apply` timeout when no `timeout=` directive is given.
const DEFAULT_MODULE_TIMEOUT_MS: u64 = 5_000;
/// A per-anemos `timeout` below this is almost certainly a typo (a real `apply` needs more); clamp up.
const MIN_MODULE_TIMEOUT_MS: u64 = 1;
const DEFAULT_DETECT_EVERY_SECS: u64 = 10;
/// Cap for the exponential respawn backoff (a module is retried forever, but never slower than this).
const DEFAULT_MAX_BACKOFF_SECS: u64 = 300;
/// A `max_backoff` below this is meaningless (it would defeat the exponential ramp) — clamped up.
const MIN_MAX_BACKOFF_SECS: u64 = 1;

/// Per-anemos schedule overrides parsed from a module line's `every=`/`timeout=` directives, keyed
/// by module name. `every` is the anemos's own cadence; `timeout` bounds one `apply`. Both fall back
/// to the global defaults when the directive is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleSchedule {
    /// How often this anemos's `apply` is dispatched (floored to `base_tick`).
    pub every: Duration,
    /// Max wait for one `apply` reply before the child is killed + its device restored.
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub registry: Vec<RegistryEntry>,
    /// Non-blocking scheduler wake period (SOW-0013): the loop wakes this often to dispatch
    /// due+idle instances and reap async results. Default 100 ms.
    pub base_tick: Duration,
    /// How often `detect` is re-run for hotplug reconciliation.
    pub detect_every: Duration,
    /// Upper bound on the per-instance exponential respawn backoff. aiolos never gives up — it keeps
    /// retrying a crashed/declared-fatal instance, but never slower than this (default 300 s).
    pub max_backoff: Duration,
    /// `host:port` the read-only status page binds to.
    pub status_bind: String,
    /// Directory holding module binaries (`<bin_dir>/<module>`).
    pub bin_dir: PathBuf,
    /// Per-module schedule (`every`/`timeout`), keyed by module name. Every registry module has an
    /// entry (defaults filled in). Used by the scheduler to pace + bound each anemos independently.
    pub schedules: HashMap<String, ModuleSchedule>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            registry: Vec::new(),
            base_tick: Duration::from_millis(DEFAULT_BASE_TICK_MS),
            detect_every: Duration::from_secs(DEFAULT_DETECT_EVERY_SECS),
            max_backoff: Duration::from_secs(DEFAULT_MAX_BACKOFF_SECS),
            status_bind: DEFAULT_STATUS_BIND.to_string(),
            bin_dir: PathBuf::from(DEFAULT_BIN_DIR),
            schedules: HashMap::new(),
        }
    }
}

impl Config {
    /// The schedule for a module, or the global defaults if it has no registry entry (e.g. a
    /// late-arriving instance whose module line was rejected). `every` is already floored to
    /// `base_tick` at parse time.
    pub fn schedule_for(&self, module_name: &str) -> ModuleSchedule {
        self.schedules
            .get(module_name)
            .copied()
            .unwrap_or_else(|| ModuleSchedule {
                every: self.base_tick.max(Duration::from_millis(DEFAULT_EVERY_MS)),
                timeout: Duration::from_millis(DEFAULT_MODULE_TIMEOUT_MS),
            })
    }

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
        let mut base_tick_ms = DEFAULT_BASE_TICK_MS;

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
                    // `base_tick`: scheduler wake period. Bare number = ms (its natural scale).
                    "base_tick" => base_tick_ms = parse_dur_ms(key, val, Unit::Millis)?,
                    "detect_every" => cfg.detect_every = Duration::from_secs(parse_secs(key, val)?),
                    "max_backoff" => {
                        let secs = parse_secs(key, val)?;
                        let clamped = secs.max(MIN_MAX_BACKOFF_SECS);
                        if clamped != secs {
                            warn!(
                                requested = secs,
                                clamped, "max_backoff too small; clamping up to the minimum"
                            );
                        }
                        cfg.max_backoff = Duration::from_secs(clamped);
                    }
                    "status_bind" => cfg.status_bind = val.to_string(),
                    // `tick`/`timeout` were the old lockstep globals (SOW-0013 replaced them with the
                    // per-anemos scheduler). Warn loudly so a stale config is noticed, not silently
                    // misread as a sub-second period.
                    "tick" | "timeout" => warn!(
                        key = %key,
                        "obsolete global since SOW-0013 (decoupled scheduler) — use base_tick and per-module every=/timeout="
                    ),
                    other => warn!(key = %other, "unknown global directive ignored"),
                }
                continue;
            }

            let entry = parse_module_line(line);
            // A module name with `:` would make the `module:id` routing key ambiguous (a source
            // prefix could match the wrong module). Reject the line loudly rather than route wrong.
            if entry.module_name.contains(':') {
                warn!(module = %entry.module_name, "module name contains ':' (breaks input routing keys) — ignoring this module line");
                continue;
            }
            cfg.registry.push(entry);
        }

        // `base_tick`: floor to a sane minimum so the loop can't spin pointlessly.
        if base_tick_ms < MIN_BASE_TICK_MS {
            warn!(
                requested_ms = base_tick_ms,
                clamped_ms = MIN_BASE_TICK_MS,
                "base_tick too small; clamping up to the minimum"
            );
            base_tick_ms = MIN_BASE_TICK_MS;
        }
        cfg.base_tick = Duration::from_millis(base_tick_ms);

        // Build per-module schedules from each module line's `every=`/`timeout=` directives, with
        // defaults filled in. `every` is floored to `base_tick` (it can't be finer than a wake).
        cfg.schedules = build_schedules(&cfg.registry, cfg.base_tick);

        Ok(cfg)
    }
}

/// Build the per-module schedule map from registry directives. `every=`/`timeout=` arrive in each
/// entry's `unknown_directives` (registry.rs only interprets `input=`); we parse them here. Defaults
/// apply when a directive is absent or unparsable. `every` is floored to `base_tick` so a module can
/// never request a cadence finer than the scheduler's wake granularity.
fn build_schedules(
    registry: &[RegistryEntry],
    base_tick: Duration,
) -> HashMap<String, ModuleSchedule> {
    let mut out = HashMap::with_capacity(registry.len());
    for entry in registry {
        let mut every = Duration::from_millis(DEFAULT_EVERY_MS);
        let mut timeout = Duration::from_millis(DEFAULT_MODULE_TIMEOUT_MS);
        for dir in &entry.unknown_directives {
            // Per-module durations: bare number = seconds (matches the seconds-scale of the other
            // operator knobs and the DESIGN `every=<sec>` note); `ms`/`s` suffix also accepted.
            if let Some(v) = dir.strip_prefix("every=") {
                match parse_dur_ms("every", v, Unit::Secs) {
                    Ok(ms) => every = Duration::from_millis(ms),
                    Err(e) => {
                        warn!(module = %entry.module_name, error = %e, "ignoring bad every= directive")
                    }
                }
            } else if let Some(v) = dir.strip_prefix("timeout=") {
                match parse_dur_ms("timeout", v, Unit::Secs) {
                    // A zero timeout would make EVERY apply time out -> kill + respawn the module
                    // forever; it is never the intent (and 0 must NOT mean "infinite": an unbounded
                    // apply would break the isolation guarantee). Treat it as unset -> keep the
                    // default, exactly as a bad/unparsable directive does.
                    Ok(0) => warn!(module = %entry.module_name,
                        "timeout=0 is invalid (would time out every apply); using the default"),
                    Ok(ms) => timeout = Duration::from_millis(ms.max(MIN_MODULE_TIMEOUT_MS)),
                    Err(e) => {
                        warn!(module = %entry.module_name, error = %e, "ignoring bad timeout= directive")
                    }
                }
            }
        }
        // Floor `every` to `base_tick`: a finer cadence is unachievable (the loop only wakes every
        // base_tick), so honour the invariant `every >= base_tick`.
        if every < base_tick {
            warn!(
                module = %entry.module_name,
                every_ms = every.as_millis() as u64,
                base_tick_ms = base_tick.as_millis() as u64,
                "every < base_tick; raising every to base_tick"
            );
            every = base_tick;
        }
        out.insert(entry.module_name.clone(), ModuleSchedule { every, timeout });
    }
    out
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

/// Default unit for a duration directive when the value carries no explicit `ms`/`s` suffix.
#[derive(Clone, Copy)]
enum Unit {
    Millis,
    Secs,
}

/// Parse a duration directive into milliseconds. Accepts an explicit `ms`/`s` suffix (e.g. `500ms`,
/// `2s`); a bare number uses `default_unit`. Used for `base_tick` (default ms) and per-module
/// `every`/`timeout` (default s).
fn parse_dur_ms(key: &str, val: &str, default_unit: Unit) -> Result<u64> {
    let v = val.trim();
    let (num, unit_ms): (&str, u64) = if let Some(n) = v.strip_suffix("ms") {
        (n.trim(), 1)
    } else if let Some(n) = v.strip_suffix('s') {
        (n.trim(), 1_000)
    } else {
        let factor = match default_unit {
            Unit::Millis => 1,
            Unit::Secs => 1_000,
        };
        (v, factor)
    };
    let n: u64 = num.parse().with_context(|| {
        format!("'{key}' must be a non-negative number (optionally suffixed ms/s), got '{val}'")
    })?;
    n.checked_mul(unit_ms)
        .with_context(|| format!("'{key}' value '{val}' overflows"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_empty() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.base_tick, Duration::from_millis(100));
        assert_eq!(c.detect_every, Duration::from_secs(10));
        assert_eq!(c.max_backoff, Duration::from_secs(300));
        assert_eq!(c.status_bind, "0.0.0.0:9876");
        assert!(c.registry.is_empty());
        assert!(c.schedules.is_empty());
    }

    #[test]
    fn parses_globals_and_modules() {
        let text = "\
# aiolos config
base_tick=200
detect_every=20
max_backoff=120
status_bind=127.0.0.1:9000

nvidia
asrock16-2t  input=nvidia   # board fans follow GPU temps
";
        let c = Config::parse(text).unwrap();
        assert_eq!(c.base_tick, Duration::from_millis(200));
        assert_eq!(c.detect_every, Duration::from_secs(20));
        assert_eq!(c.max_backoff, Duration::from_secs(120));
        assert_eq!(c.status_bind, "127.0.0.1:9000");
        assert_eq!(c.registry.len(), 2);
        assert_eq!(c.registry[0].module_name, "nvidia");
        assert_eq!(c.registry[1].inputs, vec!["nvidia".to_string()]);
    }

    #[test]
    fn base_tick_accepts_units_and_defaults_to_ms() {
        // Bare number on base_tick is milliseconds.
        assert_eq!(
            Config::parse("base_tick=250").unwrap().base_tick,
            Duration::from_millis(250)
        );
        // Explicit ms/s suffixes are honoured.
        assert_eq!(
            Config::parse("base_tick=150ms").unwrap().base_tick,
            Duration::from_millis(150)
        );
        assert_eq!(
            Config::parse("base_tick=1s").unwrap().base_tick,
            Duration::from_millis(1000)
        );
    }

    #[test]
    fn base_tick_clamped_up_from_too_small() {
        // A pointlessly tiny base_tick is floored to the minimum.
        let c = Config::parse("base_tick=1").unwrap();
        assert_eq!(c.base_tick, Duration::from_millis(MIN_BASE_TICK_MS));
    }

    #[test]
    fn module_schedule_defaults_and_overrides() {
        let c = Config::parse(
            "base_tick=100\nnvidia\nasrock16-2t input=nvidia every=2 timeout=8\nslow every=500ms",
        )
        .unwrap();
        // No directive -> defaults (every 1s, timeout 5s).
        let nv = c.schedule_for("nvidia");
        assert_eq!(nv.every, Duration::from_secs(1));
        assert_eq!(nv.timeout, Duration::from_secs(5));
        // Bare numbers on every/timeout are seconds.
        let ar = c.schedule_for("asrock16-2t");
        assert_eq!(ar.every, Duration::from_secs(2));
        assert_eq!(ar.timeout, Duration::from_secs(8));
        // ms suffix accepted on every.
        let s = c.schedule_for("slow");
        assert_eq!(s.every, Duration::from_millis(500));
    }

    #[test]
    fn every_floored_to_base_tick() {
        // every below base_tick is raised to base_tick (can't be finer than a wake).
        let c = Config::parse("base_tick=200\nm every=50ms").unwrap();
        assert_eq!(c.schedule_for("m").every, Duration::from_millis(200));
    }

    #[test]
    fn timeout_may_exceed_every_and_base_tick() {
        // The old `timeout < tick` clamp is gone: a long timeout with a short every is valid.
        let c = Config::parse("base_tick=100\nm every=200ms timeout=30").unwrap();
        let sch = c.schedule_for("m");
        assert_eq!(sch.every, Duration::from_millis(200));
        assert_eq!(sch.timeout, Duration::from_secs(30));
        assert!(sch.timeout > sch.every);
        assert!(sch.timeout > c.base_tick);
    }

    #[test]
    fn bad_global_value_errors() {
        assert!(Config::parse("base_tick=abc").is_err());
        assert!(Config::parse("max_backoff=abc").is_err());
    }

    #[test]
    fn bad_module_directive_is_ignored_not_fatal() {
        // A malformed every=/timeout= falls back to defaults rather than refusing to boot.
        let c = Config::parse("m every=abc timeout=xyz").unwrap();
        let sch = c.schedule_for("m");
        assert_eq!(sch.every, Duration::from_secs(1));
        assert_eq!(sch.timeout, Duration::from_secs(5));
    }

    #[test]
    fn timeout_zero_falls_back_to_default_not_a_kill_loop() {
        // `timeout=0` must NOT clamp to 1ms (which would time out every apply -> kill+respawn loop);
        // it falls back to the default. 0 is never valid and must not mean "infinite".
        let c = Config::parse("m timeout=0").unwrap();
        assert_eq!(c.schedule_for("m").timeout, Duration::from_secs(5));
        // A tiny but explicit nonzero value is the operator's own choice and is honoured.
        let c = Config::parse("m timeout=2ms").unwrap();
        assert_eq!(c.schedule_for("m").timeout, Duration::from_millis(2));
    }

    #[test]
    fn obsolete_tick_timeout_globals_are_ignored() {
        // The lockstep globals are warned + ignored; they must not affect base_tick or schedules.
        let c = Config::parse("tick=5\ntimeout=3\nbase_tick=120\nm").unwrap();
        assert_eq!(c.base_tick, Duration::from_millis(120));
        assert_eq!(c.schedule_for("m").every, Duration::from_secs(1));
    }

    #[test]
    fn max_backoff_clamped_up_and_parsed() {
        // A too-small max_backoff is clamped up to the minimum (it must not defeat the ramp).
        let c = Config::parse("max_backoff=0").unwrap();
        assert_eq!(c.max_backoff, Duration::from_secs(1));
        // A larger explicit value is honoured verbatim.
        let c = Config::parse("max_backoff=600").unwrap();
        assert_eq!(c.max_backoff, Duration::from_secs(600));
    }

    #[test]
    fn module_name_with_dash_is_not_global() {
        // 'asrock16-2t' contains no '=', so it is a module line, not a global.
        let c = Config::parse("asrock16-2t").unwrap();
        assert_eq!(c.registry.len(), 1);
        assert_eq!(c.registry[0].module_name, "asrock16-2t");
    }

    #[test]
    fn module_name_with_colon_is_rejected() {
        // A ':' in a module name would make the `module:id` routing key ambiguous — the line is
        // dropped (with a warning), so it never participates in routing.
        let c = Config::parse("nvidia\nbad:name input=nvidia\nnvme").unwrap();
        let names: Vec<&str> = c.registry.iter().map(|e| e.module_name.as_str()).collect();
        assert_eq!(
            names,
            vec!["nvidia", "nvme"],
            "the ':' module line is rejected"
        );
    }
}
