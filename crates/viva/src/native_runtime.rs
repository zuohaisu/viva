//! Development-only native composition (#191). Herdr owns the PTYs and renderer;
//! Viva owns member/task facts and refusal auditing. This is deliberately not
//! wired to the installed default until authority and cross-backend import pass.
mod client_authority;

use crate::{
    authority::AuthorityEngine,
    foundation::{
        envelope::{CallerIdentity, CallerRole, ChannelRegistry},
        error::{Error, Result},
        events::{self, NewEvent},
        ids::{ChannelId, ExecutionId, GrantId, MemberId, TaskId, TerminalId, WorktreeId, utc_now},
        paths,
        records::{self, LaunchSpec, RequestOrigin, get_execution},
        store::Store,
    },
    members::MemberRegistry,
    tasks::{DispatchRequest, ExecutionStart, ResultSource, TaskRegistry},
};
use herdr::embedding::{ExecutionPlan, ExecutionRequest};
use rusqlite::OptionalExtension;
use std::{
    collections::{HashMap, HashSet},
    io,
    path::PathBuf,
    str::FromStr,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
struct ExecutionBinding {
    execution_id: ExecutionId,
    task_id: TaskId,
    member_id: MemberId,
    grant_id: GrantId,
    request_key: String,
    argv: Vec<String>,
    cwd: PathBuf,
    worktree_id: Option<String>,
    workspace_id: Option<String>,
    pane_id: Option<String>,
    /// Host-issued MemberExecution channel. In-memory only: a restart cannot
    /// revive it, so member ingress fails closed instead of trusting DB alone.
    channel_id: ChannelId,
    /// Host-issued worker credential (bearer secret). Never written to events.
    credential_id: String,
    pid: Option<i64>,
    pid_start_marker: Option<String>,
    /// A deliberate owner stop was requested; the close path records the exit.
    stop_requested: bool,
}

#[derive(Default)]
struct NativeExecutionState {
    by_execution: HashMap<String, ExecutionBinding>,
    by_pane: HashMap<(String, String), String>,
    owned: HashSet<String>,
}

struct DomainBridge {
    home: PathBuf,
    host_id: String,
    store: Mutex<Option<Store>>,
    clients: Mutex<client_authority::ClientAuthority>,
    channels: Mutex<ChannelRegistry>,
    executions: Mutex<NativeExecutionState>,
}

/// Fixed grant action for a member progress report. The dispatch grant must
/// name it; the canonical grant checks then apply the usual principal, task,
/// live-status and mode rules.
const MEMBER_REPORT_ACTION: &str = "member_report";
const MEMBER_REPORT_STATUSES: [&str; 4] = ["progress", "blocked", "failed", "done"];
const MEMBER_REPORT_DETAIL_LIMIT: usize = 2000;

/// Checks that assume the native execution state lock is already held. The
/// lock order is always native-execution state -> store.
fn owner_only_phase_locked(store: &Store, state: &NativeExecutionState) -> Result<()> {
    // Active worker credentials are only acceptable when this native host
    // issued them for an execution it owns. Foreign credentials still block
    // the preview (fail closed); this is what lets the first bound member
    // ingress coexist with the owner frontend gate.
    let mut stmt = store
        .connection()
        .prepare("SELECT execution_id FROM worker_credentials WHERE status='active'")?;
    let active_credentials = stmt
        .query_map([], |row| row.get::<_, Option<String>>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if active_credentials.iter().any(|execution| {
        execution
            .as_deref()
            .is_none_or(|id| !state.owned.contains(id))
    }) {
        return Err(Error::Validation(
            "native owner frontend capability is refused while worker credentials outside this host's executions remain active".into(),
        ));
    }
    let mut stmt = store
        .connection()
        .prepare("SELECT execution_id FROM executions WHERE status IN ('requested','running')")?;
    let active = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if active.iter().any(|id| !state.owned.contains(id)) {
        return Err(Error::Validation(
            "native member/process bindings are not integrated; owner frontend capability is refused while unowned executions are pending/live".into(),
        ));
    }
    Ok(())
}

fn paused(store: &Store) -> Result<bool> {
    let paused: Option<String> = store
        .connection()
        .query_row(
            "SELECT value FROM settings WHERE key = 'paused'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(paused.as_deref() == Some("true"))
}

fn refused_event(
    store: &Store,
    task: &TaskId,
    member: &MemberId,
    grant: &GrantId,
    detail: &str,
) -> Result<()> {
    let detail: String = detail.chars().take(500).collect();
    events::append(
        store,
        NewEvent {
            domain: crate::server::DOMAIN_HOST,
            kind: "native_execution_refused".into(),
            subject_type: "execution_request".into(),
            subject_id: uuid::Uuid::new_v4().to_string(),
            origin: "native_host".into(),
            payload: serde_json::json!({
                "task_id": task.as_str(),
                "member_id": member.as_str(),
                "grant_id": grant.as_str(),
                "reason": "canonical_preparation_refused",
                "detail": detail,
            }),
        },
    )?;
    Ok(())
}

/// OS parent PID. Used only for the member ingress process binding; it is
/// never an identity on its own.
fn process_parent(pid: i64) -> Option<i64> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after_comm = stat.rsplit_once(") ")?.1;
        // Fields after `comm`: state ppid pgrp ...
        after_comm.split_whitespace().nth(1)?.parse().ok()
    }
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let written = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        (written == size).then_some(info.pbi_ppid as i64)
    }
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
    {
        let output = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "ppid="])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8_lossy(&output.stdout).trim().parse().ok()
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

/// The member ingress accepts the confirmed pane root or a process descended
/// from it. This is a lineage check, not an OS sandbox claim.
fn is_bound_process(root: i64, caller: i64) -> bool {
    if root <= 1 || caller <= 1 {
        return false;
    }
    let mut current = caller;
    for _ in 0..32 {
        if current == root {
            return true;
        }
        match process_parent(current) {
            Some(parent) if parent > 1 && parent != current => current = parent,
            _ => return false,
        }
    }
    false
}

