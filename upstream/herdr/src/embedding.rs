// Viva #191 integration glue. Standalone Herdr keeps its original paths and
// behavior; embedding is explicit and does not rewrite HOME/XDG/HERDR_*.
use std::{
    io,
    path::PathBuf,
    process::Command,
    sync::{Arc, OnceLock},
};

mod owner_credential;
pub use owner_credential::{owner_credential_for_host, OwnerAnchorHostMismatch};

/// The stable public pane id for a restored workspace pane. Viva needs it to
/// bind an adopted legacy execution to the pane id the runtime will report on
/// exit; it is the same encoding the runtime's own public API uses.
pub fn public_pane_id(workspace_id: &str, pane_number: usize) -> String {
    crate::workspace::public_pane_id_for_number(workspace_id, pane_number)
}

/// Narrow dependency inversion: the runtime does not own Viva domain records.
/// No dispatch/stop capability is exposed until ingress authority is integrated.
pub trait DomainServices: Send + Sync {
    fn server_started(&self) -> io::Result<()>;
    fn server_ready(&self) -> io::Result<()>;
    /// Viva modification (#201): checked on the serialized app queue BEFORE
    /// disconnecting clients or pausing PTY readers. No member credential loss.
    fn prepare_handoff(&self) -> io::Result<()>;
    fn handoff_rolled_back(&self) -> io::Result<()>;
    /// Actual negotiated raw frontend connection, not JSON authentication.
    fn raw_client_connection(
        &self,
        id: u64,
        peer: TransportPeer,
        connected: bool,
    ) -> io::Result<()>;
    fn client_credential(&self) -> io::Result<String>;
    /// Resolve this host incarnation's local runtime capability. A reattaching
    /// TUI has no inherited spawn descriptor; the composition root loads the
    /// private runtime file for the identity the running host reports. This
    /// is capability possession, not human-presence authentication.
    fn owner_anchor_for_host(&self, host_id: &str) -> io::Result<String>;
    fn authenticate_client(
        &self,
        credential: &str,
        expected_lifetime: Option<&str>,
        peer: TransportPeer,
    ) -> io::Result<()>;
    fn validate_client(&self, peer: TransportPeer) -> io::Result<()>;
    fn snapshot(&self) -> io::Result<serde_json::Value>;
    /// Viva modification: canonical navigation, after controller admission.
    fn workspace(
        &self,
        _action: &str,
        _value: Option<&str>,
        _project: Option<&str>,
    ) -> io::Result<serde_json::Value> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Viva workspace navigation is unavailable",
        ))
    }
    /// Prepare one canonical execution. The composition root performs the
    /// task/member/grant authority checks and records the intent BEFORE the
    /// runtime spawns anything. `replayed` means never spawn a second process.
    fn prepare_execution(&self, request: ExecutionRequest) -> io::Result<ExecutionPlan>;
    /// Confirm the real OS child that is now running this execution.
    fn execution_started(
        &self,
        execution_id: &str,
        workspace_id: &str,
        pane_id: &str,
        terminal_id: &str,
        pid: u32,
    ) -> io::Result<()>;
    /// The spawn failed after preparation: leave the execution unresolved.
    fn execution_failed(&self, execution_id: &str) -> io::Result<()>;
    /// A pane owned by an execution died. This is a process fact, never a
    /// task completion and never inferred from screen contents.
    fn execution_exited(
        &self,
        workspace_id: &str,
        pane_id: &str,
        exit_code: Option<i32>,
    ) -> io::Result<()>;
    /// Owner-gated stop: verify the execution is one this host owns and is
    /// live, then return the pane that must be closed. The close itself goes
    /// through the normal pane-death path so the exit is recorded as a fact.
    fn prepare_execution_stop(&self, execution_id: &str) -> io::Result<ExecutionStopPlan>;
    /// Persisted pause gate for NEW dispatches only; running executions are
    /// never touched by pause.
    fn set_paused(&self, paused: bool) -> io::Result<()>;
    /// One canonical, member-attributed report. The composition root must
    /// authenticate the host-issued worker credential, bind the calling OS
    /// process to the confirmed execution, and check the live grant before
    /// any canonical write. Returns the canonical event sequence.
    fn member_report(&self, request: MemberReportRequest, peer: TransportPeer) -> io::Result<i64>;
    /// Viva P2: dispatched only from the server's OS peer, not JSON claims.
    fn prepare_member_execution(
        &self,
        _request: ExecutionRequest,
        _credential: &str,
        _grant: Option<&str>,
        _peer: TransportPeer,
    ) -> io::Result<ExecutionPlan> {
        Err(io::Error::other("member dispatch unavailable"))
    }
    fn member_handoff(
        &self,
        _credential: &str,
        _grant: Option<&str>,
        _detail: &str,
        _task: Option<&str>,
        _member: Option<&str>,
        _peer: TransportPeer,
    ) -> io::Result<i64> {
        Err(io::Error::other("member handoff unavailable"))
    }
    fn redact_persistence(&self, value: &serde_json::Value) -> io::Result<serde_json::Value> {
        Ok(value.clone())
    }
    fn redact_log_chunk(&self, bytes: &[u8]) -> io::Result<Vec<u8>> {
        Ok(bytes.to_vec())
    }
    fn flush_log(&self) -> io::Result<Vec<u8>> {
        Ok(Vec::new())
    }
    fn refused(&self, method: &'static str, peer: Option<TransportPeer>) -> io::Result<()>;
}

