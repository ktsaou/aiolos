//! Per-module supervisor: one persistent `detect` process + the `run` instances it reconciles.
//!
//! The supervisor is the SOLE writer of `state.instances` for its module's keys (insert on spawn,
//! remove on vanish/death) — so there is no registration race with the worker threads. It prunes
//! the blackboard whenever it removes an instance, so stale readings are never relayed as inputs.

use crate::instance::{
    set_nonblocking, write_line_deadline, Instance, InstanceCmd, LineReader, ReadOutcome,
    TickReport,
};
use crate::registry::RegistryEntry;
use crate::{AppState, InstanceEntry, ModuleHealth, StderrTail, ACTIVE_INSTANCES, SHUTDOWN_FLAG};
use protocol::{Detected, FoundEntry, Request, Status};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

const STEP: Duration = Duration::from_millis(500);
const DETECT_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const TAIL_CAP: usize = 50;
const MAX_DETECT_LINE: usize = 64 * 1024;
/// Shift used to jump the exponential backoff straight to its `max_backoff` cap for a declared
/// `fatal` (decision 15: never give up, retry slowly). `2^32` s saturates any sane cap via `.min`,
/// and `saturating_pow` keeps it in-bounds even for larger shifts.
const BACKOFF_SATURATE_SHIFT: u32 = 32;

/// Why an instance worker thread exited, carried OUT of the thread via its `JoinHandle` return value
/// so the supervisor learns the reason without racing the main loop's shared-state update. (The main
/// loop sets `last_status` only when it reaps the worker's `TickReport`; reading that in `reap_dead`
/// to classify the exit would race a worker that posted `fatal` and exited before the reap.)
/// `Fatal` = the module DECLARED a fatal `apply` (jump straight to the long backoff); `Ended` = any
/// other self-exit (crash, timeout-kill, stdin EOF, or shutdown) → normal escalating backoff.
enum WorkerExit {
    Fatal,
    Ended,
}

/// Exponential respawn backoff (pure, so it is unit-testable): `2^count` seconds, capped at
/// `max_secs` (and never below 1 s so a misconfigured 0 cap can't busy-loop). aiolos retries
/// forever (decision 15) — the delay only grows to the cap, it never gives up.
fn backoff_delay_secs(count: u32, max_secs: u64) -> u64 {
    2u64.saturating_pow(count).min(max_secs.max(1))
}

/// Spawn a supervisor thread for one registry entry. `max_backoff` caps the exponential respawn
/// backoff (a crashed/declared-fatal instance is retried forever, but never slower than this).
/// `results` is the shared channel every worker posts its async `apply` result to (SOW-0013): the
/// main loop drains it non-blockingly, so no instance ever blocks the scheduler or a sibling.
pub fn run_module(
    entry: RegistryEntry,
    state: Arc<RwLock<AppState>>,
    bin_dir: PathBuf,
    detect_every: Duration,
    max_backoff: Duration,
    results: mpsc::Sender<TickReport>,
) -> thread::JoinHandle<()> {
    let module_name = entry.module_name.clone();
    info!(module=%module_name, "starting module supervisor");
    thread::spawn(move || {
        Supervisor {
            module_name,
            state,
            bin_dir,
            detect_every,
            max_backoff,
            results,
            running: HashMap::new(),
            detect_proc: None,
            last_found: Vec::new(),
            backoff: HashMap::new(),
            spawn_counts: HashMap::new(),
        }
        .run();
    })
}

struct DetectProc {
    child: Child,
    stdin_fd: std::os::unix::io::RawFd,
    _stdin: ChildStdin,
    reader: LineReader,
}

/// Result of one detect round-trip (computed while the detect proc is borrowed, then acted on).
enum DetectOutcome {
    /// A parsed (or parse-failed) module reply.
    Reply(serde_json::Result<Detected>),
    /// The module didn't answer (timeout/EOF/IO) — backstop path.
    ReadFail,
}

