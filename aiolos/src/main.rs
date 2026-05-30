//! aiolos — domain-agnostic orchestrator for autonomous module binaries (anemoi).
//!
//! std threads + blocking I/O, no async runtime (DESIGN.md §10). It spawns/supervises modules,
//! drives the heartbeat, relays declared `input=` data between them, holds all state, and serves a
//! read-only status page. All device knowledge lives in the anemoi.

mod config;
mod instance;
mod module;
mod registry;
mod status_page;

use anyhow::Result;
use config::Config;
use instance::{InstanceCmd, TickReport, TickStatus};
use protocol::{Inputs, Reading};
use std::collections::{HashMap, HashSet, VecDeque};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Bound on each module's one-shot `restore` in `aiolos restore` (a wedged BMC/NVML can't hang us).
const RESTORE_TIMEOUT: Duration = Duration::from_secs(10);

/// Minimum gap between respawns of a module's supervisor thread (avoids a tight crash loop).
const SUPERVISOR_RESPAWN_BACKOFF: Duration = Duration::from_secs(5);

/// A supervisor thread plus what the watchdog needs to respawn it if it dies.
struct SupervisorHandle {
    entry: registry::RegistryEntry,
    handle: thread::JoinHandle<()>,
    respawns: u32,
    last_spawn: Instant,
}

/// Set by the SIGTERM/SIGINT handler; polled by the main loop and supervisors.
pub static SHUTDOWN_FLAG: AtomicBool = AtomicBool::new(false);
/// Live `run` instance workers — graceful shutdown waits for this to reach 0 (devices restored).
pub static ACTIVE_INSTANCES: AtomicUsize = AtomicUsize::new(0);

/// Tail of a module instance's stderr (capped), shared with the status page.
pub type StderrTail = Arc<Mutex<VecDeque<String>>>;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr) // logs to stderr -> journal; never collides with anything
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cfg = Config::load()?;

    // `aiolos restore`: config-agnostic fail-safe used by systemd's ExecStopPost. aiolos reads its
    // OWN registry and runs each configured module's uniform `restore` one-shot, so the unit file
    // never has to name modules. Belt-and-suspenders for a hard kill where modules couldn't
    // self-restore on signal/EOF.
    if std::env::args().nth(1).as_deref() == Some("restore") {
        return run_restore(&cfg);
    }

    info!(
        base_tick_ms = cfg.base_tick.as_millis() as u64,
        detect_every_s = cfg.detect_every.as_secs(),
        max_backoff_s = cfg.max_backoff.as_secs(),
        status_bind = %cfg.status_bind,
        bin_dir = %cfg.bin_dir.display(),
        modules = cfg.registry.len(),
        "aiolos starting",
    );

    let state: Arc<RwLock<AppState>> = Arc::new(RwLock::new(AppState::default()));

    // Status page thread.
    {
        let state = Arc::clone(&state);
        let bind = cfg.status_bind.clone();
        thread::spawn(move || {
            if let Err(e) = status_page::serve(&bind, state) {
                tracing::error!(error=%e, bind=%bind, "status page failed to start");
            }
        });
    }

    install_signal_handlers();

    // module_name -> its input source modules (for routing).
    let input_map: HashMap<String, Vec<String>> = cfg
        .registry
        .iter()
        .map(|e| (e.module_name.clone(), e.inputs.clone()))
        .collect();

    // Single shared results channel: every worker posts its async `apply` result here; the main loop
    // drains it non-blockingly each base tick (SOW-0013). The receiver lives only in main; each
    // supervisor (and thus each worker) gets a clone of the sender. Unbounded by design, but bounded
    // in practice: at most one apply is in flight per instance, so the queue depth between drains is
    // <= the instance count, and the main loop drains every base_tick (100ms) — far faster than it
    // fills. (A bounded channel would add backpressure we never want on the fail-safe result path.)
    let (results_tx, results_rx) = mpsc::channel::<TickReport>();

    // Supervisor per module, tracked so the watchdog can respawn one whose thread dies (panics).
    let mut supervisors: Vec<SupervisorHandle> = cfg
        .registry
        .iter()
        .map(|entry| SupervisorHandle {
            entry: entry.clone(),
            handle: module::run_module(
                entry.clone(),
                Arc::clone(&state),
                cfg.bin_dir.clone(),
                cfg.detect_every,
                cfg.max_backoff,
                results_tx.clone(),
            ),
            respawns: 0,
            last_spawn: Instant::now(),
        })
        .collect();

    let base_tick = cfg.base_tick;
    let mut next_wake = Instant::now() + base_tick;
    let mut wake_count: u64 = 0;

    loop {
        if SHUTDOWN_FLAG.load(Ordering::Acquire) {
            info!("signal received — graceful shutdown");
            graceful_shutdown(&state, cfg.max_apply_timeout());
            break;
        }

        // Watchdog: if a supervisor thread died (panicked), respawn it (its Drop already cleaned up
        // its instances, so the fresh supervisor re-detects from a clean slate). Backoff-bounded.
        respawn_dead_supervisors(&mut supervisors, &state, &cfg, &results_tx);

        let now = Instant::now();
        if now < next_wake {
            // Sleep in short steps so a signal is noticed promptly between wakes. Capped so a large
            // base_tick still polls the shutdown flag often.
            thread::sleep((next_wake - now).min(Duration::from_millis(100)));
            continue;
        }
        next_wake = now + base_tick;
        wake_count += 1;

        // (1) Drain any results workers posted since the last wake (async rendezvous), then
        // (2) dispatch a fresh apply to every instance that is due and idle. Both are non-blocking.
        reap_results(&state, &results_rx, wake_count);
        dispatch_due(&state, &input_map, &cfg, wake_count);
    }

    info!("aiolos exiting");
    Ok(())
}