fn refused_member_report(
    store: &Store,
    execution_id: Option<&str>,
    member_id: Option<&str>,
    grant_id: Option<&str>,
    reason: &str,
) -> Result<()> {
    // Fixed reason markers only: never the credential, detail or request body.
    events::append(
        store,
        NewEvent {
            domain: crate::server::DOMAIN_HOST,
            kind: "native_member_report_refused".into(),
            subject_type: "execution".into(),
            subject_id: execution_id.unwrap_or("unbound").to_string(),
            origin: "native_host".into(),
            payload: serde_json::json!({
                "execution_id": execution_id,
                "member_id": member_id,
                "grant_id": grant_id,
                "reason": reason,
            }),
        },
    )?;
    Ok(())
}

fn process_start_marker(pid: i64) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after_comm = stat.rsplit_once(") ")?.1;
        after_comm.split_whitespace().nth(19).map(str::to_string)
    }
    #[cfg(target_os = "macos")]
    {
        // OS birth identity via libproc, not a spawned `ps` (which may be
        // unavailable in the host's restricted environment).
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let written = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if written == size {
            Some(format!(
                "{}.{}",
                info.pbi_start_tvsec, info.pbi_start_tvusec
            ))
        } else {
            None
        }
    }
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
    {
        let output = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let marker = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!marker.is_empty()).then_some(marker)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

impl DomainBridge {
    fn require_owner_only_phase(&self) -> io::Result<()> {
        let state = self
            .executions
            .lock()
            .map_err(|_| io::Error::other("native execution state poisoned"))?;
        self.with_store(|store| owner_only_phase_locked(store, &state))
    }

    fn with_store<T>(&self, f: impl FnOnce(&Store) -> Result<T>) -> io::Result<T> {
        let mut slot = self
            .store
            .lock()
            .map_err(|_| io::Error::other("native domain lock poisoned"))?;
        if slot.is_none() {
            paths::ensure_private_dir(&self.home)?;
            let path = paths::database_path(&self.home);
            // Existing data is never migrated/reconciled by this preview. Reuse
            // exact-schema attach; normal crash reconciliation would incorrectly
            // stop executions whose live ownership has not been integrated yet.
            *slot = Some(
                if path.exists() {
                    Store::attach_for_handoff(&path, crate::server::migrations())
                } else {
                    Store::open(&path, crate::server::migrations())
                }
                .map_err(io::Error::other)?,
            );
        }
        f(slot.as_ref().expect("store initialized")).map_err(io::Error::other)
    }
}