struct Supervisor {
    module_name: String,
    state: Arc<RwLock<AppState>>,
    bin_dir: PathBuf,
    detect_every: Duration,
    /// Cap for the exponential respawn backoff (per-id and the declared-fatal jump).
    max_backoff: Duration,
    /// Shared channel every worker posts its async `apply` result to (drained by the main loop).
    results: mpsc::Sender<TickReport>,
    running: HashMap<String, (mpsc::Sender<InstanceCmd>, thread::JoinHandle<WorkerExit>)>,
    detect_proc: Option<DetectProc>,
    last_found: Vec<FoundEntry>,
    /// Per-id crash-loop backoff: (consecutive failures, last failure time).
    backoff: HashMap<String, (u32, Instant)>,
    /// Per-id lifetime spawn count (restart_count = spawn_count - 1).
    spawn_counts: HashMap<String, u32>,
}

impl Supervisor {
    fn run(&mut self) {
        let mut detect_next = Instant::now() + self.run_detect(); // initial detect
        self.reconcile();

        loop {
            if SHUTDOWN_FLAG.load(Ordering::Acquire) {
                // Main orchestrates instance shutdown; we just stop detecting/spawning.
                // Dropping our detect child closes its stdin -> EOF -> it exits.
                return;
            }
            thread::sleep(STEP);
            if SHUTDOWN_FLAG.load(Ordering::Acquire) {
                return;
            }

            self.reap_dead();
            if Instant::now() >= detect_next {
                detect_next = Instant::now() + self.run_detect();
            }
            self.reconcile();
        }
    }

    // ----- detect -------------------------------------------------------------

    fn ensure_detect_proc(&mut self) -> anyhow::Result<()> {
        if let Some(p) = self.detect_proc.as_mut() {
            match p.child.try_wait() {
                Ok(Some(_)) | Err(_) => self.detect_proc = None,
                Ok(None) => return Ok(()),
            }
        }
        let bin = self.bin_dir.join(&self.module_name);
        let mut child = Command::new(&bin)
            .arg("detect")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // detect logs flow to our stderr/journal; never blocks
            .spawn()
            .map_err(|e| anyhow::anyhow!("spawn {} detect: {e}", bin.display()))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stdin_fd = std::os::unix::io::AsRawFd::as_raw_fd(&stdin);
        set_nonblocking(stdin_fd)?;
        let reader = LineReader::new(stdout)?;
        self.detect_proc = Some(DetectProc {
            child,
            stdin_fd,
            _stdin: stdin,
            reader,
        });
        info!(module=%self.module_name, "detect process spawned");
        Ok(())
    }

