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
    /// in docs/validation/v12-final-acceptance.md §5.
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
        OfficeRequestKind::WorkbenchView => workbench_view(&shared),
        OfficeRequestKind::WorkbenchDiff { worktree_id } => workbench_diff(&shared, worktree_id),
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
        OfficeRequestKind::AgentReport {
            terminal_id,
            agent,
            status,
            detail,
        } => agent_report(&shared, terminal_id, agent, status, detail),
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
            | OfficeRequestKind::Pause
            | OfficeRequestKind::Resume
            | OfficeRequestKind::Shutdown { .. }
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
        ) {
            return Err(audit_grant_denial(
                shared,
                request,
                "shutdown/pause/resume are owner actions; member grants cannot carry them",
            ));
        }
        let action_needed = match request.kind {
            OfficeRequestKind::Dispatch { .. } => "dispatch_delegated",
            OfficeRequestKind::AgentReport { .. } => "agent_report",
            OfficeRequestKind::AgentContentSubmit { .. } => "agent_content",
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
fn workbench_view(shared: &OfficeShared) -> OfficeResult<serde_json::Value> {
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

    serde_json::to_value(&model).map_err(|e| OfficeError::Validation(format!("view: {e}")))
}

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
    let outcome = (|| -> OfficeResult<usize> {
        // Collect BEFORE accepting so the sender-side snapshot is one
        // consistent set.
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
            let store = shared.store.lock().expect("office store");
            let _ = record_recovery(
                &store,
                &shared.host_id,
                "live_handoff_failed",
                &format!(
                    "the transfer did not complete ({err}); this host continues serving \
                     every terminal — the type-1 fallback, no process touched"
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

    struct RunningHost {
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

    struct RunningHost {
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
        // the terminal.
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
        resume_thread
            .join()
            .expect("resumed host thread joins")
            .expect("resumed host ends cleanly");
        drop(dir);
    }
}

#[cfg(test)]
mod s4_agent_status_tests {
    use super::*;
    use crate::office::OfficeClient;
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
mod s6_pause_tests {
    use super::*;
    use crate::office::OfficeClient;
    use std::time::{Duration, Instant};

    struct RunningHost {
        /// Holds the temp home alive for the whole test; never read.
        _dir: tempfile::TempDir,
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
        RunningHost {
            _dir: dir,
            home,
            server,
        }
    }

    impl RunningHost {
        fn client(&self) -> OfficeClient {
            OfficeClient::connect(&self.home).expect("client")
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