/// Domain facts required before a native process may start. The runtime does
/// not authenticate these IDs itself; the composition root does.
#[derive(Clone, Debug)]
pub struct ExecutionRequest {
    pub task_id: String,
    pub member_id: String,
    pub grant_id: String,
    pub request_key: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub worktree_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ExecutionStopPlan {
    pub execution_id: String,
    pub workspace_id: Option<String>,
    pub pane_id: Option<String>,
}

/// A member-initiated report. `credential` is a host-issued bearer secret; it
/// is never an identity claim and never written to events or logs.
#[derive(Clone)]
pub struct MemberReportRequest {
    pub credential: String,
    pub status: String,
    pub detail: String,
}
impl std::fmt::Debug for MemberReportRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .write_str("MemberReportRequest { credential: [REDACTED], status, detail: [REDACTED] }")
    }
}

/// What the runtime may launch after canonical preparation. The environment
/// carries context plus the host-issued member credential for the bound
/// process; a grant ID is context, never a credential or user authority.
#[derive(Clone)]
pub struct ExecutionPlan {
    pub execution_id: String,
    pub replayed: bool,
    /// Intent state at the time of the replay (`confirmed`, `exited`, ...);
    /// freshly prepared plans report `intended` and the handler reports the
    /// post-spawn `confirmed` state.
    pub intent_state: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub label: String,
    pub existing_pid: Option<u32>,
}
impl std::fmt::Debug for ExecutionPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionPlan")
            .field("execution_id", &self.execution_id)
            .field("replayed", &self.replayed)
            .field("intent_state", &self.intent_state)
            .field("argv_env_label", &"[REDACTED]")
            .finish()
    }
}

/// OS-reported transport facts, NOT a member identity or owner credential.
/// PID reuse/inherited descriptors require lifecycle validation before authority.
/// Deliberately not Deserialize: request JSON cannot construct this value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportPeer {
    pub pid: Option<u32>,
    pub uid: Option<u32>,
}

/// Revalidate this at admission: a PID/UID alone is not a live process binding.
pub fn peer_lifetime_marker(peer: TransportPeer) -> io::Result<String> {
    let pid = peer.pid.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "connection PID unavailable",
        )
    })?;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        crate::platform::process_lifetime_marker(pid).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "connection process lifetime unavailable",
            )
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native process binding unavailable",
        ))
    }
}

pub(crate) fn server_ready() -> io::Result<()> {
    if let Some(ctx) = context() {
        ctx.domain.server_ready()?;
    }
    Ok(())
}