    /// Run one detect round-trip and react to the module's DECLARED status (never inferring faults
    /// from empty/exit/silence). Returns how long until the next detect attempt: `detect_every`
    /// normally, or `max_backoff` after a declared `fatal`. On `error`/unresponsive it keeps
    /// `last_found` unchanged so `reconcile` does NOT tear down healthy instances.
    fn run_detect(&mut self) -> Duration {
        if let Err(e) = self.ensure_detect_proc() {
            self.set_health("error", Some(format!("detect spawn failed: {e}")));
            warn!(module=%self.module_name, error=%e, "detect spawn failed — keeping instances");
            return self.detect_every;
        }
        let deadline = Instant::now() + DETECT_TIMEOUT;

        // Write the request using a copied fd (no live mut borrow of detect_proc during self calls).
        let stdin_fd = self.detect_proc.as_ref().unwrap().stdin_fd;
        let line = Request::Detect.to_line().unwrap_or_default();
        let wrote = matches!(write_line_deadline(stdin_fd, &line, deadline), Ok(true));

        let outcome = if !wrote {
            DetectOutcome::ReadFail
        } else {
            let p = self.detect_proc.as_mut().unwrap();
            loop {
                match p.reader.read_line_deadline(deadline, MAX_DETECT_LINE) {
                    ReadOutcome::Line(s) => {
                        if protocol::is_hello(&s) {
                            continue; // skip optional hello
                        }
                        break DetectOutcome::Reply(Detected::from_line(&s));
                    }
                    ReadOutcome::Eof | ReadOutcome::Timeout | ReadOutcome::TooLong => {
                        break DetectOutcome::ReadFail
                    }
                    ReadOutcome::Io(_) => break DetectOutcome::ReadFail,
                }
            }
        };
        // detect_proc borrow has ended; safe to mutate self below.

        match outcome {
            // Backstop only: the module was too broken to even answer. Recycle + surface as
            // "unresponsive"; keep instances (last_found unchanged).
            DetectOutcome::ReadFail => {
                self.kill_detect();
                self.set_health(
                    "unresponsive",
                    Some("detect did not reply (timeout/EOF)".into()),
                );
                warn!(module=%self.module_name, "detect unresponsive — recycling; keeping instances");
                self.detect_every
            }
            DetectOutcome::Reply(Err(e)) => {
                self.kill_detect();
                self.set_health("protocol_error", Some(format!("detect parse: {e}")));
                warn!(module=%self.module_name, error=%e, "detect protocol error — recycling; keeping instances");
                self.detect_every
            }
            DetectOutcome::Reply(Ok(d)) => match d.status {
                Status::Ok => {
                    if let Some(w) = &d.error {
                        warn!(module=%self.module_name, warning=%w, "detect ok with warnings");
                    }
                    self.set_health("ok", d.error.clone());
                    self.last_found = d.found; // authoritative — empty legitimately means none
                    self.detect_every
                }
                // Declared transient failure: keep instances, surface the reason, recycle the proc.
                Status::Error => {
                    let msg = d.error.unwrap_or_else(|| "detect error".into());
                    self.set_health("error", Some(msg.clone()));
                    warn!(module=%self.module_name, error=%msg, "detect reported error — keeping instances, recycling proc");
                    self.kill_detect();
                    self.detect_every
                }
                // Declared fatal: keep instances, surface loudly, retry only on a long backoff.
                Status::Fatal => {
                    let msg = d.error.unwrap_or_else(|| "detect fatal".into());
                    self.set_health("fatal", Some(msg.clone()));
                    error!(module=%self.module_name, error=%msg, "detect reported FATAL — long backoff, keeping instances");
                    self.max_backoff
                }
            },
        }
    }

    /// Record this module's detect health for the status page.
    fn set_health(&self, status: &str, error: Option<String>) {
        if let Ok(mut s) = self.state.write() {
            s.modules.insert(
                self.module_name.clone(),
                ModuleHealth {
                    detect_status: status.to_string(),
                    detect_error: error,
                },
            );
        }
    }

    fn kill_detect(&mut self) {
        if let Some(mut p) = self.detect_proc.take() {
            let _ = p.child.kill();
            let _ = p.child.wait();
        }
    }

    // ----- reconcile ----------------------------------------------------------

    /// Spawn `run` instances for detected ids not yet running (backoff-aware); shut down instances
    /// whose id vanished from the latest detect.
    fn reconcile(&mut self) {
        if SHUTDOWN_FLAG.load(Ordering::Acquire) {
            return;
        }
        let found_ids: HashSet<String> = self.last_found.iter().map(|e| e.id.clone()).collect();

        // Vanished -> graceful shutdown + remove.
        for id in self.running.keys().cloned().collect::<Vec<_>>() {
            if !found_ids.contains(&id) {
                info!(module=%self.module_name, id=%id, "instance vanished — shutting down");
                if let Some((tx, _)) = self.running.remove(&id) {
                    let _ = tx.send(InstanceCmd::Shutdown);
                }
                self.remove_instance_state(&id);
                self.backoff.remove(&id);
            }
        }

        // New / dead -> (re)spawn, respecting backoff.
        let to_spawn: Vec<(String, String)> = self
            .last_found
            .iter()
            .filter(|e| !self.running.contains_key(&e.id) && self.backoff_expired(&e.id))
            .map(|e| (e.id.clone(), e.name.clone()))
            .collect();
        for (id, name) in to_spawn {
            self.spawn_instance(id, name);
        }
    }