/// Watchdog gating (pure, so it is unit-testable): respawn a supervisor only when it has finished,
/// the backoff has elapsed, and we are not shutting down. A live supervisor thread only returns on
/// the shutdown flag, so `finished && !shutting_down` means it panicked.
fn should_respawn(finished: bool, since_last_spawn: Duration, shutting_down: bool) -> bool {
    finished && !shutting_down && since_last_spawn >= SUPERVISOR_RESPAWN_BACKOFF
}

/// Respawn any supervisor whose thread has finished while we are NOT shutting down (a supervisor
/// only returns on the shutdown flag, so finishing otherwise means it panicked). Backoff-bounded so
/// a supervisor that panics on startup can't spin. Never gives up (decision 15): it keeps retrying.
fn respawn_dead_supervisors(
    supervisors: &mut [SupervisorHandle],
    state: &Arc<RwLock<AppState>>,
    cfg: &Config,
    results_tx: &mpsc::Sender<TickReport>,
) {
    let shutting_down = SHUTDOWN_FLAG.load(Ordering::Acquire);
    for s in supervisors.iter_mut() {
        if !should_respawn(
            s.handle.is_finished(),
            s.last_spawn.elapsed(),
            shutting_down,
        ) {
            continue;
        }
        s.respawns += 1;
        warn!(module = %s.entry.module_name, respawns = s.respawns, "supervisor thread died — respawning");
        s.handle = module::run_module(
            s.entry.clone(),
            Arc::clone(state),
            cfg.bin_dir.clone(),
            cfg.detect_every,
            cfg.max_backoff,
            results_tx.clone(),
        );
        s.last_spawn = Instant::now();
    }
}

