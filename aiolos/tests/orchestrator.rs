//! End-to-end orchestrator integration tests against the `mock` anemos.
//!
//! These spawn the real `aiolos` binary with a temp config + a temp bin dir of symlinks to the
//! `mock` helper, and assert behaviour via mock-written marker files (no HTTP, so no port binding).
//! The status page binds 127.0.0.1:0 (never queried). Run via `cargo test --workspace`.
//!
//! Coverage: reconcile/spawn, per-tick liveness, graceful shutdown + device-restore path,
//! ISOLATION (a hung sibling and a partial-line-flooding sibling never stall a healthy one, and
//! both get killed + respawned), `input=` routing (one tick stale), and detect-set hotplug.

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const AIOLOS: &str = env!("CARGO_BIN_EXE_aiolos");
const MOCK: &str = env!("CARGO_BIN_EXE_mock");

static UNIQ: AtomicUsize = AtomicUsize::new(0);

/// A running aiolos under test. Drop kills it (so a panicking test never leaks the process).
struct Harness {
    dir: PathBuf,
    child: Child,
}

impl Harness {
    /// `modules`: registry lines (e.g. "sensor", "fan input=sensor"). `env`: extra MOCK_* vars.
    /// `extra_conf`: extra global config lines (besides the fast tick/timeout/detect defaults).
    fn start(modules: &[&str], env: &[(&str, &str)], extra_conf: &[&str]) -> Harness {
        let uniq = UNIQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aiolos-it-{}-{}", std::process::id(), uniq));
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();

        // Symlink the mock under each distinct module name (first token of each registry line).
        for line in modules {
            let name = line.split_whitespace().next().unwrap();
            let link = bin.join(name);
            if !link.exists() {
                std::os::unix::fs::symlink(MOCK, &link).unwrap();
            }
        }

        // Fast timings for tests; status on an ephemeral port we never query.
        let mut conf = String::from("status_bind=127.0.0.1:0\ntick=2\ntimeout=1\ndetect_every=1\n");
        for line in extra_conf {
            conf.push_str(line);
            conf.push('\n');
        }
        for line in modules {
            conf.push_str(line);
            conf.push('\n');
        }
        let conf_path = dir.join("aiolos.conf");
        fs::write(&conf_path, conf).unwrap();

        let mut cmd = Command::new(AIOLOS);
        cmd.env("AIOLOS_CONF", &conf_path)
            .env("AIOLOS_BIN_DIR", &bin)
            .env("RUST_LOG", "warn")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Every mock instance writes its markers under WORKDIR; set it per module.
        for line in modules {
            let name = line.split_whitespace().next().unwrap();
            let norm: String = name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_uppercase()
                    } else {
                        '_'
                    }
                })
                .collect();
            cmd.env(format!("MOCK_{norm}_WORKDIR"), &dir);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }

        let child = cmd.spawn().expect("spawn aiolos");
        Harness { dir, child }
    }

    fn marker(&self, file: &str) -> PathBuf {
        self.dir.join(file)
    }

    fn marker_len(&self, file: &str) -> u64 {
        fs::metadata(self.marker(file))
            .map(|m| m.len())
            .unwrap_or(0)
    }

    fn marker_exists(&self, file: &str) -> bool {
        self.marker(file).exists()
    }

    fn read_marker(&self, file: &str) -> Option<String> {
        fs::read_to_string(self.marker(file)).ok()
    }

    /// SIGTERM and wait up to `grace` for a clean exit; return whether it exited in time.
    fn shutdown(&mut self, grace: Duration) -> bool {
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Poll until `f()` is true or the deadline passes.
fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    f()
}

#[test]
fn reconcile_tick_and_graceful_restore() {
    let mut h = Harness::start(&["sensor"], &[], &[]);

    // The instance is spawned (starts marker) and ticked (applies grow).
    assert!(
        wait_until(Duration::from_secs(8), || h
            .marker_len("sensor-thing0.applies")
            >= 1),
        "sensor was never ticked"
    );
    assert!(h.marker_exists("sensor-thing0.starts"));

    // SIGTERM -> graceful shutdown -> the fail-safe restore path runs.
    assert!(
        h.shutdown(Duration::from_secs(6)),
        "aiolos did not exit on SIGTERM"
    );
    assert!(
        h.marker_exists("sensor-thing0.restored"),
        "restore (fail-safe) path did not run on shutdown"
    );
}

#[test]
fn hung_sibling_does_not_stall_healthy_one() {
    let mut h = Harness::start(&["good", "bad"], &[("MOCK_BAD_BEHAVIOR", "hang")], &[]);

    // The healthy module keeps ticking despite the hung sibling (isolation).
    assert!(
        wait_until(Duration::from_secs(12), || h
            .marker_len("good-thing0.applies")
            >= 3),
        "healthy module stalled behind a hung sibling"
    );
    // The hung module is killed at timeout and respawned (starts > 1).
    assert!(
        wait_until(Duration::from_secs(12), || h
            .marker_len("bad-thing0.starts")
            >= 2),
        "hung module was not killed + respawned"
    );

    assert!(h.shutdown(Duration::from_secs(6)));
}

#[test]
fn partial_line_flood_is_killed_not_wedged() {
    // The specific regression: a module writing a partial line (no newline) then hanging must be
    // killed within ~timeout (deadline-bounded read), not wedge its instance thread.
    let mut h = Harness::start(
        &["good", "flood"],
        &[("MOCK_FLOOD_BEHAVIOR", "partial")],
        &[],
    );

    assert!(
        wait_until(Duration::from_secs(12), || h
            .marker_len("good-thing0.applies")
            >= 3),
        "healthy module stalled behind a partial-line flooder"
    );
    assert!(
        wait_until(Duration::from_secs(12), || h
            .marker_len("flood-thing0.starts")
            >= 2),
        "partial-line flooder was not killed + respawned (would indicate a wedge)"
    );

    assert!(h.shutdown(Duration::from_secs(6)));
}

#[test]
fn input_routing_delivers_peer_readings() {
    let mut h = Harness::start(
        &["sensor", "consumer input=sensor"],
        &[("MOCK_SENSOR_TEMP", "63")],
        &[],
    );

    // consumer receives sensor's temp via routed inputs (one tick stale -> needs a couple ticks).
    assert!(
        wait_until(Duration::from_secs(12), || {
            h.read_marker("consumer-thing0.lastinput").as_deref() == Some("63")
        }),
        "routed input temp never reached the consumer (got {:?})",
        h.read_marker("consumer-thing0.lastinput")
    );

    assert!(h.shutdown(Duration::from_secs(6)));
}

#[test]
fn detect_set_change_adds_and_removes() {
    // Start with ids a,b; after ~1.5s detect switches to a,c. Expect b removed (graceful restore)
    // and c added (started); a untouched.
    let mut h = Harness::start(
        &["dyn"],
        &[
            ("MOCK_DYN_IDS", "a,b"),
            ("MOCK_DYN_IDS2", "a,c"),
            ("MOCK_DYN_SWITCH_MS", "1500"),
        ],
        &[],
    );

    assert!(
        wait_until(Duration::from_secs(10), || h.marker_exists("dyn-c.starts")),
        "new detected id was not spawned"
    );
    assert!(
        wait_until(Duration::from_secs(10), || h
            .marker_exists("dyn-b.restored")),
        "vanished id was not gracefully shut down (restore path)"
    );
    assert!(h.marker_exists("dyn-a.starts"), "stable id should remain");

    assert!(h.shutdown(Duration::from_secs(6)));
}
