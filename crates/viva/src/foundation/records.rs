//! Office reference records: session kinds, attribution snapshots, execution
//! records, launch specifications, terminal events, and the workbench
//! query/action contracts.
//!
//! G0 semantics encoded here (rollout §2 + issue #10 supplement):
//! - A plain conversation session never needs a task; a task-execution
//!   session is unrepresentable without its task.
//! - A harness-native session reference is a pointer to a foreign record
//!   (Pi, Codex, …). The harness owns the chat transcript; Viva keeps
//!   metadata only — the two are never the same fact.
//! - Completion requires evidence. A process exit code is a process fact and
//!   is stored separately; no API and no schema path lets a zero exit code
//!   mark an execution completed.
//! - Terminal ownership is typed: a member-execution terminal carries its
//!   execution id; user shells and other auxiliaries cannot fake one.
//! - The workbench is a projection layer over existing facts; it never keeps
//!   its own workflow state.

use std::path::PathBuf;
use std::str::FromStr as _;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::{
    ExecutionId, GrantId, LaunchSpecId, MemberId, ProjectId, SessionId, TaskId, TerminalId,
    WorkspaceId, WorktreeId, utc_now,
};
use crate::foundation::store::Store;

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// What a session is. The task is part of the kind: a `TaskExecution` value
/// cannot exist without a `TaskId`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "task_id", rename_all = "snake_case")]
pub enum SessionKind {
    Conversation,
    TaskExecution(TaskId),
}

/// Pointer to a harness-native session. The harness (Pi, Codex, …) owns the
/// transcript and its internal state; Viva stores only this reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessSessionRef {
    pub harness: String,
    pub native_session_id: String,
}

/// An office-side session record: metadata and references, never a copy of
/// the conversation itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: SessionId,
    pub kind: SessionKind,
    pub title: String,
    pub member_id: Option<MemberId>,
    pub harness_ref: Option<HarnessSessionRef>,
    pub created_at: String,
}

impl SessionRecord {
    pub fn new(kind: SessionKind, title: impl Into<String>) -> Self {
        Self {
            session_id: SessionId::new(),
            kind,
            title: title.into(),
            member_id: None,
            harness_ref: None,
            created_at: utc_now(),
        }
    }

    #[must_use]
    pub fn with_member(mut self, member: MemberId) -> Self {
        self.member_id = Some(member);
        self
    }

    #[must_use]
    pub fn with_harness_ref(mut self, harness_ref: HarnessSessionRef) -> Self {
        self.harness_ref = Some(harness_ref);
        self
    }

    /// The attached task, if this is a task-execution session.
    pub fn task_id(&self) -> Option<&TaskId> {
        match &self.kind {
            SessionKind::Conversation => None,
            SessionKind::TaskExecution(task) => Some(task),
        }
    }
}

pub fn insert_session(store: &Store, session: &SessionRecord) -> OfficeResult<()> {
    store.connection().execute(
        "INSERT INTO office_sessions(
            session_id, kind, title, task_id, member_id,
            harness, harness_native_session_id, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            session.session_id.as_str(),
            match session.kind {
                SessionKind::Conversation => "conversation",
                SessionKind::TaskExecution(_) => "task_execution",
            },
            session.title,
            session.task_id().map(|t| t.as_str()),
            session.member_id.as_ref().map(|m| m.as_str()),
            session.harness_ref.as_ref().map(|h| h.harness.as_str()),
            session
                .harness_ref
                .as_ref()
                .map(|h| h.native_session_id.as_str()),
            session.created_at,
        ],
    )?;
    Ok(())
}