impl herdr::embedding::DomainServices for DomainBridge {
    fn server_started(&self) -> io::Result<()> {
        self.with_store(|store| {
            events::append(store, NewEvent {
                domain: crate::server::DOMAIN_HOST,
                kind: "native_domain_attached".into(),
                subject_type: "runtime_process".into(),
                subject_id: self.host_id.clone(),
                origin: "native_host".into(),
                payload: serde_json::json!({"pid": std::process::id(), "backend": "herdr-source-library", "state": "domain_attached_not_listener_ready"}),
            })?;
            Ok(())
        })?;
        // Do not seed even an ordinary shell beside unbound canonical work.
        self.require_owner_only_phase()
    }
    fn server_ready(&self) -> io::Result<()> {
        self.require_owner_only_phase()?;
        self.clients
            .lock()
            .map_err(|_| io::Error::other("native client authority poisoned"))?
            .activate(&self.host_id)
    }
    fn client_credential(&self) -> io::Result<String> {
        client_authority::client_credential(&self.home)
    }
    fn authenticate_client(
        &self,
        credential: &str,
        expected_lifetime: Option<&str>,
        peer: herdr::embedding::TransportPeer,
    ) -> io::Result<()> {
        let result = self.require_owner_only_phase().and_then(|_| {
            self.clients
                .lock()
                .map_err(|_| io::Error::other("native client authority poisoned"))?
                .authenticate(credential, peer, expected_lifetime, |channel, lifetime| self.with_store(|store| {
                    events::append(store, NewEvent {
                        domain: crate::server::DOMAIN_HOST,
                        kind: "native_client_admitted".into(),
                        subject_type: "runtime_process".into(),
                        subject_id: self.host_id.clone(),
                        origin: "native_host".into(),
                        payload: serde_json::json!({"authority": "host_issued_user_capability", "channel_id": channel.channel_id.as_str(), "process_lifetime": lifetime, "transport_peer": {"pid":peer.pid,"uid":peer.uid}, "state": "process_binding_admitted_not_human_or_current_fd_writer_proof"}),
                    })?;
                    Ok(())
                }))
        });
        if result.is_err() {
            self.refused("viva.client.authenticate", Some(peer))?;
        }
        result
    }
    fn validate_client(&self, peer: herdr::embedding::TransportPeer) -> io::Result<()> {
        let result = self.require_owner_only_phase().and_then(|_| {
            self.clients
                .lock()
                .map_err(|_| io::Error::other("native client authority poisoned"))?
                .validate(peer)
        });
        if result.is_err() {
            self.refused("native.raw_client", Some(peer))?;
        }
        result
    }
    fn prepare_execution(&self, request: ExecutionRequest) -> io::Result<ExecutionPlan> {
        if request.argv.is_empty() || request.argv[0].trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "execution argv must name a program",
            ));
        }
        if !request.cwd.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "execution cwd must be absolute",
            ));
        }
        let task_id = TaskId::from_str(&request.task_id).map_err(io::Error::other)?;
        let member_id = MemberId::from_str(&request.member_id).map_err(io::Error::other)?;
        let grant_id = GrantId::from_str(&request.grant_id).map_err(io::Error::other)?;
        let mut state = self
            .executions
            .lock()
            .map_err(|_| io::Error::other("native execution state poisoned"))?;
        self.with_store(|store| {
            owner_only_phase_locked(store, &state)?;
            if paused(store)? {
                return Err(Error::Validation(
                    "Viva is paused: no new native dispatch".into(),
                ));
            }
            let tasks = TaskRegistry::new(store);
            let start = match tasks.prepare_member_dispatch(DispatchRequest {
                task_id: &task_id,
                member_id: &member_id,
                grant_id: &grant_id,
                request_key: &request.request_key,
                cwd: &request.cwd,
                worktree_id: request.worktree_id.as_deref(),
            }) {
                Ok(start) => start,
                Err(err) => {
                    refused_event(store, &task_id, &member_id, &grant_id, &err.to_string())?;
                    return Err(err);
                }
            };
            match start {
                ExecutionStart::Replayed(intent) => {
                    let execution_id = intent.execution_id.clone().ok_or_else(|| {
                        Error::Validation("replayed intent has no execution".into())
                    })?;
                    Ok(ExecutionPlan {
                        execution_id: execution_id.to_string(),
                        replayed: true,
                        intent_state: intent.state.as_str().to_string(),
                        argv: Vec::new(),
                        cwd: PathBuf::new(),
                        env: Vec::new(),
                        label: String::new(),
                        existing_pid: intent.pid.map(|pid| pid as u32),
                    })
                }
                ExecutionStart::Created(intent) => {
                    let execution_id = intent.execution_id.clone().ok_or_else(|| {
                        Error::Validation("created intent has no execution".into())
                    })?;
                    let task = tasks.require_task(&task_id)?;
                    let member = MemberRegistry::new(store).get(&member_id)?.ok_or_else(|| {
                        Error::NotFound {
                            entity: "member",
                            id: member_id.to_string(),
                        }
                    })?;
                    let mut label = format!("{} · {}", member.display_name, task.goal);
                    if label.chars().count() > 80 {
                        label = label.chars().take(79).collect::<String>() + "…";
                    }
                    // Host-issued MemberExecution channel: the role binding is
                    // decided here, never by the child's env or argv.
                    let caller = self
                        .channels
                        .lock()
                        .map_err(|_| io::Error::other("native channel registry poisoned"))?
                        .issue(CallerRole::MemberExecution(execution_id.clone()));
                    let credential = match AuthorityEngine::new(store).issue_worker_credential(
                        &caller.channel_id,
                        Some(execution_id.clone()),
                        member_id.clone(),
                    ) {
                        Ok(credential) => credential,
                        Err(err) => {
                            let _ = self
                                .channels
                                .lock()
                                .map_err(|_| io::Error::other("native channel registry poisoned"))?
                                .revoke(&caller.channel_id);
                            return Err(err);
                        }
                    };
                    state.owned.insert(execution_id.to_string());
                    state.by_execution.insert(
                        execution_id.to_string(),
                        ExecutionBinding {
                            execution_id: execution_id.clone(),
                            task_id: task_id.clone(),
                            member_id: member_id.clone(),
                            grant_id: grant_id.clone(),
                            request_key: request.request_key.clone(),
                            argv: request.argv.clone(),
                            cwd: request.cwd.clone(),
                            worktree_id: request.worktree_id.clone(),
                            workspace_id: None,
                            pane_id: None,
                            channel_id: caller.channel_id.clone(),
                            credential_id: credential.clone(),
                            pid: None,
                            pid_start_marker: None,
                            stop_requested: false,
                        },
                    );
                    Ok(ExecutionPlan {
                        execution_id: execution_id.to_string(),
                        replayed: false,
                        intent_state: "intended".to_string(),
                        argv: request.argv,
                        cwd: request.cwd,
                        env: vec![
                            ("VIVA_MEMBER_NAME".into(), member.display_name),
                            ("VIVA_MEMBER_ID".into(), member_id.to_string()),
                            ("VIVA_TASK_ID".into(), task_id.to_string()),
                            ("VIVA_EXECUTION_ID".into(), execution_id.to_string()),
                            ("VIVA_MEMBER_CHANNEL".into(), caller.channel_id.to_string()),
                            ("VIVA_MEMBER_CREDENTIAL".into(), credential),
                        ],
                        label,
                        existing_pid: None,
                    })
                }
            }
        })
    }
    fn execution_started(
        &self,
        execution_id: &str,
        workspace_id: &str,
        pane_id: &str,
        terminal_id: &str,
        pid: u32,
    ) -> io::Result<()> {
        let mut state = self
            .executions
            .lock()
            .map_err(|_| io::Error::other("native execution state poisoned"))?;
        let binding = state
            .by_execution
            .get(execution_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unknown native execution"))?;
        let (execution, task_id, grant_id, request_key, member_id, argv, cwd, worktree_id) = (
            binding.execution_id.clone(),
            binding.task_id.clone(),
            binding.grant_id.clone(),
            binding.request_key.clone(),
            binding.member_id.clone(),
            binding.argv.clone(),
            binding.cwd.clone(),
            binding.worktree_id.clone(),
        );
        let marker = self.with_store(|store| {
            // Read the OS start marker first: a command can exit immediately,
            // and the earlier we read it the smaller the confirmation race.
            let marker = process_start_marker(pid as i64).ok_or_else(|| {
                Error::Validation(
                    "cannot read the OS process start marker; refusing to confirm the launch"
                        .into(),
                )
            })?;
            let record = get_execution(store, &execution)?.ok_or_else(|| Error::NotFound {
                entity: "execution",
                id: execution.to_string(),
            })?;
            // Herdr's terminal ids use `term_`; Viva's canonical terminal id
            // grammar uses `term-`. Keep a deterministic correlation id in the
            // LaunchSpec and retain the real native terminal id in the event.
            let viva_terminal_id = format!("term-{}", terminal_id.trim_start_matches("term_"));
            let terminal = TerminalId::from_str(&viva_terminal_id)?;
            let mut spec = LaunchSpec::new(
                argv,
                &cwd,
                RequestOrigin::MemberDispatch {
                    grant_id: grant_id.clone(),
                },
            )?
            .for_execution(execution.clone())
            .on_terminal(terminal)
            .with_member(member_id.clone())
            .with_bindings(
                record.attribution.model_binding,
                record.attribution.tool_binding,
            );
            if let Some(worktree) = &worktree_id {
                spec = spec.in_worktree(WorktreeId::from_str(worktree)?);
            }
            records::insert_launch_spec(store, &spec)?;
            TaskRegistry::new(store).confirm_launch(&request_key, pid as i64, marker.clone())?;
            events::append(
                store,
                NewEvent {
                    domain: crate::server::DOMAIN_HOST,
                    kind: "native_execution_started".into(),
                    subject_type: "execution".into(),
                    subject_id: execution.to_string(),
                    origin: "native_host".into(),
                    payload: serde_json::json!({
                        "execution_id": execution.as_str(),
                        "task_id": task_id.as_str(),
                        "member_id": member_id.as_str(),
                        "grant_id": grant_id.as_str(),
                        "workspace_id": workspace_id,
                        "pane_id": pane_id,
                        "native_terminal_id": terminal_id,
                        "launch_spec_terminal_id": viva_terminal_id.as_str(),
                        "pid": pid,
                        "pid_start_marker": marker,
                        "launch_spec_id": spec.spec_id.as_str(),
                    }),
                },
            )?;
            Ok(marker)
        })?;
        let binding = state
            .by_execution
            .get_mut(execution_id)
            .expect("binding remains after confirmation");
        binding.workspace_id = Some(workspace_id.to_string());
        binding.pane_id = Some(pane_id.to_string());
        binding.pid = Some(pid as i64);
        binding.pid_start_marker = Some(marker);
        state.by_pane.insert(
            (workspace_id.to_string(), pane_id.to_string()),
            execution_id.to_string(),
        );
        Ok(())
    }
    fn execution_failed(&self, execution_id: &str) -> io::Result<()> {
        let mut state = self
            .executions
            .lock()
            .map_err(|_| io::Error::other("native execution state poisoned"))?;
        let Some(binding) = state.by_execution.remove(execution_id) else {
            return Ok(());
        };
        // Keep this execution in `owned`: it was prepared by this host and its
        // intent is now unresolved. Forgetting it here would make the owner
        // gate reject every later native start as an "unowned" execution.
        // A later process exit (if any) is still allowed to be unknown.
        if let (Some(workspace_id), Some(pane_id)) = (&binding.workspace_id, &binding.pane_id) {
            state
                .by_pane
                .remove(&(workspace_id.clone(), pane_id.clone()));
        }
        self.with_store(|store| {
            // A failed start must not leave a usable worker credential behind.
            let revoked = AuthorityEngine::new(store)
                .revoke_worker_credential(&binding.credential_id)
                .is_ok();
            let _ = self
                .channels
                .lock()
                .map_err(|_| io::Error::other("native channel registry poisoned"))?
                .revoke(&binding.channel_id);
            if !revoked {
                events::append(
                    store,
                    NewEvent {
                        domain: crate::server::DOMAIN_HOST,
                        kind: "native_member_credential_revoke_failed".into(),
                        subject_type: "execution".into(),
                        subject_id: binding.execution_id.to_string(),
                        origin: "native_host".into(),
                        payload: serde_json::json!({
                            "execution_id": binding.execution_id.as_str(),
                            "reason": "unresolved_start",
                        }),
                    },
                )?;
            }
            let outcome = TaskRegistry::new(store).mark_unresolved(&binding.request_key);
            let kind = if outcome.is_ok() {
                "native_execution_unresolved"
            } else {
                "native_execution_unresolved_record_failed"
            };
            events::append(
                store,
                NewEvent {
                    domain: crate::server::DOMAIN_HOST,
                    kind: kind.into(),
                    subject_type: "execution".into(),
                    subject_id: binding.execution_id.to_string(),
                    origin: "native_host".into(),
                    payload: serde_json::json!({
                        "execution_id": binding.execution_id.as_str(),
                        "task_id": binding.task_id.as_str(),
                        "member_id": binding.member_id.as_str(),
                        "request_key": binding.request_key,
                        "detail": outcome.err().map(|err| err.to_string()),
                    }),
                },
            )?;
            Ok(())
        })
    }
    fn execution_exited(
        &self,
        workspace_id: &str,
        pane_id: &str,
        exit_code: Option<i32>,
    ) -> io::Result<()> {
        let mut state = self
            .executions
            .lock()
            .map_err(|_| io::Error::other("native execution state poisoned"))?;
        let Some(execution_id) = state
            .by_pane
            .remove(&(workspace_id.to_string(), pane_id.to_string()))
        else {
            return Ok(());
        };
        let Some(binding) = state.by_execution.remove(&execution_id) else {
            return Ok(());
        };
        state.owned.remove(&execution_id);
        let stop_requested = binding.stop_requested;
        drop(state);
        self.with_store(|store| {
            let tasks = TaskRegistry::new(store);
            let _ = self
                .channels
                .lock()
                .map_err(|_| Error::Validation("native channel registry poisoned".into()))?
                .revoke(&binding.channel_id);
            // The member process is gone: its credential can authorize nothing.
            if AuthorityEngine::new(store)
                .revoke_worker_credential(&binding.credential_id)
                .is_err()
            {
                events::append(
                    store,
                    NewEvent {
                        domain: crate::server::DOMAIN_HOST,
                        kind: "native_member_credential_revoke_failed".into(),
                        subject_type: "execution".into(),
                        subject_id: binding.execution_id.to_string(),
                        origin: "native_host".into(),
                        payload: serde_json::json!({
                            "execution_id": binding.execution_id.as_str(),
                            "reason": "process_exit",
                        }),
                    },
                )?;
            }
            let stored_code = exit_code.unwrap_or(-1);
            if let Err(err) = tasks.record_exit(&binding.request_key, stored_code) {
                events::append(
                    store,
                    NewEvent {
                        domain: crate::server::DOMAIN_HOST,
                        kind: "native_execution_exit_record_failed".into(),
                        subject_type: "execution".into(),
                        subject_id: binding.execution_id.to_string(),
                        origin: "native_host".into(),
                        payload: serde_json::json!({
                            "execution_id": binding.execution_id.as_str(),
                            "reason": err.to_string(),
                        }),
                    },
                )?;
                return Err(err);
            }
            if exit_code.is_none() {
                store.connection().execute(
                    "UPDATE executions SET status = 'stopped', updated_at = ?2
                     WHERE execution_id = ?1 AND status = 'failed'",
                    rusqlite::params![binding.execution_id.as_str(), utc_now()],
                )?;
            }
            let _ = tasks.record_result(
                &binding.task_id,
                Some(binding.execution_id.clone()),
                ResultSource::Process,
                "process_exit",
                format!("{}:exit", binding.request_key),
                serde_json::json!({
                    "exit_code": exit_code,
                    "stored_exit_sentinel": stored_code,
                    "exit_code_known": exit_code.is_some(),
                    "stop_requested": stop_requested,
                    "source": "herdr_pane_child",
                }),
            );
            events::append(
                store,
                NewEvent {
                    domain: crate::server::DOMAIN_HOST,
                    kind: "native_execution_exited".into(),
                    subject_type: "execution".into(),
                    subject_id: binding.execution_id.to_string(),
                    origin: "native_host".into(),
                    payload: serde_json::json!({
                        "execution_id": binding.execution_id.as_str(),
                        "task_id": binding.task_id.as_str(),
                        "member_id": binding.member_id.as_str(),
                        "workspace_id": workspace_id,
                        "pane_id": pane_id,
                        "exit_code": exit_code,
                        "stored_exit_sentinel": stored_code,
                        "exit_code_known": exit_code.is_some(),
                        "stop_requested": stop_requested,
                    }),
                },
            )?;
            Ok(())
        })
    }
    fn prepare_execution_stop(
        &self,
        execution_id: &str,
    ) -> io::Result<herdr::embedding::ExecutionStopPlan> {
        let mut state = self
            .executions
            .lock()
            .map_err(|_| io::Error::other("native execution state poisoned"))?;
        if !state.owned.contains(execution_id) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the execution is not owned by this native host",
            ));
        }
        let Some(binding) = state.by_execution.get_mut(execution_id) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the execution is not live",
            ));
        };
        let Some(workspace_id) = binding.workspace_id.clone() else {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "the execution has not started",
            ));
        };
        binding.stop_requested = true;
        let plan = herdr::embedding::ExecutionStopPlan {
            execution_id: binding.execution_id.to_string(),
            workspace_id: Some(workspace_id.clone()),
            pane_id: binding.pane_id.clone(),
        };
        let (execution, task, member, grant, pane) = (
            binding.execution_id.clone(),
            binding.task_id.clone(),
            binding.member_id.clone(),
            binding.grant_id.clone(),
            binding.pane_id.clone(),
        );
        drop(state);
        self.with_store(|store| {
            events::append(
                store,
                NewEvent {
                    domain: crate::server::DOMAIN_HOST,
                    kind: "native_execution_stop_requested".into(),
                    subject_type: "execution".into(),
                    subject_id: execution.to_string(),
                    origin: "native_host".into(),
                    payload: serde_json::json!({
                        "execution_id": execution.as_str(),
                        "task_id": task.as_str(),
                        "member_id": member.as_str(),
                        "grant_id": grant.as_str(),
                        "workspace_id": workspace_id,
                        "pane_id": pane,
                        "authority": "owner_capability",
                    }),
                },
            )?;
            Ok(())
        })?;
        Ok(plan)
    }
    fn set_paused(&self, paused: bool) -> io::Result<()> {
        self.with_store(|store| {
            store.connection().execute(
                "INSERT OR REPLACE INTO settings(key, value) VALUES ('paused', ?1)",
                [if paused { "true" } else { "false" }],
            )?;
            events::append(
                store,
                NewEvent {
                    domain: crate::server::DOMAIN_HOST,
                    kind: if paused {
                        "native_host_paused".into()
                    } else {
                        "native_host_resumed".into()
                    },
                    subject_type: "runtime_process".into(),
                    subject_id: self.host_id.clone(),
                    origin: "native_host".into(),
                    payload: serde_json::json!({
                        "paused": paused,
                        "authority": "owner_capability",
                        "scope": "new_dispatches_only",
                    }),
                },
            )?;
            Ok(())
        })
    }
    fn member_report(
        &self,
        request: herdr::embedding::MemberReportRequest,
        peer: herdr::embedding::TransportPeer,
    ) -> io::Result<i64> {
        if !MEMBER_REPORT_STATUSES.contains(&request.status.as_str()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unknown member report status",
            ));
        }
        #[cfg(unix)]
        if peer.uid != Some(unsafe { libc::geteuid() }) {
            self.with_store(|store| {
                refused_member_report(store, None, None, None, "peer_uid_mismatch")
            })?;
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "member report peer uid does not match this host",
            ));
        }
        let peer_pid = peer.pid.map(i64::from).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "member report requires a real socket peer pid",
            )
        })?;
        let state = self
            .executions
            .lock()
            .map_err(|_| io::Error::other("native execution state poisoned"))?;
        let Some(binding) = state
            .by_execution
            .values()
            .find(|binding| binding.credential_id == request.credential)
            .cloned()
        else {
            drop(state);
            self.with_store(|store| {
                refused_member_report(store, None, None, None, "unknown_credential")
            })?;
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "member credential is not active on this host",
            ));
        };
        let (Some(root_pid), Some(root_marker)) = (binding.pid, binding.pid_start_marker.clone())
        else {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "execution is not confirmed yet",
            ));
        };
        if process_start_marker(root_pid) != Some(root_marker) {
            drop(state);
            self.with_store(|store| {
                refused_member_report(
                    store,
                    Some(binding.execution_id.as_str()),
                    Some(binding.member_id.as_str()),
                    Some(binding.grant_id.as_str()),
                    "process_binding_changed",
                )
            })?;
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the confirmed execution process is gone or was reused",
            ));
        }
        if !is_bound_process(root_pid, peer_pid) {
            drop(state);
            self.with_store(|store| {
                refused_member_report(
                    store,
                    Some(binding.execution_id.as_str()),
                    Some(binding.member_id.as_str()),
                    Some(binding.grant_id.as_str()),
                    "peer_not_bound_to_execution",
                )
            })?;
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the calling process is not the confirmed execution or its descendant",
            ));
        }
        drop(state);

        enum Decision {
            Accepted(i64),
            NotReady,
            Denied(String),
        }
        let decision = self.with_store(|store| {
            let Some(execution) = get_execution(store, &binding.execution_id)? else {
                return Ok(Decision::Denied("execution_missing".into()));
            };
            if execution.status != crate::foundation::records::ExecutionStatus::Running {
                return Ok(Decision::NotReady);
            }
            let caller = CallerIdentity {
                channel_id: binding.channel_id.clone(),
                claimed_role: CallerRole::MemberExecution(binding.execution_id.clone()),
            };
            let channels = self
                .channels
                .lock()
                .map_err(|_| Error::Validation("native channel registry poisoned".into()))?;
            let verdict = AuthorityEngine::new(store).check_bound_worker_action(
                &channels,
                &caller,
                &request.credential,
                Some(binding.grant_id.clone()),
                MEMBER_REPORT_ACTION,
                Some(&binding.task_id),
            )?;
            let member = match verdict {
                Ok(member) => member,
                Err(reason) => {
                    refused_member_report(
                        store,
                        Some(binding.execution_id.as_str()),
                        Some(binding.member_id.as_str()),
                        Some(binding.grant_id.as_str()),
                        &format!("authority_refused:{reason}"),
                    )?;
                    return Ok(Decision::Denied(reason.to_string()));
                }
            };
            let redactor = crate::redaction::Redactor::new([request.credential.clone()]);
            let detail = redactor.redact_str(&request.detail);
            let detail: String = detail.chars().take(MEMBER_REPORT_DETAIL_LIMIT).collect();
            let recorded = events::append(
                store,
                NewEvent {
                    domain: crate::server::DOMAIN_HOST,
                    kind: "native_member_reported".into(),
                    subject_type: "execution".into(),
                    subject_id: binding.execution_id.to_string(),
                    origin: "native_host".into(),
                    payload: serde_json::json!({
                        "execution_id": binding.execution_id.as_str(),
                        "task_id": binding.task_id.as_str(),
                        "member_id": member.as_str(),
                        "grant_id": binding.grant_id.as_str(),
                        "channel_id": binding.channel_id.as_str(),
                        "status": request.status,
                        "detail": detail,
                        "detail_chars": detail.chars().count(),
                        "source": "native_member_process",
                        "claim_scope": "member_report_not_task_completion",
                    }),
                },
            )?;
            Ok(Decision::Accepted(recorded.seq))
        })?;
        match decision {
            Decision::Accepted(seq) => Ok(seq),
            Decision::NotReady => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "execution is not confirmed yet",
            )),
            Decision::Denied(reason) => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("member report refused: {reason}"),
            )),
        }
    }
    fn snapshot(&self) -> io::Result<serde_json::Value> {
        let native_execution_count = self
            .executions
            .lock()
            .map(|state| state.by_execution.len())
            .unwrap_or(0);
        self.with_store(|store| {
            Ok(serde_json::json!({
                "schema": 1,
                "backend": "herdr-source-library",
                "runtime_pid": std::process::id(),
                "runtime_host_id": self.host_id,
                "execution_attribution": "pane_bound_owner_dispatch",
                "integration_state": "development-owner-dispatch",
                "member_dispatch": "owner_gated_launch_spec",
                "member_process_ingress": "bound_worker_report_only",
                "member_report_action": MEMBER_REPORT_ACTION,
                "native_execution_count": native_execution_count,
                "members": MemberRegistry::new(store).list()?,
                "tasks": TaskRegistry::new(store).list_tasks(0, 100)?,
                "tasks_limit": 100,
            }))
        })
    }
    fn refused(
        &self,
        method: &'static str,
        peer: Option<herdr::embedding::TransportPeer>,
    ) -> io::Result<()> {
        self.with_store(|store| {
            events::append(store, NewEvent {
                domain: crate::server::DOMAIN_HOST,
                kind: "native_automation_refused".into(),
                subject_type: "native_ingress".into(),
                subject_id: uuid::Uuid::new_v4().to_string(),
                origin: match method {
                    "native_cli.unsupported" => "native_cli:unbound",
                    "plugin.command" => "native_plugin:unbound",
                    _ => "native_api:unbound",
                }.into(),
                payload: serde_json::json!({"method": method, "actor": "unbound", "reason": "authority_not_integrated", "transport_peer": peer.map(|peer| serde_json::json!({"pid": peer.pid, "uid": peer.uid}))}),
            })?;
            Ok(())
        })
    }
}