pub(crate) fn raw_client_connection(
    id: u64,
    stream: &crate::ipc::LocalStream,
    connected: bool,
) -> io::Result<()> {
    if let Some(ctx) = context() {
        ctx.domain
            .raw_client_connection(id, crate::ipc::transport_peer(stream)?, connected)?;
    }
    Ok(())
}

pub(crate) fn authenticate_client(
    credential: &str,
    expected_lifetime: Option<&str>,
    stream: &crate::ipc::LocalStream,
) -> io::Result<()> {
    let ctx = context().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Viva embedding unavailable",
        )
    })?;
    ctx.domain.authenticate_client(
        credential,
        expected_lifetime,
        crate::ipc::transport_peer(stream)?,
    )
}

pub(crate) fn validate_client(stream: &crate::ipc::LocalStream) -> io::Result<()> {
    if let Some(ctx) = context() {
        ctx.domain
            .validate_client(crate::ipc::transport_peer(stream)?)?;
    }
    Ok(())
}

/// The host identity the *running* server advertises. The client process has
/// its own domain bridge but not the server's identity, so this asks the live
/// endpoint instead of reading a local field.
fn running_host_id(timeout: std::time::Duration) -> io::Result<String> {
    use crate::api::client::{parse_response_value, ApiClient};
    let request = crate::api::schema::Request {
        id: "viva.owner-anchor-host-id".into(),
        method: crate::api::schema::Method::VivaSnapshot(crate::api::schema::EmptyParams {}),
    };
    let response = ApiClient::local()
        .request_value_with_timeout(&request, timeout)
        .and_then(parse_response_value)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let value = serde_json::to_value(&response.result).map_err(io::Error::other)?;
    value
        .get("snapshot")
        .and_then(|snapshot| snapshot.get("runtime_host_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("running host has no runtime identity"))
}

/// The anchor this process can present. A spawning TUI holds it in memory or
/// over an inherited descriptor; a reattaching TUI resolves it from the private
/// runtime store for the host identity the running host advertises.
fn owner_credential(
    known: io::Result<String>,
    deadline: std::time::Instant,
    running_host: impl FnMut(std::time::Duration) -> io::Result<String>,
    load: impl FnMut(&str) -> io::Result<String>,
) -> io::Result<String> {
    match known {
        Ok(anchor) => Ok(anchor),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            owner_credential_for_host(deadline, running_host, load)
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn authenticate_local_client() -> io::Result<()> {
    let Some(ctx) = context() else {
        return Ok(());
    };
    // The client socket can appear just before the ready capability is published.
    // Retry only readiness or an anchor the host no longer accepts after a new
    // incarnation; never an unchanged bad credential and never an effect.
    use crate::api::client::{parse_response_value, ApiClient, ApiClientError};
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let credential = match owner_credential(
            ctx.domain.client_credential(),
            deadline,
            running_host_id,
            |host_id| ctx.domain.owner_anchor_for_host(host_id),
        ) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(error) => return Err(error),
        };
        let request = crate::api::schema::Request {
            id: "viva.client-authenticate".into(),
            method: crate::api::schema::Method::VivaClientAuthenticate(
                crate::api::schema::VivaClientAuthenticateParams {
                    credential: credential.clone(),
                    process_lifetime: Some(peer_lifetime_marker(TransportPeer {
                        pid: Some(std::process::id()),
                        uid: None,
                    })?),
                },
            ),
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "native client authentication readiness timed out",
            ));
        }
        match ApiClient::local()
            .request_value_with_timeout(&request, remaining)
            .and_then(parse_response_value)
        {
            Ok(_) => return Ok(()),
            Err(ApiClientError::ErrorResponse(error))
                if error.error.code == "client_not_ready" && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(ApiClientError::ErrorResponse(error))
                if matches!(
                    error.error.code.as_str(),
                    "untrusted_client" | "stale_owner_anchor"
                ) && Instant::now() < deadline
                    && ctx
                        .domain
                        .client_credential()
                        .is_ok_and(|latest| latest != credential) =>
            {
                // The anchor changed under us (a new host incarnation). Reload
                // the changed proof; do not retry an unchanged bad key.
                continue;
            }
            Err(ApiClientError::ErrorResponse(error))
                if error.error.code == "stale_owner_anchor" && Instant::now() < deadline =>
            {
                // A spawned or runtime-stored anchor belongs to a dead host
                // incarnation. Drop it and resolve the live host's anchor once.
                clear_spawned_owner_anchor();
                continue;
            }
            Err(error) => return Err(io::Error::other(error)),
        }
    }
}