/// `aiolos restore`: run every configured module's `restore` one-shot to hand its device back to
/// firmware/BMC auto. Reads the registry from config (agnostic of which modules exist), dedupes by
/// module binary, runs them concurrently, and waits with a shared bound so a wedged device can't
/// hang us. Best-effort: failures are logged, not fatal (this is a safety net).
fn run_restore(cfg: &Config) -> Result<()> {
    let mut seen = HashSet::new();
    let mut children = Vec::new();
    for entry in &cfg.registry {
        if !seen.insert(entry.module_name.clone()) {
            continue; // one restore per distinct module binary
        }
        let bin = cfg.bin_dir.join(&entry.module_name);
        match Command::new(&bin)
            .arg("restore")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit()) // module logs flow to our journal
            .spawn()
        {
            Ok(child) => {
                info!(module = %entry.module_name, "restore one-shot spawned");
                children.push((entry.module_name.clone(), child));
            }
            Err(e) => warn!(module = %entry.module_name, error = %e, "restore spawn failed"),
        }
    }

    let deadline = Instant::now() + RESTORE_TIMEOUT;
    for (name, mut child) in children {
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if status.success() {
                        info!(module = %name, "restored");
                    } else {
                        warn!(module = %name, ?status, "restore exited non-zero");
                    }
                    break;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        warn!(module = %name, "restore timed out — killing");
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    warn!(module = %name, error = %e, "restore wait failed");
                    break;
                }
            }
        }
    }
    info!("restore complete");
    Ok(())
}

/// Decide whether to dispatch a fresh `apply` to one instance this wake (pure, so it is
/// unit-testable). The two SOW-0013 gates: the worker must be **idle** (at most one apply in flight
/// — a busy anemos is delayed, never queued) AND the instance must be **due**
/// (`now - last_dispatch >= every`; never-dispatched is due immediately).
fn should_dispatch(
    busy: bool,
    last_dispatch: Option<Instant>,
    now: Instant,
    every: Duration,
) -> bool {
    if busy {
        return false;
    }
    match last_dispatch {
        None => true,
        Some(t) => now.saturating_duration_since(t) >= every,
    }
}

/// Non-blocking dispatch pass (SOW-0013): for every live instance, if it is due and idle, build its
/// inputs from the CURRENT blackboard and send one `Tick` (marking it busy). A busy instance is
/// skipped (delayed to a later wake, never queued) and its skip counter bumped. Never blocks: the
/// `cmd_tx` send is to an unbounded channel an idle worker is already waiting on.
fn dispatch_due(
    state: &Arc<RwLock<AppState>>,
    input_map: &HashMap<String, Vec<String>>,
    cfg: &Config,
    wake_count: u64,
) {
    let now = Instant::now();
    let mut s = state.write().unwrap_or_else(|e| e.into_inner());

    // Reconcile scheduler slots with the live instance set (the supervisor owns `instances`):
    // drop slots for instances that vanished so they never leak.
    let live: HashSet<String> = s.instances.keys().cloned().collect();
    s.sched.retain(|k, _| live.contains(k));

    // Snapshot the dispatch decisions (key, inputs, cmd_tx, timeout) under the read of the
    // blackboard, then send after — keeping the critical section tight and lock-discipline simple.
    let mut to_send: Vec<(String, Option<Inputs>, mpsc::Sender<InstanceCmd>, Duration)> =
        Vec::new();
    let inputs_map = build_inputs(&s, input_map);

    let keys: Vec<String> = s.instances.keys().cloned().collect();
    for key in keys {
        let (module_name, cmd_tx) = {
            let e = &s.instances[&key];
            (e.module_name.clone(), e.cmd_tx.clone())
        };
        let sched = cfg.schedule_for(&module_name);
        let slot = s.sched.entry(key.clone()).or_default();
        if should_dispatch(slot.busy, slot.last_dispatch, now, sched.every) {
            let inputs = inputs_map.get(&key).cloned().flatten();
            slot.busy = true;
            slot.last_dispatch = Some(now);
            to_send.push((key, inputs, cmd_tx, sched.timeout));
        } else if slot.busy {
            // Due-but-busy is the delay-not-skip case worth counting; not-yet-due is just waiting.
            // NB: increments once per WAKE while due+busy (not once per missed `every` window), so it
            // scales with base_tick — read it as a relative "chronically slow?" signal, not an
            // absolute missed-dispatch count.
            if matches!(slot.last_dispatch, Some(t) if now.saturating_duration_since(t) >= sched.every)
            {
                slot.skipped_busy = slot.skipped_busy.saturating_add(1);
            }
        }
    }
    drop(s); // release the lock before the (non-blocking) sends + logging

    for (key, inputs, cmd_tx, timeout) in to_send {
        if let Some(ref inp) = inputs {
            info!(wake = wake_count, to = %key, inputs = %summarize_inputs(inp), "routing inputs");
        }
        if cmd_tx.send(InstanceCmd::Tick { timeout, inputs }).is_err() {
            // The worker is gone (it exited between snapshot and send). This dispatch never happened:
            // clear busy AND reset last_dispatch so the slot records no phantom dispatch. (The
            // supervisor will remove the instance and its slot shortly; until then the slot reflects
            // reality.)
            if let Some(slot) = state
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .sched
                .get_mut(&key)
            {
                slot.busy = false;
                slot.last_dispatch = None;
            }
        }
    }
}