#[cfg(unix)]
fn run_member_client(home: &std::path::Path, args: &[String]) -> Result<()> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    if args.first().map(String::as_str) != Some("report") {
        return Err(Error::Validation(
            "runtime member needs: report --status <progress|blocked|failed|done> [--detail <text>]"
                .into(),
        ));
    }
    let mut status: Option<String> = None;
    let mut detail = String::new();
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--status" => {
                status = args.get(index + 1).cloned();
                index += 2;
            }
            "--detail" => {
                detail = args.get(index + 1).cloned().unwrap_or_default();
                index += 2;
            }
            other => {
                return Err(Error::Validation(format!(
                    "unknown runtime member flag {other}"
                )));
            }
        }
    }
    let status =
        status.ok_or_else(|| Error::Validation("runtime member report needs --status".into()))?;
    // The credential is delivered through the process environment on purpose:
    // argv is visible in the process list and is not a safe channel.
    let credential = std::env::var("VIVA_MEMBER_CREDENTIAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            Error::Validation(
                "VIVA_MEMBER_CREDENTIAL is not set; this process was not started as a bound member execution"
                    .into(),
            )
        })?;
    let socket = home.join("runtime").join("workbench").join("herdr.sock");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let mut stream = match UnixStream::connect(&socket) {
            Ok(stream) => stream,
            Err(err) => {
                return Err(Error::Validation(format!(
                    "cannot reach the native host at {}: {err}",
                    socket.display()
                )));
            }
        };
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
        let request = serde_json::json!({
            "id": format!("member-report-{}", uuid::Uuid::new_v4().simple()),
            "method": "viva.member.report",
            "params": {"credential": credential, "status": status, "detail": detail},
        });
        writeln!(stream, "{request}")
            .map_err(|err| Error::Validation(format!("cannot send the member report: {err}")))?;
        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .map_err(|err| Error::Validation(format!("cannot read the member reply: {err}")))?;
        let value: serde_json::Value = serde_json::from_str(&line)
            .map_err(|err| Error::Validation(format!("invalid member reply: {err}")))?;
        if let Some(error) = value.get("error") {
            let code = error["code"].as_str().unwrap_or("member_report_failed");
            if code == "member_not_ready" && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(25));
                continue;
            }
            let message = error["message"].as_str().unwrap_or("");
            return Err(Error::Validation(format!(
                "member report refused ({code}): {message}"
            )));
        }
        let seq = value["result"]["event_seq"].as_i64().ok_or_else(|| {
            Error::Validation("member report reply has no canonical event sequence".into())
        })?;
        println!(
            "{}",
            serde_json::json!({"accepted": true, "event_seq": seq})
        );
        return Ok(());
    }
}

