//! Signal-aware, line-delimited stdin reader for anemoi (the module side of the stdio protocol).
//!
//! A module's `run` loop holds a hardware claim (manual fan control) that MUST be restored before
//! the process exits — on a `shutdown` request, on stdin EOF (parent gone), AND on a termination
//! signal. std's blocking `read_line` retries across `EINTR`, so a `SIGTERM` arriving while the
//! loop is parked between ticks would NOT wake it. This reader instead sets stdin non-blocking and
//! waits with `poll(2)` in short steps, checking an async-signal-safe shutdown flag set by the
//! `SIGTERM`/`SIGINT` handler. The loop therefore wakes promptly and runs its fail-safe restore in
//! normal (allocation-safe) code — never inside the signal handler (NVML/IPMI allocate + take
//! locks, so restoring there could deadlock).

use std::io;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Set by the SIGTERM/SIGINT handler; polled by `StdinReader`. Process-global (one run loop per
/// process).
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// True once a termination signal (SIGTERM/SIGINT) has been received.
pub fn shutdown_requested() -> bool {
    SHUTDOWN.load(Ordering::Acquire)
}

/// Install SIGTERM + SIGINT handlers that request a graceful shutdown. The handler does ONLY an
/// async-signal-safe atomic store; the actual device restore happens in the run loop. Call once at
/// module startup, before the loop. `SA_RESTART` is intentionally NOT set so a signal interrupts a
/// blocked `poll(2)` and the loop wakes immediately.
pub fn install_shutdown_handlers() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_signal as usize;
        sa.sa_flags = 0;
        for sig in [libc::SIGTERM, libc::SIGINT] {
            if libc::sigaction(sig, &sa, std::ptr::null_mut()) != 0 {
                // Near-impossible for SIGTERM/SIGINT, but if it fails the module would die on that
                // signal WITHOUT restoring its device — surface it rather than fail silently.
                eprintln!(
                    "WARNING: sigaction({sig}) failed: {} — device may not restore on this signal",
                    std::io::Error::last_os_error()
                );
            }
        }
    }
}

extern "C" fn on_signal(_sig: libc::c_int) {
    // async-signal-safe: only a relaxed-release atomic store.
    SHUTDOWN.store(true, Ordering::Release);
}

/// What the reader observed while waiting for the next protocol line.
pub enum Event {
    /// One complete protocol line (newline stripped).
    Line(String),
    /// A termination signal was received — restore the device and exit.
    Shutdown,
    /// stdin was closed by the parent (or an unrecoverable read error) — restore and exit.
    Eof,
}

/// Max bytes buffered for a single line before the buffer is dropped as a flood (bounds memory).
const MAX_LINE: usize = 1 << 20;

/// Non-blocking, signal-aware line reader over a module's stdin (fd 0 by default).
pub struct StdinReader {
    fd: RawFd,
    buf: Vec<u8>,
}

impl StdinReader {
    /// Wrap this process's stdin (fd 0) and set it non-blocking.
    pub fn new() -> io::Result<Self> {
        Self::from_fd(libc::STDIN_FILENO)
    }

    /// Wrap an arbitrary fd (used by tests; `new` wraps stdin).
    pub fn from_fd(fd: RawFd) -> io::Result<Self> {
        set_nonblocking(fd)?;
        Ok(StdinReader {
            fd,
            buf: Vec::with_capacity(4096),
        })
    }

    /// Wait for the next complete line, a shutdown signal, or EOF. Polls in `step` increments so a
    /// signal is noticed within ~`step` even with no stdin traffic; a signal also interrupts the
    /// poll (no `SA_RESTART`) for an immediate wake.
    pub fn next_event(&mut self, step: Duration) -> Event {
        loop {
            // Serve a buffered complete line first (one read may deliver several).
            if let Some(line) = self.take_line() {
                return Event::Line(line);
            }
            if shutdown_requested() {
                return Event::Shutdown;
            }
            match poll_in(self.fd, step) {
                PollIn::Ready => match self.fill() {
                    FillOutcome::Eof => return Event::Eof,
                    FillOutcome::Read | FillOutcome::WouldBlock => continue,
                    FillOutcome::Err => return Event::Eof,
                },
                // Timeout or EINTR (a signal): loop to re-check the shutdown flag.
                PollIn::TimedOutOrSignal => continue,
                PollIn::Err => return Event::Eof,
            }
        }
    }

