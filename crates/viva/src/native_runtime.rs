//! Development-only native composition (#191). Herdr owns the PTYs and renderer;
//! Viva owns member/task facts and refusal auditing. This is deliberately not
//! wired to the installed default until authority and cross-backend import pass.
mod client_authority;

use crate::{
    foundation::{
        error::{Error, Result},
        events::{self, NewEvent},
        ids::{ExecutionId, GrantId, MemberId, TaskId, TerminalId, WorktreeId, utc_now},
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
    executions: Mutex<NativeExecutionState>,
}

/// Checks that assume the native execution state lock is already held. The
/// lock order is always native-execution state -> store.
fn owner_only_phase_locked(store: &Store, state: &NativeExecutionState) -> Result<()> {
    let active_credentials: i64 = store.connection().query_row(
        "SELECT count(*) FROM worker_credentials WHERE status='active'",
        [],
        |row| row.get(0),
    )?;
    if active_credentials != 0 {
        return Err(Error::Validation(
            "native member/process bindings are not integrated; owner frontend capability is refused while worker credentials remain active".into(),
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
                        },
                    );
                    Ok(ExecutionPlan {
                        execution_id: execution_id.to_string(),
                        replayed: false,
                        argv: request.argv,
                        cwd: request.cwd,
                        env: vec![
                            ("VIVA_MEMBER_NAME".into(), member.display_name),
                            ("VIVA_MEMBER_ID".into(), member_id.to_string()),
                            ("VIVA_TASK_ID".into(), task_id.to_string()),
                            ("VIVA_EXECUTION_ID".into(), execution_id.to_string()),
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
        self.with_store(|store| {
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
            let marker = process_start_marker(pid as i64).ok_or_else(|| {
                Error::Validation(
                    "cannot read the OS process start marker; refusing to confirm the launch"
                        .into(),
                )
            })?;
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
            Ok(())
        })?;
        let binding = state
            .by_execution
            .get_mut(execution_id)
            .expect("binding remains after confirmation");
        binding.workspace_id = Some(workspace_id.to_string());
        binding.pane_id = Some(pane_id.to_string());
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
        state.owned.remove(execution_id);
        if let (Some(workspace_id), Some(pane_id)) = (&binding.workspace_id, &binding.pane_id) {
            state
                .by_pane
                .remove(&(workspace_id.clone(), pane_id.clone()));
        }
        self.with_store(|store| {
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
        drop(state);
        self.with_store(|store| {
            let tasks = TaskRegistry::new(store);
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
                    }),
                },
            )?;
            Ok(())
        })
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
                "member_process_ingress": "not_integrated",
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
            executions: Mutex::new(NativeExecutionState::default()),
            home,
            host_id: format!("native-{}", uuid::Uuid::new_v4().simple()),
            store: Mutex::new(None),
        }),
    })?;
    Ok(())
}