#[cfg(unix)]
fn send_native_request(
    socket: &std::path::Path,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket).map_err(|err| {
        Error::Validation(format!(
            "cannot reach the native host at {}: {err}",
            socket.display()
        ))
    })?;
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
    writeln!(
        stream,
        "{}",
        serde_json::json!({
            "id": format!("native-call-{}", uuid::Uuid::new_v4().simple()),
            "method": method,
            "params": params,
        })
    )
    .map_err(|err| Error::Validation(format!("cannot send {method}: {err}")))?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|err| Error::Validation(format!("cannot read {method} reply: {err}")))?;
    let value: serde_json::Value = serde_json::from_str(&line)
        .map_err(|err| Error::Validation(format!("invalid {method} reply: {err}")))?;
    if let Some(error) = value.get("error") {
        let code = error["code"].as_str().unwrap_or("native_call_failed");
        let message = error["message"].as_str().unwrap_or("");
        return Err(Error::Validation(format!(
            "{method} failed ({code}): {message}"
        )));
    }
    Ok(value["result"].clone())
}

#[cfg(unix)]
fn authenticate_owner(socket: &std::path::Path, home: &std::path::Path) -> Result<()> {
    let capability: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            home.join("runtime")
                .join("workbench")
                .join("owner-channel.json"),
        )
        .map_err(|err| Error::Validation(format!("owner capability is unavailable: {err}")))?,
    )
    .map_err(|err| Error::Validation(format!("invalid owner capability file: {err}")))?;
    let credential = capability["credential"]
        .as_str()
        .ok_or_else(|| Error::Validation("owner capability has no credential".into()))?;
    let peer = herdr::embedding::TransportPeer {
        pid: Some(std::process::id()),
        uid: Some(unsafe { libc::geteuid() }),
    };
    let lifetime = herdr::embedding::peer_lifetime_marker(peer).map_err(Error::from)?;
    send_native_request(
        socket,
        "viva.client.authenticate",
        serde_json::json!({"credential": credential, "process_lifetime": lifetime}),
    )?;
    Ok(())
}

