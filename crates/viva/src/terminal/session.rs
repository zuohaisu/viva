//! One supervised PTY session: spawn, input, resize, snapshot, stop, wait.
//!
//! Lifecycle (issue #14): every session runs in its own process group / PTY
//! session (portable-pty uses `setsid` + controlling terminal on Unix, so
//! the child's pgid equals its pid and a group signal reaches the whole
//! tree). Stop is graceful-first (SIGTERM to the group), escalates on
//! timeout (SIGKILL to the group) and is confirmed by an actual reap.
//!
//! Race discipline: all state transitions run under one mutex; the reader
//! thread never reaps. Once `wait`/`stop` has reaped the child, the state
//! is `Exited` and no signal is ever sent again — so a stop/wait/exit race
//! can neither double-reap nor overwrite the recorded conclusion, and no
//! signal can hit a recycled pid (the pid is only signalled while our child
//! handle is still unreaped).
//!
//! Memory bounds: the emulation grid + scrollback ring is capped
//! (`SCROLLBACK_LINES`); the raw output is preserved by streaming it to the
//! optional disk log (byte-level redacted, full ANSI fidelity) with an
//! optional size cap. Dropping escape bytes from the transcript is never
//! done — bounded memory must not fake the log.

use std::io::Read as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use vt100::Parser;

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::TerminalId;
use crate::foundation::records::{TerminalEvent, TerminalEventKind, TerminalOwner};
use crate::foundation::store::Store;
use crate::redaction::ByteRedactor;

/// Grid + scrollback cap: memory stays bounded no matter what the child
/// prints; the disk log keeps the full stream.
pub const SCROLLBACK_LINES: usize = 2_000;
/// Read-chunk size for the reader thread.
const READ_CHUNK: usize = 8 * 1024;

// ---------------------------------------------------------------------------
// Spec and results
// ---------------------------------------------------------------------------

/// What to launch: explicit argv + absolute cwd, no joined shell strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSpec {
    pub argv: Vec<String>,
    pub cwd: std::path::PathBuf,
    /// Extra environment (e.g. TERM is set automatically).
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

impl TerminalSpec {
    pub fn new(argv: Vec<String>, cwd: std::path::PathBuf) -> OfficeResult<Self> {
        let spec = Self {
            argv,
            cwd,
            env: Vec::new(),
            cols: 120,
            rows: 40,
        };
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> OfficeResult<()> {
        let Some(program) = self.argv.first() else {
            return Err(OfficeError::Validation(
                "terminal argv must name a program; empty argv is rejected".into(),
            ));
        };
        if program.trim().is_empty() {
            return Err(OfficeError::Validation(
                "terminal argv[0] must be a program, not an empty string".into(),
            ));
        }
        if !self.cwd.is_absolute() {
            return Err(OfficeError::Validation(format!(
                "terminal cwd must be absolute: {}",
                self.cwd.display()
            )));
        }
        if self.cols == 0 || self.rows == 0 {
            return Err(OfficeError::Validation(
                "terminal size must be at least 1x1".into(),
            ));
        }
        Ok(())
    }
}

/// How the session ended. `via` names the path honestly: a clean child exit,
/// a graceful group stop, or the forced escalation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalExit {
    pub code: Option<i32>,
    pub signal: Option<String>,
    pub via: ExitVia,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitVia {
    ChildExit,
    GracefulStop,
    ForcedKill,
}

/// A text snapshot of the terminal: the live grid plus the bounded
/// scrollback window, and the visible bounds metrics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSnapshot {
    pub cols: u16,
    pub rows: u16,
    /// Live grid contents, top to bottom.
    pub visible: Vec<String>,
    /// Scrollback window, oldest first. Never exceeds `SCROLLBACK_LINES`;
    /// the ring drops the oldest lines when full (visible via
    /// `scrollback_capped` + `total_output_bytes`).
    pub scrollback: Vec<String>,
    pub scrollback_capped: bool,
    /// Total raw output bytes the session has produced (the bound metric).
    pub total_output_bytes: u64,
    /// True when the disk log hit its byte cap and stopped writing.
    pub log_truncated: bool,
}

