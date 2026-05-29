//! Per-module supervisor: one persistent `detect` process + the `run` instances it reconciles.
//!
//! The supervisor is the SOLE writer of `state.instances` for its module's keys (insert on spawn,
//! remove on vanish/death) — so there is no registration race with the worker threads. It prunes
//! the blackboard whenever it removes an instance, so stale readings are never relayed as inputs.

use crate::instance::{
    set_nonblocking, write_line_deadline, Instance, InstanceCmd, LineReader, ReadOutcome,
};
use crate::registry::RegistryEntry;
use crate::{AppState, InstanceEntry, StderrTail, ACTIVE_INSTANCES, SHUTDOWN_FLAG};
use protocol::{FoundEntry, Request, Response};
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

/// Spawn a supervisor thread for one registry entry.
pub fn run_module(
    entry: RegistryEntry,
    state: Arc<RwLock<AppState>>,
    bin_dir: PathBuf,
    detect_every: Duration,
) -> thread::JoinHandle<()> {
    let module_name = entry.module_name.clone();
    info!(module=%module_name, "starting module supervisor");
    thread::spawn(move || {
        Supervisor {
            module_name,
            state,
            bin_dir,
            detect_every,
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

struct Supervisor {
    module_name: String,
    state: Arc<RwLock<AppState>>,
    bin_dir: PathBuf,
    detect_every: Duration,
    running: HashMap<String, (mpsc::Sender<InstanceCmd>, thread::JoinHandle<()>)>,
    detect_proc: Option<DetectProc>,
    last_found: Vec<FoundEntry>,
    /// Per-id crash-loop backoff: (consecutive failures, last failure time).
    backoff: HashMap<String, (u32, Instant)>,
    /// Per-id lifetime spawn count (restart_count = spawn_count - 1).
    spawn_counts: HashMap<String, u32>,
}

impl Supervisor {
    fn run(&mut self) {
        let _ = self.run_detect(); // initial; logged on failure
        self.reconcile();
        let mut last_detect = Instant::now();

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
            if last_detect.elapsed() >= self.detect_every {
                let _ = self.run_detect();
                last_detect = Instant::now();
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

    /// Run one detect round-trip; on success update `last_found`. Kills+drops the detect proc on
    /// any I/O error/timeout so it respawns next call.
    fn run_detect(&mut self) -> anyhow::Result<()> {
        if let Err(e) = self.ensure_detect_proc() {
            warn!(module=%self.module_name, error=%e, "detect spawn failed");
            return Err(e);
        }
        let deadline = Instant::now() + DETECT_TIMEOUT;
        let p = self.detect_proc.as_mut().unwrap();

        if let Ok(line) = Request::Detect.to_line() {
            match write_line_deadline(p.stdin_fd, &line, deadline) {
                Ok(true) => {}
                _ => {
                    self.kill_detect();
                    return Err(anyhow::anyhow!("detect write failed/timeout"));
                }
            }
        }

        loop {
            match p.reader.read_line_deadline(deadline, MAX_DETECT_LINE) {
                ReadOutcome::Line(s) => match Response::from_line(&s) {
                    Ok(Response::Hello(_)) => continue, // skip optional hello
                    Ok(Response::Found(f)) => {
                        self.last_found = f.found;
                        return Ok(());
                    }
                    Ok(Response::Applied(_)) => {
                        warn!(module=%self.module_name, "unexpected 'applied' in detect");
                        return Ok(()); // keep last_found
                    }
                    Err(e) => {
                        self.kill_detect();
                        return Err(anyhow::anyhow!("detect parse: {e}"));
                    }
                },
                ReadOutcome::Eof | ReadOutcome::Timeout | ReadOutcome::TooLong => {
                    warn!(module=%self.module_name, "detect read failed — respawning");
                    self.kill_detect();
                    return Err(anyhow::anyhow!("detect read failed"));
                }
                ReadOutcome::Io(e) => {
                    self.kill_detect();
                    return Err(anyhow::anyhow!("detect io: {e}"));
                }
            }
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
            s.blackboard.remove(&key); // prune: never relay a dead instance's stale readings
        }
    }

    fn key(&self, id: &str) -> String {
        format!("{}:{}", self.module_name, id)
    }

    // ----- backoff ------------------------------------------------------------

    fn backoff_expired(&self, id: &str) -> bool {
        match self.backoff.get(id) {
            None => true,
            Some((count, last)) => {
                let delay = Duration::from_secs(2u64.saturating_pow(*count).min(300));
                last.elapsed() >= delay
            }
        }
    }

    fn record_failure(&mut self, id: &str) {
        let e = self
            .backoff
            .entry(id.to_string())
            .or_insert((0, Instant::now()));
        e.0 = e.0.saturating_add(1);
        e.1 = Instant::now();
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
            warn!(module=%self.module_name, id=%id, "instance worker exited — will respawn (backoff)");
            self.running.remove(&id);
            self.remove_instance_state(&id);
            self.record_failure(&id);
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
                    last_seen_tick: 0,
                    cmd_tx: cmd_tx.clone(),
                    stderr_tail: tail,
                },
            );
        }

        let module_name = self.module_name.clone();
        let id_for_thread = id.clone();
        let handle = thread::spawn(move || {
            worker(inst, cmd_rx, &module_name, &id_for_thread);
        });

        self.running.insert(id.clone(), (cmd_tx, handle));
        info!(module=%self.module_name, id=%id, restart=restart_count, "instance spawned");
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.kill_detect();
    }
}

/// Instance worker thread body: owns the child, services Tick/Shutdown, exits on a fatal result so
/// the supervisor respawns it. Tracks the live-instance count for graceful shutdown.
fn worker(mut inst: Instance, cmd_rx: mpsc::Receiver<InstanceCmd>, module_name: &str, id: &str) {
    ACTIVE_INSTANCES.fetch_add(1, Ordering::AcqRel);
    loop {
        match cmd_rx.recv() {
            Err(_) => break, // all senders dropped
            Ok(InstanceCmd::Tick {
                timeout,
                inputs,
                reply,
            }) => {
                let res = inst.tick(timeout, inputs);
                let fatal = res.status.is_fatal();
                let _ = reply.send(res);
                if fatal {
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
    ACTIVE_INSTANCES.fetch_sub(1, Ordering::AcqRel);
}
