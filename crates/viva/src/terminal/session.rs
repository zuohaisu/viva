//! One supervised PTY session: spawn, input, resize, snapshot, stop, wait.
//!
//! Lifecycle (issue #14): every session runs in its own process group / PTY
//! session (`setsid` + controlling terminal on Unix, so the child's pgid
//! equals its pid and a group signal reaches the whole tree). Stop is
//! graceful-first (SIGTERM to the group), escalates on timeout (SIGKILL to
//! the group) and is confirmed by an actual reap.
//!
//! Race discipline: all state transitions run under one mutex; the reader
//! thread never reaps. Once `wait`/`stop` has reaped the child, the state
//! is `Exited` and no signal is ever sent again — so a stop/wait/exit race
//! can neither double-reap nor overwrite the recorded conclusion, and no
//! signal can hit a recycled pid (the pid is only signalled while the
//! session is still unreaped).
//!
//! Memory bounds: the emulation grid + scrollback ring is capped
//! (`SCROLLBACK_LINES`); the raw output is preserved by streaming it to the
//! optional disk log (byte-level redacted, full ANSI fidelity) with an
//! optional size cap. Dropping escape bytes from the transcript is never
//! done — bounded memory must not fake the log.
//!
//! Live handoff (V14 S3, issue #45; ADR 0012 decision 2): the PTY backend
//! is owned (`terminal/pty.rs`) so the master fd is real and transferable.
//! An adopted session runs on an fd received from the previous server: the
//! child is no longer OUR child, so liveness is `kill(pid, 0)` against the
//! recorded start marker's pid and the exit STATUS is honestly unobservable
//! (code `None`) — the handoff manifest carries the old session's
//! scrollback as history so the pane keeps its past above the live grid.

use std::io::Read as _;
use std::io::Write as _;
use std::os::fd::FromRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use vt100::Parser;

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::TerminalId;
use crate::foundation::records::{TerminalEvent, TerminalEventKind, TerminalOwner};
use crate::foundation::store::Store;
use crate::redaction::ByteRedactor;
use crate::terminal::pty::{OwnedMaster, spawn_child};

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
/// a graceful group stop, or the forced escalation. For an ADOPTED session
/// the exit code is unobservable (`code: None`, `signal: None`) — the fact
/// of the exit is recorded, its status is not guessed.
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

/// Lifecycle state plus the child slot, guarded by ONE mutex. Every
/// operation under the core lock is non-blocking (a `try_wait`-style check
/// only) — no lock is ever held while waiting on an external event. That is
/// the structural property that makes stop/wait/exit races safe: a stop can
/// always acquire the lock and signal, and no ABBA lock cycle can close.
struct SessionCore {
    state: SessionState,
    /// Set once stop() escalates to SIGKILL; drives exit attribution.
    escalated: bool,
    /// Our own child (spawn path): reaped via non-blocking waitpid.
    child: Option<std::process::Child>,
    /// An adopted session's pid (handoff path): NOT our child, so liveness
    /// is `kill(pid, 0)` and the exit status is unobservable.
    adopted_pid: Option<u32>,
}

struct SessionShared {
    parser: Mutex<Parser>,
    writer: Mutex<Box<dyn std::io::Write + Send>>,
    core: Mutex<SessionCore>,
    total_output_bytes: AtomicU64,
    eof_seen: AtomicBool,
    /// History text carried across a live handoff: the old session's
    /// scrollback, shown above the live grid. Empty for spawned sessions.
    history: Vec<String>,
    /// Live handoff (issue #45): set when this server gives the session up.
    /// The reader exits on the next poll tick instead of racing the new
    /// server's reader for the same bytes.
    reader_detached: AtomicBool,
    reader_gone: AtomicBool,
}