pub fn list_sessions(store: &Store) -> OfficeResult<Vec<SessionRecord>> {
    let mut stmt = store.connection().prepare(
        "SELECT session_id, kind, title, task_id, member_id,
                harness, harness_native_session_id, created_at
         FROM office_sessions ORDER BY created_at, session_id",
    )?;
    let rows = stmt.query_map([], |row| {
        let kind: String = row.get(1)?;
        let task_id: Option<String> = row.get(3)?;
        let kind = match kind.as_str() {
            "conversation" => SessionKind::Conversation,
            "task_execution" => {
                let task = task_id
                    .as_deref()
                    .and_then(|t| TaskId::from_str(t).ok())
                    .expect("DB CHECK guarantees a task id");
                SessionKind::TaskExecution(task)
            }
            other => panic!("office_sessions.kind holds an unknown value `{other}`"),
        };
        let member_id: Option<String> = row.get(4)?;
        let harness: Option<String> = row.get(5)?;
        let native: Option<String> = row.get(6)?;
        Ok(SessionRecord {
            session_id: SessionId::from_str(&row.get::<_, String>(0)?).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            kind,
            title: row.get(2)?,
            member_id: member_id
                .as_deref()
                .and_then(|m| MemberId::from_str(m).ok()),
            harness_ref: harness.zip(native).map(|(harness, native_session_id)| {
                HarnessSessionRef {
                    harness,
                    native_session_id,
                }
            }),
            created_at: row.get(7)?,
        })
    })?;
    let mut sessions = Vec::new();
    for row in rows {
        sessions.push(row?);
    }
    Ok(sessions)
}

// ---------------------------------------------------------------------------
// Executions
// ---------------------------------------------------------------------------

/// Lifecycle of an execution. `Completed` always carries evidence — see
/// [`ExecutionRecord::complete`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Requested,
    Running,
    Stopped,
    Failed,
    Completed,
}

impl ExecutionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecutionStatus::Requested => "requested",
            ExecutionStatus::Running => "running",
            ExecutionStatus::Stopped => "stopped",
            ExecutionStatus::Failed => "failed",
            ExecutionStatus::Completed => "completed",
        }
    }
}

/// Snapshot of who/what/where an execution runs as, captured once at
/// creation. Member names and bindings are configuration facts; the snapshot
/// records what the configuration was when the execution was attributed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributionSnapshot {
    pub member_id: MemberId,
    pub role: String,
    pub model_binding: String,
    pub tool_binding: String,
    pub workspace_id: Option<WorkspaceId>,
    pub project_id: Option<ProjectId>,
    pub captured_at: String,
}

impl AttributionSnapshot {
    pub fn capture(
        member_id: MemberId,
        role: impl Into<String>,
        model_binding: impl Into<String>,
        tool_binding: impl Into<String>,
    ) -> Self {
        Self {
            member_id,
            role: role.into(),
            model_binding: model_binding.into(),
            tool_binding: tool_binding.into(),
            workspace_id: None,
            project_id: None,
            captured_at: utc_now(),
        }
    }

    #[must_use]
    pub fn in_workspace(mut self, workspace: WorkspaceId) -> Self {
        self.workspace_id = Some(workspace);
        self
    }

    #[must_use]
    pub fn in_project(mut self, project: ProjectId) -> Self {
        self.project_id = Some(project);
        self
    }

    pub fn to_json(&self) -> OfficeResult<String> {
        Ok(serde_json::to_string(self)?)
    }
}