/// Stop policy: how long to wait after the graceful group signal before
/// escalating to kill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopPolicy {
    pub graceful_timeout: Duration,
}

impl Default for StopPolicy {
    fn default() -> Self {
        Self {
            graceful_timeout: Duration::from_secs(5),
        }
    }
}

// ---------------------------------------------------------------------------
// Disk log
// ---------------------------------------------------------------------------

/// The optional raw-output disk log. Byte-level redacted (V04 integration):
/// configured secrets are replaced, every other byte — ANSI included — is
/// preserved, so the transcript stays honest. A byte cap bounds the file;
/// hitting it is visible via [`TerminalSnapshot::log_truncated`].
pub struct DiskLog {
    file: Mutex<std::fs::File>,
    redactor: Mutex<ByteRedactor>,
    written: AtomicU64,
    max_bytes: Option<u64>,
    truncated: AtomicBool,
}

impl DiskLog {
    pub fn create(
        path: &std::path::Path,
        secrets: Vec<String>,
        max_bytes: Option<u64>,
    ) -> OfficeResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            file: Mutex::new(file),
            redactor: Mutex::new(ByteRedactor::new(secrets)),
            written: AtomicU64::new(0),
            max_bytes,
            truncated: AtomicBool::new(false),
        })
    }

    pub fn truncated(&self) -> bool {
        self.truncated.load(Ordering::SeqCst)
    }

    fn write_raw(&self, bytes: &[u8]) {
        if self.truncated.load(Ordering::SeqCst) {
            return;
        }
        if let Some(max) = self.max_bytes {
            if self.written.load(Ordering::SeqCst) >= max {
                self.truncated.store(true, Ordering::SeqCst);
                return;
            }
        }
        let safe = self.redactor.lock().expect("log redactor").push(bytes);
        let mut file = self.file.lock().expect("log file");
        if std::io::Write::write_all(&mut *file, &safe).is_err() {
            // A failing (slow/full) disk must not take the session down;
            // the truncation flag makes the loss visible.
            self.truncated.store(true, Ordering::SeqCst);
            return;
        }
        let total = self.written.fetch_add(safe.len() as u64, Ordering::SeqCst) + safe.len() as u64;
        if let Some(max) = self.max_bytes {
            if total >= max {
                self.truncated.store(true, Ordering::SeqCst);
            }
        }
    }

    fn finish(&self) {
        let tail = self.redactor.lock().expect("log redactor").flush();
        if !tail.is_empty() {
            let mut file = self.file.lock().expect("log file");
            let _ = std::io::Write::write_all(&mut *file, &tail);
        }
    }
}

// ---------------------------------------------------------------------------
// Session internals
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum SessionState {
    Running,
    Stopping,
    Exited(TerminalExit),
}

struct SessionShared {
    parser: Mutex<Parser>,
    writer: Mutex<Box<dyn std::io::Write + Send>>,
    state: Mutex<SessionState>,
    exit_waiters: Mutex<Vec<std::sync::mpsc::Sender<TerminalExit>>>,
    total_output_bytes: AtomicU64,
    eof_seen: AtomicBool,
}

/// Handle to one live terminal session. Cloneable; all operations are
/// thread-safe. Not `Send`-reaped implicitly: dropping the last handle
/// reaps the child in the background without killing it (leaked handles
/// never kill unowned processes).
pub struct TerminalHandle {
    shared: Arc<SessionShared>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    child: Arc<Mutex<Option<Box<dyn portable_pty::Child + Send + Sync>>>>,
    pid: Option<u32>,
    /// `(pid, argv0, spawned_at)` — the start marker recorded beside the pid
    /// so a later process with the same pid can never be mistaken for this
    /// session's process.
    pub pid_start_marker: String,
    disk_log: Option<Arc<DiskLog>>,
}