#[cfg(unix)]
fn run_control_client(home: &std::path::Path, args: &[String]) -> Result<()> {
    let socket = home.join("runtime").join("workbench").join("herdr.sock");
    let result = match (
        args.first().map(String::as_str),
        args.get(1).map(String::as_str),
    ) {
        (Some("host"), Some("pause")) => {
            authenticate_owner(&socket, home)?;
            send_native_request(
                &socket,
                "viva.host.pause",
                serde_json::json!({"paused": true}),
            )?
        }
        (Some("host"), Some("resume")) => {
            authenticate_owner(&socket, home)?;
            send_native_request(
                &socket,
                "viva.host.pause",
                serde_json::json!({"paused": false}),
            )?
        }
        (Some("execution"), Some("stop")) => {
            let mut execution_id = None;
            let mut index = 2;
            while index < args.len() {
                match args[index].as_str() {
                    "--execution" => {
                        execution_id = args.get(index + 1).cloned();
                        index += 2;
                    }
                    other => {
                        return Err(Error::Validation(format!(
                            "unknown execution stop flag {other}"
                        )));
                    }
                }
            }
            let execution_id = execution_id
                .ok_or_else(|| Error::Validation("execution stop needs --execution <id>".into()))?;
            authenticate_owner(&socket, home)?;
            send_native_request(
                &socket,
                "viva.execution.stop",
                serde_json::json!({"execution_id": execution_id}),
            )?
        }
        _ => {
            return Err(Error::Validation(
                "runtime control needs host pause | host resume | execution stop --execution <id>"
                    .into(),
            ));
        }
    };
    println!("{result}");
    Ok(())
}