/// One supervised execution of a task. The `task_id` field has no default and
/// no setter: an execution without a task cannot be constructed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub execution_id: ExecutionId,
    pub task_id: TaskId,
    pub session_id: Option<SessionId>,
    pub attribution: AttributionSnapshot,
    pub status: ExecutionStatus,
    /// Exit code of the supervised process, when it has exited. A zero exit
    /// code is a process fact, never a completion verdict.
    pub process_exit: Option<i32>,
    /// Evidence string required for `Completed`: what proves the outcome.
    pub completion_evidence: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl ExecutionRecord {
    pub fn start(task_id: TaskId, attribution: AttributionSnapshot) -> Self {
        let now = utc_now();
        Self {
            execution_id: ExecutionId::new(),
            task_id,
            session_id: None,
            attribution,
            status: ExecutionStatus::Requested,
            process_exit: None,
            completion_evidence: None,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    #[must_use]
    pub fn with_session(mut self, session: SessionId) -> Self {
        self.session_id = Some(session);
        self
    }

    /// Record that the supervised process exited. Status is deliberately
    /// untouched: an exit code never completes, stops or fails an execution
    /// by itself.
    pub fn record_process_exit(&mut self, code: i32) {
        self.process_exit = Some(code);
        self.updated_at = utc_now();
    }

    /// Mark completed **with evidence**. There is no way to set `Completed`
    /// without evidence here, and the schema CHECK backstops raw SQL.
    pub fn complete(&mut self, evidence: impl Into<String>) {
        self.status = ExecutionStatus::Completed;
        self.completion_evidence = Some(evidence.into());
        self.updated_at = utc_now();
    }

    /// Transition to a non-completed status.
    pub fn mark(&mut self, status: ExecutionStatus) -> OfficeResult<()> {
        if status == ExecutionStatus::Completed {
            return Err(OfficeError::Validation(
                "completion requires evidence; use ExecutionRecord::complete".into(),
            ));
        }
        self.status = status;
        self.updated_at = utc_now();
        Ok(())
    }
}

pub fn insert_execution(store: &Store, execution: &ExecutionRecord) -> OfficeResult<()> {
    store.connection().execute(
        "INSERT INTO office_executions(
            execution_id, task_id, session_id, member_id, attribution, status,
            process_exit, completion_evidence, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            execution.execution_id.as_str(),
            execution.task_id.as_str(),
            execution.session_id.as_ref().map(|s| s.as_str()),
            execution.attribution.member_id.as_str(),
            execution.attribution.to_json()?,
            execution.status.as_str(),
            execution.process_exit,
            execution.completion_evidence,
            execution.created_at,
            execution.updated_at,
        ],
    )?;
    Ok(())
}

pub fn get_execution(
    store: &Store,
    execution_id: &ExecutionId,
) -> OfficeResult<Option<ExecutionRecord>> {
    let mut stmt = store.connection().prepare(
        "SELECT task_id, session_id, member_id, attribution, status,
                process_exit, completion_evidence, created_at, updated_at
         FROM office_executions WHERE execution_id = ?1",
    )?;
    let row = stmt
        .query_row([execution_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })
        .optional()?;
    let Some((
        task_id,
        session_id,
        member_id,
        attribution_json,
        status,
        process_exit,
        completion_evidence,
        created_at,
        updated_at,
    )) = row
    else {
        return Ok(None);
    };
    let attribution: AttributionSnapshot =
        serde_json::from_str(&attribution_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
        })?;
    // The `member_id` column is the query-facing copy of the snapshot's
    // member; a mismatch would mean someone edited one side by hand.
    if attribution.member_id.as_str() != member_id {
        return Err(OfficeError::Validation(format!(
            "execution `{execution_id}` attribution member does not match its member_id column"
        )));
    }
    let status = match status.as_str() {
        "requested" => ExecutionStatus::Requested,
        "running" => ExecutionStatus::Running,
        "stopped" => ExecutionStatus::Stopped,
        "failed" => ExecutionStatus::Failed,
        "completed" => ExecutionStatus::Completed,
        other => panic!("office_executions.status holds an unknown value `{other}`"),
    };
    Ok(Some(ExecutionRecord {
        execution_id: execution_id.clone(),
        task_id: TaskId::from_str(&task_id).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        session_id: session_id
            .as_deref()
            .and_then(|s| SessionId::from_str(s).ok()),
        attribution,
        status,
        process_exit: process_exit.map(|code| code as i32),
        completion_evidence,
        created_at,
        updated_at,
    }))
}

// ---------------------------------------------------------------------------
// Launch specifications
// ---------------------------------------------------------------------------

/// Where a launch request came from. Member dispatch and automation must name
/// their grant; the grant's scope enforcement belongs to the authority domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum RequestOrigin {
    MemberDispatch { grant_id: GrantId },
    UserDirect,
    Automation { grant_id: GrantId },
}

/// Resource budget for a launch. V01 fixes the shape; enforcement lands with
/// the execution supervisor (V05+) and the baselines (V12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceBudget {
    /// Cap on captured output bytes (bounded logging).
    pub max_output_bytes: u64,
    /// Wall-clock cap in seconds.
    pub max_runtime_secs: u64,
}

/// Explicit launch specification. argv is a vector (program + arguments);
/// there is deliberately no field that accepts a joined shell string as a
/// generic entry point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchSpec {
    pub spec_id: LaunchSpecId,
    pub execution_id: Option<ExecutionId>,
    pub terminal_id: Option<TerminalId>,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub member_id: Option<MemberId>,
    pub model_binding: Option<String>,
    pub tool_binding: Option<String>,
    pub worktree_id: Option<WorktreeId>,
    pub request_origin: RequestOrigin,
    pub budget: Option<ResourceBudget>,
    pub created_at: String,
}