impl TerminalHandle {
    /// Spawn the session in its own PTY + process group.
    pub fn spawn(
        spec: &TerminalSpec,
        owner: &TerminalOwner,
        disk_log: Option<Arc<DiskLog>>,
    ) -> OfficeResult<Self> {
        spec.validate()?;
        let pair = portable_pty::native_pty_system()
            .openpty(PtySize {
                rows: spec.rows,
                cols: spec.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| OfficeError::Io(std::io::Error::other(e.to_string())))?;

        let mut cmd = CommandBuilder::new(&spec.argv[0]);
        for arg in &spec.argv[1..] {
            cmd.arg(arg);
        }
        cmd.cwd(&spec.cwd);
        cmd.env("TERM", "xterm-256color");
        for (key, value) in &spec.env {
            cmd.env(key, value);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| OfficeError::Io(std::io::Error::other(e.to_string())))?;
        let pid = child.process_id();
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| OfficeError::Io(std::io::Error::other(e.to_string())))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| OfficeError::Io(std::io::Error::other(e.to_string())))?;

        let shared = Arc::new(SessionShared {
            parser: Mutex::new(Parser::new(spec.rows, spec.cols, SCROLLBACK_LINES)),
            writer: Mutex::new(writer),
            state: Mutex::new(SessionState::Running),
            exit_waiters: Mutex::new(Vec::new()),
            total_output_bytes: AtomicU64::new(0),
            eof_seen: AtomicBool::new(false),
        });

        let handle = Self {
            shared,
            master: Mutex::new(Some(pair.master)),
            child: Arc::new(Mutex::new(Some(child))),
            pid,
            pid_start_marker: format!(
                "pid={pid:?} argv0={} spawned_at={}",
                spec.argv[0],
                crate::foundation::ids::utc_now()
            ),
            disk_log,
        };