    fn remove_instance_state(&self, id: &str) {
        let key = self.key(id);
        if let Ok(mut s) = self.state.write() {
            s.instances.remove(&key);
            // Prune the blackboard so a dead instance's stale readings are never relayed.
            s.blackboard.remove(&key);
            // Drop the scheduler slot too, so its lifecycle matches the instance's. Otherwise a stale
            // `busy`/`last_dispatch` could survive into a respawn with the same key when the main
            // loop's `dispatch_due` prune doesn't fall between this removal and the (backoff-delayed)
            // respawn — e.g. if `base_tick` exceeds the respawn backoff. Removing it here makes the
            // cleanup synchronous; `dispatch_due` recreates a fresh slot on next dispatch.
            s.sched.remove(&key);
        }
    }

    fn key(&self, id: &str) -> String {
        format!("{}:{}", self.module_name, id)
    }

    // ----- backoff ------------------------------------------------------------

    fn backoff_expired(&self, id: &str) -> bool {
        match self.backoff.get(id) {
            None => true,
            Some((count, last)) => last.elapsed() >= self.backoff_delay(*count),
        }
    }

    /// Exponential backoff delay for `count` consecutive failures, capped at `max_backoff`:
    /// 2,4,8,… seconds, never exceeding the configured cap. aiolos retries forever (decision 15).
    fn backoff_delay(&self, count: u32) -> Duration {
        Duration::from_secs(backoff_delay_secs(count, self.max_backoff.as_secs()))
    }

    fn record_failure(&mut self, id: &str) {
        let e = self
            .backoff
            .entry(id.to_string())
            .or_insert((0, Instant::now()));
        e.0 = e.0.saturating_add(1);
        e.1 = Instant::now();
    }

    /// Jump straight to the `max_backoff` cap for a module-DECLARED fatal — retry slowly forever
    /// (decision 15), don't crash-loop. The saturating shift makes `2^count` exceed any sane cap, so
    /// `backoff_delay` clamps it to exactly `max_backoff`.
    fn record_fatal(&mut self, id: &str) {
        self.backoff
            .insert(id.to_string(), (BACKOFF_SATURATE_SHIFT, Instant::now()));
    }

    // ----- reap ---------------------------------------------------------------

    fn reap_dead(&mut self) {
        let dead: Vec<String> = self
            .running
            .iter()
            .filter(|(_, (_, h))| h.is_finished())
            .map(|(id, _)| id.clone())
            .collect();
        for id in dead {
            // Take the finished worker's handle and JOIN it for its declared exit reason. The reason
            // travels with the thread (no shared-state race vs the main loop's `last_status` update);
            // `is_finished` is already true so `join` returns immediately. A panicked worker yields
            // `Err` → treated as a crash (normal escalating backoff), never as a declared fatal.
            let was_fatal = match self.running.remove(&id) {
                Some((_tx, handle)) => matches!(handle.join(), Ok(WorkerExit::Fatal)),
                None => false, // can't happen (id came from `running`), but never misclassify as fatal
            };
            warn!(module=%self.module_name, id=%id, fatal=was_fatal, "instance worker exited — will respawn (backoff)");
            self.remove_instance_state(&id);
            if was_fatal {
                self.record_fatal(&id);
            } else {
                self.record_failure(&id);
            }
        }
    }

    // ----- spawn --------------------------------------------------------------