/// Handle to one live terminal session. Cloneable; all operations are
/// thread-safe. Dropping the last handle reaps the child in the background
/// without killing it (leaked handles never kill unowned processes).
pub struct TerminalHandle {
    shared: Arc<SessionShared>,
    master: Mutex<Option<OwnedMaster>>,
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
        let (master, slave_fd) = OwnedMaster::open(spec)?;
        let child = spawn_child(spec, slave_fd)?;
        // The parent's slave reference is done: the child owns its dups.
        #[cfg(unix)]
        unsafe {
            libc::close(slave_fd);
        }
        let pid = child.id();
        let writer_fd = master.dup()?;
        let reader_fd = master.dup()?;

        let shared = Arc::new(SessionShared {
            parser: Mutex::new(Parser::new(spec.rows, spec.cols, SCROLLBACK_LINES)),
            // SAFETY: fresh dup of our own master fd, owned by the writer.
            writer: Mutex::new(Box::new(unsafe { std::fs::File::from_raw_fd(writer_fd) })),
            core: Mutex::new(SessionCore {
                state: SessionState::Running,
                escalated: false,
                child: Some(child),
                adopted_pid: None,
            }),
            total_output_bytes: AtomicU64::new(0),
            eof_seen: AtomicBool::new(false),
            history: Vec::new(),
            reader_detached: AtomicBool::new(false),
            reader_gone: AtomicBool::new(false),
        });

        let handle = Self {
            shared,
            master: Mutex::new(Some(master)),
            pid: Some(pid),
            pid_start_marker: format!(
                "pid={pid:?} argv0={} spawned_at={}",
                spec.argv[0],
                crate::foundation::ids::utc_now()
            ),
            disk_log,
        };
        spawn_reader(&handle.shared, reader_fd, handle.disk_log.clone());

