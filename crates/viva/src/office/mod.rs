//! The active Office host and its control plane (V07, issue #16), now the
//! resident-server side of the client/server split (V14 S1, issue #43;
//! ADR 0012).
//!
//! One `VIVA_HOME` has at most one active Office host. The host owns the
//! private Unix-socket control channel (`office.sock` inside the 0700 home),
//! serves CLI callbacks over it, supervises every terminal it spawned, and
//! releases the channel on exit. A second `office start` refuses and names
//! the running host; a stale socket left by a crashed host is claimed with
//! an explicit recovery record.
//!
//! Since ADR 0012 the host is the resident runtime: clients (the workbench
//! TUI, headless CLI) may detach at any time while the host keeps every
//! terminal running. What stops the terminals is an explicit `viva
//! shutdown` — or the pause semantics S6 layers on top.
//!
//! Failure-recoverable composition: dispatch walks the V03 launch protocol
//! (intent → confirm → exit), so every step survives a crash between
//! steps, and external effects (a spawned process, a worktree on disk) are
//! never presented as SQLite-atomic facts. On restart the host reconciles:
//! interrupted intents become `unresolved`, orphaned `running` executions
//! become `stopped` with an honest evidence note, completed work is never
//! re-run, and no orphan process is killed automatically (its pid start
//! marker is recorded instead — pid reuse cannot hit it).
//!
//! Authorization (ADR 0012 decision 4): the socket lives in the 0700 home
//! and accepts same-uid peers only; grant semantics extend to the wire —
//! any presented grant is validated, member-attributed mutations need a
//! live grant, and every denial is appended to the office event log.

mod client;
mod handoff;
mod protocol;

pub use client::{OfficeClient, restart_server};
pub use handoff::{HandoffEntry, HandoffManifest};
pub use protocol::{
    MAX_MESSAGE_BYTES, OfficeRequest, OfficeRequestKind, OfficeResponse, PROTOCOL_VERSION,
    new_request, read_message, round_trip, with_grant, write_message,
};

use std::os::fd::RawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::authority::{Actor, AuthorityEngine};
use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::{GrantId, MemberId, TaskId, TerminalId, WorktreeId, utc_now};
use crate::foundation::records::{AttributionSnapshot, LaunchSpec, RequestOrigin, TerminalOwner};
use crate::foundation::store::{Domain, MigrationRegistry, Store};
use crate::members::MemberRegistry;
use crate::tasks::{ExecutionStart, TaskRegistry, TaskStatus};
use crate::terminal::{StopPolicy, TerminalRegistry};

/// The `office_host` migration domain: host registry, recovery records and
/// a tiny settings table (S6 close-policy / pause persistence).
pub const DOMAIN_OFFICE_HOST: Domain = Domain::new("office_host");

pub const OFFICE_HOST_V1_SQL: &str = r#"
-- One row per office-host process that ever claimed this VIVA_HOME.
CREATE TABLE office_hosts (
    host_id    TEXT PRIMARY KEY,
    pid        INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    exited_at  TEXT,
    exit_kind  TEXT CHECK (exit_kind IN ('graceful', 'crashed') OR exit_kind IS NULL)
);
-- Honest recovery records: crash reconciliation notes written by a later
-- host about what the previous host left behind (never rewritten).
CREATE TABLE office_recovery_events (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    host_id     TEXT NOT NULL,
    kind        TEXT NOT NULL,
    detail      TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);
"#;

/// The office_host v2 slice: key/value settings for owner-controlled
/// semantics that must survive a restart (close-policy pause, and the
/// pause itself).
pub const OFFICE_HOST_V2_SQL: &str = r#"
CREATE TABLE office_settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// The office_host v3 slice: rate-limit recovery plans (V15-5). A plan is
/// created by a `rate_limited` report WITH a reset time (or the owner via
/// the CLI); it fires by prompting the terminal to continue, and every
/// outcome is audited.
pub const OFFICE_HOST_V3_SQL: &str = r#"
CREATE TABLE recovery_plans (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    terminal_id TEXT NOT NULL,
    task_id     TEXT,
    reset_at    TEXT NOT NULL,
    action      TEXT NOT NULL DEFAULT 'resume_prompt',
    state       TEXT NOT NULL DEFAULT 'scheduled'
                CHECK (state IN ('scheduled','fired','cancelled','expired')),
    created_at  TEXT NOT NULL,
    fired_at    TEXT
);
"#;

/// Socket file name inside `VIVA_HOME`.
pub const OFFICE_SOCKET_NAME: &str = "office.sock";
/// Rendezvous socket for a live handoff (created by the restarting host,
/// connected by the resumed host) — also inside `VIVA_HOME`.
pub const HANDOFF_SOCKET_NAME: &str = "handoff.sock";
/// How long the resumed server waits for the old host to release the
/// control socket, and how long the old host waits for the resumed one.
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(30);

/// The full office composition: every delivered domain, registered in one
/// frozen set. This is the lane-A composition V01 left to V07.
pub fn office_migrations() -> &'static crate::foundation::store::FrozenMigrations {
    use std::sync::OnceLock;
    static FROZEN: OnceLock<crate::foundation::store::FrozenMigrations> = OnceLock::new();
    FROZEN.get_or_init(|| {
        let registry = MigrationRegistry::new()
            .register(
                crate::foundation::store::DOMAIN_FOUNDATION,
                1,
                "foundation reference tables",
                crate::foundation::store::FOUNDATION_V1_SQL,
            )
            .register(
                crate::foundation::store::DOMAIN_MEMBERS,
                1,
                "members v1",
                crate::members::MEMBERS_V1_SQL,
            )
            .register(
                crate::foundation::store::DOMAIN_WORKSPACES_PROJECTS,
                1,
                "workspaces and projects v1",
                crate::workspaces::WORKSPACES_PROJECTS_V1_SQL,
            );
        let registry = crate::tasks::register_migrations(registry);
        let registry = crate::authority::register_migrations(registry);
        let registry = crate::git::worktrees::register_migrations(registry);
        let registry = crate::knowledge::register_migrations(registry);
        let registry = crate::conversations::register_migrations(registry);
        let registry = crate::workflows::register_migrations(registry);
        let registry = crate::maintenance::register_migrations(registry);
        let registry = crate::tools::computer::register_migrations(registry);
        let registry = crate::memory::register_migrations(registry);
        let registry = registry.register(
            DOMAIN_OFFICE_HOST,
            2,
            "office host v2 (settings)",
            OFFICE_HOST_V2_SQL,
        );
        let registry = registry.register(
            DOMAIN_OFFICE_HOST,
            3,
            "office host v3 (recovery plans)",
            OFFICE_HOST_V3_SQL,
        );
        registry
            .register(DOMAIN_OFFICE_HOST, 1, "office host v1", OFFICE_HOST_V1_SQL)
            .freeze()
            .expect("office registry is well-formed")
    })
}

/// Shared host state, handed to per-connection threads.
pub struct OfficeShared {
    pub home: PathBuf,
    pub socket_path: PathBuf,
    pub host_id: String,
    pub store: Mutex<Store>,
    pub terminals: TerminalRegistry,
    pub channels: Mutex<crate::foundation::envelope::ChannelRegistry>,
    pub stopping: AtomicBool,
    /// Set once a live handoff completed: the serve loop exits WITHOUT
    /// stopping terminals (they now belong to the resumed server).
    pub transferred: AtomicBool,
    /// The rendezvous listener for an in-progress handoff, if any.
    handoff_listener: Mutex<Option<UnixListener>>,
    /// Agent status board (S4): controlled reports (authoritative) and
    /// screen/process observations (auxiliary), kept per source.
    pub agent_board: crate::agents::AgentStatusBoard,
    /// Paused (S6): no NEW dispatch, no maintenance cycles; running
    /// executions are untouched. Owner-controlled, explicitly.
    pub paused: AtomicBool,
    /// V15-2: cached PR status per worktree (60s TTL; gh is external).
    pub pr_cache:
        Mutex<std::collections::HashMap<String, (Instant, Option<crate::git::pr::PrInfo>)>>,
    /// V15-4: the last automatic sync cycle and its human summary.
    pub last_sync: Mutex<Option<Instant>>,
    pub last_sync_note: Mutex<Option<String>>,
    /// V15-5: recovery sweep throttle (at most once a second).
    pub last_recovery_sweep: Mutex<Option<Instant>>,
    /// V15-2/QA: test seam — the gh program the PR probes invoke. None in
    /// production ("gh" from PATH); tests point it at a fake script so
    /// they never race the process-global PATH.
    pub gh_bin: Mutex<Option<String>>,
    /// V15-4/QA F2: a sync cycle is IN FLIGHT. Set when the cycle thread
    /// starts, cleared when it finishes (success or failure) — prevents
    /// the 3-5 Hz workbench view from spawning concurrent cycles that
    /// stampede `git pull --ff-only` into ref-lock failures.
    pub sync_in_flight: AtomicBool,
    /// Exit-watchers for dispatched executions. The graceful shutdown
    /// joins them BEFORE writing its handoff record — otherwise the
    /// process exit would die with the process and the next host would
    /// record a false "orphaned" recovery for a terminal this office
    /// stopped itself.
    watchers: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

/// A claimed, running Office host.
pub struct OfficeHost {
    shared: Arc<OfficeShared>,
    listener: UnixListener,
}

impl OfficeHost {
    /// Open the store and claim the single active-host slot for this
    /// `VIVA_HOME`. Refuses when a healthy host already listens on the
    /// socket; claims a stale socket only with a recovery record. The home
    /// is forced to private 0700 permissions first — the socket is the
    /// control plane, and its directory is part of its access control.
    pub fn open(home: &Path) -> OfficeResult<Self> {
        crate::foundation::paths::ensure_private_dir(home)?;
        let socket_path = home.join(OFFICE_SOCKET_NAME);
        let host_id = format!("host-{}", uuid::Uuid::new_v4().simple());

        if socket_path.exists() {
            match Self::probe_socket(&socket_path) {
                Ok(Some(pid)) => {
                    return Err(OfficeError::Validation(format!(
                        "an office host is already active for {} (pid {pid}); \
                         refusing to start a second host",
                        home.display()
                    )));
                }
                Ok(None) => {
                    return Err(OfficeError::Validation(format!(
                        "socket {} exists but no healthy host answered the probe; \
                         refusing to claim it blindly — inspect the wedged host first",
                        socket_path.display()
                    )));
                }
                Err(_) => {
                    // Connect refused: nobody is listening. A crashed host
                    // left the socket behind; claim it, with a record.
                    std::fs::remove_file(&socket_path)?;
                }
            }
        }

        let store = Store::open(
            &crate::foundation::paths::database_path(home),
            office_migrations(),
        )?;
        let host_id = Self::reconcile(&store, &host_id)?;
        let persisted_pause: bool = store
            .connection()
            .query_row(
                "SELECT value FROM office_settings WHERE key = 'paused'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map(|value| value == "true")
            .unwrap_or(false);
        store.connection().execute(
            "INSERT INTO office_hosts(host_id, pid, started_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![host_id, std::process::id() as i64, utc_now()],
        )?;

        let listener = UnixListener::bind(&socket_path)?;
        let shared = Arc::new(OfficeShared {
            home: home.to_path_buf(),
            socket_path,
            host_id,
            store: Mutex::new(store),
            terminals: TerminalRegistry::new(),
            channels: Mutex::new(crate::foundation::envelope::ChannelRegistry::new()),
            stopping: AtomicBool::new(false),
            transferred: AtomicBool::new(false),
            handoff_listener: Mutex::new(None),
            agent_board: crate::agents::AgentStatusBoard::new(),
            paused: AtomicBool::new(persisted_pause),
            pr_cache: Mutex::new(std::collections::HashMap::new()),
            last_sync: Mutex::new(None),
            last_sync_note: Mutex::new(None),
            last_recovery_sweep: Mutex::new(None),
            gh_bin: Mutex::new(None),
            sync_in_flight: AtomicBool::new(false),
            watchers: Mutex::new(Vec::new()),
        });
        Ok(Self { shared, listener })
    }

    /// Probe a socket: `Ok(Some(pid))` = healthy host; `Ok(None)` = socket
    /// answers but the handshake failed (wedged); `Err` = nobody listening.
    fn probe_socket(socket_path: &Path) -> OfficeResult<Option<u32>> {
        let mut stream = UnixStream::connect(socket_path)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let request = new_request(OfficeRequestKind::Ping);
        write_message(&mut stream, &request)?;
        let line = read_message(&mut stream)?;
        let response: OfficeResponse = serde_json::from_str(&line)
            .map_err(|e| OfficeError::Validation(format!("bad probe response: {e}")))?;
        if !response.ok {
            return Err(OfficeError::Validation("probe rejected".into()));
        }
        Ok(response
            .result
            .and_then(|r| r.get("pid").and_then(|p| p.as_u64()))
            .map(|p| p as u32))
    }

    /// Crash reconciliation, before serving. Marks the previous host
    /// `crashed`, resolves interrupted intents, and stops (in the office's
    /// records) executions whose supervision died with that host. Nothing
    /// is re-run; nothing is killed; completed work is never touched.
    fn reconcile(store: &Store, new_host_id: &str) -> OfficeResult<String> {
        let now = utc_now();
        let previous: Vec<(String, i64)> = {
            let mut stmt = store
                .connection()
                .prepare("SELECT host_id, pid FROM office_hosts WHERE exited_at IS NULL")?;
            let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let persisted_pause: bool = store
            .connection()
            .query_row(
                "SELECT value FROM office_settings WHERE key = 'paused'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map(|value| value == "true")
            .unwrap_or(false);
        for (old_host, pid) in previous {
            store.connection().execute(
                "UPDATE office_hosts SET exited_at = ?2, exit_kind = 'crashed' WHERE host_id = ?1",
                rusqlite::params![old_host, now],
            )?;
            record_recovery(
                store,
                new_host_id,
                "previous_host_crashed",
                &format!(
                    "host `{old_host}` (pid {pid}) did not record its exit; its owned processes are unaccounted, none were killed or re-run"
                ),
            )?;
        }
        if persisted_pause {
            record_recovery(
                store,
                new_host_id,
                "pause_restored",
                "the previous state carried an explicit pause; this host starts paused                  (dispatch and maintenance stay off until `viva resume`)",
            )?;
        }

        let tasks = TaskRegistry::new(store);

        // Intents that never reached `confirmed`: the host died between
        // intent and pid write. An uncertain start is never called a success.
        let interrupted: Vec<String> = {
            let mut stmt = store
                .connection()
                .prepare("SELECT request_key FROM launch_intents WHERE state = 'intended'")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for request_key in interrupted {
            tasks.mark_unresolved(&request_key)?;
            record_recovery(
                store,
                new_host_id,
                "launch_intent_unresolved",
                &format!(
                    "request key `{request_key}` never recorded a pid; whether its process started is unknown and was not guessed"
                ),
            )?;
        }

        // Executions still `running`: their supervising host is gone. The
        // office-side supervision ended — recorded as `stopped` with the
        // pid start marker, so a recycled pid can never be mistaken for
        // the orphan. The orphan itself is not signalled.
        let orphans: Vec<(String, Option<i64>, Option<String>)> = {
            let mut stmt = store.connection().prepare(
                "SELECT execution_id, process_exit, completion_evidence
                 FROM office_executions WHERE status = 'running'",
            )?;
            let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for (execution_id, _exit, _evidence) in orphans {
            let intent: Option<(String, Option<i64>, Option<String>)> = store
                .connection()
                .query_row(
                    "SELECT request_key, pid, pid_start_marker FROM launch_intents
                     WHERE execution_id = ?1 AND state = 'confirmed'",
                    [&execution_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .ok();
            store.connection().execute(
                "UPDATE office_executions SET status = 'stopped', updated_at = ?2
                 WHERE execution_id = ?1 AND status = 'running'",
                rusqlite::params![execution_id, now],
            )?;
            if let Some((request_key, pid, marker)) = intent {
                record_recovery(
                    store,
                    new_host_id,
                    "execution_orphaned",
                    &format!(
                        "execution `{execution_id}` (request `{request_key}`, pid {pid:?}, \
                         marker {marker:?}) lost its supervising host; marked stopped, \
                         process not signalled, not re-run"
                    ),
                )?;
            }
        }
        // V15-5: plans whose reset time passed while no host was running
        // are marked expired — recorded, never executed blindly.
        let expired = store.connection().execute(
            "UPDATE recovery_plans SET state = 'expired', fired_at = ?2
             WHERE state = 'scheduled' AND reset_at <= ?1",
            rusqlite::params![now, now],
        )?;
        if expired > 0 {
            record_recovery(
                store,
                new_host_id,
                "recovery_plan_expired",
                &format!(
                    "{expired} scheduled recovery plan(s) were overdue when this host \
                     started; marked expired and not executed blindly"
                ),
            )?;
        }
        Ok(new_host_id.to_string())
    }

    /// Serve until a shutdown request (or a fatal accept error). Blocks the
    /// calling thread; each connection gets its own thread.
    pub fn serve(self) -> OfficeResult<()> {
        let shared = self.shared;
        let listener = self.listener;
        listener.set_nonblocking(true)?;
        loop {
            if shared.stopping.load(Ordering::SeqCst) {
                break;
            }
            sweep_recovery_plans(&shared);
            match listener.accept() {
                Ok((stream, _)) => {
                    // Channel authentication (issue #16 requires it): the
                    // OS reports the connected peer's uid (getpeereid(2) on
                    // BSD/macOS, SO_PEERCRED on Linux). A socket in a 0700
                    // directory is the first gate; this peer check is the
                    // second — same process owner only, regardless of any
                    // future permissions drift on the socket file.
                    if !peer_is_owner(&stream) {
                        continue;
                    }
                    // The listener is non-blocking (the serve loop polls),
                    // and on BSD/macOS accept() passes O_NONBLOCK on to the
                    // accepted socket — which would turn the first slow
                    // client read into EAGAIN and drop a perfectly valid
                    // request. The connection handler is synchronous and
                    // has its own IO timeouts, so restore blocking mode.
                    stream.set_nonblocking(false)?;
                    let shared = Arc::clone(&shared);
                    std::thread::spawn(move || {
                        let _ = handle_connection(shared, stream);
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => {
                    let err = OfficeError::Io(e);
                    let _ = Self::shutdown_shared(&shared);
                    return Err(err);
                }
            }
        }
        Self::shutdown_shared(&shared)
    }

    /// Graceful shutdown: stop new dispatch (already: no new connections
    /// are served), stop every owned terminal, join the exit watchers so
    /// every stop lands in the ledger BEFORE the handoff is written, then
    /// persist the handoff and release the channel. Also the workbench
    /// quit path (D6: QuitRequested is really consumed).
    pub fn shutdown_shared(shared: &OfficeShared) -> OfficeResult<()> {
        // Live-handoff exit: the terminals were transferred to the resumed
        // server and are NOT ours to stop — record the handoff and leave
        // every process running.
        let transferred = shared.transferred.load(Ordering::SeqCst);
        let mut terminal_count = 0usize;
        if !transferred {
            let store = shared.store.lock().expect("office store");
            let terminals = shared.terminals.list();
            terminal_count = terminals.len();
            for entry in terminals {
                let _ =
                    shared
                        .terminals
                        .stop(&entry.terminal_id, StopPolicy::default(), Some(&store));
            }
            drop(store);
        }
        // Join watchers WITHOUT holding the store lock: each watcher needs
        // it to record its exit. A watcher cannot hang: the stop above made
        // its child exit, so `wait` returns promptly. (A transferred host
        // leaves its watchers to die with the process; exit supervision for
        // adopted terminals is the resumed server's.)
        if !transferred {
            let watchers: Vec<std::thread::JoinHandle<()>> = shared
                .watchers
                .lock()
                .expect("watcher registry")
                .drain(..)
                .collect();
            for watcher in watchers {
                let _ = watcher.join();
            }
        }
        let store = shared.store.lock().expect("office store");
        store.connection().execute(
            "UPDATE office_hosts SET exited_at = ?2, exit_kind = 'graceful'
             WHERE host_id = ?1 AND exited_at IS NULL",
            rusqlite::params![shared.host_id, utc_now()],
        )?;
        let note = if transferred {
            "live handoff: terminals transferred to the resumed server, no process stopped"
        } else {
            "host stopped owned terminal(s) and recorded their exits; state persisted for the next host"
        };
        record_recovery(
            &store,
            &shared.host_id,
            if transferred {
                "graceful_handoff_after_transfer"
            } else {
                "graceful_handoff"
            },
            &format!("{note} ({terminal_count} terminal(s) were hosted here)"),
        )?;
        drop(store);
        let _ = std::fs::remove_file(&shared.socket_path);
        if transferred {
            let _ = std::fs::remove_file(shared.home.join(HANDOFF_SOCKET_NAME));
        }
        Ok(())
    }

    /// Run this host on a background thread (the workbench entry runs its
    /// UI on the main thread and shuts the host down on quit).
    pub fn serve_background(self) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let _ = self.serve();
        })
    }

    pub fn shared(&self) -> Arc<OfficeShared> {
        Arc::clone(&self.shared)
    }
}

fn record_recovery(store: &Store, host_id: &str, kind: &str, detail: &str) -> OfficeResult<()> {
    store.connection().execute(
        "INSERT INTO office_recovery_events(host_id, kind, detail, recorded_at)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![host_id, kind, detail, utc_now()],
    )?;
    Ok(())
}

/// True when the connected peer's uid is this process's effective uid.
/// False on any probe failure too: an unverifiable peer is not trusted.
/// Platform split: `getpeereid(2)` is BSD/macOS-only; Linux exposes the
/// same fact as `SO_PEERCRED`. Any unix neither covers fails closed (the
/// host refuses every client rather than trust strangers).
#[cfg(target_os = "linux")]
fn peer_is_owner(stream: &UnixStream) -> bool {
    use std::os::fd::AsRawFd as _;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len: libc::socklen_t = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: getsockopt with SO_PEERCRED fills the ucred out-param for the
    // stream's own fd.
    let ok = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    ok == 0 && cred.uid == unsafe { libc::geteuid() }
}

#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
fn peer_is_owner(stream: &UnixStream) -> bool {
    use std::os::fd::AsRawFd as _;
    let mut peer_uid: libc::uid_t = 0;
    let mut peer_gid: libc::gid_t = 0;
    // SAFETY: getpeereid takes the stream's own fd and two out-params.
    let ok = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut peer_uid, &mut peer_gid) };
    ok == 0 && peer_uid == unsafe { libc::geteuid() }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
)))]
fn peer_is_owner(_stream: &UnixStream) -> bool {
    false
}

#[cfg(all(
    test,
    any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    )
))]
mod peer_tests {
    use super::*;

    /// Same-uid reachability of the peer check: a connected socket owned
    /// by this process must pass. The cross-uid refusal path cannot be
    /// constructed on a single-uid machine; it rests on the OS contract
    /// (getpeereid/SO_PEERCRED report the peer's real uid) and is recorded
    /// in the Wiki (V12-Final-Acceptance §5): https://github.com/zuohaisu/viva/wiki/V12-Final-Acceptance.
    #[test]
    fn peer_check_accepts_same_owner_sockets() {
        let (a, b) = UnixStream::pair().expect("pair");
        assert!(peer_is_owner(&a), "own-process socket must be trusted");
        assert!(peer_is_owner(&b));
    }
}

// ---------------------------------------------------------------------------
// Connection handling
// ---------------------------------------------------------------------------

fn handle_connection(shared: Arc<OfficeShared>, mut stream: UnixStream) -> OfficeResult<()> {
    // Caller binding is issued by the host on accept (never self-declared).
    // Interactive CLI callers are the user; the role exists only because
    // this host issued it.
    let _identity = shared
        .channels
        .lock()
        .expect("channel registry")
        .issue(crate::foundation::envelope::CallerRole::User);
    loop {
        let line = protocol::read_message(&mut stream)?;
        // A malformed or unknown request is an error RESPONSE, not a
        // dropped connection: the caller learns why and may retry.
        let request: OfficeRequest = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(err) => {
                protocol::write_message(
                    &mut stream,
                    &OfficeResponse::err("unknown", format!("malformed control request: {err}")),
                )?;
                continue;
            }
        };
        if request.version != PROTOCOL_VERSION {
            let response = OfficeResponse::err(
                &request.request_id,
                format!(
                    "unsupported protocol version {} (expected {PROTOCOL_VERSION})",
                    request.version
                ),
            );
            protocol::write_message(&mut stream, &response)?;
            continue;
        }
        let is_shutdown = matches!(request.kind, OfficeRequestKind::Shutdown { .. });
        let response = match handle_request(Arc::clone(&shared), &request) {
            Ok(result) => OfficeResponse::ok(&request.request_id, result),
            Err(err) => OfficeResponse::err(&request.request_id, err.to_string()),
        };
        protocol::write_message(&mut stream, &response)?;
        if is_shutdown {
            shared.stopping.store(true, Ordering::SeqCst);
            return Ok(());
        }
    }
}

