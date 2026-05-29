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
use instance::{InstanceCmd, TickResult, TickStatus};
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
        tick_s = cfg.tick.as_secs(),
        timeout_s = cfg.timeout.as_secs(),
        detect_every_s = cfg.detect_every.as_secs(),
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

    // module_name -> input module (for routing).
    let input_map: HashMap<String, Option<String>> = cfg
        .registry
        .iter()
        .map(|e| (e.module_name.clone(), e.input.clone()))
        .collect();

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
            ),
            respawns: 0,
            last_spawn: Instant::now(),
        })
        .collect();

    let tick = cfg.tick;
    let timeout = cfg.timeout;
    let mut next_tick = Instant::now() + tick;
    let mut tick_count: u64 = 0;

    loop {
        if SHUTDOWN_FLAG.load(Ordering::Acquire) {
            info!("signal received — graceful shutdown");
            graceful_shutdown(&state);
            break;
        }

        // Watchdog: if a supervisor thread died (panicked), respawn it (its Drop already cleaned up
        // its instances, so the fresh supervisor re-detects from a clean slate). Backoff-bounded.
        respawn_dead_supervisors(&mut supervisors, &state, &cfg);

        let now = Instant::now();
        if now < next_tick {
            // Sleep in short steps so a signal is noticed promptly between ticks.
            thread::sleep((next_tick - now).min(Duration::from_millis(200)));
            continue;
        }
        next_tick = now + tick;
        tick_count += 1;

        heartbeat(&state, &input_map, timeout, tick_count);
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

type Snapshot = Vec<(String, mpsc::Sender<InstanceCmd>)>;
type InputsMap = HashMap<String, Option<Inputs>>;

/// One heartbeat: fan out `apply` to every instance, then collect replies under one shared
/// deadline, then update state + blackboard. No instance waits on another.
fn heartbeat(
    state: &Arc<RwLock<AppState>>,
    input_map: &HashMap<String, Option<String>>,
    timeout: Duration,
    tick_count: u64,
) {
    // Snapshot instances + build each one's inputs from the PREVIOUS tick's blackboard.
    let (snapshot, inputs_map): (Snapshot, InputsMap) = {
        let s = state.read().unwrap();
        let snapshot = s
            .instances
            .iter()
            .map(|(k, v)| (k.clone(), v.cmd_tx.clone()))
            .collect();
        let inputs_map = build_inputs(&s, input_map);
        (snapshot, inputs_map)
    };

    // Fan out.
    let backstop = Instant::now() + timeout + Duration::from_secs(1);
    let mut pending: Vec<(String, mpsc::Receiver<TickResult>)> = Vec::new();
    for (key, cmd_tx) in snapshot {
        let inputs = inputs_map.get(&key).cloned().flatten();
        if let Some(ref inp) = inputs {
            info!(tick = tick_count, to = %key, inputs = %summarize_inputs(inp), "routing inputs");
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        if cmd_tx
            .send(InstanceCmd::Tick {
                timeout,
                inputs,
                reply: reply_tx,
            })
            .is_ok()
        {
            pending.push((key, reply_rx));
        }
    }

    // Collect (bounded by the shared backstop).
    let mut results: Vec<(String, TickResult)> = Vec::new();
    for (key, reply_rx) in pending {
        let remaining = backstop.saturating_duration_since(Instant::now());
        if let Ok(result) = reply_rx.recv_timeout(remaining) {
            results.push((key, result));
        } else {
            warn!(key=%key, "no heartbeat reply within backstop");
        }
    }

    // Log each result (outside the write lock) so the journal shows readings the parent received.
    for (key, r) in &results {
        info!(
            tick = tick_count,
            instance = %key,
            status = r.status.as_str(),
            error = r.error.as_deref().unwrap_or(""),
            readings = %summarize_readings(&r.readings),
            "tick result",
        );
    }

    // Apply results.
    let mut s = state.write().unwrap();
    for (key, r) in results {
        let is_ok = r.status == TickStatus::Ok;
        if let Some(e) = s.instances.get_mut(&key) {
            e.last_status = r.status.as_str().to_string();
            e.last_error = r.error;
            e.last_seen_tick = tick_count;
            if is_ok {
                e.last_readings = r.readings.clone();
            }
        }
        if is_ok && !r.readings.is_empty() {
            s.blackboard.insert(key, r.readings);
        }
    }
    s.tick_count = tick_count;
}

/// For each instance, if its module has `input=<peer>`, gather that peer's instances' last
/// readings (keyed by peer id) from the blackboard. Uninterpreted, one heartbeat stale.
fn build_inputs(
    state: &AppState,
    input_map: &HashMap<String, Option<String>>,
) -> HashMap<String, Option<Inputs>> {
    let mut out = HashMap::with_capacity(state.instances.len());
    for (key, entry) in &state.instances {
        let inputs = match input_map.get(&entry.module_name) {
            Some(Some(src)) => {
                let prefix = format!("{src}:");
                let mut m: Inputs = HashMap::new();
                for (bkey, readings) in &state.blackboard {
                    if let Some(peer_id) = bkey.strip_prefix(&prefix) {
                        if !readings.is_empty() {
                            m.insert(peer_id.to_string(), readings.clone());
                        }
                    }
                }
                if m.is_empty() {
                    None
                } else {
                    Some(m)
                }
            }
            _ => None,
        };
        out.insert(key.clone(), inputs);
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

fn graceful_shutdown(state: &Arc<RwLock<AppState>>) {
    let txs: Vec<mpsc::Sender<InstanceCmd>> = {
        let s = state.read().unwrap();
        s.instances.values().map(|e| e.cmd_tx.clone()).collect()
    };
    info!(instances = txs.len(), "sending shutdown to all instances");
    for tx in &txs {
        let _ = tx.send(InstanceCmd::Shutdown);
    }
    // Wait (bounded) for every worker to restore its device and exit.
    let deadline = Instant::now() + Duration::from_secs(5);
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
    pub last_seen_tick: u64,
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