        // The spawn event carries the ownership boundary: only a
        // member-execution terminal carries an execution id.
        let _ = owner;
        Ok(handle)
    }

    /// Adopt a session transferred by live handoff: an existing master fd
    /// (received via SCM_RIGHTS), the child's pid + start marker from the
    /// manifest, and the old session's scrollback as history. The child is
    /// not ours, so exits are detected by liveness and their status is
    /// honestly unobservable.
    #[allow(clippy::too_many_arguments)]
    pub fn adopt(
        master_fd: std::os::fd::RawFd,
        pid: u32,
        pid_start_marker: String,
        cols: u16,
        rows: u16,
        history: Vec<String>,
        disk_log: Option<Arc<DiskLog>>,
    ) -> OfficeResult<Self> {
        if cols == 0 || rows == 0 {
            return Err(OfficeError::Validation(
                "terminal size must be at least 1x1".into(),
            ));
        }
        // SAFETY: the received fd is a valid open master transferred to us;
        // the sender keeps its own reference until the ack.
        let master = unsafe { OwnedMaster::from_received(master_fd) };
        let writer_fd = master.dup()?;
        let reader_fd = master.dup()?;

        let shared = Arc::new(SessionShared {
            parser: Mutex::new(Parser::new(rows, cols, SCROLLBACK_LINES)),
            // SAFETY: fresh dup of the received master fd.
            writer: Mutex::new(Box::new(unsafe { std::fs::File::from_raw_fd(writer_fd) })),
            core: Mutex::new(SessionCore {
                state: SessionState::Running,
                escalated: false,
                child: None,
                adopted_pid: Some(pid),
            }),
            total_output_bytes: AtomicU64::new(0),
            eof_seen: AtomicBool::new(false),
            history,
            reader_detached: AtomicBool::new(false),
            reader_gone: AtomicBool::new(false),
        });
        let handle = Self {
            shared,
            master: Mutex::new(Some(master)),
            pid: Some(pid),
            pid_start_marker,
            disk_log,
        };
        spawn_reader(&handle.shared, reader_fd, handle.disk_log.clone());

        // Nudge a repaint: a resize down and back sends SIGWINCH to the
        // child's foreground group, so full-screen programs (the agents'
        // TUIs) redraw their screens on the fresh emulator.
        if rows > 1 {
            let _ = handle.resize(cols, rows - 1);
            let _ = handle.resize(cols, rows);
        }
        Ok(handle)
    }

    /// A duplicate of the master fd for live-handoff transfer. The
    /// original stays owned by this session until it is dropped.
    pub fn master_fd_for_transfer(&self) -> OfficeResult<Option<std::os::fd::RawFd>> {
        match self.master.lock().expect("pty master").as_ref() {
            Some(master) => Ok(Some(master.dup()?)),
            None => Ok(None),
        }
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
            master.resize(cols, rows)?;
        }
        if let Ok(mut parser) = self.shared.parser.lock() {
            parser.screen_mut().set_size(rows, cols);
        }
        Ok(())
    }

    /// Snapshot the live grid + bounded scrollback. The scrollback read
    /// temporarily moves the emulator's view; the live offset is restored.
    /// Handoff history (if any) leads the scrollback window.
    pub fn snapshot(&self) -> OfficeResult<TerminalSnapshot> {
        let mut parser = self.shared.parser.lock().expect("pty parser");
        let (rows, cols) = parser.screen().size();
        // Total scrollback currently retained by the ring. The ring itself
        // drops the oldest lines once full — the bound is structural.
        parser.screen_mut().set_scrollback(usize::MAX);
        let retained = parser.screen().scrollback();
        let parser_capped = retained >= SCROLLBACK_LINES;
        // The scrollback window adjacent to the live grid (what a user
        // scrolls to first), capped at one grid height.
        parser
            .screen_mut()
            .set_scrollback(retained.min(rows as usize));
        let parser_scrollback: Vec<String> = parser
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
        drop(parser);

        let history = &self.shared.history;
        let mut scrollback = history.clone();
        scrollback.extend(parser_scrollback);
        // The bound stays structural: trim the oldest lines.
        let overflow = scrollback.len().saturating_sub(SCROLLBACK_LINES);
        if overflow > 0 {
            scrollback.drain(..overflow);
        }
        Ok(TerminalSnapshot {
            cols,
            rows,
            visible,
            scrollback_capped: parser_capped || overflow > 0,
            scrollback,
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

    /// True once the reader thread saw the stream end: every byte the
    /// child ever wrote has been counted and parsed by then. Distinguishes
    /// "child exited" from "output drained" — the CI flake was judging
    /// the volume in the gap between the two.
    pub fn output_stream_drained(&self) -> bool {
        self.shared.eof_seen.load(Ordering::SeqCst)
    }

    /// Non-blocking exit check. All reap discipline lives here: the core
    /// lock is held only across the non-blocking check, never across a
    /// blocking wait, so a concurrent stop() can always run its
    /// signal/escalation protocol and no lock cycle can deadlock.
    pub fn try_wait(&self) -> OfficeResult<Option<TerminalExit>> {
        let mut core = self.shared.core.lock().expect("session core");
        reap_under_lock(&mut core)
    }

    /// Block until the child exits. Idempotent: the first recorded
    /// conclusion is permanent. Implemented as `try_wait` polling — the
    /// sleep happens outside every lock, so a concurrent stop() can always
    /// acquire the core lock, signal the group, and escalate on time.
    pub fn wait(&self) -> OfficeResult<TerminalExit> {
        loop {
            if let Some(exit) = self.try_wait()? {
                return Ok(exit);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Stop the session: SIGTERM to the process group, escalate to SIGKILL
    /// on timeout, confirm by reaping. Idempotent — once a conclusion is
    /// recorded, later stops return it unchanged and never signal again.
    pub fn stop(&self, policy: StopPolicy) -> OfficeResult<TerminalExit> {
        {
            let mut core = self.shared.core.lock().expect("session core");
            if let SessionState::Exited(exit) = core.state.clone() {
                return Ok(exit);
            }
            core.state = SessionState::Stopping;
        }

        // Graceful: TERM to the whole process group (pgid == pid under the
        // pty session discipline). Always timely: nothing above blocks on a
        // lock held elsewhere across a wait.
        self.signal_group(signal::SIGTERM);

        let deadline = Instant::now() + policy.graceful_timeout;
        loop {
            if let Some(exit) = self.try_wait()? {
                if let Some(log) = &self.disk_log {
                    log.finish();
                }
                return Ok(exit);
            }
            if Instant::now() >= deadline {
                // Escalate: KILL the group, then confirm.
                {
                    let mut core = self.shared.core.lock().expect("session core");
                    if let SessionState::Exited(exit) = core.state.clone() {
                        return Ok(exit);
                    }
                    core.escalated = true;
                }
                self.signal_group(signal::SIGKILL);
                // BOUNDED confirmation (issue #45 QA F8): an unreapable
                // process (zombie whose parent never waits, D-state) must
                // never block stop()/shutdown forever. Past the deadline the
                // conclusion is recorded honestly: the kill was delivered,
                // the STATUS is unobservable (code None) — idempotent like
                // every other stop.
                let kill_deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    if let Some(exit) = self.try_wait()? {
                        if let Some(log) = &self.disk_log {
                            log.finish();
                        }
                        return Ok(exit);
                    }
                    if Instant::now() >= kill_deadline {
                        if let Some(log) = &self.disk_log {
                            log.finish();
                        }
                        let mut core = self.shared.core.lock().expect("session core");
                        if let SessionState::Exited(exit) = core.state.clone() {
                            return Ok(exit);
                        }
                        let exit = TerminalExit {
                            code: None,
                            signal: None,
                            via: ExitVia::ForcedKill,
                        };
                        core.state = SessionState::Exited(exit.clone());
                        core.child = None;
                        core.adopted_pid = None;
                        return Ok(exit);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn signal_group(&self, sig: i32) {
        #[cfg(unix)]
        {
            if let Some(pid) = self.pid {
                let pgid = -(pid as i32);
                // The negative pid targets the whole process group. Signals
                // are only ever sent while the session is unreaped, so a
                // recycled pid cannot be hit.
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

    /// Restart this server's reader after a failed handoff detachment
    /// (issue #45 QA F3): the detachment must not leave the host "alive but
    /// blind". Fails when the previous reader never confirmed exit - the
    /// caller records the terminal as degraded (input works, output frozen
    /// until restart) instead of pretending.
    pub fn reattach_reader(&self) -> OfficeResult<()> {
        if !self.shared.reader_gone.load(Ordering::SeqCst) {
            return Err(OfficeError::Validation(
                "the detached reader never confirmed exit; reattaching would race it".into(),
            ));
        }
        let master_guard = self.master.lock().expect("pty master");
        let Some(master) = master_guard.as_ref() else {
            return Err(OfficeError::Validation(
                "the session has no live master fd to read".into(),
            ));
        };
        let reader_fd = master.dup()?;
        drop(master_guard);
        self.shared.reader_detached.store(false, Ordering::SeqCst);
        self.shared.reader_gone.store(false, Ordering::SeqCst);
        spawn_reader(&self.shared, reader_fd, self.disk_log.clone());
        Ok(())
    }

    /// Stop this server's reader for the session (live-handoff step): blocks
    /// until the reader thread has exited, so the transferred fd's bytes are
    /// contended by nobody. Called by the OLD server before it acks.
    pub fn detach_reader(&self) {
        self.shared.reader_detached.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.shared.reader_gone.load(Ordering::SeqCst) {
            if Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Reader thread: raw output → emulator (bounded ring) + disk log
/// (byte-redacted). Never reaps; never kills. EOF/error only marks the
/// stream closed.
fn spawn_reader(
    shared: &Arc<SessionShared>,
    reader_fd: std::os::fd::RawFd,
    log: Option<Arc<DiskLog>>,
) {
    // SAFETY: fresh dup of our own master fd, owned by this thread.
    let mut reader = unsafe { std::fs::File::from_raw_fd(reader_fd) };
    let shared = Arc::downgrade(shared);
    std::thread::spawn(move || {
        let mut buf = [0u8; READ_CHUNK];
        loop {
            // Poll with a tick instead of an unconditional blocking read:
            // a live handoff must STOP this reader before the transfer ack
            // (two readers on one master would split the child's bytes).
            #[cfg(unix)]
            {
                let mut poll_fd = libc::pollfd {
                    fd: reader_fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one pollfd for our own fd, 100ms tick.
                let ready = unsafe { libc::poll(&mut poll_fd, 1, 100) };
                if ready == 0 {
                    let Some(shared) = shared.upgrade() else {
                        break;
                    };
                    if shared.reader_detached.load(Ordering::SeqCst) {
                        break;
                    }
                    continue;
                }
                if ready < 0 {
                    break;
                }
            }
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
            let Some(shared) = shared.upgrade() else {
                break;
            };
            if shared.reader_detached.load(Ordering::SeqCst) {
                break;
            }
        }
        if let Some(shared) = shared.upgrade() {
            shared.eof_seen.store(true, Ordering::SeqCst);
            shared.reader_gone.store(true, Ordering::SeqCst);
            if let Some(log) = &log {
                log.finish();
            }
        }
    });
}

/// Zombie-aware liveness for an adopted session's pid (issue #45 QA F8).
/// `waitpid(WNOHANG)` reaps when we are the parent; ECHILD (not our child)
/// falls back to `kill(pid, 0)` — there a still-unreaped zombie of ANOTHER
/// parent reads as alive, which is bounded by stop()'s deadline instead of
/// spinning forever here.
fn adopted_pid_alive(pid: u32) -> bool {
    // SAFETY: waitpid with WNOHANG and a null status out-param.
    let reaped = unsafe { libc::waitpid(pid as libc::pid_t, std::ptr::null_mut(), libc::WNOHANG) };
    if reaped == pid as libc::pid_t {
        return false; // we just reaped it: it exited
    }
    if reaped < 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ECHILD) {
            // Not our child.
            return unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
        }
        // EINTR or unknown: conservatively alive (the caller's deadline bounds this).
        return true;
    }
    true // 0: running, not yet exited
}

/// Reap the child if it has exited. Called ONLY with the core lock held
/// and ONLY across non-blocking work. Attribution: a reap after escalation
/// is the forced kill's conclusion; after a stop request (pre-escalation)
/// it is the graceful stop's; otherwise the child's own exit.
fn reap_under_lock(core: &mut SessionCore) -> OfficeResult<Option<TerminalExit>> {
    if let SessionState::Exited(exit) = core.state.clone() {
        return Ok(Some(exit));
    }
    if let Some(pid) = core.adopted_pid {
        // Adopted (handoff) session: the exit STATUS is unobservable and
        // never guessed. Liveness (issue #45 QA F8) must be ZOMBIE-AWARE:
        // kill(pid, 0) reports an unreaped zombie as alive, which once hung
        // stop() forever. First try waitpid(WNOHANG) — when this process IS
        // the parent (the common handoff-into-a-thread case) it reaps the
        // zombie outright; only otherwise fall back to kill(pid, 0).
        let alive = adopted_pid_alive(pid);
        if alive {
            return Ok(None);
        }
        let via = if core.escalated {
            ExitVia::ForcedKill
        } else if core.state == SessionState::Stopping {
            ExitVia::GracefulStop
        } else {
            ExitVia::ChildExit
        };
        let exit = TerminalExit {
            code: None,
            signal: None,
            via,
        };
        core.state = SessionState::Exited(exit.clone());
        return Ok(Some(exit));
    }
    let Some(child) = core.child.as_mut() else {
        return Ok(None);
    };
    let Some(status) = child
        .try_wait()
        .map_err(|e| OfficeError::Io(std::io::Error::other(e.to_string())))?
    else {
        return Ok(None);
    };
    let via = if core.escalated {
        ExitVia::ForcedKill
    } else if core.state == SessionState::Stopping {
        ExitVia::GracefulStop
    } else {
        ExitVia::ChildExit
    };
    let exit = exit_from_status(&status, via);
    core.state = SessionState::Exited(exit.clone());
    core.child = None;
    Ok(Some(exit))
}

impl Drop for TerminalHandle {
    fn drop(&mut self) {
        // Never kill on drop (stopping is an explicit office decision), but
        // reap in the background so an exited child does not linger as a
        // zombie. Polling reap: no lock is held across the sleep. If the
        // child is still running it keeps running — the office records
        // ownership and decides separately.
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let outcome = {
                    let mut core = shared.core.lock().expect("session core");
                    reap_under_lock(&mut core).ok().flatten()
                };
                if outcome.is_some() || Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
    }
}

/// Normalize the OS exit status into the office's honest exit record: the
/// exit code, or the signal name when the process was killed.
fn exit_from_status(status: &std::process::ExitStatus, via: ExitVia) -> TerminalExit {
    use std::os::unix::process::ExitStatusExt as _;
    let signal_number = status.signal();
    TerminalExit {
        code: if signal_number.is_some() {
            None
        } else {
            Some(status.code().unwrap_or(-1))
        },
        signal: signal_number.map(signal_name),
        via,
    }
}

fn signal_name(number: i32) -> String {
    match number {
        1 => "SIGHUP".into(),
        2 => "SIGINT".into(),
        3 => "SIGQUIT".into(),
        9 => "SIGKILL".into(),
        15 => "SIGTERM".into(),
        other => format!("signal-{other}"),
    }
}

/// The `signal` module alias used above (kept tiny and explicit).
#[cfg(unix)]
mod signal {
    pub const SIGTERM: i32 = 15;
    pub const SIGKILL: i32 = 9;
}

#[cfg(test)]
mod zombie_adoption_tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// QA F8 regression (issue #45): an adopted session whose child is an
    /// UNREAPED ZOMBIE must still stop within a bounded time. Before the
    /// fix, kill(pid, 0) saw the zombie as alive and stop() spun forever -
    /// this hung the CI run for 3h15m.
    #[test]
    fn stop_on_a_zombie_adopted_session_returns_bounded() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A real child spawned by THIS process, killed and left unreaped:
        // exactly the zombie the old host's given-up reaper leaves behind.
        let spec = TerminalSpec::new(
            vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()],
            dir.path().to_path_buf(),
        )
        .expect("spec");
        let (master, slave_fd) = OwnedMaster::open(&spec).expect("pty");
        let child = crate::terminal::pty::spawn_child(&spec, slave_fd).expect("child");
        let pid = child.id();
        // Move the Child handle somewhere nothing reaps it (the old host's
        // reaper already gave up in the incident).
        std::thread::spawn(move || {
            let child = child;
            std::thread::sleep(Duration::from_secs(120));
            drop(child);
        });
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
        // Give the kernel a moment to turn the child into a zombie.
        std::thread::sleep(Duration::from_millis(150));

        // Adopt the zombie pid on a fresh master (the transferred fd).
        let master_fd = master.dup().expect("dup");
        let handle = TerminalHandle::adopt(
            master_fd,
            pid,
            format!("pid={pid:?} argv0=/bin/sh spawned_at=adopted"),
            80,
            24,
            vec![],
            None,
        )
        .expect("adopt");

        // stop() must return - bounded, from a thread so a regression
        // fails the test instead of hanging the suite.
        let (tx, rx) = mpsc::channel();
        let stopper = std::thread::spawn(move || {
            let outcome = handle.stop(StopPolicy {
                graceful_timeout: Duration::from_millis(200),
            });
            let _ = tx.send(outcome);
        });
        let exit = rx
            .recv_timeout(Duration::from_secs(15))
            .expect("stop() must return within 15s on a zombie adopted session")
            .expect("stop succeeds");
        stopper.join().expect("stopper joins");
        assert!(
            matches!(exit.via, ExitVia::GracefulStop | ExitVia::ForcedKill),
            "an honest conclusion, got {exit:?}"
        );
        drop(master);
        drop(dir);
    }
}