pub struct Context {
    pub directory: PathBuf,
    pub arguments: Vec<String>,
    pub child_prefix: Vec<String>,
    /// Candidate owner anchor for the server this process may spawn. Delivered
    /// over an inherited descriptor; never written to disk by the client.
    pub owner_anchor: Option<String>,
    pub domain: Arc<dyn DomainServices>,
    /// Cross-backend inheritance prepared by the composition root. The
    /// server bootstrap consumes it; the TUI process leaves it empty.
    #[cfg(unix)]
    pub legacy_import: std::sync::Mutex<Option<LegacyImport>>,
}

/// A fully prepared takeover of a live legacy Viva host: the snapshot Herdr
/// restores plus one owned PTY master per pane. Building this value already
/// received the fds; nothing here may be acknowledged before adoption.
#[cfg(unix)]
pub struct LegacyImport {
    pub snapshot: crate::persist::SessionSnapshot,
    pub panes: Vec<LegacyImportPane>,
    /// Sends the prepared ACK that retires the old host. The bootstrap calls
    /// it exactly once, after every pane is adopted and both endpoints serve.
    pub confirm: Box<dyn FnOnce() -> io::Result<()> + Send>,
}

/// One adopted legacy pane. `master_fd` ownership transfers to the runtime
/// when `TerminalRuntime::from_handoff_fd` takes it.
#[cfg(unix)]
pub struct LegacyImportPane {
    pub pane_id: u32,
    pub master_fd: std::os::fd::RawFd,
    pub child_pid: u32,
    pub rows: u16,
    pub cols: u16,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    pub initial_history_ansi: Option<String>,
}

// The snapshot shape is built by the composition root, so the exact same
// structs Herdr persistence uses are part of the embedding contract.
#[cfg(unix)]
pub use crate::persist::{
    DirectionSnapshot, LayoutSnapshot, PaneSnapshot, SessionSnapshot, TabSnapshot,
    WorkspaceSnapshot, SNAPSHOT_VERSION,
};

static CONTEXT: OnceLock<Context> = OnceLock::new();

/// The anchor handed to the server this process spawned. The client keeps it
/// only in memory so it can authenticate to its own child.
static SPAWNED_OWNER_ANCHOR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn spawned_owner_anchor() -> Option<String> {
    SPAWNED_OWNER_ANCHOR.lock().ok()?.clone()
}

pub(crate) fn set_spawned_owner_anchor(anchor: &str) {
    if let Ok(mut slot) = SPAWNED_OWNER_ANCHOR.lock() {
        *slot = Some(anchor.to_owned());
    }
}

pub(crate) fn clear_spawned_owner_anchor() {
    if let Ok(mut slot) = SPAWNED_OWNER_ANCHOR.lock() {
        *slot = None;
    }
}

/// Deliver the candidate anchor to a server we are about to spawn over a pipe
/// that is not close-on-exec. Returns the parent's read end, which the spawner
/// must close after the child exists so only the child holds the descriptor.
#[cfg(unix)]
pub(crate) fn prepare_owner_anchor(
    command: &mut Command,
) -> io::Result<Option<std::os::fd::RawFd>> {
    use std::io::Write;
    use std::os::fd::FromRawFd;
    let Some(anchor) = context().and_then(|ctx| ctx.owner_anchor.clone()) else {
        return Ok(None);
    };
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    let written = {
        let mut pipe = unsafe { std::fs::File::from_raw_fd(write_fd) };
        pipe.write_all(format!("{anchor}\n").as_bytes())
    };
    if let Err(error) = written {
        unsafe {
            libc::close(read_fd);
        }
        return Err(error);
    }
    if unsafe { libc::fcntl(read_fd, libc::F_SETFD, 0) } < 0 {
        let error = io::Error::last_os_error();
        unsafe {
            libc::close(read_fd);
        }
        return Err(error);
    }
    command.env("VIVA_OWNER_ANCHOR_FD", read_fd.to_string());
    set_spawned_owner_anchor(&anchor);
    Ok(Some(read_fd))
}