    fn take_line(&mut self) -> Option<String> {
        match self.buf.iter().position(|&b| b == b'\n') {
            Some(pos) => {
                let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
                line.pop(); // '\n'
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                Some(String::from_utf8_lossy(&line).into_owned())
            }
            None => {
                // No terminator yet: drop an oversized buffer (flood) to bound memory.
                if self.buf.len() > MAX_LINE {
                    self.buf.clear();
                }
                None
            }
        }
    }

    fn fill(&mut self) -> FillOutcome {
        let mut tmp = [0u8; 8192];
        let n = unsafe { libc::read(self.fd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len()) };
        if n == 0 {
            return FillOutcome::Eof;
        }
        if n < 0 {
            let e = io::Error::last_os_error();
            return match e.raw_os_error() {
                Some(libc::EAGAIN) | Some(libc::EINTR) => FillOutcome::WouldBlock,
                _ => FillOutcome::Err,
            };
        }
        self.buf.extend_from_slice(&tmp[..n as usize]);
        FillOutcome::Read
    }
}

enum FillOutcome {
    Read,
    WouldBlock,
    Eof,
    Err,
}

enum PollIn {
    Ready,
    TimedOutOrSignal,
    Err,
}

/// poll `fd` for `POLLIN` with a timeout. Unlike the orchestrator's `poll_fd`, `EINTR` returns
/// `TimedOutOrSignal` (no internal retry) so a signal wakes the caller to re-check the flag.
fn poll_in(fd: RawFd, timeout: Duration) -> PollIn {
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let r = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, ms) };
    if r < 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EINTR) {
            return PollIn::TimedOutOrSignal;
        }
        return PollIn::Err;
    }
    if r == 0 {
        return PollIn::TimedOutOrSignal;
    }
    // Readable, or POLLHUP/POLLERR (always reported) — a subsequent read() returns 0/err.
    PollIn::Ready
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a pipe; returns (read_fd, write_fd).
    fn pipe() -> (RawFd, RawFd) {
        let mut fds = [0 as RawFd; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        (fds[0], fds[1])
    }

    fn write_all(fd: RawFd, data: &[u8]) {
        let n = unsafe { libc::write(fd, data.as_ptr() as *const libc::c_void, data.len()) };
        assert_eq!(n as usize, data.len());
    }

    #[test]
    fn reads_multiple_lines_then_eof() {
        let (r, w) = pipe();
        write_all(w, b"line1\nline2\n");
        let mut rd = StdinReader::from_fd(r).unwrap();
        match rd.next_event(Duration::from_millis(50)) {
            Event::Line(s) => assert_eq!(s, "line1"),
            _ => panic!("expected line1"),
        }
        match rd.next_event(Duration::from_millis(50)) {
            Event::Line(s) => assert_eq!(s, "line2"),
            _ => panic!("expected line2"),
        }
        unsafe { libc::close(w) }; // EOF
        match rd.next_event(Duration::from_millis(50)) {
            Event::Eof => {}
            _ => panic!("expected EOF after the write end closed"),
        }
        unsafe { libc::close(r) };
    }

    #[test]
    fn strips_trailing_cr() {
        let (r, w) = pipe();
        write_all(w, b"hello\r\n");
        let mut rd = StdinReader::from_fd(r).unwrap();
        match rd.next_event(Duration::from_millis(50)) {
            Event::Line(s) => assert_eq!(s, "hello"),
            _ => panic!("expected CRLF-stripped line"),
        }
        unsafe {
            libc::close(w);
            libc::close(r);
        }
    }

    #[test]
    fn assembles_a_line_split_across_two_writes() {
        let (r, w) = pipe();
        write_all(w, b"par");
        write_all(w, b"tial\n");
        let mut rd = StdinReader::from_fd(r).unwrap();
        match rd.next_event(Duration::from_millis(50)) {
            Event::Line(s) => assert_eq!(s, "partial"),
            _ => panic!("expected reassembled line"),
        }
        unsafe {
            libc::close(w);
            libc::close(r);
        }
    }
}