/// Non-blocking reap pass (SOW-0013): drain every `TickReport` workers posted since the last wake
/// and fold each into shared state. A worker becomes idle the moment it posts, so clearing `busy`
/// here lets the next due wake re-dispatch a FRESH apply (run-latest-when-free, never replay).
fn reap_results(
    state: &Arc<RwLock<AppState>>,
    results_rx: &mpsc::Receiver<TickReport>,
    wake_count: u64,
) {
    // Collect everything currently queued without blocking.
    let reports: Vec<TickReport> = results_rx.try_iter().collect();
    if reports.is_empty() {
        return;
    }

    // Log each result (outside the write lock) so the journal shows readings the parent received.
    for r in &reports {
        info!(
            wake = wake_count,
            instance = %r.key,
            status = r.result.status.as_str(),
            latency_ms = r.latency.as_millis() as u64,
            error = r.result.error.as_deref().unwrap_or(""),
            readings = %summarize_readings(&r.result.readings),
            "apply result",
        );
    }

    apply_results(
        &mut state.write().unwrap_or_else(|e| e.into_inner()),
        reports,
        wake_count,
    );
}

/// Fold async `apply` results into shared state. Updates per-instance status/readings/latency AND
/// the blackboard ONLY for instances that still exist: a supervisor may have removed (and
/// blackboard-pruned) this instance between dispatch and now, so re-inserting its readings would
/// orphan a stale blackboard entry that nothing prunes again — routed to consumers forever. Gating
/// on liveness prevents that. Each report also clears the instance's `busy` flag (it is idle again)
/// and records its latency, so the scheduler can re-dispatch it when next due.
fn apply_results(s: &mut AppState, reports: Vec<TickReport>, wake_count: u64) {
    for r in reports {
        let TickReport {
            key,
            result,
            latency,
        } = r;
        let is_ok = result.status == TickStatus::Ok;

        // The worker posted -> it is idle again. Clear busy + record latency even if the instance
        // entry was just removed (the slot is pruned next dispatch pass either way).
        if let Some(slot) = s.sched.get_mut(&key) {
            slot.busy = false;
            slot.last_latency = Some(latency);
        }

        if let Some(e) = s.instances.get_mut(&key) {
            e.last_status = result.status.as_str().to_string();
            e.last_error = result.error;
            e.last_seen = Instant::now();
            if is_ok {
                e.last_readings = result.readings.clone();
            }
            if is_ok && !result.readings.is_empty() {
                s.blackboard.insert(key, result.readings);
            }
        }
    }
    s.tick_count = wake_count;
}

