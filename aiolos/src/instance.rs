//! A running module instance bound to one detected ID, plus the deadline-bounded, non-blocking
//! stdio primitives used to talk to every module child (also reused by the detect process).
//!
//! The central robustness property: NO orchestrator-side read or write on a child pipe can ever
//! block past the caller's deadline. We set both pipe ends non-blocking and drive them with
//! `poll(2)` against a wall-clock deadline. A module that writes a partial line, floods stdout
//! without a newline, or stops reading its stdin is killed within ~timeout — it can never wedge
//! the instance thread (which would otherwise defeat the isolation guarantee).

use anyhow::Result;
use protocol::{Inputs, Request, Response};
use std::ffi::c_void;
use std::io;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

/// Max bytes we will buffer for a single response line before declaring a protocol violation.
/// Generous for any legitimate readings list; bounds memory against a stdout flood.
const MAX_LINE: usize = 256 * 1024;

// ---------------------------------------------------------------------------
// Tick result
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickStatus {
    /// Module replied `status:ok`.
    Ok,
    /// Module replied `status:error` (transient; instance kept — detect reconciles real loss).
    Error,
    /// No response within the deadline; the child was SIGKILLed.
    Timeout,
    /// The child exited / stdin broke (EOF).
    Dead,
    /// Malformed or unexpected response; the child was SIGKILLed.
    Protocol,
}

impl TickStatus {
    /// Fatal results make the instance worker exit so the supervisor respawns it.
    pub fn is_fatal(self) -> bool {
        matches!(
            self,
            TickStatus::Timeout | TickStatus::Dead | TickStatus::Protocol
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TickStatus::Ok => "ok",
            TickStatus::Error => "error",
            TickStatus::Timeout => "timeout",
            TickStatus::Dead => "dead",
            TickStatus::Protocol => "protocol_error",
        }
    }
}

pub struct TickResult {
    pub status: TickStatus,
    pub error: Option<String>,
    pub readings: Vec<protocol::Reading>,
}