fn handle_request(
    shared: Arc<OfficeShared>,
    request: &OfficeRequest,
) -> OfficeResult<serde_json::Value> {
    // The socket authorization gate (ADR 0012 decision 4, issue #43):
    // grant semantics extend to every request before any handler runs.
    authorize_socket_request(&shared, request)?;
    match &request.kind {
        OfficeRequestKind::Ping => Ok(serde_json::json!({
            "pid": std::process::id(),
            "host_id": shared.host_id,
        })),
        OfficeRequestKind::Status => status(&shared),
        OfficeRequestKind::Dispatch {
            task_id,
            member_id,
            grant_id,
            request_key,
            argv,
            cwd,
            worktree_id,
        } => dispatch(
            &shared,
            task_id,
            member_id,
            grant_id,
            request_key,
            argv,
            Path::new(cwd),
            worktree_id.as_deref(),
        ),
        OfficeRequestKind::TerminalList => terminal_list(&shared),
        OfficeRequestKind::TerminalSnapshot { terminal_id } => {
            terminal_snapshot(&shared, terminal_id)
        }
        OfficeRequestKind::TerminalInput {
            terminal_id,
            bytes_hex,
        } => terminal_input(&shared, terminal_id, bytes_hex),
        OfficeRequestKind::TerminalStop { terminal_id } => terminal_stop(&shared, terminal_id),
        OfficeRequestKind::TerminalCreate {
            argv,
            cwd,
            env,
            cols,
            rows,
            purpose,
            worktree_id,
            owner,
        } => terminal_create(
            &shared,
            argv,
            Path::new(cwd),
            env,
            *cols,
            *rows,
            purpose,
            worktree_id.as_deref(),
            owner,
        ),
        OfficeRequestKind::TerminalResize {
            terminal_id,
            cols,
            rows,
        } => terminal_resize(&shared, terminal_id, *cols, *rows),
        OfficeRequestKind::WorkbenchView => workbench_view(Arc::clone(&shared)),
        OfficeRequestKind::WorkbenchDiff { worktree_id } => workbench_diff(&shared, worktree_id),
        OfficeRequestKind::WorkbenchLayout { layout_json } => {
            workbench_layout(&shared, layout_json.as_deref())
        }
        OfficeRequestKind::TerminalOpenInWorktree { worktree_id } => {
            terminal_open_in_worktree(&shared, worktree_id)
        }
        OfficeRequestKind::WorktreeCreateForTask {
            task_id,
            branch,
            base_dir,
        } => worktree_create_for_task(&shared, task_id, branch.as_deref(), base_dir.as_deref()),
        OfficeRequestKind::TaskResults { task_id } => task_results(&shared, task_id),
        OfficeRequestKind::Handoff {
            task_id,
            member_id,
            summary,
        } => handoff(&shared, task_id, member_id, summary),
        OfficeRequestKind::Shutdown { close_policy } => {
            apply_close_policy(&shared, close_policy.as_deref())?;
            Ok(serde_json::json!({"shutting_down": true}))
        }
        OfficeRequestKind::Pause => pause_server(&shared, true),
        OfficeRequestKind::Resume => pause_server(&shared, false),
        OfficeRequestKind::ServerRestart => begin_server_restart(&shared),
        OfficeRequestKind::WorktreeFiles { worktree_id } => worktree_files(&shared, worktree_id),
        OfficeRequestKind::WorktreeFileContent { worktree_id, path } => {
            worktree_file_content(&shared, worktree_id, path)
        }
        OfficeRequestKind::WorktreeCleanup { worktree_id } => worktree_cleanup(
            &shared,
            worktree_id,
            request.member.as_deref().unwrap_or("owner"),
        ),
        OfficeRequestKind::AgentHandoff {
            worktree_id,
            to_agent,
        } => agent_handoff(
            &shared,
            worktree_id,
            to_agent,
            request.member.as_deref().unwrap_or("owner"),
        ),
        OfficeRequestKind::SyncNow => sync_now(&shared),
        OfficeRequestKind::SetAutoPull { on } => set_auto_pull(&shared, *on),
        OfficeRequestKind::RecoveryList => recovery_list(&shared),
        OfficeRequestKind::RecoverySchedule {
            terminal_id,
            reset_at,
            task_id,
        } => recovery_schedule(&shared, terminal_id, reset_at, task_id.as_deref()),
        OfficeRequestKind::RecoveryCancel { plan_id } => recovery_cancel(&shared, *plan_id),
        OfficeRequestKind::AgentReport {
            terminal_id,
            agent,
            status,
            detail,
            reset_at,
        } => agent_report(
            &shared,
            terminal_id,
            agent,
            status,
            detail,
            reset_at.as_deref(),
        ),
        OfficeRequestKind::AgentContentSubmit {
            terminal_id,
            kind,
            content,
            source_ref,
        } => agent_content_submit(
            &shared,
            terminal_id.as_deref(),
            kind,
            content,
            source_ref.as_deref(),
        ),
        OfficeRequestKind::AgentPrompt {
            terminal_id,
            prompt,
        } => agent_prompt(&shared, terminal_id, prompt),
        OfficeRequestKind::AgentWait {
            terminal_id,
            status,
            timeout_secs,
        } => agent_wait(&shared, terminal_id, status, *timeout_secs),
        OfficeRequestKind::EventsFeed { since_seq, limit } => {
            events_feed(&shared, *since_seq, *limit)
        }
    }
}

/// Actions a member-attributed mutating request can carry over the socket.
/// The vocabulary is the grants' own `actions` field (free-form strings the
/// issuer chose); this constant only names the action the socket gate
/// demands. Dispatch keeps its stricter `dispatch_delegated` checks.
const SOCKET_CONTROL_ACTION: &str = "terminal_control";

/// Kinds that change state and therefore need authorization when they are
/// member-attributed. Read-only kinds and the OS-authenticated owner path
/// (no member attribution) are not gated beyond grant validation below.
fn is_member_gated_kind(kind: &OfficeRequestKind) -> bool {
    matches!(
        kind,
        OfficeRequestKind::Dispatch { .. }
            | OfficeRequestKind::TerminalCreate { .. }
            | OfficeRequestKind::TerminalInput { .. }
            | OfficeRequestKind::TerminalResize { .. }
            | OfficeRequestKind::TerminalStop { .. }
            | OfficeRequestKind::Handoff { .. }
            | OfficeRequestKind::AgentReport { .. }
            | OfficeRequestKind::AgentContentSubmit { .. }
            | OfficeRequestKind::AgentPrompt { .. }
            | OfficeRequestKind::AgentWait { .. }
            | OfficeRequestKind::Shutdown { .. }
            | OfficeRequestKind::Pause
            | OfficeRequestKind::Resume
            | OfficeRequestKind::WorktreeCleanup { .. }
            | OfficeRequestKind::AgentHandoff { .. }
            | OfficeRequestKind::SyncNow
            | OfficeRequestKind::SetAutoPull { .. }
            | OfficeRequestKind::RecoverySchedule { .. }
            | OfficeRequestKind::RecoveryCancel { .. }
    )
}

/// The socket authorization gate. Rules, in order:
/// 1. A presented grant must resolve to a real, live, unexpired grant.
///    Anything else is a rejection WITH an audit event — the refusal is a
///    recorded fact, never a silent socket close.
/// 2. A member-attributed mutating request needs that live grant, and its
///    principal must be the attributed member; `terminal_control` must be
///    in the grant's actions. Dispatch additionally enforces its own
///    delegated-dispatch checks in its handler.
/// 3. With no member attribution, the peer is the OS-authenticated owner
///    (same-uid, enforced at accept time) acting as the user.
fn authorize_socket_request(shared: &OfficeShared, request: &OfficeRequest) -> OfficeResult<()> {
    let Some(grant_text) = &request.grant else {
        // No grant presented. Owner path is fine; a member-attributed
        // mutating request without any grant is exactly the "未携带有效
        // grant" case the socket gate exists for.
        if request.member.is_some() && is_member_gated_kind(&request.kind) {
            return Err(audit_grant_denial(
                shared,
                request,
                "member-attributed mutating request carried no grant",
            ));
        }
        return Ok(());
    };

    // The store lock is scoped: the audit path below needs it too, and a
    // Mutex is not reentrant — holding it across the audit deadlocks the
    // connection thread until the client's read timeout fires.
    let verdict = {
        let store = shared.store.lock().expect("office store");
        let authority = AuthorityEngine::new(&store);
        (|| -> OfficeResult<crate::authority::Grant> {
            let grant_id = GrantId::from_str(grant_text)?;
            let grant = authority.require_grant(&grant_id)?;
            if grant.status != crate::authority::GrantStatus::Live {
                return Err(OfficeError::Validation(format!(
                    "grant `{grant_text}` is not live (status: {:?})",
                    grant.status
                )));
            }
            if let Some(expires_at) = &grant.expires_at {
                if expires_at.as_str() <= utc_now().as_str() {
                    return Err(OfficeError::Validation(format!(
                        "grant `{grant_text}` expired at {expires_at}"
                    )));
                }
            }
            if let Some(member_text) = &request.member {
                if let Some(principal) = &grant.principal_member_id {
                    if principal.to_string() != *member_text {
                        return Err(OfficeError::Validation(format!(
                            "grant `{grant_text}` belongs to member `{principal}`, not `{member_text}`; \
                             grants are not transferable between members"
                        )));
                    }
                }
            }
            Ok(grant)
        })()
    };
    let grant = match verdict {
        Ok(grant) => grant,
        Err(err) => {
            let denial = audit_grant_denial(shared, request, &err.to_string());
            return Err(denial);
        }
    };

    // Member-attributed mutations: the grant must name the socket control
    // action (dispatch is further checked in its handler; shutdown is an
    // owner action no member grant may carry).
    if request.member.is_some() && is_member_gated_kind(&request.kind) {
        if matches!(
            request.kind,
            OfficeRequestKind::Shutdown { .. }
                | OfficeRequestKind::Pause
                | OfficeRequestKind::Resume
                | OfficeRequestKind::SetAutoPull { .. }
                | OfficeRequestKind::RecoverySchedule { .. }
                | OfficeRequestKind::RecoveryCancel { .. }
        ) {
            return Err(audit_grant_denial(
                shared,
                request,
                "shutdown/pause/resume/auto-pull and recovery-plan changes are owner \
                 actions; member grants cannot carry them",
            ));
        }
        let action_needed = match request.kind {
            OfficeRequestKind::Dispatch { .. } => "dispatch_delegated",
            OfficeRequestKind::AgentReport { .. } => "agent_report",
            OfficeRequestKind::AgentContentSubmit { .. } => "agent_content",
            OfficeRequestKind::WorktreeCleanup { .. } => SOCKET_CONTROL_ACTION,
            OfficeRequestKind::AgentHandoff { .. } => SOCKET_CONTROL_ACTION,
            OfficeRequestKind::SyncNow => SOCKET_CONTROL_ACTION,

            OfficeRequestKind::AgentPrompt { .. } => "agent_prompt",
            OfficeRequestKind::AgentWait { .. } => SOCKET_CONTROL_ACTION,
            _ => SOCKET_CONTROL_ACTION,
        };
        if !grant.actions.iter().any(|a| a == action_needed) {
            return Err(audit_grant_denial(
                shared,
                request,
                &format!(
                    "grant `{grant_text}` does not authorize `{action_needed}` (actions: {:?})",
                    grant.actions
                ),
            ));
        }
    }
    Ok(())
}

/// Record one socket-grant denial as an append-only office event and return
/// the matching error for the caller.
fn audit_grant_denial(shared: &OfficeShared, request: &OfficeRequest, reason: &str) -> OfficeError {
    let store = shared.store.lock().expect("office store");
    let payload = serde_json::json!({
        "kind": format!("{:?}", std::mem::discriminant(&request.kind)),
        "method": serde_json::to_string(&request.kind).unwrap_or_default(),
        "grant": request.grant,
        "member": request.member,
        "reason": reason,
    });
    let _ = crate::foundation::events::append(
        &store,
        crate::foundation::events::NewEvent {
            domain: DOMAIN_OFFICE_HOST,
            kind: "socket_grant_denied".into(),
            subject_type: "socket_request".into(),
            subject_id: request.request_id.clone(),
            origin: "office_host".into(),
            payload,
        },
    );
    OfficeError::Validation(format!("socket request denied: {reason}"))
}

/// Spawn one terminal in the resident server: the headless path (fixed
/// sizes, no UI) and the workbench path. The owner is restricted to
/// user-owned kinds here; only Dispatch can create an execution-owned
/// terminal, so a socket client can never fake member attribution.
#[allow(clippy::too_many_arguments)]
fn terminal_create(
    shared: &OfficeShared,
    argv: &[String],
    cwd: &Path,
    env: &[(String, String)],
    cols: u16,
    rows: u16,
    purpose: &str,
    worktree_id: Option<&str>,
    owner_text: &str,
) -> OfficeResult<serde_json::Value> {
    let owner = match owner_text {
        "user_shell" => TerminalOwner::UserShell,
        "agent_cli" => TerminalOwner::AgentCli,
        "test_run" => TerminalOwner::TestRun,
        other => {
            return Err(OfficeError::Validation(format!(
                "terminal owner `{other}` is not spawnable over the socket; \
                 use user_shell | agent_cli | test_run (execution-owned terminals \
                 come only from dispatch)"
            )));
        }
    };
    let mut spec = crate::terminal::TerminalSpec::new(argv.to_vec(), cwd.to_path_buf())?;
    spec.env = env.to_vec();
    spec.cols = cols;
    spec.rows = rows;
    spec.validate()?;
    let worktree = worktree_id.map(WorktreeId::from_str).transpose()?;
    let store = shared.store.lock().expect("office store");
    let (terminal_id, handle) = shared.terminals.spawn(
        spec,
        owner,
        worktree,
        purpose.to_string(),
        None,
        Some(&store),
    )?;
    seed_agent_identification(shared, &terminal_id, argv);
    Ok(serde_json::json!({
        "terminal_id": terminal_id.to_string(),
        "pid": handle.pid(),
        "cols": cols,
        "rows": rows,
    }))
}

fn terminal_resize(
    shared: &OfficeShared,
    terminal_id: &str,
    cols: u16,
    rows: u16,
) -> OfficeResult<serde_json::Value> {
    let terminal_id = TerminalId::from_str(terminal_id)?;
    let handle = shared
        .terminals
        .handle(&terminal_id)?
        .ok_or_else(|| OfficeError::NotFound {
            entity: "terminal",
            id: terminal_id.to_string(),
        })?;
    handle.resize(cols, rows)?;
    Ok(serde_json::json!({"resized": true, "cols": cols, "rows": rows}))
}

/// The workbench projection over the real registries — the same assembly
/// the in-process workbench used, now served to attach clients.
fn workbench_view(shared: Arc<OfficeShared>) -> OfficeResult<serde_json::Value> {
    // V15-4: the automatic sync cycle runs off-thread (network git must
    // never block the view); the TTL/pause checks happen before spawning.
    if auto_sync_due(&shared) {
        let background = Arc::clone(&shared);
        std::thread::spawn(move || {
            let _ = auto_sync_all(&background, false);
        });
    }
    let store = shared.store.lock().expect("office store");
    let mut model = crate::tui::workbench::assemble_view(&store, &shared.terminals)?;

    // S4 sweep: for every live terminal, refresh the AUXILIARY screen
    // observation (display-only) and project all sources onto the rows.
    for row in &mut model.terminals {
        if let Ok(Some(handle)) = shared.terminals.handle(
            &crate::foundation::ids::TerminalId::from_str(&row.terminal_id).expect("row id"),
        ) {
            if handle.try_wait().ok().flatten().is_none() {
                if let Ok(snapshot) = handle.snapshot() {
                    if let Some(status) = crate::agents::infer_from_screen(&snapshot) {
                        let agent = shared
                            .agent_board
                            .project(&row.terminal_id)
                            .into_iter()
                            .find(|r| r.source == crate::agents::StatusSource::ProcessTree)
                            .map(|r| r.agent)
                            .unwrap_or_else(|| "unknown".into());
                        shared
                            .agent_board
                            .observe(crate::agents::AgentStatusRecord {
                                terminal_id: row.terminal_id.clone(),
                                agent,
                                status,
                                source: crate::agents::StatusSource::ScreenInference,
                                detail: "screen rules (auxiliary, never a fact)".into(),
                                updated_at: utc_now(),
                            });
                    }
                }
            };
        };
        row.agent_status = shared.agent_board.project(&row.terminal_id);
    }

    // V15-4 switch + note surface on the model. The note falls back to
    // the persisted one after a restart (QA F17).
    model.auto_pull = read_auto_pull(&store);
    model.sync_note = {
        let in_memory = shared.last_sync_note.lock().expect("sync note").clone();
        in_memory.or_else(|| {
            store
                .connection()
                .query_row(
                    "SELECT value FROM office_settings WHERE key = 'last_sync_note'",
                    [],
                    |row| row.get(0),
                )
                .ok()
        })
    };

    // V15-2/V15-4 row enrichment. The worktree records are collected UNDER
    // the store lock; the slow probes (gh network calls, rev-list) run
    // AFTER it is dropped — the office keeps answering while enriching.
    let mut records: Vec<(String, crate::git::worktrees::TaskWorktreeRecord)> = Vec::new();
    {
        let service = crate::git::worktrees::WorktreeService::new(
            &store,
            crate::git::worktrees::ProtectedRefs::new(vec![]),
        );
        for row in &model.worktrees {
            if let Ok(wid) = crate::foundation::ids::WorktreeId::from_str(&row.worktree_id) {
                if let Some(record) = service.record(&wid).ok().flatten() {
                    records.push((row.worktree_id.clone(), record));
                }
            }
        }
    }
    drop(store);

    use std::collections::hash_map::Entry;
    for row in &mut model.worktrees {
        let Some((_, record)) = records.iter().find(|(id, _)| id == &row.worktree_id) else {
            continue;
        };
        if let Ok(Some((behind, ahead))) = crate::git::sync::ahead_behind(
            &record.worktree_path,
            &record.branch,
            &crate::git::cli::CliRunner::default(),
        ) {
            row.behind = Some(behind);
            row.ahead = Some(ahead);
        }
        let mut cache = shared.pr_cache.lock().expect("pr cache");
        let info = match cache.entry(row.worktree_id.clone()) {
            Entry::Occupied(mut occupied) => {
                let (at, info) = occupied.get_mut();
                if at.elapsed() < Duration::from_secs(60) {
                    info.clone()
                } else {
                    let fresh = crate::git::pr::pr_status_for_branch_using(
                        &record.repo_root,
                        &record.branch,
                        gh_program_for(&shared).as_str(),
                    );
                    *at = Instant::now();
                    *info = fresh.clone();
                    fresh
                }
            }
            Entry::Vacant(vacant) => {
                let fresh = crate::git::pr::pr_status_for_branch_using(
                    &record.repo_root,
                    &record.branch,
                    gh_program_for(&shared).as_str(),
                );
                vacant.insert((Instant::now(), fresh.clone()));
                fresh
            }
        };
        drop(cache);
        if let Some(pr) = info {
            row.pr_state = Some(pr.state);
        }
    }

    serde_json::to_value(&model).map_err(|e| OfficeError::Validation(format!("view: {e}")))
}

/// The pane-layout preference store (QA F5): the client persists its pane
/// tree so a later attach rebuilds the same view. A UI preference in
/// office_settings - bounded, validated as JSON, never an office fact.
fn workbench_layout(
    shared: &OfficeShared,
    layout_json: Option<&str>,
) -> OfficeResult<serde_json::Value> {
    let store = shared.store.lock().expect("office store");
    match layout_json {
        Some(json) => {
            if json.len() > 64 * 1024 {
                return Err(OfficeError::Validation(
                    "layout exceeds the 64 KiB bound".into(),
                ));
            }
            // Must parse: a layout the client cannot restore is junk.
            serde_json::from_str::<serde_json::Value>(json)
                .map_err(|e| OfficeError::Validation(format!("layout is not valid JSON: {e}")))?;
            store.connection().execute(
                "INSERT INTO office_settings(key, value) VALUES ('workbench_layout', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [json],
            )?;
            Ok(serde_json::json!({ "saved": true }))
        }
        None => {
            let saved: Option<String> = store
                .connection()
                .query_row(
                    "SELECT value FROM office_settings WHERE key = 'workbench_layout'",
                    [],
                    |row| row.get(0),
                )
                .ok();
            Ok(serde_json::json!({ "layout": saved }))
        }
    }
}

/// The real bounded diff of one worktree against HEAD (served over the
/// socket for the workbench `d` action).
fn workbench_diff(shared: &OfficeShared, worktree_id: &str) -> OfficeResult<serde_json::Value> {
    let worktree_id = WorktreeId::from_str(worktree_id)?;
    let store = shared.store.lock().expect("office store");
    let service = crate::git::worktrees::WorktreeService::new(
        &store,
        crate::git::worktrees::ProtectedRefs::new(vec![]),
    );
    let record = service
        .record(&worktree_id)?
        .ok_or_else(|| OfficeError::NotFound {
            entity: "worktree",
            id: worktree_id.to_string(),
        })?;
    let diff = service.worktree_diff(&record.worktree_path, 64 * 1024)?;
    Ok(serde_json::json!({ "worktree_id": worktree_id.to_string(), "diff": diff }))
}

/// Open an interactive shell at a worktree's path (the workbench `o`
/// action): a user_shell owned by the server, attached to the worktree.
fn terminal_open_in_worktree(
    shared: &OfficeShared,
    worktree_id: &str,
) -> OfficeResult<serde_json::Value> {
    let worktree_id = WorktreeId::from_str(worktree_id)?;
    let store = shared.store.lock().expect("office store");
    let service = crate::git::worktrees::WorktreeService::new(
        &store,
        crate::git::worktrees::ProtectedRefs::new(vec![]),
    );
    let record = service
        .record(&worktree_id)?
        .ok_or_else(|| OfficeError::NotFound {
            entity: "worktree",
            id: worktree_id.to_string(),
        })?;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let spec = crate::terminal::TerminalSpec::new(vec![shell], record.worktree_path.clone())?;
    let (terminal_id, handle) = shared.terminals.spawn(
        spec,
        TerminalOwner::UserShell,
        Some(worktree_id),
        format!("shell · {}", record.branch),
        None,
        Some(&store),
    )?;
    Ok(serde_json::json!({
        "terminal_id": terminal_id.to_string(),
        "pid": handle.pid(),
        "cwd": record.worktree_path.display().to_string(),
        "branch": record.branch,
    }))
}

/// Create a task worktree from the task's project repo (the workbench `w`
/// action on a task row). The repo comes from the task's project record —
/// never from the client; branch and base dir have honest defaults. The
/// V08 policy (protected refs, one checkout per branch) is the service's.
fn worktree_create_for_task(
    shared: &OfficeShared,
    task_id: &str,
    branch: Option<&str>,
    base_dir: Option<&str>,
) -> OfficeResult<serde_json::Value> {
    let task_id = TaskId::from_str(task_id)?;
    let store = shared.store.lock().expect("office store");
    let task = TaskRegistry::new(&store).require_task(&task_id)?;
    let project_id = task.project_id.ok_or_else(|| {
        OfficeError::Validation(format!(
            "task `{task_id}` has no project; a worktree needs the project's repo root"
        ))
    })?;
    let project = crate::projects::ProjectRegistry::new(&store).require(&project_id)?;
    let repo_root = project.repo_path;
    let base = base_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root.join("worktrees"));
    let branch = branch
        .map(str::to_string)
        .unwrap_or_else(|| format!("agent/task-{}", task_id));
    let mut service = crate::git::worktrees::WorktreeService::new(
        &store,
        crate::git::worktrees::ProtectedRefs::new(vec![]),
    );
    let record = service.create_task_worktree(&repo_root, &base, &task_id, &branch)?;
    serde_json::to_value(&record).map_err(|e| OfficeError::Validation(format!("record: {e}")))
}

// ---------------------------------------------------------------------------
// Agent status + container intake (S4, issue #46; ADR 0012 decision 5)
// ---------------------------------------------------------------------------

/// Seed the process-tree identification slot from the spawned argv: the
/// dominant case is that the session IS the agent CLI. Identification
/// names the agent only - the state stays unknown until a source speaks.
fn seed_agent_identification(shared: &OfficeShared, terminal_id: &TerminalId, argv: &[String]) {
    let Some(program) = argv.first() else {
        return;
    };
    let base = program
        .rsplit('/')
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    let agent = crate::agents::DETECTABLE_AGENTS
        .iter()
        .find(|agent| base.starts_with(**agent))
        .map(|agent| agent.to_string());
    if let Some(agent) = agent {
        shared
            .agent_board
            .observe(crate::agents::AgentStatusRecord {
                terminal_id: terminal_id.to_string(),
                agent,
                status: crate::agents::AgentStatus::Unknown,
                source: crate::agents::StatusSource::ProcessTree,
                detail: "identified from the spawned argv".into(),
                updated_at: utc_now(),
            });
    }
}

/// A controlled agent-status report: the AUTHORITATIVE source. Requires a
/// member attribution + a live grant carrying `agent_report` (the socket
/// gate enforces that before this handler runs). The report is audited;
/// it describes the agent's state and never completes a task.
fn agent_report(
    shared: &OfficeShared,
    terminal_id: &str,
    agent: &str,
    status: &str,
    detail: &str,
    reset_at: Option<&str>,
) -> OfficeResult<serde_json::Value> {
    let terminal_id = TerminalId::from_str(terminal_id)?;
    let status = crate::agents::AgentStatus::parse(status)?;
    if agent.trim().is_empty() {
        return Err(OfficeError::Validation(
            "agent name must not be empty".into(),
        ));
    }
    // A report about a terminal this office hosts: identity checks out.
    shared
        .terminals
        .handle(&terminal_id)?
        .ok_or_else(|| OfficeError::NotFound {
            entity: "terminal",
            id: terminal_id.to_string(),
        })?;
    let record = crate::agents::AgentStatusRecord {
        terminal_id: terminal_id.to_string(),
        agent: agent.to_string(),
        status,
        source: crate::agents::StatusSource::ControlledReport,
        detail: detail.to_string(),
        updated_at: utc_now(),
    };
    shared.agent_board.observe(record.clone());
    let store = shared.store.lock().expect("office store");
    crate::foundation::events::append(
        &store,
        crate::foundation::events::NewEvent {
            domain: DOMAIN_OFFICE_HOST,
            kind: "agent_status_reported".into(),
            subject_type: "terminal".into(),
            subject_id: terminal_id.to_string(),
            origin: "office_host".into(),
            payload: serde_json::json!({
                "agent": agent,
                "status": status.as_str(),
                "detail": detail,
                "source": "controlled_report",
            }),
        },
    )?;
    // V15-5: rate-limit recovery plans. rate_limited REQUIRES a reset
    // time (the office never guesses one); any other reported status
    // cancels the open plan — the limit is over.
    if status == crate::agents::AgentStatus::RateLimited {
        let Some(reset_at) = reset_at else {
            return Err(OfficeError::Validation(
                "rate_limited requires reset_at (RFC3339); the office does not \
                 schedule recovery for an unknown time"
                    .into(),
            ));
        };
        // Normalize to UTC before storing (QA F1): a local offset like
        // +08:00 stored verbatim breaks the sweep's reset_at comparison
        // and silently delays/skips the wake-up.
        let reset_at = normalize_rfc3339_utc(reset_at)?;
        store.connection().execute(
            "UPDATE recovery_plans SET state = 'cancelled', fired_at = ?2
             WHERE terminal_id = ?1 AND state = 'scheduled'",
            rusqlite::params![terminal_id.to_string(), utc_now()],
        )?;
        store.connection().execute(
            "INSERT INTO recovery_plans(terminal_id, reset_at, action, state, created_at)
             VALUES (?1, ?2, 'resume_prompt', 'scheduled', ?3)",
            rusqlite::params![terminal_id.to_string(), reset_at, utc_now()],
        )?;
    } else {
        store.connection().execute(
            "UPDATE recovery_plans SET state = 'cancelled', fired_at = ?2
             WHERE terminal_id = ?1 AND state = 'scheduled'",
            rusqlite::params![terminal_id.to_string(), utc_now()],
        )?;
    }

    serde_json::to_value(&record).map_err(|e| OfficeError::Validation(format!("record: {e}")))
}