    fn spawn_instance(&mut self, id: String, name: String) {
        if SHUTDOWN_FLAG.load(Ordering::Acquire) {
            return;
        }
        let mut inst = match Instance::new(&self.bin_dir, self.module_name.clone(), id.clone()) {
            Ok(i) => i,
            Err(e) => {
                error!(module=%self.module_name, id=%id, error=%e, "instance spawn failed");
                self.record_failure(&id);
                return;
            }
        };

        let count = self.spawn_counts.entry(id.clone()).or_insert(0);
        *count += 1;
        let restart_count = *count - 1;

        // stderr tail reader (drains continuously so the child never blocks on a full stderr pipe).
        // Each line is BOTH kept in the status-page tail AND forwarded to aiolos's journal so the
        // modules' decision logs land in the `aiolos` namespace alongside the orchestrator's.
        let tail: StderrTail = Arc::new(Mutex::new(VecDeque::with_capacity(TAIL_CAP)));
        if let Some(stderr) = inst.stderr.take() {
            let tail = Arc::clone(&tail);
            let module = self.module_name.clone();
            let id_for_log = id.clone();
            thread::spawn(move || {
                let reader = std::io::BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    tracing::info!(target: "anemos", module=%module, id=%id_for_log, "{line}");
                    if let Ok(mut t) = tail.lock() {
                        if t.len() == TAIL_CAP {
                            t.pop_front();
                        }
                        t.push_back(line);
                    }
                }
            });
        }

        let (cmd_tx, cmd_rx) = mpsc::channel::<InstanceCmd>();

        // Publish the entry BEFORE spawning the worker (supervisor is the sole writer — no race).
        if let Ok(mut s) = self.state.write() {
            s.instances.insert(
                self.key(&id),
                InstanceEntry {
                    module_name: self.module_name.clone(),
                    id: id.clone(),
                    name,
                    last_status: "starting".to_string(),
                    last_error: None,
                    last_readings: Vec::new(),
                    restart_count,
                    last_seen: Instant::now(),
                    cmd_tx: cmd_tx.clone(),
                    stderr_tail: tail,
                },
            );
        }

        let module_name = self.module_name.clone();
        let id_for_thread = id.clone();
        let key = self.key(&id);
        let results = self.results.clone();
        let handle = thread::spawn(move || {
            worker(
                inst,
                cmd_rx,
                &module_name,
                &id_for_thread,
                key,
                restart_count,
                results,
            )
        });

        self.running.insert(id.clone(), (cmd_tx, handle));
        info!(module=%self.module_name, id=%id, restart=restart_count, "instance spawned");
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.kill_detect();
        // If we're being dropped via a PANIC unwind (not a global shutdown), hand our run instances
        // off cleanly: tell each to restore + exit and deregister it from shared state. This runs
        // before the thread's `is_finished()` flips true, so main's watchdog (R7) can respawn a
        // fresh supervisor from a clean slate — no orphaned or duplicated instances on the device.
        // During a normal global shutdown, main drives instance shutdown instead, so we skip this.
        if !SHUTDOWN_FLAG.load(Ordering::Acquire) {
            let ids: Vec<String> = self.running.keys().cloned().collect();
            if !ids.is_empty() {
                warn!(module=%self.module_name, instances=ids.len(), "supervisor dropping (panic?) — shutting its instances down for clean respawn");
            }
            for id in ids {
                if let Some((tx, _)) = self.running.remove(&id) {
                    let _ = tx.send(InstanceCmd::Shutdown);
                }
                self.remove_instance_state(&id);
            }
        }
    }
}