/// For each instance, if its module has `input=<peer...>`, gather every named peer's instances'
/// last readings from the blackboard into this module's `apply.inputs`. Keyed by the full
/// `module:id` blackboard key (not the bare peer id) so the consumer can attribute each reading to
/// its SOURCE MODULE (e.g. tell GPU temps from NVMe temps) and so keys never collide across
/// sources. Uninterpreted, one heartbeat stale.
fn build_inputs(
    state: &AppState,
    input_map: &HashMap<String, Vec<String>>,
) -> HashMap<String, Option<Inputs>> {
    let mut out = HashMap::with_capacity(state.instances.len());
    for (key, entry) in &state.instances {
        let sources = match input_map.get(&entry.module_name) {
            Some(srcs) if !srcs.is_empty() => srcs,
            _ => {
                out.insert(key.clone(), None);
                continue;
            }
        };
        let mut m: Inputs = HashMap::new();
        for src in sources {
            let prefix = format!("{src}:");
            for (bkey, readings) in &state.blackboard {
                // The `:` delimiter prevents typical prefix collisions (source "nv" does not match
                // "nvme:..."); module names are constrained not to contain `:`. Insert under the
                // full `module:id` key.
                if bkey.starts_with(&prefix) && !readings.is_empty() {
                    m.insert(bkey.clone(), readings.clone());
                }
            }
        }
        out.insert(key.clone(), if m.is_empty() { None } else { Some(m) });
    }
    out
}