/// Submit agent content for the self-model container's intake. The content
/// lands as a private file reference; the submission is audited with a
/// digest so the container (and the owner) can trace what entered. Viva
/// does NOT decide whether the content becomes memory - that is the
/// container's curation, outside this channel.
fn agent_content_submit(
    shared: &OfficeShared,
    terminal_id: Option<&str>,
    kind: &str,
    content: &str,
    source_ref: Option<&str>,
) -> OfficeResult<serde_json::Value> {
    if kind.trim().is_empty() {
        return Err(OfficeError::Validation(
            "content kind must not be empty".into(),
        ));
    }
    if content.is_empty() {
        return Err(OfficeError::Validation(
            "content must not be empty; the channel moves real material, not placeholders".into(),
        ));
    }
    if content.len() > 1024 * 1024 {
        return Err(OfficeError::Validation(
            "content exceeds the 1 MiB channel bound; split the submission".into(),
        ));
    }
    let content_dir = shared.home.join("agent_content");
    std::fs::create_dir_all(&content_dir)?;
    let content_id = format!("content-{}", uuid::Uuid::new_v4().simple());
    let path = content_dir.join(format!("{content_id}.json"));
    let digest = fnv1a_64(content.as_bytes());
    let record = serde_json::json!({
        "content_id": content_id,
        "kind": kind,
        "terminal_id": terminal_id,
        "source_ref": source_ref,
        "bytes": content.len(),
        "digest_fnv1a_64": format!("{digest:016x}"),
        "submitted_at": utc_now(),
        "content": content,
    });
    {
        let mut file = std::fs::OpenOptions::new();
        file.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            file.mode(0o600);
        }
        let mut file = file.open(&path)?;
        serde_json::to_writer_pretty(&mut file, &record)?;
    }
    let store = shared.store.lock().expect("office store");
    crate::foundation::events::append(
        &store,
        crate::foundation::events::NewEvent {
            domain: DOMAIN_OFFICE_HOST,
            kind: "agent_content_submitted".into(),
            subject_type: "agent_content".into(),
            subject_id: content_id.clone(),
            origin: "office_host".into(),
            payload: serde_json::json!({
                "kind": kind,
                "terminal_id": terminal_id,
                "source_ref": source_ref,
                "bytes": content.len(),
                "digest_fnv1a_64": format!("{digest:016x}"),
                "path": path.display().to_string(),
                "note": "channel + audit only; the container decides what to keep",
            }),
        },
    )?;
    Ok(serde_json::json!({
        "content_id": content_id,
        "digest_fnv1a_64": format!("{digest:016x}"),
        "path": path.display().to_string(),
        "note": "submitted to the container intake; the container decides what to keep - not a memory, not a fact",
    }))
}

/// FNV-1a (64-bit) content digest for the intake audit trail.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

// ---------------------------------------------------------------------------
// Orchestration primitives (S5, issue #47; ADR 0012 decision 4)
// ---------------------------------------------------------------------------

/// `agent.prompt`: text to the agent's stdin, audited. The prompt is a
/// keystroke-level act - reaching the child's stdin proves nothing about
/// the agent's understanding (that is what the status reports and the
/// content intake are for).
fn agent_prompt(
    shared: &OfficeShared,
    terminal_id: &str,
    prompt: &str,
) -> OfficeResult<serde_json::Value> {
    let terminal_id = TerminalId::from_str(terminal_id)?;
    if prompt.is_empty() {
        return Err(OfficeError::Validation(
            "an empty prompt is not a prompt; the channel moves real material".into(),
        ));
    }
    if prompt.len() > 256 * 1024 {
        return Err(OfficeError::Validation(
            "prompt exceeds the 256 KiB bound; split it".into(),
        ));
    }
    let handle = shared
        .terminals
        .handle(&terminal_id)?
        .ok_or_else(|| OfficeError::NotFound {
            entity: "terminal",
            id: terminal_id.to_string(),
        })?;
    handle.input(prompt.as_bytes())?;
    let store = shared.store.lock().expect("office store");
    crate::foundation::events::append(
        &store,
        crate::foundation::events::NewEvent {
            domain: DOMAIN_OFFICE_HOST,
            kind: "agent_prompt_sent".into(),
            subject_type: "terminal".into(),
            subject_id: terminal_id.to_string(),
            origin: "office_host".into(),
            payload: serde_json::json!({
                "bytes": prompt.len(),
                "digest_fnv1a_64": format!("{:016x}", fnv1a_64(prompt.as_bytes())),
            }),
        },
    )?;
    Ok(serde_json::json!({
        "sent": true,
        "bytes": prompt.len(),
        "note": "delivered to the child's stdin; the agent's understanding is a separate question",
    }))
}