#[cfg(not(unix))]
pub(crate) fn prepare_owner_anchor(_command: &mut Command) -> io::Result<Option<i32>> {
    Ok(None)
}

pub fn context() -> Option<&'static Context> {
    CONTEXT.get()
}

/// Move the prepared legacy takeover out of the context. `None` in the TUI
/// process and for ordinary native starts.
#[cfg(unix)]
pub(crate) fn take_legacy_import() -> Option<LegacyImport> {
    context()?.legacy_import.lock().ok()?.take()
}
/// Viva modification (#191 P1): domain CLI autostart uses the same detached
/// daemon setup as the native frontend, never the client's controlling TTY.
pub fn configure_host_daemon(command: &mut std::process::Command) {
    crate::platform::detach_server_daemon_command(command);
}

pub fn enabled() -> bool {
    context().is_some()
}

/// Development composition entry, not the installed default or a release gate.
/// A second call in one process is rejected rather than changing live authority.
pub fn run(context: Context) -> io::Result<()> {
    if let Err(error) = validate_arguments(&context.arguments) {
        context.domain.refused("native_cli.unsupported", None)?;
        return Err(error);
    }
    std::fs::create_dir_all(&context.directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&context.directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let args = context.arguments.clone();
    CONTEXT
        .set(context)
        .map_err(|_| io::Error::other("native runtime context already installed"))?;
    crate::run_args(args)
}

fn validate_arguments(args: &[String]) -> io::Result<()> {
    // These operations must be integrated with Viva first. In particular, never
    // let Herdr's updater replace a Viva executable or use an external bridge.
    // Viva modification (#201): only the internal server form may import.
    if args.iter().any(|arg| arg == "--handoff-import")
        && !(args.len() == 5 && args[1] == "server" && args[2] == "--handoff-import")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid handoff import arguments",
        ));
    }
    let supported_namespace = matches!(
        args.get(1).map(String::as_str),
        None | Some(
            "server"
                | "pane"
                | "tab"
                | "workspace"
                | "api"
                | "status"
                | "--version"
                | "-V"
                | "version"
                | "--help"
                | "-h"
                | "help"
        )
    );
    if args.iter().skip(1).any(|arg| {
        arg == "--session"
            || arg.starts_with("--session=")
            || arg == "--remote"
            || arg.starts_with("--remote=")
    }) || !supported_namespace
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "native operation is not integrated with Viva authority; no legacy UI fallback",
        ));
    }
    Ok(())
}

pub(crate) fn arguments() -> Vec<String> {
    context()
        .map(|ctx| ctx.arguments.clone())
        .unwrap_or_else(|| std::env::args().collect())
}

pub(crate) fn child_command(executable: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(executable);
    if let Some(ctx) = context() {
        command.args(&ctx.child_prefix);
    }
    command
}

pub(crate) fn server_started() -> io::Result<()> {
    if let Some(ctx) = context() {
        ctx.domain.server_started()?;
    }
    Ok(())
}

pub(crate) fn snapshot() -> io::Result<serde_json::Value> {
    context()
        .ok_or_else(|| io::Error::other("Viva domain services are not embedded"))?
        .domain
        .snapshot()
}

// Viva modification (#191 P1): reuse Viva Navigation rather than a second registry.
pub(crate) fn workspace(
    action: &str,
    value: Option<&str>,
    project: Option<&str>,
) -> io::Result<serde_json::Value> {
    context()
        .ok_or_else(|| io::Error::other("Viva domain unavailable"))?
        .domain
        .workspace(action, value, project)
}