/// Compact one-line summary of routed inputs (peer id → its temp readings) for the journal.
fn summarize_inputs(inputs: &Inputs) -> String {
    inputs
        .iter()
        .map(|(id, readings)| {
            let temps: Vec<String> = readings
                .iter()
                .filter(|r| r.kind == "temp")
                .filter_map(|r| r.get_i64("temp"))
                .map(|t| t.to_string())
                .collect();
            format!("{id}:temp={}", temps.join("/"))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Compact one-line summary of a readings list (every kind/label + its numeric fields).
fn summarize_readings(readings: &[Reading]) -> String {
    readings
        .iter()
        .map(|r| {
            let fields: Vec<String> = r.fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
            format!("{}/{}[{}]", r.kind, r.label, fields.join(","))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn graceful_shutdown(state: &Arc<RwLock<AppState>>, max_apply_timeout: Duration) {
    let txs: Vec<mpsc::Sender<InstanceCmd>> = {
        let s = state.read().unwrap_or_else(|e| e.into_inner());
        s.instances.values().map(|e| e.cmd_tx.clone()).collect()
    };
    info!(instances = txs.len(), "sending shutdown to all instances");
    for tx in &txs {
        let _ = tx.send(InstanceCmd::Shutdown);
    }
    // Wait (bounded) for every worker to restore its device and exit. A worker mid-`apply` only sees
    // the Shutdown once its in-flight apply finishes (up to its `timeout`), then runs its own ~2 s
    // restore grace — so the deadline must outlive `max_apply_timeout + that grace`, or we'd exit
    // before a slow module confirms restore. (Stdin EOF would still restore it, but we prefer to
    // confirm.) A 5 s floor keeps fast configs unchanged.
    let deadline =
        Instant::now() + (max_apply_timeout + Duration::from_secs(2)).max(Duration::from_secs(5));
    while ACTIVE_INSTANCES.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    let remaining = ACTIVE_INSTANCES.load(Ordering::Acquire);
    if remaining > 0 {
        warn!(
            remaining,
            "instances still active after grace; exiting anyway (stdin EOF restores)"
        );
    } else {
        info!("all instances shut down and devices restored");
    }
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct AppState {
    pub tick_count: u64,
    pub instances: HashMap<String, InstanceEntry>,
    /// Last good readings per instance key (`module:id`); pruned when an instance is removed.
    pub blackboard: HashMap<String, Vec<Reading>>,
    /// Per-module detect health (status + last declared error), for the status page.
    pub modules: HashMap<String, ModuleHealth>,
    /// Per-instance scheduler state (SOW-0013): busy/idle, last dispatch, last apply latency, and
    /// the delay-not-skip counter. Keyed by `module:id`, pruned when the instance is removed. Added
    /// here so the status page / metrics can surface per-instance cadence health (read-only).
    pub sched: HashMap<String, InstanceSched>,
}

/// Per-instance scheduler bookkeeping owned by the main loop (SOW-0013). `last_dispatch` is an
/// `Instant`, so this is not serialized directly — consumers read `last_latency`/`skipped_busy`.
#[derive(Default, Clone)]
pub struct InstanceSched {
    /// True between dispatching an `apply` and reaping its result (at most one in flight).
    pub busy: bool,
    /// When the most recent `apply` was dispatched (`None` until the first dispatch).
    pub last_dispatch: Option<Instant>,
    /// Wall-clock the most recent completed `apply` took (round-trip incl. kill on timeout).
    pub last_latency: Option<Duration>,
    /// How many times a dispatch was delayed because the instance was still busy (due-but-busy).
    pub skipped_busy: u64,
}

/// A module's last detect outcome, as declared by the module (ok/error/fatal/unresponsive/…).
#[derive(Clone, Default)]
pub struct ModuleHealth {
    pub detect_status: String,
    pub detect_error: Option<String>,
}

pub struct InstanceEntry {
    pub module_name: String,
    pub id: String,
    pub name: String,
    pub last_status: String,
    pub last_error: Option<String>,
    pub last_readings: Vec<Reading>,
    pub restart_count: u32,
    /// Wall-clock of this instance's last reported result (initialised to spawn time). Drives the
    /// cadence-independent `seconds_since_seen` staleness metric — SOW-0013 turned the tick into a
    /// 100 ms wake, so a tick-count staleness was no longer a stable unit.
    pub last_seen: Instant,
    pub cmd_tx: mpsc::Sender<InstanceCmd>,
    pub stderr_tail: StderrTail,
}

// ---------------------------------------------------------------------------
// Signal handling
// ---------------------------------------------------------------------------

fn install_signal_handlers() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handle_signal as usize;
        sa.sa_flags = 0;
        for sig in [libc::SIGTERM, libc::SIGINT] {
            if libc::sigaction(sig, &sa, std::ptr::null_mut()) < 0 {
                warn!(signal = sig, "failed to install signal handler");
            }
        }
    }
}

extern "C" fn handle_signal(_sig: i32) {
    // async-signal-safe: only a relaxed atomic store.
    SHUTDOWN_FLAG.store(true, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;
    use instance::TickResult;

    fn report(key: &str, status: TickStatus, readings: Vec<Reading>) -> TickReport {
        TickReport {
            key: key.to_string(),
            result: TickResult {
                status,
                error: None,
                readings,
            },
            latency: Duration::from_millis(7),
        }
    }

    #[test]
    fn apply_results_does_not_resurrect_a_removed_instances_blackboard_entry() {
        // Race guard: a result arriving for an instance the supervisor already removed must NOT
        // re-create a blackboard entry (which nothing would prune again -> stale routed forever).
        use protocol::Reading;
        use serde_json::json;

        let mut s = AppState::default();
        // One live instance "mod:a"; "mod:ghost" is intentionally absent (already removed).
        let (tx, _rx) = mpsc::channel();
        s.instances.insert(
            "mod:a".to_string(),
            InstanceEntry {
                module_name: "mod".into(),
                id: "a".into(),
                name: "a".into(),
                last_status: "starting".into(),
                last_error: None,
                last_readings: Vec::new(),
                restart_count: 0,
                last_seen: Instant::now(),
                cmd_tx: tx,
                stderr_tail: Arc::new(Mutex::new(VecDeque::new())),
            },
        );

        let mk = |t: i64| vec![Reading::new("temp", "GPU", json!({ "temp": t }))];
        apply_results(
            &mut s,
            vec![
                report("mod:a", TickStatus::Ok, mk(50)),
                report("mod:ghost", TickStatus::Ok, mk(99)),
            ],
            1,
        );

        assert!(
            s.blackboard.contains_key("mod:a"),
            "a live instance's readings must be stored"
        );
        assert!(
            !s.blackboard.contains_key("mod:ghost"),
            "a removed instance must NOT get a (resurrected) blackboard entry"
        );
    }

    #[test]
    fn apply_results_clears_busy_and_records_latency() {
        // Reaping a worker's report marks the instance idle again (so it can be re-dispatched) and
        // records the apply latency for surfacing.
        let mut s = AppState::default();
        s.sched.insert(
            "mod:a".into(),
            InstanceSched {
                busy: true,
                last_dispatch: Some(Instant::now()),
                last_latency: None,
                skipped_busy: 0,
            },
        );
        apply_results(&mut s, vec![report("mod:a", TickStatus::Ok, Vec::new())], 5);
        let slot = &s.sched["mod:a"];
        assert!(!slot.busy, "worker posted -> instance is idle again");
        assert_eq!(slot.last_latency, Some(Duration::from_millis(7)));
        assert_eq!(s.tick_count, 5);
    }

    #[test]
    fn should_dispatch_gates_on_due_and_idle() {
        let now = Instant::now();
        let every = Duration::from_secs(1);

        // Never dispatched + idle -> due immediately.
        assert!(should_dispatch(false, None, now, every));
        // Busy is never dispatched (at most one apply in flight; delayed, not queued).
        assert!(!should_dispatch(true, None, now, every));
        // Idle but not yet due -> wait.
        let half_ago = now - every / 2;
        assert!(!should_dispatch(false, Some(half_ago), now, every));
        // Idle and the full `every` has elapsed -> due.
        let full_ago = now - every;
        assert!(should_dispatch(false, Some(full_ago), now, every));
        // Busy AND due -> still not dispatched (delay-not-skip).
        assert!(!should_dispatch(true, Some(full_ago), now, every));
    }

    #[test]
    fn should_dispatch_respects_every_equal_base_tick() {
        // With every == base_tick (the floor), an instance is due every wake once idle.
        let now = Instant::now();
        let every = Duration::from_millis(100);
        let one_tick_ago = now - every;
        assert!(should_dispatch(false, Some(one_tick_ago), now, every));
        // Just dispatched this wake -> not due yet.
        assert!(!should_dispatch(false, Some(now), now, every));
    }

    #[test]
    fn build_inputs_merges_multiple_sources_keyed_by_module_id() {
        // Multi-input routing: a consumer wired `input=nvidia input=nvme` must receive BOTH
        // sources' readings, keyed by the full `module:id` (so it can attribute source), and must
        // NOT receive an unrelated module's readings.
        use protocol::Reading;
        use serde_json::json;

        let mut s = AppState::default();
        let (tx, _rx) = mpsc::channel();
        s.instances.insert(
            "asrock16-2t:board".to_string(),
            InstanceEntry {
                module_name: "asrock16-2t".into(),
                id: "board".into(),
                name: "board".into(),
                last_status: "ok".into(),
                last_error: None,
                last_readings: Vec::new(),
                restart_count: 0,
                last_seen: Instant::now(),
                cmd_tx: tx,
                stderr_tail: Arc::new(Mutex::new(VecDeque::new())),
            },
        );
        s.blackboard.insert(
            "nvidia:GPU-1".into(),
            vec![Reading::new("temp", "GPU", json!({"temp": 63}))],
        );
        s.blackboard.insert(
            "nvme:SER-A".into(),
            vec![Reading::new("temp", "Composite", json!({"temp": 40}))],
        );
        s.blackboard.insert(
            "nvme:SER-B".into(),
            vec![Reading::new("temp", "Composite", json!({"temp": 44}))],
        );
        // Unrelated module — must NOT be routed to asrock.
        s.blackboard.insert(
            "other:x".into(),
            vec![Reading::new("temp", "x", json!({"temp": 99}))],
        );

        let mut input_map: HashMap<String, Vec<String>> = HashMap::new();
        input_map.insert("asrock16-2t".into(), vec!["nvidia".into(), "nvme".into()]);

        let out = build_inputs(&s, &input_map);
        let inputs = out
            .get("asrock16-2t:board")
            .expect("consumer present")
            .as_ref()
            .expect("inputs routed");
        assert_eq!(inputs.len(), 3, "both sources merged (1 GPU + 2 NVMe)");
        assert!(inputs.contains_key("nvidia:GPU-1"), "keyed by module:id");
        assert!(inputs.contains_key("nvme:SER-A"));
        assert!(inputs.contains_key("nvme:SER-B"));
        assert!(
            !inputs.contains_key("other:x"),
            "unwired module must not be routed"
        );
    }

    #[test]
    fn build_inputs_none_when_no_source_readings_and_skips_empty() {
        use protocol::Reading;
        use serde_json::json;

        // A consumer instance wired to `sources`, with an empty blackboard to start.
        fn consumer(sources: Vec<String>) -> (AppState, HashMap<String, Vec<String>>) {
            let mut s = AppState::default();
            let (tx, _rx) = mpsc::channel();
            s.instances.insert(
                "asrock16-2t:board".to_string(),
                InstanceEntry {
                    module_name: "asrock16-2t".into(),
                    id: "board".into(),
                    name: "board".into(),
                    last_status: "ok".into(),
                    last_error: None,
                    last_readings: Vec::new(),
                    restart_count: 0,
                    last_seen: Instant::now(),
                    cmd_tx: tx,
                    stderr_tail: Arc::new(Mutex::new(VecDeque::new())),
                },
            );
            let mut map = HashMap::new();
            map.insert("asrock16-2t".to_string(), sources);
            (s, map)
        }

        // (a) sources wired but blackboard empty -> None (the `inputs` key is omitted, not `{}`).
        let (s, map) = consumer(vec!["nvidia".into(), "nvme".into()]);
        assert!(build_inputs(&s, &map)["asrock16-2t:board"].is_none());

        // (b) two sources, only one has readings -> only the present source is routed.
        let (mut s, map) = consumer(vec!["nvidia".into(), "nvme".into()]);
        s.blackboard.insert(
            "nvidia:GPU-1".into(),
            vec![Reading::new("temp", "GPU", json!({"temp": 70}))],
        );
        let got = build_inputs(&s, &map);
        let inputs = got["asrock16-2t:board"]
            .as_ref()
            .expect("the present source must route");
        assert_eq!(inputs.len(), 1);
        assert!(inputs.contains_key("nvidia:GPU-1"));

        // (c) a source instance with an EMPTY readings list is skipped (never routed as empty).
        let (mut s, map) = consumer(vec!["nvme".into()]);
        s.blackboard.insert("nvme:SER-A".into(), Vec::new());
        assert!(build_inputs(&s, &map)["asrock16-2t:board"].is_none());
    }

    #[test]
    fn watchdog_respawns_only_a_dead_supervisor_after_backoff() {
        let past = SUPERVISOR_RESPAWN_BACKOFF + Duration::from_secs(1);
        // Dead, backoff elapsed, not shutting down -> respawn.
        assert!(should_respawn(true, past, false));
        // Still alive -> never respawn.
        assert!(!should_respawn(false, past, false));
        // Dead but inside the backoff window -> wait (no tight crash loop).
        assert!(!should_respawn(true, Duration::from_secs(0), false));
        // Shutting down -> a finished thread is expected; never respawn.
        assert!(!should_respawn(true, past, true));
    }
}
