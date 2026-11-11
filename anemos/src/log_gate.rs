//! Change-based log gate: per-tick decisions are logged only when their outcome changes, plus a
//! periodic heartbeat, so a steady-state controller does not flood the journal every second.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Default heartbeat: an unchanged outcome is still re-logged this often, proving liveness.
pub const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(300);

pub struct LogGate {
    heartbeat: Duration,
    /// scope -> (last logged key, when it was logged)
    seen: HashMap<String, (String, Instant)>,
}

impl Default for LogGate {
    fn default() -> Self {
        Self::new()
    }
}

impl LogGate {
    pub fn new() -> Self {
        Self::with_heartbeat(DEFAULT_HEARTBEAT)
    }

    pub fn with_heartbeat(heartbeat: Duration) -> Self {
        Self {
            heartbeat,
            seen: HashMap::new(),
        }
    }

    /// True when `key` differs from the last one logged under `scope`, or the heartbeat elapsed.
    /// `key` must encode the OUTCOME (e.g. commanded duty), not raw readings that jitter.
    pub fn due(&mut self, scope: &str, key: &str) -> bool {
        self.due_at(Instant::now(), scope, key)
    }

    fn due_at(&mut self, now: Instant, scope: &str, key: &str) -> bool {
        match self.seen.get_mut(scope) {
            Some((last_key, last_at))
                if last_key == key && now.saturating_duration_since(*last_at) < self.heartbeat =>
            {
                false
            }
            Some(entry) => {
                *entry = (key.to_string(), now);
                true
            }
            None => {
                self.seen.insert(scope.to_string(), (key.to_string(), now));
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_first_change_and_heartbeat_only() {
        let mut g = LogGate::with_heartbeat(Duration::from_secs(10));
        let t0 = Instant::now();
        assert!(g.due_at(t0, "s", "a"), "first sight logs");
        assert!(
            !g.due_at(t0 + Duration::from_secs(1), "s", "a"),
            "unchanged is quiet"
        );
        assert!(
            g.due_at(t0 + Duration::from_secs(2), "s", "b"),
            "change logs"
        );
        assert!(
            !g.due_at(t0 + Duration::from_secs(11), "s", "b"),
            "heartbeat counts from last log"
        );
        assert!(
            g.due_at(t0 + Duration::from_secs(12), "s", "b"),
            "heartbeat elapsed logs"
        );
    }

    #[test]
    fn scopes_are_independent() {
        let mut g = LogGate::new();
        assert!(g.due("x", "1"));
        assert!(g.due("y", "1"));
        assert!(!g.due("x", "1"));
    }
}