pub(crate) fn prepare_execution(request: ExecutionRequest) -> io::Result<ExecutionPlan> {
    context()
        .ok_or_else(|| io::Error::other("Viva domain services are not embedded"))?
        .domain
        .prepare_execution(request)
}

pub(crate) fn execution_started(
    execution_id: &str,
    workspace_id: &str,
    pane_id: &str,
    terminal_id: &str,
    pid: u32,
) -> io::Result<()> {
    context()
        .ok_or_else(|| io::Error::other("Viva domain services are not embedded"))?
        .domain
        .execution_started(execution_id, workspace_id, pane_id, terminal_id, pid)
}

pub(crate) fn execution_failed(execution_id: &str) -> io::Result<()> {
    context()
        .ok_or_else(|| io::Error::other("Viva domain services are not embedded"))?
        .domain
        .execution_failed(execution_id)
}

pub(crate) fn execution_pane_exited(
    workspace_id: &str,
    pane_id: &str,
    exit_code: Option<i32>,
) -> io::Result<()> {
    match context() {
        Some(ctx) => ctx
            .domain
            .execution_exited(workspace_id, pane_id, exit_code),
        None => Ok(()),
    }
}

pub(crate) fn prepare_execution_stop(execution_id: &str) -> io::Result<ExecutionStopPlan> {
    context()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Viva embedding unavailable",
            )
        })?
        .domain
        .prepare_execution_stop(execution_id)
}

pub(crate) fn set_paused(paused: bool) -> io::Result<()> {
    context()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Viva embedding unavailable",
            )
        })?
        .domain
        .set_paused(paused)
}

pub(crate) fn member_report(
    params: &crate::api::schema::VivaMemberReportParams,
    stream: &crate::ipc::LocalStream,
) -> io::Result<i64> {
    let ctx = context().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Viva embedding unavailable",
        )
    })?;
    // Captured from the accepted connection, never a JSON peer claim. The
    // composition root additionally verifies the caller is the confirmed
    // process (or a descendant) before any canonical write.
    let peer = crate::ipc::transport_peer(stream)?;
    ctx.domain.member_report(
        MemberReportRequest {
            credential: params.credential.clone(),
            status: params.status.clone(),
            detail: params.detail.clone(),
        },
        peer,
    )
}

// Viva P2: scrub complete structured values; tracing uses a streaming redactor.
pub(crate) fn redact_persistence(value: &serde_json::Value) -> io::Result<serde_json::Value> {
    match context() {
        Some(ctx) => ctx.domain.redact_persistence(value),
        None => Ok(value.clone()),
    }
}
pub(crate) fn redact_log_chunk(bytes: &[u8]) -> io::Result<Vec<u8>> {
    match context() {
        Some(ctx) => ctx.domain.redact_log_chunk(bytes),
        None => Ok(bytes.to_vec()),
    }
}
pub(crate) fn flush_log() -> io::Result<Vec<u8>> {
    match context() {
        Some(ctx) => ctx.domain.flush_log(),
        None => Ok(Vec::new()),
    }
}

pub(crate) fn constrain_config(config: &mut crate::config::Config) {
    if enabled() {
        config.update.version_check = false;
        config.update.manifest_check = false;
        config.onboarding = Some(false);
        // Native output is not yet integrated with Viva persistence redaction.
        config.experimental.pane_history = false;
    }
}

pub(crate) fn admit_from_socket(
    request: &crate::api::schema::Request,
    stream: &crate::ipc::LocalStream,
) -> io::Result<()> {
    if !enabled() {
        return Ok(());
    }
    // Captured from the accepted connection, never a request/pane/env claim.
    let peer = crate::ipc::transport_peer(stream).ok();
    admit_with_peer(request, peer)
}

pub(crate) fn admit(request: &crate::api::schema::Request) -> io::Result<()> {
    admit_with_peer(request, None)
}