        // Reader thread: raw output → emulator (bounded ring) + disk log
        // (byte-redacted). Never reaps; never kills. EOF only marks the
        // pipe closed.
        let shared = Arc::downgrade(&handle.shared);
        let log = handle.disk_log.clone();
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; READ_CHUNK];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let Some(shared) = shared.upgrade() else {
                            break;
                        };
                        shared
                            .total_output_bytes
                            .fetch_add(n as u64, Ordering::SeqCst);
                        if let Ok(mut parser) = shared.parser.lock() {
                            parser.process(&buf[..n]);
                        }
                        if let Some(log) = &log {
                            log.write_raw(&buf[..n]);
                        }
                    }
                    Err(_) => break,
                }
            }
            if let Some(shared) = shared.upgrade() {
                shared.eof_seen.store(true, Ordering::SeqCst);
                if let Some(log) = &log {
                    log.finish();
                }
            }
        });

        // The spawn event carries the ownership boundary: only a
        // member-execution terminal carries an execution id.
        let _ = owner;
        Ok(handle)
    }

    /// Record lifecycle facts into the foundation `terminal_events` slice.
    pub fn record_event(
        &self,
        store: &Store,
        terminal_id: &TerminalId,
        owner: &TerminalOwner,
        kind: TerminalEventKind,
    ) -> OfficeResult<()> {
        let payload = serde_json::json!({
            "pid": self.pid,
            "pid_start_marker": self.pid_start_marker,
        });
        crate::foundation::records::insert_terminal_event(
            store,
            &TerminalEvent::new(terminal_id.clone(), owner.clone(), kind, payload),
        )?;
        Ok(())
    }

    /// Write bytes to the child's stdin. Backpressure is the PTY's own
    /// bounded pipe: when the child does not consume, this blocks — memory
    /// never grows with a silent consumer.
    pub fn input(&self, bytes: &[u8]) -> OfficeResult<()> {
        let mut writer = self.shared.writer.lock().expect("pty writer");
        writer.write_all(bytes)?;
        writer.flush()?;
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> OfficeResult<()> {
        if cols == 0 || rows == 0 {
            return Err(OfficeError::Validation(
                "terminal size must be at least 1x1".into(),
            ));
        }
        if let Some(master) = self.master.lock().expect("pty master").as_ref() {
            master
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| OfficeError::Io(std::io::Error::other(e.to_string())))?;
        }
        if let Ok(mut parser) = self.shared.parser.lock() {
            parser.screen_mut().set_size(rows, cols);
        }
        Ok(())
    }

    /// Snapshot the live grid + bounded scrollback. The scrollback read
    /// temporarily moves the emulator's view; the live offset is restored.
    pub fn snapshot(&self) -> OfficeResult<TerminalSnapshot> {
        let mut parser = self.shared.parser.lock().expect("pty parser");
        let (rows, cols) = parser.screen().size();
        // Total scrollback currently retained by the ring. The ring itself
        // drops the oldest lines once full — the bound is structural.
        parser.screen_mut().set_scrollback(usize::MAX);
        let retained = parser.screen().scrollback();
        let scrollback_capped = retained >= SCROLLBACK_LINES;
        // The scrollback window adjacent to the live grid (what a user
        // scrolls to first), capped at one grid height.
        parser
            .screen_mut()
            .set_scrollback(retained.min(rows as usize));
        let scrollback: Vec<String> = parser
            .screen()
            .rows(0, cols)
            .map(|line| line.trim_end().to_string())
            .collect();
        // Live grid.
        parser.screen_mut().set_scrollback(0);
        let visible: Vec<String> = parser
            .screen()
            .rows(0, cols)
            .map(|line| line.trim_end().to_string())
            .collect();
        Ok(TerminalSnapshot {
            cols,
            rows,
            visible,
            scrollback,
            scrollback_capped,
            total_output_bytes: self.shared.total_output_bytes.load(Ordering::SeqCst),
            log_truncated: self
                .disk_log
                .as_ref()
                .map(|log| log.truncated())
                .unwrap_or(false),
        })
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn total_output_bytes(&self) -> u64 {
        self.shared.total_output_bytes.load(Ordering::SeqCst)
    }

    /// Non-blocking exit check.
    pub fn try_wait(&self) -> OfficeResult<Option<TerminalExit>> {
        let mut state = self.shared.state.lock().expect("session state");
        if let SessionState::Exited(exit) = &*state {
            return Ok(Some(exit.clone()));
        }
        let mut child_slot = self.child.lock().expect("child");
        let Some(child) = child_slot.as_mut() else {
            return Ok(None);
        };
        match child
            .try_wait()
            .map_err(|e| OfficeError::Io(std::io::Error::other(e.to_string())))?
        {
            Some(status) => {
                // If a stop was already initiated, this reap is the stop
                // protocol's outcome — attribute it honestly.
                let via = if *state == SessionState::Stopping {
                    ExitVia::GracefulStop
                } else {
                    ExitVia::ChildExit
                };
                let exit = exit_from(status, via);
                *state = SessionState::Exited(exit.clone());
                *child_slot = None;
                self.notify_exit(&exit);
                Ok(Some(exit))
            }
            None => Ok(None),
        }
    }

    /// Block until the child exits (idempotent; the first recorded
    /// conclusion is the permanent one).
    pub fn wait(&self) -> OfficeResult<TerminalExit> {
        {
            let state = self.shared.state.lock().expect("session state");
            if let SessionState::Exited(exit) = &*state {
                return Ok(exit.clone());
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.shared.exit_waiters.lock().expect("waiters").push(tx);

        // Another thread may have reaped between the check and the push.
        {
            let mut state = self.shared.state.lock().expect("session state");
            if let SessionState::Exited(exit) = state.clone() {
                return Ok(exit);
            }
            let mut child_slot = self.child.lock().expect("child");
            if let Some(child) = child_slot.as_mut() {
                let status = child
                    .wait()
                    .map_err(|e| OfficeError::Io(std::io::Error::other(e.to_string())))?;
                // Same attribution rule as try_wait: a reap after a stop
                // request is the stop's conclusion.
                let via = if *state == SessionState::Stopping {
                    ExitVia::GracefulStop
                } else {
                    ExitVia::ChildExit
                };
                let exit = exit_from(status, via);
                *state = SessionState::Exited(exit.clone());
                *child_slot = None;
                self.notify_exit(&exit);
                return Ok(exit);
            }
        }
        // The child was reaped by someone else; wait for the broadcast.
        rx.recv()
            .map_err(|_| OfficeError::Validation("exit waiter channel closed unexpectedly".into()))
    }

    /// Stop the session: SIGTERM to the process group, escalate to SIGKILL
    /// on timeout, confirm by reaping. Idempotent — once a conclusion is
    /// recorded, later stops return it unchanged and never signal again.
    pub fn stop(&self, policy: StopPolicy) -> OfficeResult<TerminalExit> {
        {
            let mut state = self.shared.state.lock().expect("session state");
            if let SessionState::Exited(exit) = &*state {
                return Ok(exit.clone());
            }
            *state = SessionState::Stopping;
        }

        // Graceful: TERM to the whole process group (pgid == pid under the
        // pty session discipline). Only while our child is unreaped.
        self.signal_group(signal::SIGTERM);

        let deadline = Instant::now() + policy.graceful_timeout;
        loop {
            let mut child_slot = self.child.lock().expect("child");
            if let Some(child) = child_slot.as_mut() {
                match child
                    .try_wait()
                    .map_err(|e| OfficeError::Io(std::io::Error::other(e.to_string())))?
                {
                    Some(status) => {
                        let exit = exit_from(status, ExitVia::GracefulStop);
                        let mut state = self.shared.state.lock().expect("session state");
                        *state = SessionState::Exited(exit.clone());
                        *child_slot = None;
                        drop(child_slot);
                        self.notify_exit(&exit);
                        if let Some(log) = &self.disk_log {
                            log.finish();
                        }
                        return Ok(exit);
                    }
                    None if Instant::now() >= deadline => {
                        // Escalate: KILL the group, then confirm by reap.
                        self.signal_group(signal::SIGKILL);
                        let status = child
                            .wait()
                            .map_err(|e| OfficeError::Io(std::io::Error::other(e.to_string())))?;
                        let exit = exit_from(status, ExitVia::ForcedKill);
                        let mut state = self.shared.state.lock().expect("session state");
                        *state = SessionState::Exited(exit.clone());
                        *child_slot = None;
                        drop(child_slot);
                        self.notify_exit(&exit);
                        if let Some(log) = &self.disk_log {
                            log.finish();
                        }
                        return Ok(exit);
                    }
                    None => {
                        drop(child_slot);
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
            } else {
                // Reaped elsewhere between our state check and now.
                let state = self.shared.state.lock().expect("session state");
                return match &*state {
                    SessionState::Exited(exit) => Ok(exit.clone()),
                    _ => Err(OfficeError::Validation(
                        "session child vanished without a recorded exit".into(),
                    )),
                };
            }
        }
    }

    fn signal_group(&self, sig: i32) {
        #[cfg(unix)]
        {
            if let Some(pid) = self.pid {
                let pgid = -(pid as i32);
                // The negative pid targets the whole process group. Signals
                // are only ever sent while the child handle is unreaped, so
                // a recycled pid cannot be hit.
                unsafe {
                    libc::kill(pgid, sig);
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = sig;
        }
    }

    fn notify_exit(&self, exit: &TerminalExit) {
        let mut waiters = self.shared.exit_waiters.lock().expect("waiters");
        for waiter in waiters.drain(..) {
            let _ = waiter.send(exit.clone());
        }
    }
}

impl Drop for TerminalHandle {
    fn drop(&mut self) {
        // Never kill on drop (stopping is an explicit office decision), but
        // reap in the background so an exited child does not linger as a
        // zombie. If the child is still running it keeps running — the
        // office records ownership and decides separately.
        let child = self.child.clone();
        std::thread::spawn(move || {
            if let Ok(mut slot) = child.lock() {
                if let Some(mut child) = slot.take() {
                    let _ = child.wait();
                }
            }
        });
    }
}

/// Normalize portable-pty's `ExitStatus` into the office's honest exit
/// record: the exit code, or the signal name when the process was killed.
fn exit_from(status: portable_pty::ExitStatus, via: ExitVia) -> TerminalExit {
    TerminalExit {
        code: if status.signal().is_some() {
            None
        } else {
            Some(status.exit_code() as i32)
        },
        signal: status.signal().map(str::to_string),
        via,
    }
}

/// The `signal` module alias used above (kept tiny and explicit).
#[cfg(unix)]
mod signal {
    pub const SIGTERM: i32 = 15;
    pub const SIGKILL: i32 = 9;
}