impl TickResult {
    fn simple(status: TickStatus, error: Option<String>) -> Self {
        TickResult {
            status,
            error,
            readings: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Commands the supervisor / main loop send to an instance worker
// ---------------------------------------------------------------------------

pub enum InstanceCmd {
    Tick {
        timeout: Duration,
        inputs: Option<Inputs>,
        reply: SyncSender<TickResult>,
    },
    Shutdown,
}

// ---------------------------------------------------------------------------
// Instance
// ---------------------------------------------------------------------------

pub struct Instance {
    pub module_name: String,
    pub id: String,
    child: Child,
    stdin_fd: RawFd,
    stdin: Option<ChildStdin>, // kept to own the fd; taken+dropped to send EOF on shutdown
    reader: LineReader,
    /// Taken once by the supervisor to spawn the stderr-tail reader.
    pub stderr: Option<ChildStderr>,
}

impl Instance {
    pub fn new(bin_dir: &Path, module_name: String, id: String) -> Result<Self> {
        let bin = bin_dir.join(&module_name);
        let mut child = Command::new(&bin)
            .arg("run")
            .arg(&id)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| anyhow::anyhow!("spawn {}: {e}", bin.display()))?;

        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take();

        let stdin_fd = stdin.as_raw_fd();
        set_nonblocking(stdin_fd)?;
        let reader = LineReader::new(stdout)?;

        Ok(Instance {
            module_name,
            id,
            child,
            stdin_fd,
            stdin: Some(stdin),
            reader,
            stderr,
        })
    }

    /// Send one `apply` and collect one response within `timeout`. Never blocks past `timeout`.
    pub fn tick(&mut self, timeout: Duration, inputs: Option<Inputs>) -> TickResult {
        let deadline = Instant::now() + timeout;

        let line = match (Request::Apply { inputs }).to_line() {
            Ok(l) => l,
            Err(e) => {
                return TickResult::simple(TickStatus::Error, Some(format!("serialize: {e}")))
            }
        };

        if self.stdin.is_none() {
            return TickResult::simple(TickStatus::Dead, Some("stdin closed".into()));
        }
        match write_line_deadline(self.stdin_fd, &line, deadline) {
            Ok(true) => {}
            Ok(false) => {
                warn!(module=%self.module_name, id=%self.id, "stdin write timeout — SIGKILL");
                self.kill();
                return TickResult::simple(TickStatus::Timeout, None);
            }
            Err(e) => return TickResult::simple(TickStatus::Dead, Some(format!("stdin: {e}"))),
        }

        loop {
            match self.reader.read_line_deadline(deadline, MAX_LINE) {
                ReadOutcome::Line(s) => match Response::from_line(&s) {
                    Ok(Response::Hello(h)) => {
                        debug!(module=%self.module_name, id=%self.id, name=%h.hello.name, "hello");
                        continue; // skip optional hello, keep reading within the deadline
                    }
                    Ok(Response::Applied(ap)) => {
                        let status = match ap.status {
                            protocol::Status::Ok => TickStatus::Ok,
                            protocol::Status::Error => TickStatus::Error,
                        };
                        return TickResult {
                            status,
                            error: ap.error,
                            readings: ap.readings.unwrap_or_default(),
                        };
                    }
                    Ok(Response::Found(_)) => {
                        self.kill();
                        return TickResult::simple(
                            TickStatus::Protocol,
                            Some("unexpected 'found' in run mode".into()),
                        );
                    }
                    Err(e) => {
                        self.kill();
                        return TickResult::simple(
                            TickStatus::Protocol,
                            Some(format!("parse: {e}")),
                        );
                    }
                },
                ReadOutcome::Eof => return TickResult::simple(TickStatus::Dead, None),
                ReadOutcome::Timeout => {
                    warn!(module=%self.module_name, id=%self.id, "apply timeout — SIGKILL");
                    self.kill();
                    return TickResult::simple(TickStatus::Timeout, None);
                }
                ReadOutcome::TooLong => {
                    warn!(module=%self.module_name, id=%self.id, "stdout flood — SIGKILL");
                    self.kill();
                    return TickResult::simple(
                        TickStatus::Protocol,
                        Some("response exceeded max line length".into()),
                    );
                }
                ReadOutcome::Io(e) => {
                    return TickResult::simple(TickStatus::Dead, Some(format!("read: {e}")))
                }
            }
        }
    }

    /// Graceful stop: send `shutdown`, also close stdin (EOF) as a backup trigger for the module's
    /// device-restore, then wait up to `grace` for exit; SIGKILL if it overstays.
    pub fn shutdown(&mut self, grace: Duration) {
        let deadline = Instant::now() + grace;
        if let Ok(line) = Request::Shutdown.to_line() {
            let _ = write_line_deadline(self.stdin_fd, &line, deadline);
        }
        // Drop the write end: the module's stdin EOF path restores the device even if it ignored
        // the shutdown line. (Spec: shutdown OR EOF both restore.)
        self.stdin = None;
        self.wait_until(deadline);
    }

    fn wait_until(&mut self, deadline: Instant) {
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        warn!(module=%self.module_name, id=%self.id, "did not exit in grace — SIGKILL");
                        self.kill();
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return,
            }
        }
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // Universal reaper: guarantees no zombie regardless of exit path (timeout/dead/shutdown/
        // panic). kill is harmless if the child already exited; wait reaps the zombie.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// Low-level, deadline-bounded, non-blocking pipe I/O (shared with module.rs detect)
// ---------------------------------------------------------------------------

pub(crate) enum ReadOutcome {
    Line(String),
    Eof,
    Timeout,
    TooLong,
    Io(io::Error),
}

/// Reads whole `\n`-delimited lines from a non-blocking fd without ever blocking past a deadline.
pub(crate) struct LineReader {
    fd: RawFd,
    _stdout: ChildStdout, // ownership keeps the fd open
    buf: Vec<u8>,
}

impl LineReader {
    pub(crate) fn new(stdout: ChildStdout) -> Result<Self> {
        let fd = stdout.as_raw_fd();
        set_nonblocking(fd)?;
        Ok(LineReader {
            fd,
            _stdout: stdout,
            buf: Vec::with_capacity(1024),
        })
    }

    pub(crate) fn read_line_deadline(&mut self, deadline: Instant, max_len: usize) -> ReadOutcome {
        loop {
            if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
                line.pop(); // drop '\n'
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return ReadOutcome::Line(String::from_utf8_lossy(&line).into_owned());
            }
            if self.buf.len() > max_len {
                return ReadOutcome::TooLong;
            }
            let now = Instant::now();
            if now >= deadline {
                return ReadOutcome::Timeout;
            }
            match poll_fd(self.fd, libc::POLLIN, remaining_ms(deadline, now)) {
                Ok(false) => return ReadOutcome::Timeout,
                Ok(true) => {
                    let mut tmp = [0u8; 8192];
                    let n =
                        unsafe { libc::read(self.fd, tmp.as_mut_ptr() as *mut c_void, tmp.len()) };
                    if n == 0 {
                        return ReadOutcome::Eof;
                    }
                    if n < 0 {
                        let e = io::Error::last_os_error();
                        match e.raw_os_error() {
                            Some(libc::EAGAIN) | Some(libc::EINTR) => continue,
                            _ => return ReadOutcome::Io(e),
                        }
                    } else {
                        self.buf.extend_from_slice(&tmp[..n as usize]);
                    }
                }
                Err(e) => return ReadOutcome::Io(e),
            }
        }
    }
}

/// Write `line` + `\n` to a non-blocking fd. `Ok(true)` = fully written; `Ok(false)` = deadline
/// hit (peer not draining); `Err` = real I/O error (e.g. EPIPE — peer gone).
pub(crate) fn write_line_deadline(fd: RawFd, line: &str, deadline: Instant) -> io::Result<bool> {
    let mut out = Vec::with_capacity(line.len() + 1);
    out.extend_from_slice(line.as_bytes());
    out.push(b'\n');
    let mut data: &[u8] = &out;

    while !data.is_empty() {
        let now = Instant::now();
        if now >= deadline {
            return Ok(false);
        }
        match poll_fd(fd, libc::POLLOUT, remaining_ms(deadline, now))? {
            false => return Ok(false),
            true => {
                let n = unsafe { libc::write(fd, data.as_ptr() as *const c_void, data.len()) };
                if n < 0 {
                    let e = io::Error::last_os_error();
                    match e.raw_os_error() {
                        Some(libc::EAGAIN) | Some(libc::EINTR) => continue,
                        _ => return Err(e),
                    }
                } else {
                    data = &data[n as usize..];
                }
            }
        }
    }
    Ok(true)
}

pub(crate) fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// poll a single fd for `events`; retries on EINTR. Returns Ok(true) if ready, Ok(false) on timeout.
pub(crate) fn poll_fd(fd: RawFd, events: libc::c_short, timeout_ms: i32) -> io::Result<bool> {
    loop {
        let mut pfd = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let r = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, timeout_ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(e);
        }
        return Ok(r > 0);
    }
}

fn remaining_ms(deadline: Instant, now: Instant) -> i32 {
    let ms = deadline.saturating_duration_since(now).as_millis();
    if ms == 0 {
        1 // sub-millisecond remaining: poll once more, then the deadline check ends it
    } else {
        ms.min(i32::MAX as u128) as i32
    }
}
