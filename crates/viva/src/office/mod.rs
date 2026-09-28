//! The active Office host and its control plane (V07, issue #16).
//!
//! One `VIVA_HOME` has at most one active Office host. The host owns the
//! private Unix-socket control channel (`office.sock` inside the 0700 home),
//! serves CLI callbacks over it, supervises every terminal it spawned, and
//! releases the channel on exit. A second `office start` refuses and names
//! the running host; a stale socket left by a crashed host is claimed with
//! an explicit recovery record.
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
//! Mutations require the live channel; with no active office the client
//! rejects cleanly and never starts a background daemon. Read-only status
//! is answerable offline from the store by design.

mod protocol;

pub use protocol::{
    MAX_MESSAGE_BYTES, OfficeRequest, OfficeRequestKind, OfficeResponse, PROTOCOL_VERSION,
    new_request, read_message, round_trip, write_message,
};

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::authority::{Actor, AuthorityEngine};
use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::{GrantId, MemberId, TaskId, TerminalId, WorktreeId, utc_now};
use crate::foundation::records::{AttributionSnapshot, LaunchSpec, RequestOrigin, TerminalOwner};
use crate::foundation::store::{Domain, MigrationRegistry, Store};
use crate::members::MemberRegistry;
use crate::tasks::{ExecutionStart, TaskRegistry, TaskStatus};
use crate::terminal::{StopPolicy, TerminalRegistry};

/// The `office_host` migration domain: host registry and recovery records.
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

/// Socket file name inside `VIVA_HOME`.
pub const OFFICE_SOCKET_NAME: &str = "office.sock";

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
}

/// A claimed, running Office host.
pub struct OfficeHost {
    shared: Arc<OfficeShared>,
    listener: UnixListener,
}

impl OfficeHost {
    /// Open the store and claim the single active-host slot for this
    /// `VIVA_HOME`. Refuses when a healthy host already listens on the
    /// socket; claims a stale socket only with a recovery record.
    pub fn open(home: &Path) -> OfficeResult<Self> {
        std::fs::create_dir_all(home)?;
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
    /// are served), stop every owned terminal, persist the handoff, mark
    /// the host exited, release the channel.
    fn shutdown_shared(shared: &OfficeShared) -> OfficeResult<()> {
        let store = shared.store.lock().expect("office store");
        let terminals = shared.terminals.list();
        let terminal_count = terminals.len();
        for entry in terminals {
            let _ = shared
                .terminals
                .stop(&entry.terminal_id, StopPolicy::default(), Some(&store));
        }
        store.connection().execute(
            "UPDATE office_hosts SET exited_at = ?2, exit_kind = 'graceful'
             WHERE host_id = ?1 AND exited_at IS NULL",
            rusqlite::params![shared.host_id, utc_now()],
        )?;
        record_recovery(
            &store,
            &shared.host_id,
            "graceful_handoff",
            &format!(
                "host stopped {} owned terminal(s); state persisted for the next host",
                terminal_count
            ),
        )?;
        drop(store);
        let _ = std::fs::remove_file(&shared.socket_path);
        Ok(())
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
        let request: OfficeRequest = serde_json::from_str(&line)
            .map_err(|e| OfficeError::Validation(format!("malformed control request: {e}")))?;
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
        let is_shutdown = matches!(request.kind, OfficeRequestKind::Shutdown);
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
        OfficeRequestKind::TaskResults { task_id } => task_results(&shared, task_id),
        OfficeRequestKind::Handoff {
            task_id,
            member_id,
            summary,
        } => handoff(&shared, task_id, member_id, summary),
        OfficeRequestKind::Shutdown => Ok(serde_json::json!({"shutting_down": true})),
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
                    .map(|h| h.try_wait().ok().flatten().is_none()),
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "host_id": shared.host_id,
        "pid": std::process::id(),
        "home": shared.home.display().to_string(),
        "stopping": shared.stopping.load(Ordering::SeqCst),
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

    // Authorization: the grant must be live, in scope for this task, and
    // carry a dispatch action in an ACT_* mode. Revocation races are
    // denied here, at the moment of effect — not at request time. `check`
    // layers two results: storage failures and the authorization verdict
    // itself; both must be honored.
    let authority = AuthorityEngine::new(&store);
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
            // Idempotency by request key: nothing duplicated.
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
    // real exit fact. The owning Arc keeps the store alive even after the
    // host struct itself is gone.
    let watcher_shared = Arc::clone(shared);
    let watcher_key = request_key.to_string();
    let watcher_task = task_id.clone();
    let watcher_execution = execution_id.clone();
    let watcher_handle = std::sync::Arc::clone(&handle);
    std::thread::spawn(move || {
        if let Ok(exit) = watcher_handle.wait() {
            // A signal exit carries no exit code; the intent protocol's
            // integer field records it as -1 rather than staying silent —
            // a stopped process must always land in the ledger.
            let code = exit.code.unwrap_or(-1);
            let store = watcher_shared.store.lock().expect("office store");
            let tasks = TaskRegistry::new(&store);
            let _ = tasks.record_exit(&watcher_key, code);
            {
                let _ = tasks.record_result(
                    &watcher_task,
                    Some(watcher_execution),
                    crate::tasks::ResultSource::Process,
                    "process_exit",
                    format!("{watcher_key}:exit"),
                    serde_json::json!({ "exit_code": code }),
                );
            }
        }
    });

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
            "no active office in {} (mutation rejected; start one with `viva office start`)",
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
    let store = Store::open(
        &crate::foundation::paths::database_path(home),
        office_migrations(),
    )?;
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