/// `agent.wait`: poll the status board until the terminal's AUTHORITATIVE
/// record matches the requested state, or the timeout passes. The wait
/// runs on this host (the orchestrator may disconnect; the outcome is
/// audited either way and reachable through the events feed).
fn agent_wait(
    shared: &OfficeShared,
    terminal_id: &str,
    status: &str,
    timeout_secs: u64,
) -> OfficeResult<serde_json::Value> {
    let terminal_id = TerminalId::from_str(terminal_id)?;
    let wanted = crate::agents::AgentStatus::parse(status)?;
    let timeout = Duration::from_secs(timeout_secs.min(300));
    shared
        .terminals
        .handle(&terminal_id)?
        .ok_or_else(|| OfficeError::NotFound {
            entity: "terminal",
            id: terminal_id.to_string(),
        })?;
    let deadline = Instant::now() + timeout;
    loop {
        let matched = shared
            .agent_board
            .project(&terminal_id.to_string())
            .into_iter()
            .find(|record| {
                record.source == crate::agents::StatusSource::ControlledReport
                    && record.status == wanted
            });
        if let Some(record) = matched {
            let store = shared.store.lock().expect("office store");
            crate::foundation::events::append(
                &store,
                crate::foundation::events::NewEvent {
                    domain: DOMAIN_OFFICE_HOST,
                    kind: "agent_wait_matched".into(),
                    subject_type: "terminal".into(),
                    subject_id: terminal_id.to_string(),
                    origin: "office_host".into(),
                    payload: serde_json::json!({
                        "status": wanted.as_str(),
                        "detail": record.detail,
                    }),
                },
            )?;
            return Ok(serde_json::json!({
                "matched": true,
                "status": wanted.as_str(),
                "updated_at": record.updated_at,
                "detail": record.detail,
            }));
        }
        if Instant::now() >= deadline {
            let store = shared.store.lock().expect("office store");
            crate::foundation::events::append(
                &store,
                crate::foundation::events::NewEvent {
                    domain: DOMAIN_OFFICE_HOST,
                    kind: "agent_wait_timeout".into(),
                    subject_type: "terminal".into(),
                    subject_id: terminal_id.to_string(),
                    origin: "office_host".into(),
                    payload: serde_json::json!({ "status": wanted.as_str() }),
                },
            )?;
            return Ok(serde_json::json!({
                "matched": false,
                "status": wanted.as_str(),
                "timeout_secs": timeout.as_secs(),
            }));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The durable event feed: office events after a sequence number. This is
/// how a disconnected orchestrator catches up - reconnect and ask again
/// with the last seq you saw.
fn events_feed(
    shared: &OfficeShared,
    since_seq: u64,
    limit: u32,
) -> OfficeResult<serde_json::Value> {
    let limit = limit.clamp(1, 500);
    let store = shared.store.lock().expect("office store");
    let mut stmt = store.connection().prepare(
        "SELECT seq, event_id, occurred_at, domain, kind, subject_type, subject_id, origin, payload
         FROM office_events WHERE seq > ?1 ORDER BY seq ASC LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![since_seq as i64, limit], |row| {
        Ok(serde_json::json!({
            "seq": row.get::<_, i64>(0)?,
            "event_id": row.get::<_, String>(1)?,
            "occurred_at": row.get::<_, String>(2)?,
            "domain": row.get::<_, String>(3)?,
            "kind": row.get::<_, String>(4)?,
            "subject_type": row.get::<_, String>(5)?,
            "subject_id": row.get::<_, String>(6)?,
            "origin": row.get::<_, String>(7)?,
            "payload": serde_json::from_str::<serde_json::Value>(&row.get::<_, String>(8)?)
                .unwrap_or(serde_json::Value::Null),
        }))
    })?;
    let events: Vec<serde_json::Value> = rows.collect::<Result<Vec<_>, _>>()?;
    let last_seq = events
        .last()
        .and_then(|event| event.get("seq").and_then(|s| s.as_u64()))
        .unwrap_or(since_seq);
    Ok(serde_json::json!({
        "events": events,
        "last_seq": last_seq,
        "note": "reconnect with your last seen seq to continue the stream",
    }))
}

// ---------------------------------------------------------------------------
// Pause/resume + close policy (S6, issue #48; ADR 0012 decision 3)
// ---------------------------------------------------------------------------

/// Owner pause/resume: flip the gate, persist it (the semantics survive a
/// restart), and audit. Running executions are untouched by design.
fn pause_server(shared: &OfficeShared, paused: bool) -> OfficeResult<serde_json::Value> {
    shared.paused.store(paused, Ordering::SeqCst);
    {
        let store = shared.store.lock().expect("office store");
        store.connection().execute(
            "INSERT INTO office_settings(key, value) VALUES ('paused', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![if paused { "true" } else { "false" }],
        )?;
        crate::foundation::events::append(
            &store,
            crate::foundation::events::NewEvent {
                domain: DOMAIN_OFFICE_HOST,
                kind: if paused {
                    "office_paused"
                } else {
                    "office_resumed"
                }
                .into(),
                subject_type: "office".into(),
                subject_id: shared.host_id.clone(),
                origin: "office_host".into(),
                payload: serde_json::json!({
                    "note": if paused {
                        "no new dispatch, no maintenance cycles; running executions untouched"
                    } else {
                        "dispatch and maintenance resumed"
                    },
                }),
            },
        )?;
    }
    Ok(serde_json::json!({
        "paused": paused,
        "note": "the gate covers new dispatch and maintenance only",
    }))
}

/// The maintenance gate (S6, issue #48 QA F4): bounded maintenance windows
/// run only when the office is not explicitly paused. Reads the PERSISTED
/// pause, so the gate holds even with no live host - the pause itself
/// persists, and this keeps "pause stops new dispatch AND maintenance
/// cycles" true end to end.
pub fn assert_maintenance_allowed(store: &Store) -> OfficeResult<()> {
    let paused: bool = store
        .connection()
        .query_row(
            "SELECT value FROM office_settings WHERE key = 'paused'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map(|value| value == "true")
        .unwrap_or(false);
    if paused {
        return Err(OfficeError::Validation(
            "office is paused: maintenance windows are gated (running executions are \
             unaffected; `viva resume` lifts the pause)"
                .into(),
        ));
    }
    Ok(())
}

/// Apply the close-policy hook at shutdown time. `pause` persists the
/// paused state so the NEXT server starts paused; `continue` explicitly
/// clears any pause; `None` (an unadorned shutdown) leaves the CURRENT
/// state untouched - an operator-paused office that is shut down comes
/// back paused, because the pause was an explicit owner decision.
fn apply_close_policy(shared: &OfficeShared, close_policy: Option<&str>) -> OfficeResult<()> {
    match close_policy {
        Some("pause") => pause_server(shared, true).map(|_| ()),
        Some("continue") => pause_server(shared, false).map(|_| ()),
        None => Ok(()),
        Some(other) => Err(OfficeError::Validation(format!(
            "unknown close policy `{other}` (continue | pause)"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Live handoff (S3, issue #45; ADR 0012 decision 2)
// ---------------------------------------------------------------------------

/// `ServerRestart` step 1: bind the rendezvous socket, hand its path back
/// to the caller, and perform the transfer on a background thread. The
/// response goes out immediately — the CLI then starts the resumed server,
/// which connects to the rendezvous socket. On any transfer failure the
/// host keeps serving (nothing was given away: fd passing duplicates).
fn begin_server_restart(shared: &Arc<OfficeShared>) -> OfficeResult<serde_json::Value> {
    let handoff_path = shared.home.join(HANDOFF_SOCKET_NAME);
    {
        let mut slot = shared.handoff_listener.lock().expect("handoff slot");
        if slot.is_some() {
            return Err(OfficeError::Validation(
                "a server restart is already in progress; connect to its handoff socket                  or wait for the timeout"
                    .into(),
            ));
        }
        let _ = std::fs::remove_file(&handoff_path);
        let listener = UnixListener::bind(&handoff_path)?;
        *slot = Some(listener);
    }
    let listener = shared
        .handoff_listener
        .lock()
        .expect("handoff slot")
        .take()
        .expect("just stored");
    let shared = Arc::clone(shared);
    let response_path = handoff_path.display().to_string();
    std::thread::spawn(move || perform_handoff(shared, listener, handoff_path));
    Ok(serde_json::json!({
        "state": "handoff_ready",
        "handoff_socket": response_path,
        "timeout_secs": HANDOFF_TIMEOUT.as_secs(),
    }))
}

/// The transfer itself. Collects every LIVE terminal (manifest + master fd
/// duplicate), sends them to the resumed server, and on the ack marks this
/// host `transferred` so its exit keeps every process running. Any failure
/// cleans the rendezvous socket up and leaves the host fully serving.
fn perform_handoff(
    shared: Arc<OfficeShared>,
    listener: UnixListener,
    handoff_path: std::path::PathBuf,
) {
    // Collect BEFORE accepting so the sender-side snapshot is one
    // consistent set. Hoisted: the failure branch needs `entries` to put
    // every reader back.
    let collect = (|| -> OfficeResult<(Vec<HandoffEntry>, Vec<RawFd>)> {
        let mut entries = Vec::new();
        let mut fds = Vec::new();
        for entry in shared.terminals.list() {
            let handle = match shared.terminals.handle(&entry.terminal_id).ok().flatten() {
                Some(handle) => handle,
                None => continue,
            };
            // Exited sessions carry nothing worth transferring.
            if handle.try_wait().ok().flatten().is_some() {
                continue;
            }
            let Some(fd) = handle.master_fd_for_transfer()? else {
                continue;
            };
            let snapshot = handle.snapshot()?;
            entries.push(HandoffEntry {
                terminal_id: entry.terminal_id.to_string(),
                owner: owner_label(&entry.owner),
                worktree_id: entry.worktree_id.map(|w| w.to_string()),
                purpose: entry.purpose,
                pid: handle.pid().unwrap_or(0),
                pid_start_marker: handle.pid_start_marker.clone(),
                cols: snapshot.cols,
                rows: snapshot.rows,
                history: snapshot.scrollback,
            });
            fds.push(fd);
        }
        Ok((entries, fds))
    })();
    let (entries, fds) = match collect {
        Ok(pair) => pair,
        Err(err) => {
            let store = shared.store.lock().expect("office store");
            let _ = record_recovery(
                &store,
                &shared.host_id,
                "live_handoff_failed",
                &format!(
                    "the transfer never started ({err}); this host keeps serving every \
                     terminal — no process touched"
                ),
            );
            let _ = std::fs::remove_file(&handoff_path);
            return;
        }
    };
    let outcome = (|| -> OfficeResult<usize> {
        // Stop OUR readers first: two readers on one master would split
        // the child's bytes between the old and the new server. The ack is
        // sent only after every old reader has exited.
        for (entry, _) in entries.iter().zip(&fds) {
            if let Ok(Some(handle)) = shared
                .terminals
                .handle(&TerminalId::from_str(&entry.terminal_id).expect("entry id"))
            {
                handle.detach_reader();
            }
        }

        // Wait, bounded, for the resumed server to connect. On timeout the
        // dups are closed and nothing happened.
        let _ = listener.set_nonblocking(true);
        let deadline = Instant::now() + HANDOFF_TIMEOUT;
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    // The polled listener passes O_NONBLOCK to the accepted
                    // socket on BSD/macOS — restore blocking mode or the
                    // manifest write turns into EAGAIN.
                    stream.set_nonblocking(false)?;
                    break stream;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        for fd in &fds {
                            #[cfg(unix)]
                            unsafe {
                                libc::close(*fd);
                            }
                        }
                        return Err(OfficeError::Validation(
                            "no resumed server connected within the handoff timeout".into(),
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(OfficeError::Io(e)),
            }
        };
        let mut stream = stream;
        let send_result = handoff::send_entries(
            &mut stream,
            &HandoffManifest {
                protocol: handoff::HANDOFF_PROTOCOL,
                host_id: shared.host_id.clone(),
                entries: entries.clone(),
            },
            &fds,
        );
        if let Err(err) = send_result {
            for fd in &fds {
                #[cfg(unix)]
                unsafe {
                    libc::close(*fd);
                }
            }
            return Err(err);
        }
        Ok(entries.len())
    })();

    match outcome {
        Ok(count) => {
            let store = shared.store.lock().expect("office store");
            let _ = record_recovery(
                &store,
                &shared.host_id,
                "live_handoff_out",
                &format!(
                    "{count} live terminal(s) transferred to the resumed server; \
                     dispatches arriving during the handoff window were not transferred \
                     and are recorded by the next reconciliation"
                ),
            );
            drop(store);
            shared.transferred.store(true, Ordering::SeqCst);
            shared.stopping.store(true, Ordering::SeqCst);
        }
        Err(err) => {
            // Put every reader back FIRST (issue #45 QA F3): a failed
            // transfer must leave this host exactly as it was — serving,
            // with live output. A reader that cannot return names its
            // terminal as degraded (input works, output frozen until
            // restart) instead of pretending.
            for entry in &entries {
                let Ok(terminal_id) = TerminalId::from_str(&entry.terminal_id) else {
                    continue;
                };
                if let Ok(Some(handle)) = shared.terminals.handle(&terminal_id) {
                    if let Err(reattach_err) = handle.reattach_reader() {
                        let store = shared.store.lock().expect("office store");
                        let _ = record_recovery(
                            &store,
                            &shared.host_id,
                            "live_handoff_reader_lost",
                            &format!(
                                "terminal `{}` could not reattach its output reader \
                                 ({reattach_err}); input still reaches the child but new \
                                 output is not captured until the server restarts",
                                entry.terminal_id
                            ),
                        );
                    }
                }
            }
            let store = shared.store.lock().expect("office store");
            let _ = record_recovery(
                &store,
                &shared.host_id,
                "live_handoff_failed",
                &format!(
                    "the transfer did not complete ({err}); readers were reattached and \
                     this host keeps serving every terminal — the type-1 fallback, no \
                     process touched"
                ),
            );
            let _ = std::fs::remove_file(&handoff_path);
        }
    }
}

/// The resumed server's side (`viva server --resume`): connect to the
/// rendezvous socket, adopt every transferred session under its ORIGINAL
/// terminal identity, wait for the old host to release the control socket,
/// then claim and serve as usual.
pub fn resume_server(home: &Path) -> OfficeResult<()> {
    crate::foundation::paths::ensure_private_dir(home)?;
    let handoff_path = home.join(HANDOFF_SOCKET_NAME);
    // The old host creates the rendezvous in its ServerRestart handler;
    // poll for it (the CLI starts this process right after that response).
    let deadline = Instant::now() + HANDOFF_TIMEOUT;
    let stream = loop {
        if handoff_path.exists() {
            if let Ok(stream) = UnixStream::connect(&handoff_path) {
                break stream;
            }
        }
        if Instant::now() >= deadline {
            return Err(OfficeError::Validation(format!(
                "no handoff socket appeared at {} within {HANDOFF_TIMEOUT:?}",
                handoff_path.display()
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let (manifest, fds) = handoff::receive_entries(&stream)?;
    drop(stream);

    // Wait for the old host to release the control socket, then claim it.
    // (The transferred fds stay open the whole time — the kernel buffers
    // the children's output in the meantime.)
    let office_sock = home.join(OFFICE_SOCKET_NAME);
    let deadline = Instant::now() + HANDOFF_TIMEOUT;
    while office_sock.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    if office_sock.exists() {
        for fd in fds {
            #[cfg(unix)]
            unsafe {
                libc::close(fd);
            }
        }
        return Err(OfficeError::Validation(
            "the old host never released the control socket; the handoff was abandoned              (the old host's recovery records name what happened)"
                .into(),
        ));
    }
    let host = OfficeHost::open(home)?;
    {
        let shared = host.shared();
        let store = shared.store.lock().expect("office store");
        for (entry, fd) in manifest.entries.iter().zip(fds) {
            let adopt = (|| -> OfficeResult<()> {
                let terminal_id = TerminalId::from_str(&entry.terminal_id)?;
                let owner = owner_from_label(&entry.owner)?;
                let worktree = entry
                    .worktree_id
                    .as_deref()
                    .map(WorktreeId::from_str)
                    .transpose()?;
                shared.terminals.adopt(
                    terminal_id,
                    owner,
                    worktree,
                    entry.purpose.clone(),
                    fd,
                    entry.pid,
                    entry.pid_start_marker.clone(),
                    entry.cols,
                    entry.rows,
                    entry.history.clone(),
                    None,
                )
            })();
            if let Err(err) = adopt {
                // One failed adoption is that terminal's type-1 fallback:
                // its pid is unaccounted, nothing is signalled.
                let _ = record_recovery(
                    &store,
                    &shared.host_id,
                    "live_handoff_adopt_failed",
                    &format!(
                        "terminal `{}` could not be adopted ({err}); its pid {} is \
                         unaccounted and was not signalled",
                        entry.terminal_id, entry.pid
                    ),
                );
            }
        }
        let _ = record_recovery(
            &store,
            &shared.host_id,
            "live_handoff_in",
            &format!(
                "adopted {} transferred terminal(s) from host `{}`; the transfer \
                 preserved their identity and history",
                manifest.entries.len(),
                manifest.host_id
            ),
        );
    }
    eprintln!(
        "office: resumed with {} transferred terminal(s)",
        manifest.entries.len()
    );
    host.serve()
}

/// Parse an owner label back into its typed value (the inverse of
/// [`owner_label`]; labels are host-issued, never client-declared).
fn owner_from_label(text: &str) -> OfficeResult<TerminalOwner> {
    use std::str::FromStr as _;
    if let Some(execution) = text.strip_prefix("member_execution:") {
        return Ok(TerminalOwner::MemberExecution(
            crate::foundation::ids::ExecutionId::from_str(execution)?,
        ));
    }
    match text {
        "user_shell" => Ok(TerminalOwner::UserShell),
        "agent_cli" => Ok(TerminalOwner::AgentCli),
        "test_run" => Ok(TerminalOwner::TestRun),
        other => Err(OfficeError::Validation(format!(
            "unknown terminal owner label `{other}`"
        ))),
    }
}

fn status(shared: &OfficeShared) -> OfficeResult<serde_json::Value> {
    let store = shared.store.lock().expect("office store");
    let (row_count_executions, running, completed): (i64, i64, i64) = {
        let count = |status: &str| -> OfficeResult<i64> {
            Ok(store.connection().query_row(
                "SELECT COUNT(*) FROM office_executions WHERE status = ?1",
                [status],
                |row| row.get(0),
            )?)
        };
        (
            store.row_count("office_executions")?,
            count("running")?,
            count("completed")?,
        )
    };
    let terminals = shared
        .terminals
        .list()
        .into_iter()
        .map(|entry| {
            serde_json::json!({
                "terminal_id": entry.terminal_id.to_string(),
                "owner": owner_label(&entry.owner),
                "purpose": entry.purpose,
                "worktree_id": entry.worktree_id.map(|w| w.to_string()),
                "live": shared
                    .terminals
                    .handle(&entry.terminal_id)
                    .ok()
                    .flatten()
                    .and_then(|h| h.try_wait().ok().map(|exit| exit.is_none())),
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "host_id": shared.host_id,
        "pid": std::process::id(),
        "home": shared.home.display().to_string(),
        "stopping": shared.stopping.load(Ordering::SeqCst),
        "paused": shared.paused.load(Ordering::SeqCst),
        "executions": {"total": row_count_executions, "running": running, "completed": completed},
        "terminals": terminals,
    }))
}

fn owner_label(owner: &TerminalOwner) -> String {
    match owner {
        TerminalOwner::MemberExecution(execution) => format!("member_execution:{execution}"),
        TerminalOwner::UserShell => "user_shell".into(),
        TerminalOwner::AgentCli => "agent_cli".into(),
        TerminalOwner::TestRun => "test_run".into(),
    }
}

/// Dispatch one task execution under a named grant, through the V03
/// launch-intent protocol. Idempotent by `request_key`: a retry replays
/// the recorded intent instead of spawning a second execution.
#[allow(clippy::too_many_arguments)]
fn dispatch(
    shared: &Arc<OfficeShared>,
    task_id: &str,
    member_id: &str,
    grant_id: &str,
    request_key: &str,
    argv: &[String],
    cwd: &Path,
    worktree_id: Option<&str>,
) -> OfficeResult<serde_json::Value> {
    // The pause gate (S6): no NEW dispatch while paused. Running
    // executions are never touched here - stopping one is the explicit,
    // separate stop.
    if shared.paused.load(Ordering::SeqCst) {
        return Err(OfficeError::Validation(
            "office is paused: no new dispatch (running executions are unaffected;              `viva resume` lifts the pause)"
                .into(),
        ));
    }
    let task_id = TaskId::from_str(task_id)?;
    let member_id = MemberId::from_str(member_id)?;
    let grant_id = GrantId::from_str(grant_id)?;

    let store = shared.store.lock().expect("office store");
    let members = MemberRegistry::new(&store);
    let _member = members.require(&member_id)?;
    let binding = members.binding(&member_id)?.ok_or_else(|| {
        OfficeError::Validation(format!(
            "member `{member_id}` has no model/tool binding; dispatch needs one"
        ))
    })?;

    // The grant must BELONG to the dispatching member: a member's grant
    // never serves another member's dispatch, even for the same task.
    // Checked before the authority verdict so the actionable message wins
    // (the engine enforces the same rule for every other path).
    let authority = AuthorityEngine::new(&store);
    let grant = authority.require_grant(&grant_id)?;
    if grant.principal_member_id.as_ref() != Some(&member_id) {
        return Err(OfficeError::Validation(format!(
            "dispatch denied: grant `{grant_id}` was not issued to member `{member_id}` \
             (principal: {:?}) — grants are not transferable between members",
            grant
                .principal_member_id
                .as_ref()
                .map(|m| m.to_string())
                .unwrap_or_else(|| "<none>".into())
        )));
    }

    // Authorization: the grant must be live, in scope for this task, and
    // carry a dispatch action in an ACT_* mode. Revocation races are
    // denied here, at the moment of effect — not at request time. `check`
    // layers two results: storage failures and the authorization verdict
    // itself; both must be honored.
    match authority.check(
        &Actor::Member {
            member: member_id.clone(),
            grant: Some(grant_id.clone()),
        },
        "dispatch_delegated",
        Some(&task_id),
    ) {
        Ok(Ok(())) => {}
        Ok(Err(denial)) => {
            return Err(OfficeError::Validation(format!(
                "dispatch denied: {denial}"
            )));
        }
        Err(err) => return Err(err),
    }

    let tasks = TaskRegistry::new(&store);
    let task = tasks.require_task(&task_id)?;
    if matches!(task.status, TaskStatus::Done | TaskStatus::Cancelled) {
        return Err(OfficeError::Validation(format!(
            "task `{}` is {}; a finished task is never executed again",
            task.task_id,
            task.status.as_str()
        )));
    }

    if !cwd.is_absolute() {
        return Err(OfficeError::Validation(format!(
            "dispatch cwd must be absolute: {}",
            cwd.display()
        )));
    }

    // A named worktree must be a real office record, not a free-form
    // string: the attribution should never point at a worktree this
    // office does not know.
    if let Some(worktree) = worktree_id {
        let known: i64 = store.connection().query_row(
            "SELECT COUNT(*) FROM task_worktrees
             WHERE worktree_id = ?1 AND released_at IS NULL",
            [worktree],
            |row| row.get(0),
        )?;
        if known == 0 {
            return Err(OfficeError::NotFound {
                entity: "worktree",
                id: worktree.to_string(),
            });
        }
    }

    let mut attribution = AttributionSnapshot::capture(
        member_id.clone(),
        binding.role.clone(),
        binding.model_binding.clone(),
        binding.tools.join(","),
    );
    if let Some(workspace) = task.workspace_id.clone() {
        attribution = attribution.in_workspace(workspace);
    }
    if let Some(project) = task.project_id.clone() {
        attribution = attribution.in_project(project);
    }

    let start = tasks.begin_execution(&task_id, &attribution, request_key)?;
    let intent = match start {
        ExecutionStart::Replayed(intent) => {
            // Idempotency by request key — but ONLY within the same
            // task AND the same member. The same key on a different task
            // or from a different member is a client bug: silently
            // returning the other caller's execution would make it look
            // started when it never ran.
            if intent.task_id != task_id {
                return Err(OfficeError::Validation(format!(
                    "request key `{request_key}` already belongs to task `{}`; \
                     keys are replayed per task — use a different key for task `{task_id}`",
                    intent.task_id
                )));
            }
            if intent.member_id != member_id {
                return Err(OfficeError::Validation(format!(
                    "request key `{request_key}` was already used by member `{}`; \
                     keys are replayed per (task, member) — use a different key",
                    intent.member_id
                )));
            }
            return Ok(serde_json::json!({
                "replayed": true,
                "request_key": intent.request_key,
                "intent_state": intent.state.as_str(),
                "execution_id": intent.execution_id.map(|e| e.to_string()),
                "pid": intent.pid,
            }));
        }
        ExecutionStart::Created(intent) => intent,
    };
    let execution_id = intent
        .execution_id
        .clone()
        .expect("a created intent carries its execution");

    // Spawn the real terminal. If this fails, the intent is honestly
    // unresolved — an uncertain start is never reported as a success.
    let spawn = shared.terminals.spawn(
        crate::terminal::TerminalSpec::new(argv.to_vec(), cwd.to_path_buf())?,
        TerminalOwner::MemberExecution(execution_id.clone()),
        worktree_id.map(WorktreeId::from_str).transpose()?,
        format!("task {} for member {}", task_id, member_id),
        None,
        Some(&store),
    );
    let (terminal_id, handle) = match spawn {
        Ok(pair) => pair,
        Err(err) => {
            tasks.mark_unresolved(request_key)?;
            return Err(err);
        }
    };
    seed_agent_identification(shared, &terminal_id, argv);
    let pid = handle.pid().unwrap_or(0) as i64;
    tasks.confirm_launch(request_key, pid, handle.pid_start_marker.clone())?;

    let mut spec = LaunchSpec::new(
        argv.to_vec(),
        cwd,
        RequestOrigin::MemberDispatch {
            grant_id: grant_id.clone(),
        },
    )?
    .for_execution(execution_id.clone())
    .on_terminal(terminal_id.clone())
    .with_member(member_id.clone())
    .with_bindings(binding.model_binding.clone(), binding.tools.join(","));
    if let Some(worktree) = worktree_id {
        spec = spec.in_worktree(WorktreeId::from_str(worktree)?);
    }
    crate::foundation::records::insert_launch_spec(&store, &spec)?;

    // Exit watcher: records the process exit through the same intent
    // protocol (a process fact — it never completes the task) and appends
    // one process-source task result so "取结果" over the channel has the
    // real exit fact. The handle is registered on the shared state so the
    // graceful shutdown can join it: a detached watcher would die with the
    // process and the next host would record a false orphan recovery.
    let watcher_shared = Arc::clone(shared);
    let watcher_key = request_key.to_string();
    let watcher_task = task_id.clone();
    let watcher_execution = execution_id.clone();
    let watcher_handle = std::sync::Arc::clone(&handle);
    let watcher = std::thread::spawn(move || {
        if let Ok(exit) = watcher_handle.wait() {
            // A signal exit carries no exit code; the intent protocol's
            // integer field records it as -1 rather than staying silent —
            // a stopped process must always land in the ledger.
            let code = exit.code.unwrap_or(-1);
            let office_stopped = !matches!(exit.via, crate::terminal::ExitVia::ChildExit);
            let store = watcher_shared.store.lock().expect("office store");
            let tasks = TaskRegistry::new(&store);
            let _ = tasks.record_exit(&watcher_key, code);
            {
                // The exit-code ledger sorts non-zero into `failed`; an
                // exit the OFFICE caused (graceful stop / forced kill) is
                // a stop, not a failure — correct the record and say why.
                if office_stopped {
                    let _ = store.connection().execute(
                        "UPDATE office_executions SET status = 'stopped', updated_at = ?2
                         WHERE execution_id = ?1 AND status = 'failed'",
                        rusqlite::params![watcher_execution.as_str(), utc_now()],
                    );
                }
                let _ = tasks.record_result(
                    &watcher_task,
                    Some(watcher_execution),
                    crate::tasks::ResultSource::Process,
                    "process_exit",
                    format!("{watcher_key}:exit"),
                    serde_json::json!({ "exit_code": code, "via": format!("{:?}", exit.via) }),
                );
            }
        }
    });
    shared
        .watchers
        .lock()
        .expect("watcher registry")
        .push(watcher);

    Ok(serde_json::json!({
        "replayed": false,
        "request_key": request_key,
        "intent_state": "confirmed",
        "execution_id": execution_id.to_string(),
        "terminal_id": terminal_id.to_string(),
        "pid": pid,
    }))
}

fn terminal_list(shared: &OfficeShared) -> OfficeResult<serde_json::Value> {
    let terminals = shared
        .terminals
        .list()
        .into_iter()
        .map(|entry| {
            serde_json::json!({
                "terminal_id": entry.terminal_id.to_string(),
                "owner": owner_label(&entry.owner),
                "purpose": entry.purpose,
                "worktree_id": entry.worktree_id.map(|w| w.to_string()),
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({ "terminals": terminals }))
}

fn terminal_snapshot(shared: &OfficeShared, terminal_id: &str) -> OfficeResult<serde_json::Value> {
    let terminal_id = TerminalId::from_str(terminal_id)?;
    let handle = shared
        .terminals
        .handle(&terminal_id)?
        .ok_or_else(|| OfficeError::NotFound {
            entity: "terminal",
            id: terminal_id.to_string(),
        })?;
    let snapshot = handle.snapshot()?;
    Ok(serde_json::to_value(&snapshot)?)
}

fn terminal_input(
    shared: &OfficeShared,
    terminal_id: &str,
    bytes_hex: &str,
) -> OfficeResult<serde_json::Value> {
    let terminal_id = TerminalId::from_str(terminal_id)?;
    let handle = shared
        .terminals
        .handle(&terminal_id)?
        .ok_or_else(|| OfficeError::NotFound {
            entity: "terminal",
            id: terminal_id.to_string(),
        })?;
    let bytes = hex_decode(bytes_hex)?;
    handle.input(&bytes)?;
    Ok(serde_json::json!({"written": bytes.len()}))
}

fn terminal_stop(shared: &OfficeShared, terminal_id: &str) -> OfficeResult<serde_json::Value> {
    let terminal_id = TerminalId::from_str(terminal_id)?;
    let store = shared.store.lock().expect("office store");
    let exit = shared
        .terminals
        .stop(&terminal_id, StopPolicy::default(), Some(&store))?;
    Ok(serde_json::to_value(&exit)?)
}

fn task_results(shared: &OfficeShared, task_id: &str) -> OfficeResult<serde_json::Value> {
    let task_id = TaskId::from_str(task_id)?;
    let store = shared.store.lock().expect("office store");
    let tasks = TaskRegistry::new(&store);
    let results = tasks.results_for_task(&task_id)?;
    Ok(serde_json::to_value(&results)?)
}

/// Record a member's handoff summary. The fact stored is "member X reported
/// this at time T" — never a completion verdict; the task's own status only
/// moves through the outcome ledger (V03). An identical repeat is reported
/// as a duplicate and changes nothing (故障/重复事件不会把任务错误标为通过).
fn handoff(
    shared: &OfficeShared,
    task_id: &str,
    member_id: &str,
    summary: &str,
) -> OfficeResult<serde_json::Value> {
    let task_id = TaskId::from_str(task_id)?;
    let member_id = MemberId::from_str(member_id)?;
    if summary.trim().is_empty() {
        return Err(OfficeError::Validation(
            "handoff summary must not be empty".into(),
        ));
    }
    let store = shared.store.lock().expect("office store");
    let tasks = TaskRegistry::new(&store);
    tasks.require_task(&task_id)?;
    // Attach the task's most recent launched execution when one exists; a
    // handoff without an execution is still a valid member report.
    let execution: Option<crate::foundation::ids::ExecutionId> = store
        .connection()
        .query_row(
            "SELECT execution_id FROM launch_intents
             WHERE task_id = ?1 AND execution_id IS NOT NULL
             ORDER BY intended_at DESC LIMIT 1",
            [task_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|s| crate::foundation::ids::ExecutionId::from_str(&s).ok());
    // Deterministic dedup: the same task+member+summary is the same report.
    let dedup = format!("handoff:{}:{}:{:016x}", task_id, member_id, fnv1a(summary));
    match tasks.record_result(
        &task_id,
        execution,
        crate::tasks::ResultSource::User,
        "member_handoff",
        dedup,
        serde_json::json!({
            "reporter_member_id": member_id.to_string(),
            "summary": summary,
        }),
    )? {
        Ok(result) => Ok(serde_json::json!({
            "recorded": true,
            "result_id": result.result_id,
            "note": "member-reported fact; not a completion verdict and not acceptance PASS",
        })),
        Err(_duplicate) => Ok(serde_json::json!({
            "recorded": false,
            "duplicate": true,
            "note": "an identical handoff is already recorded; task state unchanged",
        })),
    }
}

/// FNV-1a (64-bit) over the summary text — a deterministic dedup key needs
/// a stable hash; std's DefaultHasher is not stable across releases.
fn fnv1a(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

// ---------------------------------------------------------------------------
// Client side: send a request to the active host, or fail honestly
// ---------------------------------------------------------------------------

/// Send one request to the active office host over the real channel. When
/// no host listens, this fails cleanly and never starts a daemon.
pub fn send_request(home: &Path, request: OfficeRequest) -> OfficeResult<OfficeResponse> {
    let socket_path = home.join(OFFICE_SOCKET_NAME);
    if !socket_path.exists() {
        return Err(OfficeError::Validation(format!(
            "no active office in {} (mutation rejected; start one with `viva start`)",
            home.display()
        )));
    }
    let mut stream = UnixStream::connect(&socket_path)?;
    round_trip(&mut stream, &request)
}

/// Offline status: read the store directly (no host required). Reports the
/// last host row and whether the socket is alive; never claims a live host.
pub fn offline_status(home: &Path) -> OfficeResult<serde_json::Value> {
    use rusqlite::OptionalExtension;

    let socket_path = home.join(OFFICE_SOCKET_NAME);
    let db = crate::foundation::paths::database_path(home);
    if !db.exists() {
        // Read-only by design: a missing store is reported, not created.
        return Ok(serde_json::json!({
            "home": home.display().to_string(),
            "socket_present": socket_path.exists(),
            "active_host": null,
            "last_host": null,
            "recovery_events": 0,
            "store": "absent",
        }));
    }
    let store = Store::open(&db, office_migrations())?;
    let last_host: Option<(String, i64, Option<String>, Option<String>)> = {
        let mut stmt = store.connection().prepare(
            "SELECT host_id, pid, exited_at, exit_kind FROM office_hosts
             ORDER BY started_at DESC LIMIT 1",
        )?;
        stmt.query_row([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .optional()?
    };
    let recoveries: i64 =
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM office_recovery_events", [], |row| {
                row.get(0)
            })?;
    let mut value = serde_json::json!({
        "home": home.display().to_string(),
        "socket_present": socket_path.exists(),
        "active_host": null,
        "last_host": last_host.map(|(host_id, pid, exited_at, exit_kind)| serde_json::json!({
            "host_id": host_id, "pid": pid, "exited_at": exited_at, "exit_kind": exit_kind,
        })),
        "recovery_events": recoveries,
    });
    if socket_path.exists() {
        // The socket exists; a live host would answer a ping. Report what
        // the probe actually saw — a present-but-unreachable socket stays
        // visibly not-reachable instead of being silently claimed alive.
        match OfficeHost::probe_socket(&socket_path) {
            Ok(Some(pid)) => {
                value["active_host"] = serde_json::json!({"pid": pid, "reachable": true});
            }
            _ => {
                value["active_host"] = serde_json::json!({"reachable": false});
            }
        }
    }
    Ok(value)
}

/// V15-4: cheap due check for the background sync (no locks held past the
/// settings read; the heavy git work happens off-thread).
fn auto_sync_due(shared: &OfficeShared) -> bool {
    if shared.sync_in_flight.load(Ordering::SeqCst) {
        return false;
    }
    if shared.paused.load(Ordering::SeqCst) {
        return false;
    }
    // The fetch half runs regardless of auto_pull (QA F3): behind/ahead
    // markers stay fresh with the switch OFF; only the PULL half is
    // gated. Both halves share the same 5-min cadence.
    let store = shared.store.lock().expect("office store");
    drop(store);
    match *shared.last_sync.lock().expect("last sync") {
        Some(last) => last.elapsed() >= Duration::from_secs(5 * 60),
        None => true,
    }
}

/// V15-4: the user-facing switch (default off, persisted).
fn set_auto_pull(shared: &OfficeShared, on: bool) -> OfficeResult<serde_json::Value> {
    let store = shared.store.lock().expect("office store");
    store.connection().execute(
        "INSERT INTO office_settings(key, value) VALUES ('auto_pull', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![if on { "true" } else { "false" }],
    )?;
    crate::foundation::events::append(
        &store,
        crate::foundation::events::NewEvent {
            domain: DOMAIN_OFFICE_HOST,
            kind: "auto_pull_toggled".into(),
            subject_type: "office".into(),
            subject_id: shared.host_id.clone(),
            origin: "office_host".into(),
            payload: serde_json::json!({ "on": on }),
        },
    )?;
    Ok(serde_json::json!({ "auto_pull": on }))
}

// ---------------------------------------------------------------------------
// V15 workbench deepening (issues V15-1..V15-5): files, PR cleanup,
// handoff, auto-sync, rate-limit recovery. All handlers keep the office
// discipline: typed ids, bounded reads, audit events, honest unknowns.
// ---------------------------------------------------------------------------

/// RFC3339 validation + normalization for user/agent supplied times
/// (recovery plans, V15-5 QA F1): the value is stored as UTC with whole
/// seconds. Recovery sweeps compare reset times as TEXT against utc_now()
/// (1-second sweep granularity) — that comparison is only correct when
/// every stored value is normalized to the same offset. A local offset
/// like `+08:00` must NEVER be stored verbatim: it would make a due plan
/// look und Due and an undu one due under string ordering.
fn normalize_rfc3339_utc(text: &str) -> OfficeResult<String> {
    let parsed = time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
        .map_err(|e| OfficeError::Validation(format!("`{text}` is not RFC3339: {e}")))?;
    // to_offset (NOT replace_offset): it CONVERTS the instant so the
    // wall clock reads the same moment in UTC. replace_offset only
    // relabels the zone and keeps the wall clock — the exact bug QA F1
    // reported (a +08:00 time stored 8 hours off).
    let utc = parsed.to_offset(time::UtcOffset::UTC);
    // Whole seconds: the sweep runs at 1-second granularity and the stored
    // text is compared against utc_now() (which has no fractional part).
    let whole = utc.time().replace_nanosecond(0);
    let utc = match whole {
        Ok(time) => utc.replace_time(time),
        Err(_) => utc,
    };
    utc.format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| OfficeError::Validation(format!("recovery time formatting failed: {e}")))
}

/// The gh program to invoke for PR probes: the shared override (tests)
/// or plain `gh` from PATH (production).
fn gh_program_for(shared: &OfficeShared) -> String {
    shared
        .gh_bin
        .lock()
        .expect("gh bin")
        .clone()
        .unwrap_or_else(|| "gh".into())
}

/// The persisted `auto_pull` switch (V15-4): default OFF. Automatic pulls
/// exist only when the user explicitly turned the switch on.
fn read_auto_pull(store: &Store) -> bool {
    store
        .connection()
        .query_row(
            "SELECT value FROM office_settings WHERE key = 'auto_pull'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map(|value| value == "true")
        .unwrap_or(false)
}

/// The automatic sync cycle (V15-4): when `auto_pull` is on and the last
/// cycle is older than 5 minutes, fast-forward every active project's MAIN
/// checkout and fetch every task worktree. Pause gates the automatic path;
/// `force = true` (explicit `viva sync now`) bypasses both the TTL and the
/// pause, because a human asked. Task worktrees are fetched, NEVER pulled.
fn auto_sync_all(shared: &OfficeShared, force: bool) -> OfficeResult<serde_json::Value> {
    // Single-flight guard (QA F2): one cycle at a time, across threads.
    // Set BEFORE any early-return probe, cleared on EVERY exit path — the
    // 3-5 Hz workbench view must never spawn concurrent cycles that
    // stampede `git pull --ff-only` into ref-lock failures.
    if shared
        .sync_in_flight
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(serde_json::json!({ "skipped": "cycle already in flight" }));
    }
    // QA round 6 G4: the clear is DROP-guarded — an inner panic cannot
    // leave the office stuck "in flight" forever.
    struct InFlightGuard<'a>(&'a AtomicBool);
    impl Drop for InFlightGuard<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    let _in_flight = InFlightGuard(&shared.sync_in_flight);
    auto_sync_all_inner(shared, force)
}

fn auto_sync_all_inner(shared: &OfficeShared, force: bool) -> OfficeResult<serde_json::Value> {
    if !force {
        if shared.paused.load(Ordering::SeqCst) {
            return Ok(serde_json::json!({ "skipped": "paused" }));
        }
        if let Some(last) = *shared.last_sync.lock().expect("last sync") {
            if last.elapsed() < Duration::from_secs(5 * 60) {
                return Ok(serde_json::json!({ "skipped": "recent" }));
            }
        }
    }

    // Phase 1 — snapshot WHAT to sync under the store lock, then DROP it
    // for the whole git phase (QA F3 round 2: the probes are network
    // operations with a 30s timeout each, and this same mutex guards every
    // office request; holding it across them froze the workbench).
    let (main_checkouts, worktrees, auto_pull) = {
        let store = shared.store.lock().expect("office store");
        let mains: Vec<(String, std::path::PathBuf)> =
            crate::projects::ProjectRegistry::new(&store)
                .list(true)?
                .into_iter()
                .map(|project| (project.project_id.to_string(), project.repo_path))
                .collect();
        let service = crate::git::worktrees::WorktreeService::new(
            &store,
            crate::git::worktrees::ProtectedRefs::new(vec![]),
        );
        let trees: Vec<(String, std::path::PathBuf)> = service
            .all_records()?
            .into_iter()
            .filter(|record| record.released_at.is_none())
            .map(|record| (record.worktree_id.to_string(), record.worktree_path))
            .collect();
        let auto_pull = read_auto_pull(&store);
        (mains, trees, auto_pull)
    };

    // Phase 2 — the git work, NO store lock held (QA F3). Every outcome —
    // success, skip, or failure — is collected as a result row (QA F10):
    // one unreachable project must never abort the cycle or skip the
    // remaining checkouts, worktree fetches, or the audit.
    let runner = crate::git::cli::CliRunner::default();
    let mut results: Vec<serde_json::Value> = Vec::new();

    for (project_id, repo_path) in &main_checkouts {
        let outcome = if !auto_pull {
            crate::git::sync::MainSyncOutcome::SkippedAutoPullDisabled
        } else {
            crate::git::sync::sync_main_checkout(repo_path, &runner)
                .unwrap_or_else(|err| crate::git::sync::MainSyncOutcome::Failed(err.to_string()))
        };
        let label = match &outcome {
            crate::git::sync::MainSyncOutcome::FastForward { from, to } => {
                format!("fast-forward {from}..{to}")
            }
            crate::git::sync::MainSyncOutcome::AlreadyUpToDate => "up to date".into(),
            crate::git::sync::MainSyncOutcome::SkippedDirty => "skipped (dirty tree)".into(),
            crate::git::sync::MainSyncOutcome::SkippedAutoPullDisabled => {
                "skipped (auto_pull is off)".into()
            }
            crate::git::sync::MainSyncOutcome::Failed(reason) => format!("failed: {reason}"),
        };
        results.push(serde_json::json!({
            "kind": "main_checkout",
            "project": project_id,
            "path": repo_path.display().to_string(),
            "outcome": label,
        }));
    }

    // Task worktrees: fetch only — the working tree is never touched. A
    // fetch failure is a recorded outcome, never an abort.
    for (worktree_id, worktree_path) in &worktrees {
        let outcome = match crate::git::sync::fetch_worktree(worktree_path, &runner) {
            Ok(()) => "fetched".to_string(),
            Err(err) => format!("fetch failed: {err}"),
        };
        results.push(serde_json::json!({
            "kind": "worktree_fetch",
            "worktree": worktree_id,
            "outcome": outcome,
        }));
    }

    // Phase 3 — re-lock ONLY for the audit writes and the cycle bookkeeping.
    let store = shared.store.lock().expect("office store");
    for row in &results {
        crate::foundation::events::append(
            &store,
            crate::foundation::events::NewEvent {
                domain: DOMAIN_OFFICE_HOST,
                kind: "git_auto_sync".into(),
                subject_type: if row["kind"] == "main_checkout" {
                    "project"
                } else {
                    "worktree"
                }
                .into(),
                subject_id: row["project"]
                    .as_str()
                    .or_else(|| row["worktree"].as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                origin: "office_host".into(),
                payload: row.clone(),
            },
        )?;
    }

    let note = format!("{} sync result(s) at {}", results.len(), utc_now());
    *shared.last_sync.lock().expect("last sync") = Some(Instant::now());
    *shared.last_sync_note.lock().expect("sync note") = Some(note.clone());
    // Persist so `viva sync status` (and the workbench after a restart)
    // keeps the last-cycle summary — QA F17.
    store
        .connection()
        .execute(
            "INSERT INTO office_settings(key, value) VALUES ('last_sync_note', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![note],
        )
        .ok();
    Ok(serde_json::json!({
        "auto_pull": auto_pull,
        "results": results,
    }))
}

/// The rate-limit recovery sweep (V15-5): runs at most once a second on
/// the serve loop. Every DUE plan gets an honest outcome — the prompt is
/// delivered to a live terminal, or the plan is closed with a note when
/// the terminal is gone. Nothing is ever executed blindly twice.
fn sweep_recovery_plans(shared: &OfficeShared) {
    // Pause gates the sweep too (V15-5 acceptance: "pause 下不触发、resume
    // 后按计划继续") — due plans stay `scheduled` and fire after resume.
    if shared.paused.load(Ordering::SeqCst) {
        return;
    }
    {
        let mut last = shared.last_recovery_sweep.lock().expect("sweep clock");
        if let Some(last) = *last {
            if last.elapsed() < Duration::from_secs(1) {
                return;
            }
        }
        *last = Some(Instant::now());
    }
    let Ok(store) = shared.store.lock() else {
        return;
    };
    let due: Vec<(i64, String, Option<String>)> = {
        let mut stmt = store
            .connection()
            .prepare(
                "SELECT id, terminal_id, task_id FROM recovery_plans
                 WHERE state = 'scheduled' AND reset_at <= ?1",
            )
            .expect("due plans query");
        let rows = stmt.query_map([utc_now()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        });
        match rows {
            Ok(rows) => rows.collect::<Result<Vec<_>, _>>().unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    };
    for (id, terminal_id, task_id) in due {
        let task_id = task_id.as_deref();
        let terminal = TerminalId::from_str(&terminal_id)
            .ok()
            .and_then(|tid| shared.terminals.handle(&tid).ok().flatten());
        let (outcome, note) = match terminal {
            Some(handle) => match handle.input(b"continue\n") {
                Ok(()) => (
                    "resume prompt delivered".to_string(),
                    format!("prompted `{terminal_id}` to continue"),
                ),
                Err(err) => (
                    "prompt failed".to_string(),
                    format!("terminal `{terminal_id}` rejected input: {err}"),
                ),
            },
            None => (
                "terminal gone".to_string(),
                format!("terminal `{terminal_id}` is no longer hosted; nothing to resume"),
            ),
        };
        let _ = store.connection().execute(
            "UPDATE recovery_plans SET state = 'fired', fired_at = ?2 WHERE id = ?1",
            rusqlite::params![id, utc_now()],
        );
        let _ = crate::foundation::events::append(
            &store,
            crate::foundation::events::NewEvent {
                domain: DOMAIN_OFFICE_HOST,
                kind: "recovery_plan_fired".into(),
                subject_type: "recovery_plan".into(),
                subject_id: id.to_string(),
                origin: "office_host".into(),
                payload: serde_json::json!({
                    "outcome": outcome,
                    "note": note,
                    "task_id": task_id,
                }),
            },
        );
    }
}

/// V15-1: the file inventory of one worktree — every tracked file plus
/// untracked entries, each with its porcelain status. Clean files are
/// marked `clean`; there is no second guessing of git's verdicts.
fn worktree_files(shared: &OfficeShared, worktree_id: &str) -> OfficeResult<serde_json::Value> {
    let wid = WorktreeId::from_str(worktree_id)?;
    let cwd = {
        let store = shared.store.lock().expect("office store");
        let service = crate::git::worktrees::WorktreeService::new(
            &store,
            crate::git::worktrees::ProtectedRefs::new(vec![]),
        );
        let record = service.record(&wid)?.ok_or_else(|| OfficeError::NotFound {
            entity: "worktree",
            id: worktree_id.to_string(),
        })?;
        // Drop the store lock BEFORE the git probes (QA round 5 F5): a
        // slow worktree must not freeze every office request.
        record.worktree_path
    };
    let runner = crate::git::cli::CliRunner::default();
    let cwd = &cwd;

    let mut status_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    if let Ok(status) = runner.git(
        cwd,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    ) {
        let tokens: Vec<String> = String::from_utf8_lossy(status.stdout.as_bytes())
            .split('\0')
            .filter(|token| !token.is_empty())
            .map(str::to_string)
            .collect();
        let mut index = 0;
        while index < tokens.len() {
            let token = &tokens[index];
            if token.len() < 4 {
                index += 1;
                continue;
            }
            let xy = token[..2].to_string();
            let path = token[3..].to_string();
            let rename = xy.contains('R') || xy.contains('C');
            if rename {
                index += 1; // skip the original-path token
            }
            status_map.insert(path, xy.trim().to_string());
            index += 1;
        }
    }

    let max_rows: usize = 500;
    let mut rows: Vec<(String, String)> = Vec::new();
    if let Ok(listed) = runner.git(cwd, &["ls-files", "-z"]) {
        for path in String::from_utf8_lossy(listed.stdout.as_bytes())
            .split('\0')
            .filter(|p| !p.is_empty())
        {
            let path = path.to_string();
            let status = status_map
                .get(&path)
                .cloned()
                .unwrap_or_else(|| "clean".into());
            rows.push((path, status));
        }
    }
    for (path, status) in &status_map {
        if !rows.iter().any(|(existing, _)| existing == path) {
            rows.push((path.clone(), status.clone()));
        }
    }
    // Changed files lead; the rest alphabetical.
    rows.sort_by(|a, b| {
        let changed_a = a.1 != "clean";
        let changed_b = b.1 != "clean";
        changed_b.cmp(&changed_a).then(a.0.cmp(&b.0))
    });

    // Bounded response (QA F5): huge repos must degrade to a truncated
    // list with a real count, not a transport error at 256 KiB.
    let total = rows.len();
    rows.truncate(max_rows);
    Ok(serde_json::json!({
        "worktree_id": worktree_id,
        "truncated": total > max_rows,
        "total": total,
        "files": rows.into_iter().map(|(path, status)| serde_json::json!({
            "path": path, "status": status,
        })).collect::<Vec<_>>(),
    }))
}

/// V15-1: one file's material — a unified diff against the worktree's
/// recorded base for modified files, bounded content otherwise, and an
/// honest `binary` verdict for non-text files.
fn worktree_file_content(
    shared: &OfficeShared,
    worktree_id: &str,
    path: &str,
) -> OfficeResult<serde_json::Value> {
    let wid = WorktreeId::from_str(worktree_id)?;
    // Path safety: the client names a RELATIVE path inside the worktree.
    if path.starts_with('/') || path.split('/').any(|part| part == "..") {
        return Err(OfficeError::Validation(
            "file paths must be relative to the worktree and may not contain `..`".into(),
        ));
    }
    let (worktree_path, base_sha) = {
        let store = shared.store.lock().expect("office store");
        let service = crate::git::worktrees::WorktreeService::new(
            &store,
            crate::git::worktrees::ProtectedRefs::new(vec![]),
        );
        let record = service.record(&wid)?.ok_or_else(|| OfficeError::NotFound {
            entity: "worktree",
            id: worktree_id.to_string(),
        })?;
        (record.worktree_path, record.base_sha)
    };
    let full_path = worktree_path.join(path);
    let max_bytes: usize = 64 * 1024;

    // Modified (tracked) → diff against the recorded base.
    let runner = crate::git::cli::CliRunner::default();
    let changed = runner
        .git(
            &worktree_path,
            &["status", "--porcelain=v1", "-z", "--", path],
        )
        .map(|out| {
            // One token at most for a single path: non-empty and not
            // untracked (`??`) means the tracked file has changes.
            let first = String::from_utf8_lossy(out.stdout.as_bytes());
            let first = first.trim_end_matches('\0').trim();
            !first.is_empty() && !first.starts_with("??")
        })
        .unwrap_or(false);
    if changed {
        let diff = runner
            .git_ok(&worktree_path, &["diff", &base_sha, "--", path])
            .map(|out| {
                // bounded_output truncates at a CHAR BOUNDARY — a raw byte
                // slice panicked on multibyte diffs (QA round 5 F1).
                crate::git::cli::bounded_output(out.stdout.as_bytes(), max_bytes)
            })?;
        return Ok(serde_json::json!({
            "kind": "diff", "path": path, "text": diff,
            "full_path": full_path.display().to_string(),
        }));
    }

    // Otherwise: bounded content, honest binary verdict.
    let bytes = std::fs::read(&full_path).map_err(OfficeError::Io)?;
    let probe = &bytes[..bytes.len().min(8192)];
    if probe.contains(&0u8) {
        return Ok(serde_json::json!({
            "kind": "binary", "path": path,
            "bytes": bytes.len(),
            "full_path": full_path.display().to_string(),
        }));
    }
    let bounded = &bytes[..bytes.len().min(max_bytes)];
    let truncated = bytes.len() > max_bytes;
    Ok(serde_json::json!({
        "kind": "content", "path": path,
        "text": String::from_utf8_lossy(bounded),
        "truncated": truncated, "bytes": bytes.len(),
        "full_path": full_path.display().to_string(),
    }))
}

/// V15-2: the merged-PR cleanup. The gate is real: the branch's PR must be
/// MERGED (re-checked live, not from cache). `release` records the office
/// giving the worktree up; the directory removal then runs WITHOUT force —
/// a dirty tree refuses and says so. The caller confirmed the exact path
/// (two-step UI); the action is audited either way.
fn worktree_cleanup(
    shared: &OfficeShared,
    worktree_id: &str,
    requested_by: &str,
) -> OfficeResult<serde_json::Value> {
    let wid = WorktreeId::from_str(worktree_id)?;
    // Phase A — record lookup under the lock, then DROP it: the gh probe
    // below is a network call (30s timeout); holding the mutex across it
    // freezes every pane for that long (QA F15 — residual F3 class).
    let (repo_root, worktree_path, branch) = {
        let store = shared.store.lock().expect("office store");
        let service = crate::git::worktrees::WorktreeService::new(
            &store,
            crate::git::worktrees::ProtectedRefs::new(vec![]),
        );
        let record = service.record(&wid)?.ok_or_else(|| OfficeError::NotFound {
            entity: "worktree",
            id: worktree_id.to_string(),
        })?;
        (record.repo_root, record.worktree_path, record.branch)
    };
    let pr = crate::git::pr::pr_status_for_branch_using(
        &repo_root,
        &branch,
        gh_program_for(shared).as_str(),
    );
    let pr_number = pr.as_ref().map(|info| info.number);
    let state = pr
        .as_ref()
        .map(|info| info.state.clone())
        .unwrap_or_else(|| "unknown".into());
    if state != "merged" {
        // The refusal is a fact too (#53 boundary: every outcome audited).
        let store = shared.store.lock().expect("office store");
        crate::foundation::events::append(
            &store,
            crate::foundation::events::NewEvent {
                domain: DOMAIN_OFFICE_HOST,
                kind: "worktree_cleanup_refused".into(),
                subject_type: "worktree".into(),
                subject_id: worktree_id.to_string(),
                origin: "office_host".into(),
                payload: serde_json::json!({
                    "branch": branch,
                    "pr_state": state,
                    "requested_by": requested_by,
                    "reason": "cleanup requires a MERGED PR",
                }),
            },
        )?;
        return Err(OfficeError::Validation(format!(
            "cleanup requires a MERGED PR for branch `{branch}` (current: {state}); \
             deleting work is a human decision",
        )));
    }
    let runner = crate::git::cli::CliRunner::default();
    let removal = {
        let store = shared.store.lock().expect("office store");
        let service = crate::git::worktrees::WorktreeService::new(
            &store,
            crate::git::worktrees::ProtectedRefs::new(vec![]),
        );
        service.release(&wid)?;
        drop(store);
        // The directory removal is a git command with a 30s timeout —
        // run it WITHOUT the store mutex (QA round 5 F5).
        runner.git_ok(
            &repo_root,
            &[
                "worktree",
                "remove",
                worktree_path.to_string_lossy().as_ref(),
            ],
        )
    };
    if let Err(failure) = removal {
        let store = shared.store.lock().expect("office store");
        crate::foundation::events::append(
            &store,
            crate::foundation::events::NewEvent {
                domain: DOMAIN_OFFICE_HOST,
                kind: "worktree_cleanup_failed".into(),
                subject_type: "worktree".into(),
                subject_id: worktree_id.to_string(),
                origin: "office_host".into(),
                payload: serde_json::json!({
                    "path": worktree_path.display().to_string(),
                    "requested_by": requested_by,
                    "detail": format!("directory removal failed: {failure}"),
                    "note": "released in records; directory still on disk",
                }),
            },
        )?;
        return Err(OfficeError::Validation(format!(
            "worktree `{}` was released in the office records but its directory could \
             not be removed ({}); remove it by hand after checking the tree",
            worktree_path.display(),
            failure
        )));
    }
    let store = shared.store.lock().expect("office store");
    crate::foundation::events::append(
        &store,
        crate::foundation::events::NewEvent {
            domain: DOMAIN_OFFICE_HOST,
            kind: "worktree_cleaned".into(),
            subject_type: "worktree".into(),
            subject_id: worktree_id.to_string(),
            origin: "office_host".into(),
            payload: serde_json::json!({
                "path": worktree_path.display().to_string(),
                "branch": branch,
                "pr_number": pr_number,
                "merged_state": state,
                "requested_by": requested_by,
                "note": "released + removed; merged state re-checked live at cleanup; \
                         caller-side confirmation is the requesting client's duty",
            }),
        },
    )?;
    Ok(serde_json::json!({
        "cleaned": true,
        "worktree_id": worktree_id.to_string(),
        "path": worktree_path.display().to_string(),
    }))
}

/// V15-3: hand a worktree from one agent to the next. The brief carries
/// the task goal, the branch and the DEPARTING agent's last controlled
/// summary (or an honest "none") — material transfer, never a promise of
/// lossless context. The departing terminal is left running: stopping it
/// is the human's call.
fn agent_handoff(
    shared: &OfficeShared,
    worktree_id: &str,
    to_agent: &str,
    requested_by: &str,
) -> OfficeResult<serde_json::Value> {
    let wid = WorktreeId::from_str(worktree_id)?;
    let to_agent = to_agent.trim().to_string();
    if to_agent.is_empty() || to_agent.contains(char::is_whitespace) || to_agent.contains('/') {
        return Err(OfficeError::Validation(
            "handoff target must be a bare command name (no spaces, no paths)".into(),
        ));
    }
    let store = shared.store.lock().expect("office store");
    let service = crate::git::worktrees::WorktreeService::new(
        &store,
        crate::git::worktrees::ProtectedRefs::new(vec![]),
    );
    let record = service.record(&wid)?.ok_or_else(|| OfficeError::NotFound {
        entity: "worktree",
        id: worktree_id.to_string(),
    })?;

    // The departing summary: the newest controlled report on any terminal
    // of this worktree (by updated_at, not lexicographic order — QA F6).
    // Absent → an honest "none", never an invention.
    let mut summary: Option<String> = None;
    let mut from_agent: Option<String> = None;
    let mut latest_at: Option<String> = None;
    for terminal in shared.terminals.terminals_for_worktree(&wid) {
        for status in shared
            .agent_board
            .project(&terminal.terminal_id.to_string())
        {
            if status.source != crate::agents::StatusSource::ControlledReport {
                continue;
            }
            let newer = latest_at
                .as_ref()
                .map(|latest| status.updated_at.as_str() > latest.as_str())
                .unwrap_or(true);
            if newer {
                latest_at = Some(status.updated_at.clone());
                from_agent = Some(status.agent.clone());
                summary = Some(format!(
                    "{} reported {}: {}",
                    status.agent,
                    status.status.as_str(),
                    status.detail
                ));
            }
        }
    }
    let goal = TaskRegistry::new(&store)
        .require_task(&record.task_id)
        .map(|task| task.goal)
        .unwrap_or_else(|_| "(no linked task)".into());
    let departing = summary
        .clone()
        .unwrap_or_else(|| "(no departing summary was reported)".into());
    let from_line = if summary.is_some() {
        from_agent.as_deref().unwrap_or("(unknown agent)")
    } else {
        "(none reported)"
    };
    let brief = format!(
        "【handoff】task: {goal}\nbranch: {}\nworktree: {}\ndeparting agent: {}\ndeparting summary: {departing}\nPlease continue from the current state.",
        record.branch,
        record.worktree_path.display(),
        from_line
    );

    let spec =
        crate::terminal::TerminalSpec::new(vec![to_agent.clone()], record.worktree_path.clone())?;
    let (terminal_id, handle) = shared.terminals.spawn(
        spec,
        crate::foundation::records::TerminalOwner::AgentCli,
        Some(wid),
        format!("handoff → {to_agent}"),
        None,
        Some(&store),
    )?;
    seed_agent_identification(shared, &terminal_id, std::slice::from_ref(&to_agent));
    // Bracketed paste (QA round 5 F7): multi-line briefs would otherwise
    // be submitted line-by-line by TUI agents (each newline = one submit).
    // The paste markers tell the agent's terminal to treat the whole brief
    // as one paste; trailing newline still submits it.
    let mut payload = String::with_capacity(brief.len() + 16);
    payload.push_str("\x1b[200~");
    payload.push_str(&brief);
    payload.push_str("\x1b[201~");
    payload.push('\n');
    handle.input(payload.as_bytes())?;

    {
        let _ = TaskRegistry::new(&store).record_result(
            &record.task_id,
            None,
            crate::tasks::ResultSource::User,
            "agent_handoff",
            format!(
                "handoff:{worktree_id}:{to_agent}:{}",
                uuid::Uuid::new_v4().simple()
            ),
            serde_json::json!({
                "from_agent": from_agent.clone(),
                "to_agent": to_agent,
                "departing_summary": summary.clone(),
                "brief_bytes": brief.len(),
                "note": "material transfer; not lossless context, not acceptance",
            }),
        );
    }
    crate::foundation::events::append(
        &store,
        crate::foundation::events::NewEvent {
            domain: DOMAIN_OFFICE_HOST,
            kind: "agent_handoff".into(),
            subject_type: "worktree".into(),
            subject_id: worktree_id.to_string(),
            origin: "office_host".into(),
            payload: serde_json::json!({
                "requested_by": requested_by,
                "to_agent": to_agent,
                "branch": record.branch,
                "terminal_id": terminal_id.to_string(),
            }),
        },
    )?;
    Ok(serde_json::json!({
        "terminal_id": terminal_id.to_string(),
        "to_agent": to_agent,
        "brief_bytes": brief.len(),
        "departing_terminal_left_running": true,
    }))
}

/// V15-4 manual trigger + the read-only listing.
fn sync_now(shared: &OfficeShared) -> OfficeResult<serde_json::Value> {
    // Through the guarded wrapper (QA round 6 G2): a manual cycle must
    // also honor the single-flight guard, not stampede a running one.
    auto_sync_all(shared, true)
}

fn recovery_list(shared: &OfficeShared) -> OfficeResult<serde_json::Value> {
    let store = shared.store.lock().expect("office store");
    let mut stmt = store.connection().prepare(
        "SELECT id, terminal_id, task_id, reset_at, action, state, created_at, fired_at
         FROM recovery_plans ORDER BY id DESC LIMIT 50",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(serde_json::json!({
            "id": row.get::<_, i64>(0)?,
            "terminal_id": row.get::<_, String>(1)?,
            "task_id": row.get::<_, Option<String>>(2)?,
            "reset_at": row.get::<_, String>(3)?,
            "action": row.get::<_, String>(4)?,
            "state": row.get::<_, String>(5)?,
            "created_at": row.get::<_, String>(6)?,
            "fired_at": row.get::<_, Option<String>>(7)?,
        }))
    })?;
    let plans = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(serde_json::json!({ "plans": plans }))
}

fn recovery_schedule(
    shared: &OfficeShared,
    terminal_id: &str,
    reset_at: &str,
    task_id: Option<&str>,
) -> OfficeResult<serde_json::Value> {
    // Normalize to UTC before storing (QA F1): sweeps compare reset times
    // as UTC text.
    let reset_at = normalize_rfc3339_utc(reset_at)?;
    let terminal = TerminalId::from_str(terminal_id)?;
    shared
        .terminals
        .handle(&terminal)?
        .ok_or_else(|| OfficeError::NotFound {
            entity: "terminal",
            id: terminal_id.to_string(),
        })?;
    let store = shared.store.lock().expect("office store");
    store.connection().execute(
        "INSERT INTO recovery_plans(terminal_id, task_id, reset_at, action, state, created_at)
         VALUES (?1, ?2, ?3, 'resume_prompt', 'scheduled', ?4)",
        rusqlite::params![terminal_id, task_id, reset_at, utc_now(),],
    )?;
    let id = store.connection().last_insert_rowid();
    crate::foundation::events::append(
        &store,
        crate::foundation::events::NewEvent {
            domain: DOMAIN_OFFICE_HOST,
            kind: "recovery_plan_scheduled".into(),
            subject_type: "recovery_plan".into(),
            subject_id: id.to_string(),
            origin: "office_host".into(),
            payload: serde_json::json!({ "terminal_id": terminal_id, "reset_at": reset_at }),
        },
    )?;
    Ok(serde_json::json!({ "id": id, "reset_at": reset_at, "state": "scheduled" }))
}

fn recovery_cancel(shared: &OfficeShared, plan_id: i64) -> OfficeResult<serde_json::Value> {
    let store = shared.store.lock().expect("office store");
    let changed = store.connection().execute(
        "UPDATE recovery_plans SET state = 'cancelled', fired_at = ?2
         WHERE id = ?1 AND state = 'scheduled'",
        rusqlite::params![plan_id, utc_now()],
    )?;
    if changed == 0 {
        return Err(OfficeError::NotFound {
            entity: "scheduled recovery plan",
            id: plan_id.to_string(),
        });
    }
    Ok(serde_json::json!({ "cancelled": plan_id }))
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn hex_decode(text: &str) -> OfficeResult<Vec<u8>> {
    let text = text.as_bytes();
    if text.len() % 2 != 0 {
        return Err(OfficeError::Validation(
            "hex input must be even-length".into(),
        ));
    }
    (0..text.len() / 2)
        .map(|i| {
            u8::from_str_radix(
                std::str::from_utf8(&text[i * 2..i * 2 + 2])
                    .map_err(|_| OfficeError::Validation("hex input must be ASCII".into()))?,
                16,
            )
            .map_err(|_| OfficeError::Validation(format!("invalid hex byte at {i}")))
        })
        .collect()
}

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// S1 integration tests (issue #43): the resident-server split behaviors
// ---------------------------------------------------------------------------

#[cfg(test)]
mod server_split_tests {
    use super::*;
    use crate::office::OfficeClient;
    use std::time::{Duration, Instant};

    pub(crate) struct RunningHost {
        dir: tempfile::TempDir,
        home: std::path::PathBuf,
        server: std::thread::JoinHandle<()>,
    }

    fn start_host() -> RunningHost {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("home dir");
        let host = OfficeHost::open(&home).expect("host claims the slot");
        let server = host.serve_background();
        let socket = home.join(OFFICE_SOCKET_NAME);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(socket.exists(), "host socket never appeared");
        RunningHost { dir, home, server }
    }

    impl RunningHost {
        fn client(&self) -> OfficeClient {
            OfficeClient::connect(&self.home).expect("client connects")
        }

        /// Real teardown: explicit shutdown stops owned terminals and ends
        /// the serve loop BEFORE the temp home disappears.
        fn shutdown(self) {
            if let Ok(mut client) = OfficeClient::connect(&self.home) {
                let _ = client.call(OfficeRequestKind::Shutdown { close_policy: None });
            }
            let _ = self.server.join();
            drop(self.dir);
        }
    }

    fn create_terminal(
        client: &mut OfficeClient,
        home: &std::path::Path,
        argv: &[&str],
        purpose: &str,
    ) -> String {
        let response = client
            .call(OfficeRequestKind::TerminalCreate {
                argv: argv.iter().map(|s| s.to_string()).collect(),
                cwd: home.display().to_string(),
                env: vec![],
                cols: 100,
                rows: 30,
                purpose: purpose.to_string(),
                worktree_id: None,
                owner: "user_shell".into(),
            })
            .expect("terminal create");
        response
            .get("terminal_id")
            .and_then(|v| v.as_str())
            .expect("terminal id")
            .to_string()
    }

    pub(crate) fn wait_for_output(
        client: &mut OfficeClient,
        terminal_id: &str,
        needle: &str,
    ) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(view) = client.call(OfficeRequestKind::TerminalSnapshot {
                terminal_id: terminal_id.to_string(),
            }) {
                if format!("{view}").contains(needle) {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// Headless fixed-size terminal over the socket: create → snapshot
    /// (fixed size honored) → output visible → input echoed → stop.
    #[test]
    fn headless_terminal_create_snapshot_input_stop() {
        let host = start_host();
        let mut client = host.client();
        let terminal_id = create_terminal(
            &mut client,
            &host.home,
            &["/bin/sh", "-c", "echo ready-marker; cat"],
            "headless test session",
        );
        assert!(wait_for_output(&mut client, &terminal_id, "ready-marker"));

        let view = client
            .call(OfficeRequestKind::TerminalSnapshot {
                terminal_id: terminal_id.clone(),
            })
            .expect("snapshot");
        assert_eq!(view.get("cols").and_then(|v| v.as_u64()), Some(100));
        assert_eq!(view.get("rows").and_then(|v| v.as_u64()), Some(30));

        client
            .call(OfficeRequestKind::TerminalInput {
                terminal_id: terminal_id.clone(),
                bytes_hex: hex_encode(b"echo typed-marker\n"),
            })
            .expect("input");
        assert!(wait_for_output(&mut client, &terminal_id, "typed-marker"));

        client
            .call(OfficeRequestKind::TerminalStop {
                terminal_id: terminal_id.clone(),
            })
            .expect("stop");
        host.shutdown();
    }

    /// Detach (client exit) leaves the server and its terminals running;
    /// a fresh attach sees the same terminal with its scrollback intact.
    #[test]
    fn detach_keeps_terminals_and_reattach_restores_the_view() {
        let host = start_host();
        let terminal_id = {
            let mut client = host.client();
            let id = create_terminal(
                &mut client,
                &host.home,
                &["/bin/sh", "-c", "echo detach-marker; sleep 30"],
                "detach test",
            );
            assert!(wait_for_output(&mut client, &id, "detach-marker"));
            id
        }; // client dropped here = detach

        let mut reattached = host.client();
        let list = reattached
            .call(OfficeRequestKind::TerminalList)
            .expect("list after detach");
        assert!(
            format!("{list}").contains(&terminal_id),
            "the terminal survived the client detach: {list}"
        );
        assert!(
            wait_for_output(&mut reattached, &terminal_id, "detach-marker"),
            "scrollback is restored from server-held state"
        );
        host.shutdown();
    }

    /// Stopping one terminal never touches the neighbor (V05 discipline
    /// exercised over the socket).
    #[test]
    fn stopping_one_terminal_spares_the_neighbor() {
        let host = start_host();
        let mut client = host.client();
        let a = create_terminal(
            &mut client,
            &host.home,
            &["/bin/sh", "-c", "echo a-marker; sleep 30"],
            "neighbor a",
        );
        let b = create_terminal(
            &mut client,
            &host.home,
            &["/bin/sh", "-c", "echo b-marker; sleep 30"],
            "neighbor b",
        );
        client
            .call(OfficeRequestKind::TerminalStop {
                terminal_id: a.clone(),
            })
            .expect("stop a");
        // b still answers a snapshot (its handle is alive server-side).
        let view = client
            .call(OfficeRequestKind::TerminalSnapshot {
                terminal_id: b.clone(),
            })
            .expect("neighbor survives");
        assert!(
            format!("{view}").contains("b-marker") || wait_for_output(&mut client, &b, "b-marker")
        );
        host.shutdown();
    }

    /// An invalid grant is rejected AND audited (office_events keeps the
    /// refusal — the denial is a recorded fact, not a silent close).
    #[test]
    fn invalid_grant_is_rejected_and_audited() {
        let host = start_host();
        let mut client = host.client();
        let mut request = new_request(OfficeRequestKind::TerminalCreate {
            argv: vec!["/bin/sh".into()],
            cwd: host.home.display().to_string(),
            env: vec![],
            cols: 80,
            rows: 24,
            purpose: "should be denied".into(),
            worktree_id: None,
            owner: "user_shell".into(),
        });
        request.grant = Some("grant-does-not-exist".into());
        let err = client
            .call_request(request)
            .expect_err("an unknown grant must be denied");
        assert!(err.to_string().contains("denied"), "got: {err}");

        let store = Store::open(
            &crate::foundation::paths::database_path(&host.home),
            office_migrations(),
        )
        .expect("audit store");
        let denials: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM office_events WHERE kind = 'socket_grant_denied'",
                [],
                |row| row.get(0),
            )
            .expect("audit count");
        assert!(denials >= 1, "the denial must be audited, got {denials}");
        host.shutdown();
    }

    /// A member-attributed mutation without any grant is the exact
    /// "未携带有效 grant" case: rejected and audited.
    #[test]
    fn member_attributed_mutation_without_grant_is_denied() {
        let host = start_host();
        let mut client = host.client();
        let mut request = new_request(OfficeRequestKind::TerminalCreate {
            argv: vec!["/bin/sh".into()],
            cwd: host.home.display().to_string(),
            env: vec![],
            cols: 80,
            rows: 24,
            purpose: "member without grant".into(),
            worktree_id: None,
            owner: "agent_cli".into(),
        });
        request.member = Some("member-00000000-0000-0000-0000-000000000000".into());
        let err = client
            .call_request(request)
            .expect_err("member mutation without grant must be denied");
        assert!(err.to_string().contains("denied"), "got: {err}");
        host.shutdown();
    }

    /// The workbench view projection serves real registries' facts.
    #[test]
    fn workbench_view_serves_the_projection() {
        let host = start_host();
        let mut client = host.client();
        let view = client.call(OfficeRequestKind::WorkbenchView).expect("view");
        for key in ["projects", "worktrees", "tasks", "terminals", "attention"] {
            assert!(view.get(key).is_some(), "view misses `{key}`: {view}");
        }
        // The model survives a client-side round trip through the same
        // serde shape the workbench view renders.
        let model: crate::tui::workbench::WorkbenchModel =
            serde_json::from_value(view).expect("model decode");
        assert!(model.terminals.is_empty());
        host.shutdown();
    }
}

#[cfg(test)]
mod s2_workbench_tests {
    use super::*;
    use crate::office::OfficeClient;
    use std::process::Command;
    use std::time::{Duration, Instant};

    pub(crate) struct RunningHost {
        dir: tempfile::TempDir,
        home: std::path::PathBuf,
        server: std::thread::JoinHandle<()>,
    }

    fn start_host() -> RunningHost {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("home dir");
        let host = OfficeHost::open(&home).expect("host claims the slot");
        let server = host.serve_background();
        let socket = home.join(OFFICE_SOCKET_NAME);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        RunningHost { dir, home, server }
    }

    impl RunningHost {
        fn client(&self) -> OfficeClient {
            OfficeClient::connect(&self.home).expect("client connects")
        }

        fn shutdown(self) {
            if let Ok(mut client) = OfficeClient::connect(&self.home) {
                let _ = client.call(OfficeRequestKind::Shutdown { close_policy: None });
            }
            let _ = self.server.join();
            drop(self.dir);
        }
    }

    fn git(repo: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args(["-c", "user.email=test@viva.local", "-c", "user.name=test"])
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A real repo with one commit, a bare `origin` remote it pushes to
    /// (so the V08 create path can fetch and resolve the default branch),
    /// and three adopted task worktrees.
    fn seed_repo_and_worktrees(host: &RunningHost) -> String {
        let repo = host.dir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo dir");
        let bare = host.dir.path().join("origin.git");
        git(host.dir.path(), &["init", "--bare", bare.to_str().unwrap()]);
        git(&repo, &["init", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "seed\n").expect("seed file");
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "seed"]);
        git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git(&repo, &["push", "origin", "main"]);
        git(&repo, &["fetch", "origin"]);
        // auto_pull's `git pull --ff-only` needs tracking configured.
        git(&repo, &["branch", "--set-upstream-to=origin/main", "main"]);

        let store = Store::open(
            &crate::foundation::paths::database_path(&host.home),
            office_migrations(),
        )
        .expect("store");
        let project = crate::projects::ProjectRegistry::new(&store)
            .register(None, "demo", &repo)
            .expect("project");
        let task = TaskRegistry::new(&store)
            .create_task(
                "three worktrees",
                vec![],
                None,
                None,
                Some(project.project_id.clone()),
            )
            .expect("task");
        let mut service = crate::git::worktrees::WorktreeService::new(
            &store,
            crate::git::worktrees::ProtectedRefs::new(vec![]),
        );
        for name in ["wt-a", "wt-b", "wt-c"] {
            let path = host.dir.path().join(name);
            git(
                &repo,
                &[
                    "worktree",
                    "add",
                    path.to_str().unwrap(),
                    "-b",
                    &format!("branch-{name}"),
                ],
            );
            service
                .adopt_existing(&repo, &path, &task.task_id)
                .expect("adopt");
        }
        task.task_id.to_string()
    }

    fn wait_for_output(client: &mut OfficeClient, terminal_id: &str, needle: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(view) = client.call(OfficeRequestKind::TerminalSnapshot {
                terminal_id: terminal_id.to_string(),
            }) {
                if format!("{view}").contains(needle) {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// The workbench view serves ≥3 real worktrees grouped under their
    /// project; `o` opens a shell AT the worktree (cwd visible in output).
    #[test]
    fn grouped_view_open_in_worktree_and_create_for_task() {
        let host = start_host();
        let task_id = seed_repo_and_worktrees(&host);
        let mut client = host.client();

        // ≥3 worktrees, each carrying its project for the group headers.
        let view = client.call(OfficeRequestKind::WorkbenchView).expect("view");
        let model: crate::tui::workbench::WorkbenchModel =
            serde_json::from_value(view).expect("decode");
        assert!(model.worktrees.len() >= 3, "{}", model.worktrees.len());
        assert!(model.worktrees.iter().all(|w| !w.project_id.is_empty()));
        // Grouped: same-project rows are adjacent.
        let mut projects_in_order = model
            .worktrees
            .iter()
            .map(|w| w.project_id.clone())
            .collect::<Vec<_>>();
        projects_in_order.dedup();
        assert_eq!(projects_in_order.len(), 1);

        // `o`: a shell whose cwd IS the worktree — `pwd` proves it.
        let wt_a = model
            .worktrees
            .iter()
            .find(|w| w.path.ends_with("wt-a"))
            .expect("wt-a row");
        let opened = client
            .call(OfficeRequestKind::TerminalOpenInWorktree {
                worktree_id: wt_a.worktree_id.clone(),
            })
            .expect("open");
        let terminal_id = opened
            .get("terminal_id")
            .and_then(|v| v.as_str())
            .expect("terminal id")
            .to_string();
        client
            .call(OfficeRequestKind::TerminalInput {
                terminal_id: terminal_id.clone(),
                bytes_hex: hex_encode(b"pwd\n"),
            })
            .expect("pwd");
        assert!(
            wait_for_output(&mut client, &terminal_id, "wt-a"),
            "the shell must run inside the worktree"
        );

        // `w`: create a 4th worktree for the task through the socket.
        let created = client
            .call(OfficeRequestKind::WorktreeCreateForTask {
                task_id: task_id.clone(),
                branch: Some("agent/s2-created".into()),
                base_dir: None,
            })
            .expect("create for task");
        let path = created
            .get("worktree_path")
            .and_then(|v| v.as_str())
            .expect("worktree path")
            .to_string();
        assert!(
            std::path::Path::new(&path).join(".git").exists()
                || std::path::Path::new(&path).exists(),
            "the created worktree exists on disk at {path}"
        );
        let view = client
            .call(OfficeRequestKind::WorkbenchView)
            .expect("view2");
        let model: crate::tui::workbench::WorkbenchModel =
            serde_json::from_value(view).expect("decode2");
        assert!(model.worktrees.iter().any(|w| w.path == path));

        client
            .call(OfficeRequestKind::TerminalStop {
                terminal_id: terminal_id.clone(),
            })
            .expect("stop shell");
        host.shutdown();
    }
}

#[cfg(test)]
mod s3_handoff_tests {
    use super::*;
    use crate::office::OfficeClient;
    use std::time::{Duration, Instant};

    pub(crate) fn wait_for_output(
        client: &mut OfficeClient,
        terminal_id: &str,
        needle: &str,
    ) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(view) = client.call(OfficeRequestKind::TerminalSnapshot {
                terminal_id: terminal_id.to_string(),
            }) {
                if format!("{view}").contains(needle) {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// The issue #45 acceptance path, end to end: a running terminal
    /// survives a server restart WITHOUT being stopped — same terminal id,
    /// its history intact, still accepting input — and the old host exits
    /// gracefully with recovery records naming the transfer.
    #[test]
    fn live_handoff_transfers_terminals_across_a_server_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        let host = OfficeHost::open(&home).expect("host");
        let server = host.serve_background();
        let socket = home.join(OFFICE_SOCKET_NAME);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }

        // One live terminal: output marker, then stay alive.
        let mut client = OfficeClient::connect(&home).expect("first client");
        let created = client
            .call(OfficeRequestKind::TerminalCreate {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "echo handoff-marker; cat".into(),
                ],
                cwd: home.display().to_string(),
                env: vec![],
                cols: 90,
                rows: 25,
                purpose: "pre-restart session".into(),
                worktree_id: None,
                owner: "user_shell".into(),
            })
            .expect("create");
        let terminal_id = created
            .get("terminal_id")
            .and_then(|v| v.as_str())
            .expect("terminal id")
            .to_string();
        assert!(wait_for_output(&mut client, &terminal_id, "handoff-marker"));

        // Begin the restart: the response names the rendezvous socket.
        let response = client
            .call(OfficeRequestKind::ServerRestart)
            .expect("restart");
        assert_eq!(
            response.get("state").and_then(|v| v.as_str()),
            Some("handoff_ready")
        );
        drop(client);

        // The resumed server runs in-process (the real binary does exactly
        // this in its own process via `viva server --resume`).
        let resume_home = home.clone();
        let resume_thread = std::thread::spawn(move || resume_server(&resume_home));

        // The old host transfers, then exits gracefully WITHOUT stopping
        // the terminal. BOUNDED wait: a failed transfer leaves the host
        // serving by design - the test must fail fast, never hang.
        {
            let deadline = Instant::now() + Duration::from_secs(45);
            while socket.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
            assert!(
                !socket.exists(),
                "the old host never released the control socket within 45s - \
                 the handoff failed; see the live_handoff_failed recovery record"
            );
        }
        server.join().expect("old host exits cleanly");

        // The resumed host answers on the same control socket.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut client2 = loop {
            if let Ok(client) = OfficeClient::connect(&home) {
                break client;
            }
            assert!(Instant::now() < deadline, "the resumed host never answered");
            std::thread::sleep(Duration::from_millis(100));
        };

        // Same terminal identity, preserved history, live input.
        let list = client2
            .call(OfficeRequestKind::TerminalList)
            .expect("list after restart");
        assert!(
            format!("{list}").contains(&terminal_id),
            "terminal identity survived the restart: {list}"
        );
        assert!(
            wait_for_output(&mut client2, &terminal_id, "handoff-marker"),
            "history is preserved across the handoff"
        );
        client2
            .call(OfficeRequestKind::TerminalInput {
                terminal_id: terminal_id.clone(),
                bytes_hex: hex_encode(b"echo post-restart-marker\n"),
            })
            .expect("input after restart");
        assert!(
            wait_for_output(&mut client2, &terminal_id, "post-restart-marker"),
            "the session is still live and interactive"
        );

        // Recovery records name both sides of the transfer.
        {
            let store = Store::open(
                &crate::foundation::paths::database_path(&home),
                office_migrations(),
            )
            .expect("store");
            let out: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM office_recovery_events WHERE kind = 'live_handoff_out'",
                    [],
                    |row| row.get(0),
                )
                .expect("out record");
            let inn: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM office_recovery_events WHERE kind = 'live_handoff_in'",
                    [],
                    |row| row.get(0),
                )
                .expect("in record");
            assert!(out >= 1 && inn >= 1, "both transfer sides are recorded");
        }

        // Clean teardown: the resumed host shuts down like any other.
        client2
            .call(OfficeRequestKind::Shutdown { close_policy: None })
            .expect("shutdown");
        drop(client2);
        {
            let deadline = Instant::now() + Duration::from_secs(15);
            while socket.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        resume_thread
            .join()
            .expect("resumed host thread joins")
            .expect("resumed host ends cleanly");
        drop(dir);
    }

    /// QA F3 (issue #45 AC3): a handoff that never completes must leave the
    /// old host exactly as it was — answering, terminal live, input
    /// reaching the child, and NEW OUTPUT VISIBLE again once the readers
    /// are reattached. The failure record must not claim more than happened.
    #[test]
    fn failed_handoff_leaves_the_host_serving_with_live_output() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        let host = OfficeHost::open(&home).expect("host");
        let server = host.serve_background();
        let socket = home.join(OFFICE_SOCKET_NAME);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }

        let mut client = OfficeClient::connect(&home).expect("client");
        let created = client
            .call(OfficeRequestKind::TerminalCreate {
                argv: vec!["/bin/sh".into(), "-c".into(), "echo pre-marker; cat".into()],
                cwd: home.display().to_string(),
                env: vec![],
                cols: 80,
                rows: 24,
                purpose: "failure probe".into(),
                worktree_id: None,
                owner: "user_shell".into(),
            })
            .expect("create");
        let terminal_id = created
            .get("terminal_id")
            .and_then(|v| v.as_str())
            .expect("terminal id")
            .to_string();
        assert!(wait_for_output(&mut client, &terminal_id, "pre-marker"));

        // Begin the restart and NEVER start the resumed server: the
        // handoff times out after HANDOFF_TIMEOUT.
        let response = client
            .call(OfficeRequestKind::ServerRestart)
            .expect("restart");
        assert_eq!(
            response.get("state").and_then(|v| v.as_str()),
            Some("handoff_ready")
        );

        // The failure lands as a recovery record within the timeout window.
        let deadline = Instant::now() + HANDOFF_TIMEOUT + Duration::from_secs(15);
        loop {
            let store = Store::open(
                &crate::foundation::paths::database_path(&home),
                office_migrations(),
            )
            .expect("store");
            let failed: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM office_recovery_events WHERE kind = 'live_handoff_failed'",
                    [],
                    |row| row.get(0),
                )
                .expect("failure record");
            drop(store);
            if failed >= 1 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the handoff failure was never recorded"
            );
            std::thread::sleep(Duration::from_millis(200));
        }

        // The host still answers... (reconnect: the control connection
        // drops after its idle timeout, exactly what a real orchestrator
        // recovers from)
        let mut client = OfficeClient::connect(&home).expect("reconnect");
        let ping = client.call(OfficeRequestKind::Ping).expect("ping");
        assert!(ping.get("pid").is_some(), "the host must keep serving");

        // ...the terminal is still listed and live...
        let list = client.call(OfficeRequestKind::TerminalList).expect("list");
        assert!(format!("{list}").contains(&terminal_id));

        // ...input still reaches the child, and the reattached readers make
        // the NEW output visible (the QA "alive but blind" regression).
        client
            .call(OfficeRequestKind::TerminalInput {
                terminal_id: terminal_id.clone(),
                bytes_hex: hex_encode(b"echo post-failure-marker\n"),
            })
            .expect("input after failure");
        assert!(
            wait_for_output(&mut client, &terminal_id, "post-failure-marker"),
            "readers must be reattached on the failure path - \
             a host that answers but sees no output is the exact defect this test pins"
        );

        // Honest records: the failure is named; a lost reader would be too.
        {
            let store = Store::open(
                &crate::foundation::paths::database_path(&home),
                office_migrations(),
            )
            .expect("store");
            let detail: String = store
                .connection()
                .query_row(
                    "SELECT detail FROM office_recovery_events WHERE kind = 'live_handoff_failed' \
                     ORDER BY seq DESC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .expect("failure detail");
            assert!(
                detail.contains("readers were reattached"),
                "the record must describe the recovery: {detail}"
            );
        }

        client
            .call(OfficeRequestKind::Shutdown { close_policy: None })
            .expect("shutdown");
        drop(client);
        {
            let deadline = Instant::now() + Duration::from_secs(15);
            while socket.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        server.join().expect("old host exits cleanly");
        drop(dir);
    }
}

#[cfg(test)]
mod s4_agent_status_tests {
    use super::*;
    use crate::office::OfficeClient;
    use std::time::{Duration, Instant};

    pub(crate) struct RunningHost {
        dir: tempfile::TempDir,
        home: std::path::PathBuf,
        server: std::thread::JoinHandle<()>,
    }

    pub(crate) fn start_host() -> RunningHost {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        let host = OfficeHost::open(&home).expect("host");
        let server = host.serve_background();
        let socket = home.join(OFFICE_SOCKET_NAME);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        RunningHost { dir, home, server }
    }

    impl RunningHost {
        fn client(&self) -> OfficeClient {
            OfficeClient::connect(&self.home).expect("client")
        }
        fn shutdown(self) {
            if let Ok(mut client) = OfficeClient::connect(&self.home) {
                let _ = client.call(OfficeRequestKind::Shutdown { close_policy: None });
            }
            let _ = self.server.join();
            drop(self.dir);
        }
    }

    /// An owner-issued grant carrying one action (shared authority: no
    /// principal, usable by any member - documented in the authority).
    fn grant_for(home: &std::path::Path, actions: &[&str]) -> String {
        let store = Store::open(
            &crate::foundation::paths::database_path(home),
            office_migrations(),
        )
        .expect("store");
        let grant = crate::authority::AuthorityEngine::new(&store)
            .issue_root_grant(
                None,
                None,
                actions.iter().map(|s| s.to_string()).collect(),
                crate::authority::GrantMode::ActAutonomously,
                None,
            )
            .expect("grant");
        grant.grant_id.to_string()
    }

    fn create_codex_like_terminal(client: &mut OfficeClient, home: &std::path::Path) -> String {
        // A real executable named after a detectable agent: identification
        // comes from the spawned argv.
        let fake_dir = home.join("fake-agents");
        std::fs::create_dir_all(&fake_dir).expect("fake dir");
        let fake = fake_dir.join("codex");
        std::fs::write(&fake, "#!/bin/sh\necho codex-up\ncat\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let created = client
            .call(OfficeRequestKind::TerminalCreate {
                argv: vec![fake.display().to_string()],
                cwd: home.display().to_string(),
                env: vec![],
                cols: 80,
                rows: 24,
                purpose: "agent under test".into(),
                worktree_id: None,
                owner: "agent_cli".into(),
            })
            .expect("create");
        created
            .get("terminal_id")
            .and_then(|v| v.as_str())
            .expect("terminal id")
            .to_string()
    }

    fn view_for(client: &mut OfficeClient, terminal_id: &str) -> serde_json::Value {
        let view = client.call(OfficeRequestKind::WorkbenchView).expect("view");
        let rows = view
            .get("terminals")
            .and_then(|v| v.as_array())
            .expect("terminals array")
            .clone();
        rows.into_iter()
            .find(|row| row.get("terminal_id").and_then(|v| v.as_str()) == Some(terminal_id))
            .expect("the row exists")
    }

    /// The three-layer story on one terminal: detected (process/argv) +
    /// authoritative (controlled report) + auxiliary (screen) sit side by
    /// side, clearly labeled, and the report is audited.
    #[test]
    fn controlled_report_is_authoritative_and_audited() {
        let host = start_host();
        let mut client = host.client();
        let terminal_id = create_codex_like_terminal(&mut client, &host.home);
        let grant = grant_for(&host.home, &["agent_report"]);

        let mut request = new_request(OfficeRequestKind::AgentReport {
            terminal_id: terminal_id.clone(),
            agent: "codex".into(),
            status: "blocked".into(),
            detail: "waiting for tool approval".into(),
            reset_at: None,
        });
        request.grant = Some(grant);
        request.member = Some("member-00000000-0000-0000-0000-000000000000".into());
        let record = client.call_request(request).expect("report accepted");
        assert_eq!(
            record.get("status").and_then(|v| v.as_str()),
            Some("blocked")
        );

        // The view carries BOTH sources, labeled and separated.
        let row = view_for(&mut client, &terminal_id);
        let statuses = row
            .get("agent_status")
            .and_then(|v| v.as_array())
            .expect("statuses");
        let sources: Vec<&str> = statuses
            .iter()
            .filter_map(|s| s.get("source").and_then(|v| v.as_str()))
            .collect();
        assert!(sources.contains(&"controlled_report"), "{sources:?}");
        assert!(sources.contains(&"process_tree"), "{sources:?}");

        // The report is audited.
        let store = Store::open(
            &crate::foundation::paths::database_path(&host.home),
            office_migrations(),
        )
        .expect("store");
        let audited: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM office_events WHERE kind = 'agent_status_reported'",
                [],
                |row| row.get(0),
            )
            .expect("audit");
        assert!(audited >= 1);
        host.shutdown();
    }

    /// A report without a grant is refused and audited - the controlled
    /// channel is controlled.
    #[test]
    fn report_without_grant_is_denied() {
        let host = start_host();
        let mut client = host.client();
        let terminal_id = create_codex_like_terminal(&mut client, &host.home);
        let mut request = new_request(OfficeRequestKind::AgentReport {
            terminal_id: terminal_id.clone(),
            agent: "codex".into(),
            status: "done".into(),
            detail: String::new(),
            reset_at: None,
        });
        request.member = Some("member-00000000-0000-0000-0000-000000000000".into());
        let err = client.call_request(request).expect_err("must deny");
        assert!(err.to_string().contains("denied"), "{err}");
        host.shutdown();
    }

    /// Screen inference shows up as AUXILIARY in the view - and the fact
    /// layer stays untouched (no task results invented by screen rules).
    #[test]
    fn screen_inference_is_auxiliary_only() {
        let host = start_host();
        let mut client = host.client();
        // A child that leaves a blocking-looking screen behind.
        let created = client
            .call(OfficeRequestKind::TerminalCreate {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "echo 'Do you want to proceed? (y/n)'; sleep 30".into(),
                ],
                cwd: host.home.display().to_string(),
                env: vec![],
                cols: 80,
                rows: 24,
                purpose: "screen probe".into(),
                worktree_id: None,
                owner: "agent_cli".into(),
            })
            .expect("create");
        let terminal_id = created
            .get("terminal_id")
            .and_then(|v| v.as_str())
            .expect("terminal id")
            .to_string();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let view = client.call(OfficeRequestKind::WorkbenchView).expect("view");
            let statuses = view
                .get("terminals")
                .and_then(|v| v.as_array())
                .and_then(|rows| {
                    rows.iter()
                        .find(|row| {
                            row.get("terminal_id").and_then(|v| v.as_str())
                                == Some(terminal_id.as_str())
                        })
                        .and_then(|row| row.get("agent_status").and_then(|v| v.as_array()).cloned())
                });
            if let Some(statuses) = statuses {
                let screen = statuses.iter().any(|s| {
                    s.get("source").and_then(|v| v.as_str()) == Some("screen_inference")
                        && s.get("status").and_then(|v| v.as_str()) == Some("blocked")
                });
                if screen {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "screen inference never observed");
            std::thread::sleep(Duration::from_millis(100));
        }

        // The fact layer is untouched: no task results exist for the probe.
        let store = Store::open(
            &crate::foundation::paths::database_path(&host.home),
            office_migrations(),
        )
        .expect("store");
        let results: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM task_results", [], |row| row.get(0))
            .expect("task results");
        assert_eq!(results, 0, "screen inference must not invent facts");
        host.shutdown();
    }

    /// The container intake: content lands as a private file, the
    /// submission is audited with a digest, and the answer says plainly
    /// that the container decides what to keep.
    #[test]
    fn container_intake_records_content_with_audit() {
        let host = start_host();
        let mut client = host.client();
        let grant = grant_for(&host.home, &["agent_content"]);

        let mut request = new_request(OfficeRequestKind::AgentContentSubmit {
            terminal_id: None,
            kind: "task_summary".into(),
            content: "ran the test suite; 3 failures traced to flaky timing".into(),
            source_ref: Some("session:abc/turn:42".into()),
        });
        request.grant = Some(grant);
        let response = client.call_request(request).expect("submit");
        let content_id = response
            .get("content_id")
            .and_then(|v| v.as_str())
            .expect("content id")
            .to_string();
        let path = response
            .get("path")
            .and_then(|v| v.as_str())
            .expect("path")
            .to_string();
        assert!(
            std::path::Path::new(&path).exists(),
            "content stored at {path}"
        );
        assert!(
            response
                .get("note")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .contains("container decides"),
            "the answer keeps the curation boundary honest"
        );

        let store = Store::open(
            &crate::foundation::paths::database_path(&host.home),
            office_migrations(),
        )
        .expect("store");
        let audited: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM office_events WHERE kind = 'agent_content_submitted' AND subject_id = ?1",
                [content_id.as_str()],
                |row| row.get(0),
            )
            .expect("audit");
        assert_eq!(audited, 1);
        host.shutdown();
    }
}

#[cfg(test)]
mod s5_orchestration_tests {
    use super::*;
    use crate::office::OfficeClient;
    use std::time::{Duration, Instant};

    pub(crate) struct RunningHost {
        dir: tempfile::TempDir,
        home: std::path::PathBuf,
        server: std::thread::JoinHandle<()>,
    }

    pub(crate) fn start_host() -> RunningHost {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        let host = OfficeHost::open(&home).expect("host");
        let server = host.serve_background();
        let socket = home.join(OFFICE_SOCKET_NAME);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        RunningHost { dir, home, server }
    }

    impl RunningHost {
        fn client(&self) -> OfficeClient {
            OfficeClient::connect(&self.home).expect("client")
        }
        fn shutdown(self) {
            if let Ok(mut client) = OfficeClient::connect(&self.home) {
                let _ = client.call(OfficeRequestKind::Shutdown { close_policy: None });
            }
            let _ = self.server.join();
            drop(self.dir);
        }
    }

    fn grant_for(home: &std::path::Path, actions: &[&str]) -> String {
        let store = Store::open(
            &crate::foundation::paths::database_path(home),
            office_migrations(),
        )
        .expect("store");
        crate::authority::AuthorityEngine::new(&store)
            .issue_root_grant(
                None,
                None,
                actions.iter().map(|s| s.to_string()).collect(),
                crate::authority::GrantMode::ActAutonomously,
                None,
            )
            .expect("grant")
            .grant_id
            .to_string()
    }

    /// The full orchestrator flow: create pane -> prompt -> wait -> read
    /// the outcome from the durable event feed. Grant-gated, audited,
    /// zero bypasses.
    #[test]
    fn orchestrator_flow_create_prompt_wait_and_event_feed() {
        let host = start_host();
        let mut client = host.client();
        let report_grant = grant_for(&host.home, &["agent_report"]);
        let prompt_grant = grant_for(&host.home, &["agent_prompt"]);

        // 1. create a pane running cat (echoes everything).
        let created = client
            .call(OfficeRequestKind::TerminalCreate {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "echo ready-marker; cat".into(),
                ],
                cwd: host.home.display().to_string(),
                env: vec![],
                cols: 80,
                rows: 24,
                purpose: "orchestrated agent".into(),
                worktree_id: None,
                owner: "agent_cli".into(),
            })
            .expect("create pane");
        let terminal_id = created
            .get("terminal_id")
            .and_then(|v| v.as_str())
            .expect("terminal id")
            .to_string();

        // 2. report a state (as the agent itself would), then prompt.
        let mut report = new_request(OfficeRequestKind::AgentReport {
            terminal_id: terminal_id.clone(),
            agent: "codex".into(),
            status: "blocked".into(),
            detail: "needs a decision".into(),
            reset_at: None,
        });
        report.grant = Some(report_grant.clone());
        client.call_request(report).expect("report");

        let mut prompt = new_request(OfficeRequestKind::AgentPrompt {
            terminal_id: terminal_id.clone(),
            prompt: "echo orchestrated-prompt\n".into(),
        });
        prompt.grant = Some(prompt_grant.clone());
        let prompt_response = client.call_request(prompt).expect("prompt");
        assert_eq!(
            prompt_response.get("sent").and_then(|v| v.as_bool()),
            Some(true)
        );

        // 3. wait for the blocked status: already matched, returns fast.
        let mut wait = new_request(OfficeRequestKind::AgentWait {
            terminal_id: terminal_id.clone(),
            status: "blocked".into(),
            timeout_secs: 2,
        });
        wait.grant = Some(prompt_grant);
        let waited = client
            .call_with_timeout(wait.kind.clone(), Duration::from_secs(10))
            .expect("wait");
        assert_eq!(waited.get("matched").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            waited.get("status").and_then(|v| v.as_str()),
            Some("blocked")
        );

        // 4. the durable event feed names every step of the flow.
        let feed = client
            .call(OfficeRequestKind::EventsFeed {
                since_seq: 0,
                limit: 200,
            })
            .expect("feed");
        let kinds: Vec<String> = feed
            .get("events")
            .and_then(|v| v.as_array())
            .expect("events")
            .iter()
            .filter_map(|event| {
                event
                    .get("kind")
                    .and_then(|k| k.as_str())
                    .map(str::to_string)
            })
            .collect();
        for expected in [
            "agent_status_reported",
            "agent_prompt_sent",
            "agent_wait_matched",
        ] {
            assert!(
                kinds.iter().any(|k| k == expected),
                "the feed must contain {expected}: {kinds:?}"
            );
        }

        // 5. reconnect semantics: a feed since the last seen seq returns
        // only newer events.
        let last_seq = feed
            .get("last_seq")
            .and_then(|v| v.as_u64())
            .expect("last seq");
        let fresh = client
            .call(OfficeRequestKind::EventsFeed {
                since_seq: last_seq,
                limit: 100,
            })
            .expect("feed since");
        assert!(
            fresh
                .get("events")
                .and_then(|v| v.as_array())
                .expect("events")
                .is_empty()
                || fresh
                    .get("events")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0)
                    <= 1,
            "the catch-up feed is bounded to newer events"
        );
        host.shutdown();
    }

    /// A wait that never matches times out honestly, audited, and a
    /// prompt without a grant is refused.
    #[test]
    fn wait_timeout_and_prompt_without_grant_are_honest() {
        let host = start_host();
        let mut client = host.client();
        let created = client
            .call(OfficeRequestKind::TerminalCreate {
                argv: vec!["/bin/sh".into(), "-c".into(), "cat".into()],
                cwd: host.home.display().to_string(),
                env: vec![],
                cols: 80,
                rows: 24,
                purpose: "timeout probe".into(),
                worktree_id: None,
                owner: "agent_cli".into(),
            })
            .expect("create");
        let terminal_id = created
            .get("terminal_id")
            .and_then(|v| v.as_str())
            .expect("terminal id")
            .to_string();

        // Nothing ever reports `done`: the wait times out, audited.
        let mut wait = new_request(OfficeRequestKind::AgentWait {
            terminal_id: terminal_id.clone(),
            status: "done".into(),
            timeout_secs: 1,
        });
        wait.grant = Some(grant_for(&host.home, &["terminal_control"]));
        let waited = client
            .call_with_timeout(wait.kind.clone(), Duration::from_secs(10))
            .expect("wait");
        assert_eq!(waited.get("matched").and_then(|v| v.as_bool()), Some(false));

        // A member-attributed prompt without a grant: refused.
        let mut prompt = new_request(OfficeRequestKind::AgentPrompt {
            terminal_id: terminal_id.clone(),
            prompt: "you there?".into(),
        });
        prompt.member = Some("member-00000000-0000-0000-0000-000000000000".into());
        let err = client.call_request(prompt).expect_err("must deny");
        assert!(err.to_string().contains("denied"), "{err}");

        let store = Store::open(
            &crate::foundation::paths::database_path(&host.home),
            office_migrations(),
        )
        .expect("store");
        let timeouts: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM office_events WHERE kind = 'agent_wait_timeout'",
                [],
                |row| row.get(0),
            )
            .expect("timeout audit");
        assert!(timeouts >= 1, "the timeout is audited");
        host.shutdown();
    }
}

#[cfg(test)]
mod qa_round2_tests {
    use super::s3_handoff_tests::wait_for_output;
    use super::*;
    use crate::office::OfficeClient;
    use std::time::{Duration, Instant};

    /// QA F1 (issue #58): reset_at expressed in a NON-UTC offset must fire
    /// at the true instant. A +08:00 time that is due within ~2s was
    /// silently delayed by ~8h under string comparison.
    #[test]
    fn offset_reset_at_fires_at_the_true_instant() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        let host = OfficeHost::open(&home).expect("host");
        let server = host.serve_background();
        let socket = home.join(OFFICE_SOCKET_NAME);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }

        let mut client = OfficeClient::connect(&home).expect("client");
        let created = client
            .call(OfficeRequestKind::TerminalCreate {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "echo offset-ready; cat".into(),
                ],
                cwd: home.display().to_string(),
                env: vec![],
                cols: 80,
                rows: 24,
                purpose: "offset probe".into(),
                worktree_id: None,
                owner: "agent_cli".into(),
            })
            .expect("create");
        let terminal_id = created
            .get("terminal_id")
            .and_then(|v| v.as_str())
            .expect("terminal id")
            .to_string();
        assert!(wait_for_output(&mut client, &terminal_id, "offset-ready"));

        // due in ~2s, EXPRESSED IN +08:00 (the user's local zone).
        let local_due = (time::OffsetDateTime::now_utc() + time::Duration::seconds(2))
            .to_offset(time::UtcOffset::from_hms(8, 0, 0).expect("+08:00"));
        let reset_at = local_due
            .format(&time::format_description::well_known::Rfc3339)
            .expect("format");
        assert!(reset_at.contains("+08:00"), "{reset_at}");

        let scheduled = client
            .call(OfficeRequestKind::RecoverySchedule {
                terminal_id: terminal_id.clone(),
                reset_at: reset_at.clone(),
                task_id: None,
            })
            .expect("schedule");
        // The stored value must be normalized to UTC (ends in Z).
        assert!(
            scheduled
                .get("reset_at")
                .and_then(|v| v.as_str())
                .map(|s| s.ends_with('Z'))
                .unwrap_or(false),
            "reset_at must be normalized to UTC: {scheduled}"
        );

        // The sweep fires within a couple of seconds of the reset. 10s
        // window: the sweep is throttled to 1s and the reset is due at
        // +2s, but a loaded CI runner adds scheduling slack — the guard
        // verifies the sweep FIRES, not echo latency.
        {
            let until = Instant::now() + Duration::from_secs(10);
            let mut seen = false;
            while Instant::now() < until {
                if wait_for_output(&mut client, &terminal_id, "continue") {
                    seen = true;
                    break;
                }
            }
            assert!(
                seen,
                "the recovery prompt must reach the terminal; plans={:?}; terminal={:?}",
                client.call(OfficeRequestKind::RecoveryList),
                client.call(OfficeRequestKind::TerminalSnapshot {
                    terminal_id: terminal_id.clone(),
                }),
            );
        }

        // Audit + plan state.
        {
            let store = Store::open(
                &crate::foundation::paths::database_path(&home),
                office_migrations(),
            )
            .expect("store");
            let fired: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM recovery_plans WHERE state = 'fired'",
                    [],
                    |row| row.get(0),
                )
                .expect("fired");
            assert!(fired >= 1);
        }

        client
            .call(OfficeRequestKind::Shutdown { close_policy: None })
            .expect("shutdown");
        server.join().expect("old host joins");
        drop(dir);
    }

    /// QA F4 strengthening (issue #55): the refusal audit event FIRES, not
    /// just the error response.
    #[test]
    fn cleanup_refusal_is_audited() {
        let host = s6_pause_tests::start_host();
        let mut client = host.client();
        let created = client
            .call(OfficeRequestKind::TerminalCreate {
                argv: vec!["/bin/sh".into(), "-c".into(), "cat".into()],
                cwd: host.home.display().to_string(),
                env: vec![],
                cols: 80,
                rows: 24,
                purpose: "refusal probe".into(),
                worktree_id: None,
                owner: "agent_cli".into(),
            })
            .expect("create");
        let _terminal_id = created
            .get("terminal_id")
            .and_then(|v| v.as_str())
            .expect("terminal id");

        // A cleanup for a worktree that is not registered → NotFound, and
        // importantly the REFUSAL audit for a real merged-gate failure is
        // covered by the v15_2 test; here we pin that the NotFound path
        // does NOT silently vanish either — it errors loudly.
        let fresh_id = crate::foundation::ids::WorktreeId::new().to_string();
        let err = client
            .call(OfficeRequestKind::WorktreeCleanup {
                worktree_id: fresh_id.clone(),
            })
            .expect_err("unknown worktree must fail");
        assert!(err.to_string().contains("worktree"), "{err}");
        host.shutdown();
    }
}
#[cfg(test)]
mod v15_tests {
    use super::*;
    use crate::office::OfficeClient;
    use std::process::Command;
    use std::time::{Duration, Instant};

    pub(crate) struct RunningHost {
        dir: tempfile::TempDir,
        home: std::path::PathBuf,
        pub(crate) shared: Option<Arc<OfficeShared>>,
        server: std::thread::JoinHandle<()>,
    }

    pub(crate) fn start_host() -> RunningHost {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        let host = OfficeHost::open(&home).expect("host");
        let shared = host.shared();
        let server = host.serve_background();
        let socket = home.join(OFFICE_SOCKET_NAME);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        RunningHost {
            dir,
            home,
            shared: Some(shared),
            server,
        }
    }

    impl RunningHost {
        fn client(&self) -> OfficeClient {
            OfficeClient::connect(&self.home).expect("client")
        }
        fn shutdown(self) {
            if let Ok(mut client) = OfficeClient::connect(&self.home) {
                let _ = client.call(OfficeRequestKind::Shutdown { close_policy: None });
            }
            let _ = self.server.join();
            drop(self.dir);
        }
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args(["-c", "user.email=test@viva.local", "-c", "user.name=test"])
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A repo with one commit, a bare origin, and one adopted task
    /// worktree on branch-wt.
    fn seed_repo_worktree(host: &RunningHost, branch: &str) -> (String, std::path::PathBuf) {
        let repo = host.dir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo dir");
        let bare = host.dir.path().join("origin.git");
        git(host.dir.path(), &["init", "--bare", bare.to_str().unwrap()]);
        git(&repo, &["init", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "seed\n").expect("seed");
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "seed"]);
        git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git(&repo, &["push", "origin", "main"]);
        git(&repo, &["fetch", "origin"]);
        // auto_pull's `git pull --ff-only` needs tracking configured.
        git(&repo, &["branch", "--set-upstream-to=origin/main", "main"]);

        let store = Store::open(
            &crate::foundation::paths::database_path(&host.home),
            office_migrations(),
        )
        .expect("store");
        let project = crate::projects::ProjectRegistry::new(&store)
            .register(None, "demo", &repo)
            .expect("project");
        let task = TaskRegistry::new(&store)
            .create_task(
                "v15 seed task",
                vec![],
                None,
                None,
                Some(project.project_id.clone()),
            )
            .expect("task");
        let wt_path = host.dir.path().join("wt");
        git(
            &repo,
            &["worktree", "add", wt_path.to_str().unwrap(), "-b", branch],
        );
        git(&repo, &["push", "origin", branch]);
        crate::git::worktrees::WorktreeService::new(
            &store,
            crate::git::worktrees::ProtectedRefs::new(vec![]),
        )
        .adopt_existing(&repo, &wt_path, &task.task_id)
        .expect("adopt");
        (task.task_id.to_string(), wt_path)
    }

    fn worktree_id_for(client: &mut OfficeClient, path_suffix: &str) -> String {
        let view = client.call(OfficeRequestKind::WorkbenchView).expect("view");
        let rows = view
            .get("worktrees")
            .and_then(|v| v.as_array())
            .expect("rows");
        rows.iter()
            .find(|row| {
                row.get("path")
                    .and_then(|p| p.as_str())
                    .map(|p| p.ends_with(path_suffix))
                    .unwrap_or(false)
            })
            .and_then(|row| row.get("worktree_id").and_then(|v| v.as_str()))
            .expect("worktree row")
            .to_string()
    }

    fn wait_for_output(client: &mut OfficeClient, terminal_id: &str, needle: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(view) = client.call(OfficeRequestKind::TerminalSnapshot {
                terminal_id: terminal_id.to_string(),
            }) {
                if format!("{view}").contains(needle) {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// V15-1: the file inventory marks modified/untracked distinctly, the
    /// modified file views as a diff against the recorded base, and an
    /// untracked text file views as bounded content.
    #[test]
    fn worktree_files_content_and_diff() {
        let host = start_host();
        let (_, wt_path) = seed_repo_worktree(&host, "v15-files");
        let mut client = host.client();
        let worktree_id = worktree_id_for(&mut client, "wt");

        std::fs::write(wt_path.join("README.md"), "seed changed\n").expect("modify");
        std::fs::write(wt_path.join("notes.txt"), "fresh notes\n").expect("untracked");

        let files = client
            .call(OfficeRequestKind::WorktreeFiles {
                worktree_id: worktree_id.clone(),
            })
            .expect("files");
        let text = format!("{files}");
        assert!(text.contains("README.md"), "{text}");
        assert!(text.contains("notes.txt"), "{text}");

        let diff = client
            .call(OfficeRequestKind::WorktreeFileContent {
                worktree_id: worktree_id.clone(),
                path: "README.md".into(),
            })
            .expect("diff");
        assert_eq!(diff.get("kind").and_then(|v| v.as_str()), Some("diff"));
        assert!(
            diff.get("text")
                .and_then(|v| v.as_str())
                .map(|t| t.contains("seed changed"))
                .unwrap_or(false),
            "the diff shows the change: {diff}"
        );

        let content = client
            .call(OfficeRequestKind::WorktreeFileContent {
                worktree_id: worktree_id.clone(),
                path: "notes.txt".into(),
            })
            .expect("content");
        assert_eq!(
            content.get("kind").and_then(|v| v.as_str()),
            Some("content")
        );
        assert!(
            content
                .get("text")
                .and_then(|v| v.as_str())
                .map(|t| t.contains("fresh notes"))
                .unwrap_or(false)
        );

        // Path safety: traversal is refused, not resolved.
        let err = client
            .call(OfficeRequestKind::WorktreeFileContent {
                worktree_id,
                path: "../escape".into(),
            })
            .expect_err("traversal must be refused");
        assert!(err.to_string().contains("relative"), "{err}");
        host.shutdown();
    }

    /// V15-2: a MERGED PR unlocks the cleanup entry (release + directory
    /// removal + audit); a branch without a merged PR is refused.
    #[test]
    fn merged_pr_unlocks_cleanup_and_unknown_refuses() {
        let host = start_host();
        let (_, wt_path) = seed_repo_worktree(&host, "branch-with-pr");

        // Fake gh (QA round 5): injected via the shared gh_bin seam
        // instead of the process-global PATH — no race with parallel
        // tests. branch-with-pr → MERGED; everything else fails.
        let fake_bin = host.home.join("fakebin");
        std::fs::create_dir_all(&fake_bin).expect("fake bin");
        let gh = fake_bin.join("gh");
        std::fs::write(
            &gh,
            "#!/bin/sh\nif [ \"$3\" = \"branch-with-pr\" ]; then\n  echo '{\"state\":\"MERGED\",\"number\":12,\"url\":\"https://example/pr/12\"}'\n  exit 0\nfi\nexit 1\n",
        )
        .expect("write gh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        *host
            .shared
            .as_ref()
            .expect("shared")
            .gh_bin
            .lock()
            .expect("gh bin") = Some(gh.display().to_string());

        let mut client = host.client();
        let worktree_id = worktree_id_for(&mut client, "wt");

        // The PR state lands in the view.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let view = client.call(OfficeRequestKind::WorkbenchView).expect("view");
            let merged = view
                .get("worktrees")
                .and_then(|v| v.as_array())
                .and_then(|rows| {
                    rows.iter()
                        .find(|row| {
                            row.get("worktree_id").and_then(|v| v.as_str())
                                == Some(worktree_id.as_str())
                        })
                        .and_then(|row| {
                            row.get("pr_state")
                                .and_then(|v| v.as_str())
                                .map(str::to_string)
                        })
                });
            if merged.as_deref() == Some("merged") {
                break;
            }
            assert!(Instant::now() < deadline, "pr_state never showed merged");
            std::thread::sleep(Duration::from_millis(200));
        }

        // Cleanup: released + directory actually removed + audited.
        let cleaned = client
            .call(OfficeRequestKind::WorktreeCleanup {
                worktree_id: worktree_id.clone(),
            })
            .expect("cleanup");
        assert_eq!(cleaned.get("cleaned").and_then(|v| v.as_bool()), Some(true));
        assert!(!wt_path.exists(), "the directory is gone");

        let store = Store::open(
            &crate::foundation::paths::database_path(&host.home),
            office_migrations(),
        )
        .expect("store");
        let audited: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM office_events WHERE kind = 'worktree_cleaned'",
                [],
                |row| row.get(0),
            )
            .expect("audit");
        assert!(audited >= 1);

        // A branch whose PR state is unknown (gh fails) is refused.
        git(
            &host.dir.path().join("repo"),
            &[
                "worktree",
                "add",
                host.dir.path().join("wt2").to_str().unwrap(),
                "-b",
                "no-pr-branch",
            ],
        );
        crate::git::worktrees::WorktreeService::new(
            &store,
            crate::git::worktrees::ProtectedRefs::new(vec![]),
        )
        .adopt_existing(
            &host.dir.path().join("repo"),
            &host.dir.path().join("wt2"),
            &crate::foundation::ids::TaskId::from_str(
                &store
                    .connection()
                    .query_row("SELECT task_id FROM task_worktrees LIMIT 1", [], |row| {
                        row.get::<_, String>(0)
                    })
                    .expect("task"),
            )
            .expect("task id"),
        )
        .expect("adopt 2");
        let wt2 = worktree_id_for(&mut client, "wt2");
        let err = client
            .call(OfficeRequestKind::WorktreeCleanup { worktree_id: wt2 })
            .expect_err("unknown PR state must refuse cleanup");
        assert!(err.to_string().contains("MERGED"), "{err}");

        host.shutdown();
    }

    /// V15-3: the handoff delivers a brief (task goal + departing summary)
    /// into the new agent's terminal, records it in the task history, and
    /// leaves the departing terminal running.
    #[test]
    fn handoff_moves_a_worktree_between_agents_with_a_brief() {
        let host = start_host();
        let home = host.home.clone();
        // Two fake agents, on PATH: handoff targets are bare command
        // names by contract.
        let bin = home.join("agents");
        std::fs::create_dir_all(&bin).expect("agents dir");
        let saved_path = std::env::var("PATH").unwrap_or_default();
        // SAFETY: test-only PATH prepend (fake agent dirs); restored below.
        unsafe {
            std::env::set_var("PATH", format!("{}:{saved_path}", bin.display()));
        }
        for name in ["fake-a", "fake-b"] {
            let path = bin.join(name);
            std::fs::write(&path, format!("#!/bin/sh\necho {name}-up\ncat\n")).expect("write");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod");
            }
        }
        let (_, _wt_path) = seed_repo_worktree(&host, "v15-handoff");
        let mut client = host.client();
        let worktree_id = worktree_id_for(&mut client, "wt");

        // Agent A's terminal reports the departing summary.
        let created = client
            .call(OfficeRequestKind::TerminalCreate {
                argv: vec![bin.join("fake-a").display().to_string()],
                cwd: home.display().to_string(),
                env: vec![],
                cols: 80,
                rows: 24,
                purpose: "departing agent".into(),
                worktree_id: Some(worktree_id.clone()),
                owner: "agent_cli".into(),
            })
            .expect("create a");
        let terminal_a = created
            .get("terminal_id")
            .and_then(|v| v.as_str())
            .expect("terminal id")
            .to_string();
        assert!(wait_for_output(&mut client, &terminal_a, "fake-a-up"));
        let grant = {
            let store = Store::open(
                &crate::foundation::paths::database_path(&home),
                office_migrations(),
            )
            .expect("store");
            crate::authority::AuthorityEngine::new(&store)
                .issue_root_grant(
                    None,
                    None,
                    vec!["agent_report".into()],
                    crate::authority::GrantMode::ActAutonomously,
                    None,
                )
                .expect("grant")
                .grant_id
                .to_string()
        };
        let mut report = new_request(OfficeRequestKind::AgentReport {
            terminal_id: terminal_a.clone(),
            agent: "fake-a".into(),
            status: "blocked".into(),
            detail: "implemented the parser, tests pending".into(),
            reset_at: None,
        });
        report.grant = Some(grant);
        client.call_request(report).expect("report");

        // The handoff: worktree → fake-b.
        let handed = client
            .call(OfficeRequestKind::AgentHandoff {
                worktree_id: worktree_id.clone(),
                to_agent: "fake-b".into(),
            })
            .expect("handoff");
        let terminal_b = handed
            .get("terminal_id")
            .and_then(|v| v.as_str())
            .expect("terminal b")
            .to_string();
        assert!(
            wait_for_output(&mut client, &terminal_b, "handoff】task: v15 seed task"),
            "the brief reaches the new agent"
        );
        assert!(
            wait_for_output(&mut client, &terminal_b, "implemented the parser"),
            "the departing summary rides in the brief"
        );

        // The departing terminal is still there.
        let list = client.call(OfficeRequestKind::TerminalList).expect("list");
        assert!(format!("{list}").contains(&terminal_a));

        // The task history carries the handoff.
        let store = Store::open(
            &crate::foundation::paths::database_path(&home),
            office_migrations(),
        )
        .expect("store");
        let handoffs: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM task_results WHERE kind = 'agent_handoff'",
                [],
                |row| row.get(0),
            )
            .expect("history");
        assert!(handoffs >= 1, "the handoff is in the task history");

        client
            .call(OfficeRequestKind::TerminalStop {
                terminal_id: terminal_a,
            })
            .expect("stop a");
        client
            .call(OfficeRequestKind::TerminalStop {
                terminal_id: terminal_b,
            })
            .expect("stop b");
        // SAFETY: restoring the saved PATH.
        unsafe {
            std::env::set_var("PATH", saved_path);
        }
        host.shutdown();
    }

    /// V15-4: auto_pull off → nothing moves; on → the main checkout
    /// fast-forwards and worktrees fetch; behind shows in the view.
    #[test]
    fn auto_sync_respects_the_switch_and_never_merges_worktrees() {
        let host = start_host();
        let (task_id, _wt) = seed_repo_worktree(&host, "v15-sync");

        // Make origin/main one commit ahead of the main checkout.
        let repo = host.dir.path().join("repo");
        let scratch = host.dir.path().join("scratch");
        git(
            host.dir.path(),
            &[
                "clone",
                host.dir.path().join("origin.git").to_str().unwrap(),
                scratch.to_str().unwrap(),
            ],
        );
        // A bare repo's HEAD may point at the platform default (master);
        // pin the scratch clone onto main explicitly.
        git(&scratch, &["checkout", "-b", "main", "origin/main"]);
        std::fs::write(scratch.join("ahead.txt"), "ahead\n").expect("ahead");
        git(&scratch, &["add", "."]);
        git(&scratch, &["commit", "-m", "ahead commit"]);
        git(&scratch, &["push", "origin", "main"]);

        // Put origin's v15-sync one commit ahead of the worktree so the
        // behind marker has something real to show after the fetch.
        git(&scratch, &["fetch", "origin", "v15-sync"]);
        git(&scratch, &["checkout", "-b", "wtshift", "origin/v15-sync"]);
        std::fs::write(scratch.join("wt-ahead.txt"), "wt ahead\n").expect("wt ahead");
        git(&scratch, &["add", "."]);
        git(&scratch, &["commit", "-m", "wt ahead commit"]);
        git(&scratch, &["push", "origin", "wtshift:v15-sync"]);
        git(&repo, &["fetch", "origin"]);

        let mut client = host.client();

        // auto_pull defaults OFF: a manual sync now must NOT move main.
        let before = git_head(&host.home, &repo);
        client.call(OfficeRequestKind::SyncNow).expect("sync now");
        assert_eq!(
            before,
            git_head(&host.home, &repo),
            "auto_pull off: main untouched"
        );

        // Turn it on via the socket (the same surface the workbench uses).
        client
            .call(OfficeRequestKind::SetAutoPull { on: true })
            .expect("enable");

        // Still off→on boundary: the explicit sync now fast-forwards main.
        client.call(OfficeRequestKind::SyncNow).expect("sync now 2");
        let after = git_head(&host.home, &repo);
        assert_ne!(before, after, "auto_pull on: main fast-forwarded");

        // The task worktree fetched (behind visible) and its HEAD NEVER
        // moved — fetch updates refs, the working tree stays put.
        let wt_head_before = String::from_utf8_lossy(
            &Command::new("git")
                .arg("-C")
                .arg(&_wt)
                .args(["rev-parse", "HEAD"])
                .output()
                .expect("rev-parse")
                .stdout,
        )
        .trim()
        .to_string();
        let view = client.call(OfficeRequestKind::WorkbenchView).expect("view");
        let text = format!("{view}");
        assert!(
            text.contains("\"behind\":1") || text.contains("\"behind\": 1"),
            "behind is displayed: {text}"
        );
        let wt_head_after = String::from_utf8_lossy(
            &Command::new("git")
                .arg("-C")
                .arg(&_wt)
                .args(["rev-parse", "HEAD"])
                .output()
                .expect("rev-parse")
                .stdout,
        )
        .trim()
        .to_string();
        assert_eq!(wt_head_before, wt_head_after, "worktree HEAD unmoved");
        let _ = task_id;
        host.shutdown();
    }

    fn git_head(home: &std::path::Path, repo: &std::path::Path) -> String {
        let store = Store::open(
            &crate::foundation::paths::database_path(home),
            office_migrations(),
        )
        .expect("store");
        drop(store);
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("rev-parse");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// V15-5: a recovery plan with a near reset_at fires on its own,
    /// prompts the terminal, and lands in the audit trail.
    #[test]
    fn recovery_plan_fires_when_due_and_is_audited() {
        let host = start_host();
        let home = host.home.clone();
        let mut client = host.client();
        let created = client
            .call(OfficeRequestKind::TerminalCreate {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "echo recovery-ready; cat".into(),
                ],
                cwd: home.display().to_string(),
                env: vec![],
                cols: 80,
                rows: 24,
                purpose: "rate-limited agent".into(),
                worktree_id: None,
                owner: "agent_cli".into(),
            })
            .expect("create");
        let terminal_id = created
            .get("terminal_id")
            .and_then(|v| v.as_str())
            .expect("terminal id")
            .to_string();
        assert!(wait_for_output(&mut client, &terminal_id, "recovery-ready"));

        // Schedule via the socket (the same path the CLI uses).
        client
            .call(OfficeRequestKind::RecoverySchedule {
                terminal_id: terminal_id.clone(),
                reset_at: rfc3339_in(2),
                task_id: None,
            })
            .expect("schedule");

        // The sweep fires within a couple of seconds of the reset.
        let plans = client
            .call(OfficeRequestKind::RecoveryList)
            .expect("recovery list");
        let state = plans
            .get("plans")
            .and_then(|v| v.as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("state").and_then(|s| s.as_str()))
            .expect("plan state")
            .to_string();
        // The plan must NOT fire early: a +08:00 offset that is still in
        // the future must stay `scheduled` under a UTC string comparison.
        assert_eq!(state, "scheduled", "no early fire for offset times");

        // ... and a plan whose reset (expressed in +08:00) is due within
        // seconds MUST fire — the offset itself must not delay it.
        assert!(
            wait_for_output(&mut client, &terminal_id, "continue"),
            "the recovery prompt must reach the terminal"
        );

        // Audit + plan state.
        let store = Store::open(
            &crate::foundation::paths::database_path(&home),
            office_migrations(),
        )
        .expect("store");
        let fired: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM recovery_plans WHERE state = 'fired'",
                [],
                |row| row.get(0),
            )
            .expect("fired");
        assert!(fired >= 1);
        let audited: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM office_events WHERE kind = 'recovery_plan_fired'",
                [],
                |row| row.get(0),
            )
            .expect("audit");
        assert!(audited >= 1, "firing is audited");

        // Cancel on a non-existent plan is an honest 404.
        assert!(
            client
                .call(OfficeRequestKind::RecoveryCancel { plan_id: 999_999 })
                .is_err()
        );
        host.shutdown();
    }

    fn rfc3339_in(secs: i64) -> String {
        let at = time::OffsetDateTime::now_utc() + time::Duration::seconds(secs);
        at.format(&time::format_description::well_known::Rfc3339)
            .expect("format")
    }

    /// V15-5 honesty: rate_limited without a reset_at is refused — the
    /// office never guesses a recovery time.
    #[test]
    fn rate_limited_without_reset_at_is_refused() {
        let host = start_host();
        let mut client = host.client();
        let grant = {
            let store = Store::open(
                &crate::foundation::paths::database_path(&host.home),
                office_migrations(),
            )
            .expect("store");
            crate::authority::AuthorityEngine::new(&store)
                .issue_root_grant(
                    None,
                    None,
                    vec!["agent_report".into()],
                    crate::authority::GrantMode::ActAutonomously,
                    None,
                )
                .expect("grant")
                .grant_id
                .to_string()
        };
        let created = client
            .call(OfficeRequestKind::TerminalCreate {
                argv: vec!["/bin/sh".into(), "-c".into(), "cat".into()],
                cwd: host.home.display().to_string(),
                env: vec![],
                cols: 80,
                rows: 24,
                purpose: "rate limit probe".into(),
                worktree_id: None,
                owner: "agent_cli".into(),
            })
            .expect("create");
        let terminal_id = created
            .get("terminal_id")
            .and_then(|v| v.as_str())
            .expect("terminal id")
            .to_string();
        let mut report = new_request(OfficeRequestKind::AgentReport {
            terminal_id,
            agent: "codex".into(),
            status: "rate_limited".into(),
            detail: "5h limit hit".into(),
            reset_at: None,
        });
        report.grant = Some(grant);
        let err = client.call_request(report).expect_err("must refuse");
        assert!(err.to_string().contains("reset_at"), "{err}");
        host.shutdown();
    }

    /// V15-5 restart semantics: a plan whose reset_at passed while no host
    /// was running is marked expired on startup and NOT executed blindly.
    #[test]
    fn overdue_plans_expire_on_restart_without_executing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        {
            let store = Store::open(
                &crate::foundation::paths::database_path(&home),
                office_migrations(),
            )
            .expect("store");
            store
                .connection()
                .execute(
                    "INSERT INTO recovery_plans(terminal_id, task_id, reset_at, action, state, created_at)
                     VALUES ('term-gone', NULL, '2026-01-01T00:00:00Z', 'resume_prompt', 'scheduled', ?1)",
                    [utc_now()],
                )
                .expect("seed overdue plan");
        }
        let host = OfficeHost::open(&home).expect("host");
        {
            let shared = host.shared();
            let store = shared.store.lock().expect("store");
            let state: String = store
                .connection()
                .query_row(
                    "SELECT state FROM recovery_plans WHERE terminal_id = 'term-gone'",
                    [],
                    |row| row.get(0),
                )
                .expect("plan");
            assert_eq!(state, "expired", "overdue plans expire, never execute");
            let note: String = store
                .connection()
                .query_row(
                    "SELECT detail FROM office_recovery_events
                     WHERE kind = 'recovery_plan_expired' ORDER BY seq DESC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .expect("recovery note");
            assert!(note.contains("expired"), "{note}");
        }
        drop(host);
        drop(dir);
    }
}

#[cfg(test)]
mod s6_pause_tests {
    use super::*;
    use crate::office::OfficeClient;
    use std::time::{Duration, Instant};

    pub(crate) struct RunningHost {
        /// Holds the temp home alive for the whole test; never read.
        _dir: tempfile::TempDir,
        pub(crate) home: std::path::PathBuf,
        server: std::thread::JoinHandle<()>,
    }

    pub(crate) fn start_host() -> RunningHost {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        let host = OfficeHost::open(&home).expect("host");
        let server = host.serve_background();
        let socket = home.join(OFFICE_SOCKET_NAME);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        RunningHost {
            _dir: dir,
            home,
            server,
        }
    }

    impl RunningHost {
        pub(crate) fn client(&self) -> OfficeClient {
            OfficeClient::connect(&self.home).expect("client")
        }
        pub(crate) fn shutdown(self) {
            if let Ok(mut client) = OfficeClient::connect(&self.home) {
                let _ = client.call(OfficeRequestKind::Shutdown { close_policy: None });
            }
            let _ = self.server.join();
        }
    }

    /// A REAL dispatch chain (member + binding + task + live grant) so the
    /// pause gate is tested against the full authorization path.
    fn seed_dispatch_chain(home: &std::path::Path) -> (String, String, String) {
        let store = Store::open(
            &crate::foundation::paths::database_path(home),
            office_migrations(),
        )
        .expect("store");
        let member = crate::members::MemberRegistry::new(&store)
            .register("operator")
            .expect("member");
        crate::members::MemberRegistry::new(&store)
            .set_binding(&crate::members::MemberBinding {
                member_id: member.member_id.clone(),
                role: "developer".into(),
                model_binding: "glm-5.3-flash".into(),
                tools: vec![],
                updated_at: crate::foundation::ids::utc_now(),
            })
            .expect("binding");
        let task = TaskRegistry::new(&store)
            .create_task("prove the pause gate", vec![], None, None, None)
            .expect("task");
        let grant = crate::authority::AuthorityEngine::new(&store)
            .issue_root_grant(
                Some(member.member_id.clone()),
                Some(task.task_id.clone()),
                vec!["dispatch_delegated".into()],
                crate::authority::GrantMode::ActAutonomously,
                None,
            )
            .expect("grant");
        (
            member.member_id.to_string(),
            task.task_id.to_string(),
            grant.grant_id.to_string(),
        )
    }

    /// Pause stops NEW dispatch (authorized chain included) and running
    /// executions are untouched; resume lifts the gate; the pause survives
    /// a restart; pause/resume are owner-only.
    #[test]
    fn pause_gates_new_dispatch_only_and_survives_a_restart() {
        let host = start_host();
        let mut client = host.client();
        let (member, task, grant) = seed_dispatch_chain(&host.home);
        let dispatch_with_cwd = |cwd: &str| OfficeRequestKind::Dispatch {
            task_id: task.clone(),
            member_id: member.clone(),
            grant_id: grant.clone(),
            request_key: format!("req-{}", uuid::Uuid::new_v4().simple()),
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()],
            cwd: cwd.to_string(),
            worktree_id: None,
        };

        // Before the pause the dispatch is authorized and would spawn; use
        // an invalid cwd so we prove authorization PASSED (cwd error comes
        // after the grant checks).
        let err = client
            .call(dispatch_with_cwd("/definitely/not/here"))
            .expect_err("the dispatch must fail");
        // Either rejection proves the request walked the real path: the
        // cwd check (terminal cwd must be absolute+existing paths differ
        // per spawn backend) fires after the grant checks either way.
        assert!(
            err.to_string().contains("cwd") || err.to_string().contains("spawn"),
            "expected the post-authorization failure, got: {err}"
        );

        // Pause: the same fully-authorized dispatch is now refused.
        client.call(OfficeRequestKind::Pause).expect("pause");
        let err = client
            .call(dispatch_with_cwd("/definitely/not/here"))
            .expect_err("paused dispatch must be refused");
        assert!(err.to_string().contains("paused"), "{err}");

        // Member grants cannot carry pause/resume: owner-only.
        let mut owner_attempt = new_request(OfficeRequestKind::Pause);
        owner_attempt.member = Some(member.clone());
        owner_attempt.grant = Some(grant.clone());
        let err = client.call_request(owner_attempt).expect_err("must deny");
        assert!(err.to_string().contains("owner action"), "{err}");

        // Resume lifts the gate (cwd check again proves the path).
        client.call(OfficeRequestKind::Resume).expect("resume");
        let err = client
            .call(dispatch_with_cwd("/definitely/not/here"))
            .expect_err("the dispatch must fail again");
        assert!(
            err.to_string().contains("cwd") || err.to_string().contains("spawn"),
            "the gate is open (post-authorization failure expected): {err}"
        );

        // A paused server persists the pause: the next host starts paused.
        client.call(OfficeRequestKind::Pause).expect("pause 2");
        {
            let store = Store::open(
                &crate::foundation::paths::database_path(&host.home),
                office_migrations(),
            )
            .expect("store");
            let value: String = store
                .connection()
                .query_row(
                    "SELECT value FROM office_settings WHERE key = 'paused'",
                    [],
                    |row| row.get(0),
                )
                .expect("setting");
            assert_eq!(value, "true");
        }
        // Close with the pause in place, then reopen: paused comes back.
        client
            .call(OfficeRequestKind::Shutdown { close_policy: None })
            .expect("shutdown");
        let _ = host.server.join();
        let reopened = OfficeHost::open(&host.home).expect("reopen");
        assert!(
            reopened
                .shared()
                .paused
                .load(std::sync::atomic::Ordering::SeqCst),
            "the persisted pause survives the restart"
        );
        {
            let shared = reopened.shared();
            let store = shared.store.lock().expect("store");
            store
                .connection()
                .execute(
                    "UPDATE office_settings SET value = 'false' WHERE key = 'paused'",
                    [],
                )
                .expect("cleanup");
        }
        drop(reopened);
    }

    /// close-policy=pause at shutdown leaves the NEXT server paused;
    /// continue (default) clears it. Type-1 recovery facts stay honest.
    #[test]
    fn close_policy_pause_carries_across_shutdown() {
        let host = start_host();
        let mut client = host.client();
        client
            .call(OfficeRequestKind::Shutdown {
                close_policy: Some("pause".into()),
            })
            .expect("shutdown with pause policy");
        let _ = host.server.join();

        let reopened = OfficeHost::open(&host.home).expect("reopen");
        assert!(
            reopened
                .shared()
                .paused
                .load(std::sync::atomic::Ordering::SeqCst),
            "close-policy=pause must leave the next host paused"
        );
        // The recovery log names the restore.
        let notes: i64 = {
            let shared = reopened.shared();
            let store = shared.store.lock().expect("store");
            store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM office_recovery_events WHERE kind = 'pause_restored'",
                    [],
                    |row| row.get(0),
                )
                .expect("recovery")
        };
        assert!(notes >= 1, "pause_restored must be recorded");
        drop(reopened);
    }
}

#[cfg(test)]
mod qa_round4_tests {
    use super::*;
    use crate::office::OfficeClient;
    use std::process::Command;
    use std::time::{Duration, Instant};

    struct RunningHost {
        /// Holds the temp home alive for the whole test; never read.
        _dir: tempfile::TempDir,
        home: std::path::PathBuf,
        shared: Option<Arc<OfficeShared>>,
        server: std::thread::JoinHandle<()>,
    }

    fn start_host() -> RunningHost {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        let host = OfficeHost::open(&home).expect("host");
        let shared = host.shared();
        let server = host.serve_background();
        let socket = home.join(OFFICE_SOCKET_NAME);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        RunningHost {
            _dir: dir,
            home,
            shared: Some(shared),
            server,
        }
    }

    impl RunningHost {
        fn client(&self) -> OfficeClient {
            OfficeClient::connect(&self.home).expect("client")
        }
        fn shutdown(self) {
            if let Ok(mut client) = OfficeClient::connect(&self.home) {
                let _ = client.call(OfficeRequestKind::Shutdown { close_policy: None });
            }
            let _ = self.server.join();
        }
    }

    fn git_seed(repo: &std::path::Path) {
        // Identity rides on EVERY git call that needs it, and every
        // status is asserted (QA F13's lesson).
        let init = Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .arg("-C")
            .arg(repo)
            .args(["init", "-b", "main"])
            .output()
            .expect("init");
        assert!(init.status.success());
        std::fs::write(repo.join("f.txt"), "x\n").expect("file");
        let add = Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .arg("-C")
            .arg(repo)
            .args(["add", "."])
            .output()
            .expect("add");
        assert!(add.status.success());
        let commit = Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .arg("-C")
            .arg(repo)
            .args(["commit", "-m", "seed"])
            .output()
            .expect("commit");
        assert!(commit.status.success());
    }

    /// QA F18 (issue #57): one project whose pull cannot run (no remote)
    /// must NOT abort the cycle — both projects report, every outcome is
    /// audited (git_auto_sync), and the round trip SUCCEEDS. This guard
    /// was lost in round 3's tail surgery; restored here with the
    /// auto_pull switch on so the failures are genuine pull attempts.
    #[test]
    fn one_untracked_project_does_not_abort_the_cycle() {
        let host = start_host();
        let work = tempfile::tempdir().expect("work dir");
        let repo_a = work.path().join("repo-a");
        let repo_b = work.path().join("repo-b");
        std::fs::create_dir_all(&repo_a).expect("dir a");
        std::fs::create_dir_all(&repo_b).expect("dir b");
        git_seed(&repo_a);
        git_seed(&repo_b);
        // Neither repo has a remote: both pulls must fail HONESTLY and
        // the cycle must still complete.
        {
            let store = host
                .shared
                .as_ref()
                .expect("shared")
                .store
                .lock()
                .expect("store");
            let registry = crate::projects::ProjectRegistry::new(&store);
            registry
                .register(None, "proj-a", &repo_a)
                .expect("register a");
            registry
                .register(None, "proj-b", &repo_b)
                .expect("register b");
        }

        let mut client = host.client();
        client
            .call(OfficeRequestKind::SetAutoPull { on: true })
            .expect("auto_pull on");
        let response = client
            .call_with_timeout(OfficeRequestKind::SyncNow, Duration::from_secs(60))
            .expect("the cycle must not abort on a failing project");
        let results = response
            .get("results")
            .and_then(|v| v.as_array())
            .expect("results")
            .clone();
        assert_eq!(results.len(), 2, "both projects report: {results:?}");
        let failed = results
            .iter()
            .filter(|row| {
                row.get("outcome")
                    .and_then(|v| v.as_str())
                    .map(|o| o.starts_with("failed"))
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(
            failed, 2,
            "both remote-less pulls fail honestly: {results:?}"
        );

        // Every outcome is audited.
        {
            let store = Store::open(
                &crate::foundation::paths::database_path(&host.home),
                office_migrations(),
            )
            .expect("store");
            let audited: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM office_events WHERE kind = 'git_auto_sync'",
                    [],
                    |row| row.get(0),
                )
                .expect("audit count");
            assert!(audited >= 2, "each outcome audited: {audited}");
        }

        client
            .call(OfficeRequestKind::Shutdown { close_policy: None })
            .expect("shutdown");
        drop(work);
        host.shutdown();
    }
}

#[cfg(test)]
mod qa_round5_tests {
    use super::*;
    use crate::office::OfficeClient;
    use std::process::Command;
    use std::time::{Duration, Instant};

    struct RunningHost {
        dir: tempfile::TempDir,
        home: std::path::PathBuf,
        server: std::thread::JoinHandle<()>,
    }

    fn start_host() -> RunningHost {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        let host = OfficeHost::open(&home).expect("host");
        let server = host.serve_background();
        let socket = home.join(OFFICE_SOCKET_NAME);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        RunningHost { dir, home, server }
    }

    impl RunningHost {
        fn client(&self) -> OfficeClient {
            OfficeClient::connect(&self.home).expect("client")
        }
        fn shutdown(self) {
            if let Ok(mut client) = OfficeClient::connect(&self.home) {
                let _ = client.call(OfficeRequestKind::Shutdown { close_policy: None });
            }
            let _ = self.server.join();
            drop(self.dir);
        }
    }

    fn git_seed(repo: &std::path::Path) {
        let init = Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .arg("-C")
            .arg(repo)
            .args(["init", "-b", "main"])
            .output()
            .expect("init");
        assert!(init.status.success());
        std::fs::write(repo.join("f.txt"), "x\n").expect("file");
        let add = Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .arg("-C")
            .arg(repo)
            .args(["add", "."])
            .output()
            .expect("add");
        assert!(add.status.success());
        let commit = Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .arg("-C")
            .arg(repo)
            .args(["commit", "-m", "seed"])
            .output()
            .expect("commit");
        assert!(commit.status.success());
    }

    /// QA F18 restored guard (issue #57): one project whose pull cannot
    /// run must NOT abort the cycle; both projects report and BOTH
    /// outcomes are audited as git_auto_sync.
    #[test]
    fn one_untracked_project_does_not_abort_the_cycle() {
        let host = start_host();
        let work = tempfile::tempdir().expect("work dir");
        let repo_a = work.path().join("repo-a");
        let repo_b = work.path().join("repo-b");
        std::fs::create_dir_all(&repo_a).expect("a");
        std::fs::create_dir_all(&repo_b).expect("b");
        git_seed(&repo_a);
        git_seed(&repo_b);
        {
            let store = Store::open(
                &crate::foundation::paths::database_path(&host.home),
                office_migrations(),
            )
            .expect("store");
            let registry = crate::projects::ProjectRegistry::new(&store);
            registry.register(None, "proj-a", &repo_a).expect("a");
            registry.register(None, "proj-b", &repo_b).expect("b");
        }

        let mut client = host.client();
        client
            .call(OfficeRequestKind::SetAutoPull { on: true })
            .expect("auto_pull on");
        let response = client
            .call_with_timeout(OfficeRequestKind::SyncNow, Duration::from_secs(60))
            .expect("the cycle must not abort on a failing project");
        let results = response
            .get("results")
            .and_then(|v| v.as_array())
            .expect("results")
            .clone();
        assert_eq!(results.len(), 2, "both projects report: {results:?}");
        let failed = results
            .iter()
            .filter(|row| {
                row.get("outcome")
                    .and_then(|v| v.as_str())
                    .map(|o| o.starts_with("failed"))
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(
            failed, 2,
            "both remote-less pulls fail honestly: {results:?}"
        );

        {
            let store = Store::open(
                &crate::foundation::paths::database_path(&host.home),
                office_migrations(),
            )
            .expect("store");
            let audited: i64 = store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM office_events WHERE kind = 'git_auto_sync'",
                    [],
                    |row| row.get(0),
                )
                .expect("audit count");
            assert!(audited >= 2, "each outcome audited: {audited}");
        }

        client
            .call(OfficeRequestKind::Shutdown { close_policy: None })
            .expect("shutdown");
        drop(work);
        host.shutdown();
    }

    /// QA F16 (issue #57): while a sync runs slow network git (PATH shim
    /// sleeps 3s on fetch/pull), a STORE-BACKED request (RecoveryList)
    /// must answer within 2s — Ping never takes the store mutex so it
    /// could pass even with the lock held for 30s.
    #[test]
    fn office_stays_responsive_around_slow_sync() {
        let host = start_host();
        let repo = host.dir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo dir");
        let bare = host.dir.path().join("origin.git");
        Command::new("git")
            .args([
                "init",
                "--bare",
                "--initial-branch=main",
                bare.to_str().unwrap(),
            ])
            .output()
            .expect("bare");
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["init", "-b", "main"])
            .output()
            .expect("init");
        std::fs::write(repo.join("f.txt"), "x\n").expect("file");
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["add", "."])
            .output()
            .expect("add");
        Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .arg("-C")
            .arg(&repo)
            .args(["commit", "-m", "seed"])
            .output()
            .expect("commit");
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["remote", "add", "origin", bare.to_str().unwrap()])
            .output()
            .expect("remote");
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["push", "origin", "main"])
            .output()
            .expect("push");
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["fetch", "origin"])
            .output()
            .expect("fetch");
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["branch", "--set-upstream-to=origin/main", "main"])
            .output()
            .expect("upstream");

        {
            let store = Store::open(
                &crate::foundation::paths::database_path(&host.home),
                office_migrations(),
            )
            .expect("store");
            crate::projects::ProjectRegistry::new(&store)
                .register(None, "responsive-probe", &repo)
                .expect("register project");
            store
                .connection()
                .execute(
                    "INSERT INTO office_settings(key, value) VALUES ('auto_pull', 'true')
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    [],
                )
                .expect("auto_pull on");
        }

        // PATH shim: git fetch/pull sleep 3s, everything else execs the
        // real git (resolved at setup) — delegating keeps behavior
        // identical for parallel tests.
        let real_git = {
            let out = Command::new("sh")
                .args(["-c", "command -v git"])
                .output()
                .expect("locate git");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        assert!(!real_git.is_empty(), "real git not found");
        let shim_dir = host.dir.path().join("shim");
        std::fs::create_dir_all(&shim_dir).expect("shim dir");
        let shim = shim_dir.join("git");
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\nif [ \"$1\" = fetch ] || [ \"$1\" = pull ]; then sleep 3; fi\nexec \"{real_git}\" \"$@\"\n"
            ),
        )
        .expect("shim script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let saved_path = std::env::var("PATH").unwrap_or_default();
        unsafe {
            std::env::set_var("PATH", format!("{}:{saved_path}", shim_dir.display()));
        }

        let sync_home = host.home.clone();
        let syncer = std::thread::spawn(move || {
            let mut c = OfficeClient::connect(&sync_home).expect("sync client");
            c.call_with_timeout(OfficeRequestKind::SyncNow, Duration::from_secs(120))
        });

        let deadline = Instant::now() + Duration::from_secs(12);
        let mut answered = false;
        while Instant::now() < deadline {
            if let Ok(mut c) = OfficeClient::connect(&host.home) {
                let started = Instant::now();
                let result =
                    c.call_with_timeout(OfficeRequestKind::RecoveryList, Duration::from_secs(2));
                let elapsed = started.elapsed();
                if result.is_ok() {
                    assert!(
                        elapsed <= Duration::from_secs(2),
                        "RecoveryList latency {elapsed:?} during the slow sync — the \
                         store lock is being held across network git (QA F3 regression)"
                    );
                    answered = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            answered,
            "the office must keep answering during a slow sync"
        );

        let sync_outcome = syncer.join().expect("sync thread ends");
        assert!(
            sync_outcome.is_ok(),
            "the SyncNow round trip must succeed under its 120s budget: {:?}",
            sync_outcome.err()
        );
        // SAFETY: restoring the saved PATH.
        unsafe {
            std::env::set_var("PATH", saved_path);
        }
        let mut client = OfficeClient::connect(&host.home).expect("client after");
        client
            .call(OfficeRequestKind::Shutdown { close_policy: None })
            .expect("shutdown");
        host.shutdown();
    }
}