impl LaunchSpec {
    pub fn new(
        argv: Vec<String>,
        cwd: impl Into<PathBuf>,
        request_origin: RequestOrigin,
    ) -> OfficeResult<Self> {
        let spec = Self {
            spec_id: LaunchSpecId::new(),
            execution_id: None,
            terminal_id: None,
            argv,
            cwd: cwd.into(),
            member_id: None,
            model_binding: None,
            tool_binding: None,
            worktree_id: None,
            request_origin,
            budget: None,
            created_at: utc_now(),
        };
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> OfficeResult<()> {
        let Some(program) = self.argv.first() else {
            return Err(OfficeError::Validation(
                "launch spec argv must name a program; empty argv is rejected".into(),
            ));
        };
        if program.trim().is_empty() {
            return Err(OfficeError::Validation(
                "launch spec argv[0] must be a program, not an empty string".into(),
            ));
        }
        if !self.cwd.is_absolute() {
            return Err(OfficeError::Validation(
                "launch spec cwd must be an absolute path".into(),
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn for_execution(mut self, execution: ExecutionId) -> Self {
        self.execution_id = Some(execution);
        self
    }

    #[must_use]
    pub fn on_terminal(mut self, terminal: TerminalId) -> Self {
        self.terminal_id = Some(terminal);
        self
    }

    #[must_use]
    pub fn with_member(mut self, member: MemberId) -> Self {
        self.member_id = Some(member);
        self
    }

    #[must_use]
    pub fn with_bindings(mut self, model: impl Into<String>, tool: impl Into<String>) -> Self {
        self.model_binding = Some(model.into());
        self.tool_binding = Some(tool.into());
        self
    }

    #[must_use]
    pub fn in_worktree(mut self, worktree: WorktreeId) -> Self {
        self.worktree_id = Some(worktree);
        self
    }

    #[must_use]
    pub fn with_budget(mut self, budget: ResourceBudget) -> Self {
        self.budget = Some(budget);
        self
    }
}

pub fn insert_launch_spec(store: &Store, spec: &LaunchSpec) -> OfficeResult<()> {
    spec.validate()?;
    store.connection().execute(
        "INSERT INTO launch_specs(
            spec_id, execution_id, terminal_id, argv, cwd, member_id,
            model_binding, tool_binding, worktree_id, request_origin,
            budget_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            spec.spec_id.as_str(),
            spec.execution_id.as_ref().map(|e| e.as_str()),
            spec.terminal_id.as_ref().map(|t| t.as_str()),
            serde_json::to_string(&spec.argv)?,
            spec.cwd.to_string_lossy().as_ref(),
            spec.member_id.as_ref().map(|m| m.as_str()),
            spec.model_binding,
            spec.tool_binding,
            spec.worktree_id.as_ref().map(|w| w.as_str()),
            serde_json::to_string(&spec.request_origin)?,
            spec.budget
                .map(|budget| serde_json::to_string(&budget))
                .transpose()?,
            spec.created_at,
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Terminals
// ---------------------------------------------------------------------------

/// Who a terminal belongs to. A member-execution terminal carries its
/// execution id inside the variant; user shells and other auxiliaries have no
/// execution id to fake — the type makes it unrepresentable, and the DB
/// CHECK backstops raw SQL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalOwner {
    MemberExecution(ExecutionId),
    UserShell,
    AgentCli,
    TestRun,
}

impl TerminalOwner {
    fn db_columns(&self) -> (&'static str, Option<String>) {
        match self {
            TerminalOwner::MemberExecution(execution) => {
                ("member_execution", Some(execution.to_string()))
            }
            TerminalOwner::UserShell => ("user_shell", None),
            TerminalOwner::AgentCli => ("agent_cli", None),
            TerminalOwner::TestRun => ("test_run", None),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalEventKind {
    Spawned,
    Input,
    Resize,
    Snapshot,
    Stopped,
    Exited,
}

impl TerminalEventKind {
    /// Stable DB representation (plain token, no JSON quoting).
    pub fn as_str(&self) -> &'static str {
        match self {
            TerminalEventKind::Spawned => "spawned",
            TerminalEventKind::Input => "input",
            TerminalEventKind::Resize => "resize",
            TerminalEventKind::Snapshot => "snapshot",
            TerminalEventKind::Stopped => "stopped",
            TerminalEventKind::Exited => "exited",
        }
    }
}

/// One terminal lifecycle/state event. Terminal state and process events are
/// separate records; the terminal registry itself lands with V05.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalEvent {
    pub terminal_id: TerminalId,
    pub owner: TerminalOwner,
    pub event_kind: TerminalEventKind,
    pub payload: Value,
    pub occurred_at: String,
}

impl TerminalEvent {
    pub fn new(
        terminal_id: TerminalId,
        owner: TerminalOwner,
        event_kind: TerminalEventKind,
        payload: Value,
    ) -> Self {
        Self {
            terminal_id,
            owner,
            event_kind,
            payload,
            occurred_at: utc_now(),
        }
    }
}

pub fn insert_terminal_event(store: &Store, event: &TerminalEvent) -> OfficeResult<i64> {
    let (owner, execution_id) = event.owner.db_columns();
    store.connection().execute(
        "INSERT INTO terminal_events(terminal_id, owner, execution_id, event_kind, payload, occurred_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            event.terminal_id.as_str(),
            owner,
            execution_id,
            event.event_kind.as_str(),
            serde_json::to_string(&event.payload)?,
            event.occurred_at,
        ],
    )?;
    Ok(store.connection().last_insert_rowid())
}

// ---------------------------------------------------------------------------
// Workbench contracts
// ---------------------------------------------------------------------------

/// A project as the workbench shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub project_id: ProjectId,
    pub display_name: String,
    pub repo_path: PathBuf,
}

/// A worktree as the workbench shows it. Multiple terminals with different
/// purposes may attach to one worktree; purposes live on terminals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSummary {
    pub worktree_id: WorktreeId,
    pub project_id: ProjectId,
    pub path: PathBuf,
}

/// A terminal attached to a worktree, as the workbench shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkbenchTerminalSummary {
    pub terminal_id: TerminalId,
    pub worktree_id: WorktreeId,
    pub owner: TerminalOwner,
}

/// Workbench query contracts: pure projections over existing facts. Lists and
/// needs-attention markers are projections — the workbench never introduces a
/// second workflow state. Implementations land with their owning domains
/// (projects: V02, worktrees: V08, terminals: V05/V07); the signatures are
/// frozen here.
pub trait WorkbenchQuery {
    fn projects(&self) -> OfficeResult<Vec<ProjectSummary>>;
    fn worktrees(&self, project: &ProjectId) -> OfficeResult<Vec<WorktreeSummary>>;
    fn terminals(&self, worktree: &WorktreeId) -> OfficeResult<Vec<WorkbenchTerminalSummary>>;
}

/// Workbench action contracts: opening and stopping terminals records facts;
/// workbench actions never mutate Task or Execution state directly — their
/// domains own those transitions.
pub trait WorkbenchActions {
    fn open_terminal(
        &mut self,
        worktree: &WorktreeId,
        owner: TerminalOwner,
    ) -> OfficeResult<WorkbenchTerminalSummary>;
    fn stop_terminal(&mut self, terminal_id: &TerminalId) -> OfficeResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::store::{
        DOMAIN_FOUNDATION, FOUNDATION_V1_SQL, MigrationRegistry, Store,
    };
    use serde_json::json;

    fn store() -> Store {
        let frozen = MigrationRegistry::new()
            .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
            .freeze()
            .expect("registry");
        Store::open_in_memory(&frozen).expect("store")
    }

    #[test]
    fn conversation_session_persists_without_task() {
        let store = store();
        let session = SessionRecord::new(SessionKind::Conversation, "Morning chat")
            .with_member(MemberId::new())
            .with_harness_ref(HarnessSessionRef {
                harness: "pi".into(),
                native_session_id: "pi-native-1".into(),
            });
        insert_session(&store, &session).expect("insert");

        let sessions = list_sessions(&store).expect("list");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0], session);
        assert!(sessions[0].task_id().is_none());
        // The harness reference is a pointer, not a copy of chat facts.
        assert_eq!(
            sessions[0]
                .harness_ref
                .as_ref()
                .expect("harness ref")
                .harness,
            "pi"
        );
    }

    #[test]
    fn task_execution_session_carries_its_task() {
        let store = store();
        let task = TaskId::new();
        let session = SessionRecord::new(SessionKind::TaskExecution(task.clone()), "Ship V01");
        assert_eq!(session.task_id(), Some(&task));
        insert_session(&store, &session).expect("insert");
        assert_eq!(
            list_sessions(&store).expect("list")[0].kind,
            SessionKind::TaskExecution(task)
        );
    }

    #[test]
    fn database_backstops_task_execution_without_task() {
        let store = store();
        let err = store.connection().execute(
            "INSERT INTO office_sessions(session_id, kind, title, task_id, member_id, harness, harness_native_session_id, created_at)
             VALUES ('sess-x', 'task_execution', 't', NULL, NULL, NULL, NULL, 'now')",
            [],
        );
        assert!(
            err.is_err(),
            "task_execution without task_id must violate the CHECK"
        );
    }

    #[test]
    fn execution_requires_task_and_completion_requires_evidence() {
        let store = store();

        // Raw SQL backstop: no task, no execution row.
        let err = store.connection().execute(
            "INSERT INTO office_executions(execution_id, member_id, attribution, status, created_at, updated_at)
             VALUES ('exec-x', 'mem-x', '{}', 'requested', 'now', 'now')",
            [],
        );
        assert!(err.is_err(), "execution without task_id must be rejected");

        // API: exit code recorded, status untouched.
        let task = TaskId::new();
        let attribution =
            AttributionSnapshot::capture(MemberId::new(), "developer", "glm-5.3-flash", "pi-cli");
        let mut execution = ExecutionRecord::start(task, attribution);
        execution.record_process_exit(0);
        assert_eq!(
            execution.status,
            ExecutionStatus::Requested,
            "a zero exit code never completes an execution"
        );

        // Raw SQL backstop: completed without evidence is unrepresentable.
        execution.complete("cargo test g0: all green");
        insert_execution(&store, &execution).expect("insert");
        let err = store.connection().execute(
            "UPDATE office_executions SET status = 'completed', completion_evidence = NULL",
            [],
        );
        assert!(
            err.is_err(),
            "completed without evidence must violate the CHECK"
        );

        let reloaded = get_execution(&store, &execution.execution_id)
            .expect("get")
            .expect("exists");
        assert_eq!(reloaded.status, ExecutionStatus::Completed);
        assert_eq!(
            reloaded.completion_evidence.as_deref(),
            Some("cargo test g0: all green")
        );
        assert_eq!(reloaded.process_exit, Some(0));
        assert_eq!(
            reloaded.attribution, execution.attribution,
            "attribution roundtrips unchanged"
        );
    }

    #[test]
    fn mark_rejects_completed_without_evidence() {
        let mut execution = ExecutionRecord::start(
            TaskId::new(),
            AttributionSnapshot::capture(MemberId::new(), "qa", "glm-5.3-flash", "cli"),
        );
        let err = execution
            .mark(ExecutionStatus::Completed)
            .expect_err("completed needs evidence");
        assert!(err.to_string().contains("evidence"), "got: {err}");
        execution
            .mark(ExecutionStatus::Running)
            .expect("running is fine");
        assert_eq!(execution.status, ExecutionStatus::Running);
    }

    #[test]
    fn launch_spec_rejects_empty_argv_and_relative_cwd() {
        let err = LaunchSpec::new(
            vec![],
            std::path::Path::new("/tmp"),
            RequestOrigin::UserDirect,
        )
        .expect_err("empty argv must fail");
        assert!(err.to_string().contains("argv"), "got: {err}");

        let err = LaunchSpec::new(
            vec!["pi".into()],
            std::path::Path::new("relative/path"),
            RequestOrigin::UserDirect,
        )
        .expect_err("relative cwd must fail");
        assert!(err.to_string().contains("absolute"), "got: {err}");
    }

    #[test]
    fn launch_spec_persists_with_explicit_argv() {
        let store = store();
        let task = TaskId::new();
        let execution = ExecutionRecord::start(
            task,
            AttributionSnapshot::capture(MemberId::new(), "developer", "glm-5.3-flash", "pi-cli"),
        );
        let spec = LaunchSpec::new(
            vec!["pi".into(), "--help".into()],
            std::env::temp_dir(),
            RequestOrigin::MemberDispatch {
                grant_id: GrantId::new(),
            },
        )
        .expect("spec")
        .for_execution(execution.execution_id.clone())
        .with_member(MemberId::new())
        .with_budget(ResourceBudget {
            max_output_bytes: 1 << 20,
            max_runtime_secs: 600,
        });
        insert_launch_spec(&store, &spec).expect("insert");
        assert_eq!(store.row_count("launch_specs").expect("count"), 1);
    }

    #[test]
    fn terminal_owner_cannot_cross_impersonate() {
        let store = store();

        // Member execution terminal: valid, carries its execution.
        let event = TerminalEvent::new(
            TerminalId::new(),
            TerminalOwner::MemberExecution(ExecutionId::new()),
            TerminalEventKind::Spawned,
            json!({"argv0": "pi"}),
        );
        insert_terminal_event(&store, &event).expect("member execution terminal");

        // A user shell carrying an execution id is unrepresentable via raw SQL.
        let err = store.connection().execute(
            "INSERT INTO terminal_events(terminal_id, owner, execution_id, event_kind, payload, occurred_at)
             VALUES ('term-fake', 'user_shell', 'exec-real', 'spawned', '{}', 'now')",
            [],
        );
        assert!(
            err.is_err(),
            "user shell with execution id must violate the CHECK"
        );

        // And a member execution terminal without its execution id, too.
        let err = store.connection().execute(
            "INSERT INTO terminal_events(terminal_id, owner, execution_id, event_kind, payload, occurred_at)
             VALUES ('term-orphan', 'member_execution', NULL, 'spawned', '{}', 'now')",
            [],
        );
        assert!(
            err.is_err(),
            "member execution terminal without execution id must violate the CHECK"
        );

        assert_eq!(store.row_count("terminal_events").expect("count"), 1);
    }

    #[test]
    fn workbench_contract_shape_is_usable() {
        // A minimal in-memory implementation proves the frozen trait shapes
        // are implementable; real projections land with V02/V05/V08.
        struct Bench {
            terminals: Vec<WorkbenchTerminalSummary>,
        }
        impl WorkbenchQuery for Bench {
            fn projects(&self) -> OfficeResult<Vec<ProjectSummary>> {
                Ok(vec![ProjectSummary {
                    project_id: ProjectId::new(),
                    display_name: "viva".into(),
                    repo_path: std::path::PathBuf::from("/Users/hzuo/Documents/code/viva"),
                }])
            }
            fn worktrees(&self, _project: &ProjectId) -> OfficeResult<Vec<WorktreeSummary>> {
                Ok(vec![WorktreeSummary {
                    worktree_id: WorktreeId::new(),
                    project_id: ProjectId::new(),
                    path: std::path::PathBuf::from("/tmp/wt"),
                }])
            }
            fn terminals(
                &self,
                _worktree: &WorktreeId,
            ) -> OfficeResult<Vec<WorkbenchTerminalSummary>> {
                Ok(self.terminals.clone())
            }
        }
        impl WorkbenchActions for Bench {
            fn open_terminal(
                &mut self,
                worktree: &WorktreeId,
                owner: TerminalOwner,
            ) -> OfficeResult<WorkbenchTerminalSummary> {
                let summary = WorkbenchTerminalSummary {
                    terminal_id: TerminalId::new(),
                    worktree_id: worktree.clone(),
                    owner,
                };
                self.terminals.push(summary.clone());
                Ok(summary)
            }
            fn stop_terminal(&mut self, terminal_id: &TerminalId) -> OfficeResult<()> {
                self.terminals.retain(|t| &t.terminal_id != terminal_id);
                Ok(())
            }
        }

        let mut bench = Bench {
            terminals: Vec::new(),
        };
        let worktree = WorktreeId::new();
        let opened = bench
            .open_terminal(&worktree, TerminalOwner::UserShell)
            .expect("open");
        assert_eq!(bench.terminals(&worktree).expect("query").len(), 1);
        bench.stop_terminal(&opened.terminal_id).expect("stop");
        assert!(bench.terminals(&worktree).expect("query").is_empty());
    }
}