/// RAII guard for the live-instance count, so a worker panic can't leak the count (which would make
/// `graceful_shutdown` wait the full grace for a phantom instance). Decrements on ANY exit/unwind.
struct ActiveGuard;
impl ActiveGuard {
    fn new() -> Self {
        ACTIVE_INSTANCES.fetch_add(1, Ordering::AcqRel);
        ActiveGuard
    }
}
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        ACTIVE_INSTANCES.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Instance worker thread body: owns the child, services Tick/Shutdown, exits on a fatal result so
/// the supervisor respawns it. Tracks the live-instance count for graceful shutdown.
///
/// SOW-0013: each `apply` runs on this worker's own clock (under its own `timeout`); the result is
/// posted asynchronously to the shared `results` channel as a `TickReport` (keyed by `key`, carrying
/// the measured latency) — the main loop never blocks waiting for it. At most one `apply` is ever in
/// flight here because the main loop only dispatches a `Tick` when this instance is idle.
fn worker(
    mut inst: Instance,
    cmd_rx: mpsc::Receiver<InstanceCmd>,
    module_name: &str,
    id: &str,
    key: String,
    generation: u32,
    results: mpsc::Sender<TickReport>,
) -> WorkerExit {
    let _active = ActiveGuard::new();
    // Default exit reason is a plain end (crash/timeout/EOF/shutdown). Only a DECLARED fatal apply
    // upgrades it to `Fatal`, which the supervisor reads via `join` to choose the long backoff.
    let mut exit = WorkerExit::Ended;
    loop {
        match cmd_rx.recv() {
            Err(_) => break, // all senders dropped
            Ok(InstanceCmd::Tick { timeout, inputs }) => {
                let started = Instant::now();
                let res = inst.tick(timeout, inputs);
                let latency = started.elapsed();
                // `is_fatal()` = "exit and respawn" (a declared fatal OR a backstop:
                // timeout/dead/protocol). The backoff CLASS is narrower: only a module-DECLARED fatal
                // jumps to the long backoff; the backstops keep the normal escalating backoff.
                // Collapsing them would strand a one-off timeout on the max_backoff cap (no fan
                // control for up to that long) — so classify with `is_declared_fatal()`, not exit-ness.
                let should_exit = res.status.is_fatal();
                let declared_fatal = res.status.is_declared_fatal();
                // Post the result back to the scheduler. If the receiver is gone (shutdown), just
                // exit. A fatal result still gets posted (so status/blackboard update) before we
                // break to let the supervisor respawn us.
                let _ = results.send(TickReport {
                    key: key.clone(),
                    generation,
                    result: res,
                    latency,
                });
                if should_exit {
                    if declared_fatal {
                        exit = WorkerExit::Fatal;
                    }
                    break;
                }
            }
            Ok(InstanceCmd::Shutdown) => {
                info!(module=%module_name, id=%id, "shutdown requested");
                inst.shutdown(SHUTDOWN_GRACE);
                break;
            }
        }
    }
    drop(inst); // reap child + restore via EOF for any non-shutdown exit
                // `_active` drops here -> ACTIVE_INSTANCES decremented (also on panic unwind).
    exit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_exponential_and_capped_at_max() {
        // 2^count seconds: 2,4,8,16,… until it reaches the cap, then stays there forever.
        let cap = 300;
        assert_eq!(backoff_delay_secs(0, cap), 1); // 2^0
        assert_eq!(backoff_delay_secs(1, cap), 2);
        assert_eq!(backoff_delay_secs(2, cap), 4);
        assert_eq!(backoff_delay_secs(3, cap), 8);
        assert_eq!(backoff_delay_secs(8, cap), 256);
        assert_eq!(backoff_delay_secs(9, cap), cap, "2^9=512 capped to 300");
        assert_eq!(backoff_delay_secs(20, cap), cap, "stays at the cap");
        // The declared-fatal jump saturates to exactly the cap, for ANY cap value.
        assert_eq!(backoff_delay_secs(BACKOFF_SATURATE_SHIFT, cap), cap);
        assert_eq!(backoff_delay_secs(BACKOFF_SATURATE_SHIFT, 600), 600);
        assert_eq!(backoff_delay_secs(BACKOFF_SATURATE_SHIFT, 60), 60);
    }

    #[test]
    fn backoff_cap_never_busy_loops_on_a_zero_cap() {
        // A 0 cap (shouldn't happen — config clamps to >=1) must still never yield 0s.
        assert_eq!(backoff_delay_secs(0, 0), 1);
        assert_eq!(backoff_delay_secs(BACKOFF_SATURATE_SHIFT, 0), 1);
    }
}