pub fn run(args: &[String]) -> Result<()> {
    // No accidental installation/user-home trial before the default cutover gate.
    if std::env::var(paths::VIVA_HOME_ENV)
        .ok()
        .is_none_or(|value| value.trim().is_empty())
    {
        return Err(Error::Validation("native development preview requires an explicit isolated VIVA_HOME; it is not an installation/upgrade acceptance build".into()));
    }
    if crate::machines::invocation_target().is_some() {
        return Err(Error::Validation(
            "native preview remote machine ingress is not integrated; refusing local fallback"
                .into(),
        ));
    }
    let home = paths::viva_home(None);
    paths::ensure_private_dir(&home)?;
    // A bound member process is a client of the running host, never a second
    // host: never auto-start anything and never touch the legacy server.
    #[cfg(unix)]
    if args.first().map(String::as_str) == Some("member") {
        return run_member_client(&home, args.get(1..).unwrap_or(&[]));
    }
    #[cfg(unix)]
    if matches!(args.first().map(String::as_str), Some("host" | "execution")) {
        return run_control_client(&home, args);
    }
    // Temporary development admission gate, not the final startup behavior.
    // Never launch a second execution host beside a legacy Viva host. The
    // production workbench still uses automatic same-backend takeover (#196).
    // Cross-backend inheritance remains required BEFORE default cutover.
    if crate::server::Client::connect(&home).is_ok() {
        return Err(Error::Validation("native development preview cannot yet inherit this live legacy host; it has been left untouched. This preview is not the #191 default-startup implementation.".into()));
    }
    eprintln!(
        "native development preview: use isolated HOME/VIVA_HOME only; member execution and cross-backend inheritance are not integrated; not an installation acceptance build"
    );
    let mut arguments = vec!["viva-runtime".to_owned()];
    arguments.extend_from_slice(args);
    herdr::embedding::run(herdr::embedding::Context {
        directory: home.join("runtime").join("workbench"),
        arguments,
        child_prefix: vec!["runtime".into()],
        domain: Arc::new(DomainBridge {
            clients: Mutex::new(client_authority::ClientAuthority::new(&home)),
            channels: Mutex::new(ChannelRegistry::new()),
            executions: Mutex::new(NativeExecutionState::default()),
            home,
            host_id: format!("native-{}", uuid::Uuid::new_v4().simple()),
            store: Mutex::new(None),
        }),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{AuthorityEngine, GrantMode};
    use crate::members::{MemberBinding, MemberRegistry};
    use crate::tasks::{DispatchRequest, ExecutionStart, TaskRegistry};
    use herdr::embedding::DomainServices;

    #[test]
    fn execution_exit_settles_and_releases_the_owned_binding() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let store = Store::open(&paths::database_path(home), crate::server::migrations()).unwrap();
        let members = MemberRegistry::new(&store);
        let member = members.register("exit fixture").unwrap();
        members
            .set_binding(&MemberBinding {
                member_id: member.member_id.clone(),
                role: "exit fixture".into(),
                model_binding: "fixture:model".into(),
                tools: vec!["fixture-tool".into()],
                updated_at: utc_now(),
            })
            .unwrap();
        let tasks = TaskRegistry::new(&store);
        let task = tasks
            .create_task(
                "exit fixture task",
                vec![],
                Some(member.member_id.clone()),
                None,
                None,
            )
            .unwrap();
        let grant = AuthorityEngine::new(&store)
            .issue_root_grant(
                Some(member.member_id.clone()),
                Some(task.task_id.clone()),
                vec!["dispatch_delegated".into()],
                GrantMode::ActAutonomously,
                None,
            )
            .unwrap();
        let start = tasks
            .prepare_member_dispatch(DispatchRequest {
                task_id: &task.task_id,
                member_id: &member.member_id,
                grant_id: &grant.grant_id,
                request_key: "exit-fixture",
                cwd: std::path::Path::new("/tmp"),
                worktree_id: None,
            })
            .unwrap();
        let ExecutionStart::Created(intent) = start else {
            panic!("expected a fresh intent");
        };
        let execution_id = intent.execution_id.clone().unwrap();
        tasks
            .confirm_launch(
                &intent.request_key,
                std::process::id() as i64,
                "fixture-marker",
            )
            .unwrap();
        let mut channels = ChannelRegistry::new();
        let caller = channels.issue(CallerRole::MemberExecution(execution_id.clone()));
        let credential = AuthorityEngine::new(&store)
            .issue_worker_credential(
                &caller.channel_id,
                Some(execution_id.clone()),
                member.member_id.clone(),
            )
            .unwrap();
        let binding = ExecutionBinding {
            execution_id: execution_id.clone(),
            task_id: task.task_id.clone(),
            member_id: member.member_id.clone(),
            grant_id: grant.grant_id.clone(),
            request_key: intent.request_key.clone(),
            argv: vec!["/bin/true".into()],
            cwd: PathBuf::from("/tmp"),
            worktree_id: None,
            workspace_id: Some("ws-fixture".into()),
            pane_id: Some("pane-fixture".into()),
            channel_id: caller.channel_id.clone(),
            credential_id: credential,
            pid: None,
            pid_start_marker: None,
            stop_requested: false,
        };
        let bridge = DomainBridge {
            home: home.to_path_buf(),
            host_id: "native-exit-fixture".into(),
            store: Mutex::new(Some(store)),
            clients: Mutex::new(client_authority::ClientAuthority::new(home)),
            channels: Mutex::new(channels),
            executions: Mutex::new(NativeExecutionState {
                by_execution: HashMap::from([(execution_id.to_string(), binding)]),
                by_pane: HashMap::from([(
                    ("ws-fixture".into(), "pane-fixture".into()),
                    execution_id.to_string(),
                )]),
                owned: HashSet::from([execution_id.to_string()]),
            }),
        };

        bridge
            .execution_exited("ws-fixture", "pane-fixture", None)
            .unwrap();
        // A duplicate close notification must be a no-op, not a second write.
        bridge
            .execution_exited("ws-fixture", "pane-fixture", Some(0))
            .unwrap();

        bridge
            .with_store(|store| {
                let (status, exit): (String, Option<i64>) = store
                    .connection()
                    .query_row(
                        "SELECT status, process_exit FROM executions WHERE execution_id=?1",
                        [execution_id.as_str()],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .unwrap();
                assert_eq!(status, "stopped");
                assert_eq!(exit, Some(-1));
                let intent_state: String = store
                    .connection()
                    .query_row(
                        "SELECT state FROM launch_intents WHERE request_key='exit-fixture'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(intent_state, "exited");
                let task_status: String = store
                    .connection()
                    .query_row(
                        "SELECT status FROM tasks WHERE task_id=?1",
                        [task.task_id.as_str()],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_ne!(task_status, "done");
                Ok(())
            })
            .unwrap();
        let state = bridge.executions.lock().unwrap();
        assert!(state.by_execution.is_empty());
        assert!(state.by_pane.is_empty());
        assert!(state.owned.is_empty());
    }
}