fn admit_with_peer(
    request: &crate::api::schema::Request,
    peer: Option<TransportPeer>,
) -> io::Result<()> {
    let Some(ctx) = context() else {
        return Ok(());
    };
    let method = crate::api::api_method_name(&request.method);
    if matches!(
        method,
        "ping"
            | "viva.snapshot"
            | "viva.client.authenticate"
            | "client_shell.surface.set"
            | "client.window_title.set"
            | "client.window_title.clear"
            // Observation is not authority: these read the live grid and OS
            // process facts, exactly what the same-uid legacy client could
            // ask the old host. They mutate nothing and are what makes an
            // inherited session verifiable after a takeover.
            | "pane.list"
            | "pane.read"
            | "pane.process_info"
    ) {
        return Ok(());
    }
    if matches!(
        method,
        "viva.execution.start"
            | "viva.execution.stop"
            | "viva.host.pause"
            | "viva.workspace"
            | "server.stop"
            | "server.live_handoff"
    ) {
        // The socket path must present the host-issued owner capability. The
        // no-peer form is only the second gate after that admission (or the
        // already-authenticated client-shell connection).
        return match peer {
            Some(peer) => match ctx.domain.validate_client(peer) {
                Ok(()) => Ok(()),
                Err(error) => {
                    ctx.domain.refused(method, Some(peer))?;
                    Err(error)
                }
            },
            None => Ok(()),
        };
    }
    if matches!(method, "viva.member.report" | "viva.member.effect") {
        // Reachable without owner capability by design: the member process is
        // not the owner frontend. The typed handler is the gate — it verifies
        // the host-issued worker credential, the confirmed process lineage and
        // the live grant before any write, and audits every refusal.
        return Ok(());
    }
    // Never accept caller/member/grant claims from pane metadata or JSON params.
    // Unknown automated ingress is unbound, NOT the owner. Log only the schema's
    // fixed method name, never untrusted request IDs, tokens or command arguments.
    ctx.domain.refused(method, peer)?;
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "native automation ingress is not yet bound to Viva authority; operation refused",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_frontend_resolves_a_confirmed_snapshot_capability_rotation() {
        use std::time::{Duration, Instant};
        let mut snapshots = ["native-a", "native-b"].into_iter();
        let mut loads = Vec::new();
        let credential = owner_credential(
            Err(io::Error::from(io::ErrorKind::NotFound)),
            Instant::now() + Duration::from_secs(1),
            |_| {
                Ok(snapshots
                    .next()
                    .expect("bounded identity checks")
                    .to_owned())
            },
            |host| {
                loads.push(host.to_owned());
                if host == "native-a" {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        OwnerAnchorHostMismatch,
                    ))
                } else {
                    Ok("proof-for-b".to_owned())
                }
            },
        )
        .unwrap();
        assert_eq!(credential, "proof-for-b");
        assert_eq!(loads, ["native-a", "native-b"]);
        assert!(snapshots.next().is_none());
    }

    #[test]
    fn inherited_proof_and_non_readiness_errors_never_fall_back_to_runtime_file() {
        use std::time::Instant;
        for known in [
            Ok("inherited-proof".to_owned()),
            Err(io::Error::from(io::ErrorKind::PermissionDenied)),
        ] {
            let expected = known
                .as_ref()
                .map(String::as_str)
                .map_err(|error| error.kind());
            let result = owner_credential(
                known
                    .as_ref()
                    .map(Clone::clone)
                    .map_err(|error| io::Error::from(error.kind())),
                Instant::now(),
                |_| panic!("must not query a host"),
                |_| panic!("must not read a runtime file"),
            );
            assert_eq!(
                result
                    .as_ref()
                    .map(String::as_str)
                    .map_err(|error| error.kind()),
                expected
            );
        }
    }

    #[test]
    fn embedded_entry_refuses_unintegrated_namespaces() {
        for args in [
            vec!["viva", "update"],
            vec!["viva", "--session=x"],
            vec!["viva", "--remote", "host"],
            vec!["viva", "plugin", "run"],
        ] {
            assert_eq!(
                validate_arguments(&args.into_iter().map(str::to_owned).collect::<Vec<_>>())
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        assert!(validate_arguments(&["viva".into(), "server".into()]).is_ok());
    }
}
