//! Task records, outcome evidence and the resumable handoff history
//! (V03, issue #12).
//!
//! Layered status semantics (rollout §2):
//! - Launch intent → launch confirmation (execution + pid) → process exit are
//!   separate facts. A crash between intent and pid write is recorded as
//!   `unresolved` — an uncertain start is never reported as success or as a
//!   clean failure.
//! - A process exit code is a process fact. It never closes a task; task
//!   completion is an appended outcome event with evidence.
//! - Task results (process / QA / PR+CI / user) are separate sources with
//!   dedup keys. A duplicate or stale result can never re-close a task,
//!   because results never close tasks at all.
//! - Attribution is captured once at creation; switching the office's current
//!   member/workspace never rewrites historical ownership.

use std::str::FromStr as _;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::{ExecutionId, MemberId, TaskId, utc_now};
use crate::foundation::records::{AttributionSnapshot, ExecutionRecord, ExecutionStatus};
use crate::foundation::store::{DOMAIN_TASKS_EXECUTIONS, MigrationRegistry, Store};

/// Register the `tasks_executions` domain migrations (V03's namespace; the
/// tables reference the foundation `office_executions` slice without
/// altering it).
pub fn register_migrations(registry: MigrationRegistry) -> MigrationRegistry {
    registry.register(
        DOMAIN_TASKS_EXECUTIONS,
        1,
        "tasks and executions v1",
        TASKS_EXECUTIONS_V1_SQL,
    )
}

pub const TASKS_EXECUTIONS_V1_SQL: &str = r#"
CREATE TABLE tasks (
    task_id             TEXT PRIMARY KEY,
    goal                TEXT NOT NULL CHECK (length(trim(goal)) > 0),
    constraints_json    TEXT NOT NULL DEFAULT '[]',
    assignee_member_id  TEXT,
    workspace_id        TEXT,
    project_id          TEXT,
    status              TEXT NOT NULL DEFAULT 'open'
                        CHECK (status IN ('open', 'in_progress', 'done', 'cancelled')),
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
);

-- Append-only outcome ledger: completion / correction / withdrawal /
-- reopening are appended evidence, never overwrites.
CREATE TABLE task_outcomes (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id      TEXT NOT NULL REFERENCES tasks(task_id),
    kind         TEXT NOT NULL CHECK (kind IN ('completed', 'corrected', 'withdrawn', 'reopened')),
    evidence     TEXT NOT NULL,
    actor        TEXT NOT NULL,
    recorded_at  TEXT NOT NULL
);

-- Layered launch lifecycle. `request_key` makes execution creation
-- idempotent: a retried dispatch cannot create a second execution.
CREATE TABLE launch_intents (
    intent_id       TEXT PRIMARY KEY,
    task_id         TEXT NOT NULL REFERENCES tasks(task_id),
    request_key     TEXT NOT NULL UNIQUE,
    member_id       TEXT NOT NULL,
    state           TEXT NOT NULL CHECK (state IN ('intended', 'confirmed', 'exited', 'unresolved')),
    execution_id    TEXT REFERENCES office_executions(execution_id),
    pid             INTEGER,
    pid_start_marker TEXT,
    intended_at     TEXT NOT NULL,
    confirmed_at    TEXT,
    exited_at       TEXT,
    exit_code       INTEGER
);

-- Task results from separate sources. A result informs; it never closes the
-- task, so a duplicate or stale result cannot re-close anything.
CREATE TABLE task_results (
    result_id     TEXT PRIMARY KEY,
    task_id       TEXT NOT NULL REFERENCES tasks(task_id),
    execution_id  TEXT,
    source        TEXT NOT NULL CHECK (source IN ('process', 'qa', 'pr_ci', 'user')),
    kind          TEXT NOT NULL,
    dedup_key     TEXT NOT NULL UNIQUE,
    payload       TEXT NOT NULL,
    recorded_at   TEXT NOT NULL
);

-- Generated handoff briefs, persisted so a restart can serve them again.
CREATE TABLE task_briefs (
    brief_id     TEXT PRIMARY KEY,
    task_id      TEXT NOT NULL REFERENCES tasks(task_id),
    brief_json   TEXT NOT NULL,
    generated_at TEXT NOT NULL
);
"#;

// ---------------------------------------------------------------------------
// Task records
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: TaskId,
    pub goal: String,
    pub constraints: Vec<String>,
    pub assignee_member_id: Option<MemberId>,
    pub workspace_id: Option<crate::foundation::ids::WorkspaceId>,
    pub project_id: Option<crate::foundation::ids::ProjectId>,
    pub status: TaskStatus,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Open,
    InProgress,
    Done,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Open => "open",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Done => "done",
            TaskStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskOutcome {
    pub seq: i64,
    pub task_id: TaskId,
    pub kind: OutcomeKind,
    pub evidence: String,
    pub actor: String,
    pub recorded_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    Completed,
    Corrected,
    Withdrawn,
    Reopened,
}

impl OutcomeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            OutcomeKind::Completed => "completed",
            OutcomeKind::Corrected => "corrected",
            OutcomeKind::Withdrawn => "withdrawn",
            OutcomeKind::Reopened => "reopened",
        }
    }
}

/// Where a task result came from. The sources are deliberately separate:
/// a process exit, a QA conclusion and a PR/CI status are different facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultSource {
    Process,
    Qa,
    PrCi,
    User,
}

impl ResultSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            ResultSource::Process => "process",
            ResultSource::Qa => "qa",
            ResultSource::PrCi => "pr_ci",
            ResultSource::User => "user",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskResult {
    pub result_id: String,
    pub task_id: TaskId,
    pub execution_id: Option<ExecutionId>,
    pub source: ResultSource,
    pub kind: String,
    pub payload: serde_json::Value,
    pub recorded_at: String,
}

// ---------------------------------------------------------------------------
// Launch intents
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentState {
    /// Launch wanted; no process exists yet.
    Intended,
    /// Execution created and pid recorded.
    Confirmed,
    /// Supervised process exited (code is a process fact only).
    Exited,
    /// The host died between intent and pid write — pending reconciliation.
    Unresolved,
}

impl IntentState {
    pub fn as_str(&self) -> &'static str {
        match self {
            IntentState::Intended => "intended",
            IntentState::Confirmed => "confirmed",
            IntentState::Exited => "exited",
            IntentState::Unresolved => "unresolved",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchIntent {
    pub intent_id: String,
    pub task_id: TaskId,
    pub request_key: String,
    pub member_id: MemberId,
    pub state: IntentState,
    pub execution_id: Option<ExecutionId>,
    pub pid: Option<i64>,
    pub pid_start_marker: Option<String>,
    pub intended_at: String,
    pub confirmed_at: Option<String>,
    pub exited_at: Option<String>,
    pub exit_code: Option<i32>,
}

/// What `begin_execution` decided.
#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionStart {
    /// A fresh execution was created under this request key.
    Created(LaunchIntent),
    /// The same request key already created an execution; nothing duplicated.
    Replayed(LaunchIntent),
}

// ---------------------------------------------------------------------------
// Task registry
// ---------------------------------------------------------------------------

pub struct TaskRegistry<'a> {
    store: &'a Store,
    /// When configured (by the composition layer, V04 integration), every
    /// generated brief passes through redaction before persisting.
    redactor: Option<crate::redaction::Redactor>,
}

impl<'a> TaskRegistry<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self {
            store,
            redactor: None,
        }
    }

    /// Route generated briefs through redaction before they touch the
    /// database (QA round: briefs are Viva-written output and must respect
    /// the V04 streaming-redaction discipline).
    pub fn with_redaction(mut self, secrets: Vec<String>) -> Self {
        self.redactor = Some(crate::redaction::Redactor::new(secrets));
        self
    }

    /// Create a task. The attribution context is part of the record at
    /// creation; later "current member/workspace" changes never rewrite it.
    pub fn create_task(
        &self,
        goal: impl Into<String>,
        constraints: Vec<String>,
        assignee: Option<MemberId>,
        workspace_id: Option<crate::foundation::ids::WorkspaceId>,
        project_id: Option<crate::foundation::ids::ProjectId>,
    ) -> OfficeResult<TaskRecord> {
        let goal = goal.into();
        if goal.trim().is_empty() {
            return Err(OfficeError::Validation(
                "task goal must not be empty".into(),
            ));
        }
        let now = utc_now();
        let task = TaskRecord {
            task_id: TaskId::new(),
            goal,
            constraints,
            assignee_member_id: assignee,
            workspace_id,
            project_id,
            status: TaskStatus::Open,
            created_at: now.clone(),
            updated_at: now,
        };
        self.store.connection().execute(
            "INSERT INTO tasks(task_id, goal, constraints_json, assignee_member_id, workspace_id, project_id, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                task.task_id.as_str(),
                task.goal,
                serde_json::to_string(&task.constraints)?,
                task.assignee_member_id.as_ref().map(|m| m.as_str()),
                task.workspace_id.as_ref().map(|w| w.as_str()),
                task.project_id.as_ref().map(|p| p.as_str()),
                task.status.as_str(),
                task.created_at,
                task.updated_at,
            ],
        )?;
        Ok(task)
    }

    pub fn get_task(&self, task_id: &TaskId) -> OfficeResult<Option<TaskRecord>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT task_id, goal, constraints_json, assignee_member_id, workspace_id,
                    project_id, status, created_at, updated_at
             FROM tasks WHERE task_id = ?1",
        )?;
        let row = stmt
            .query_row([task_id.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })
            .optional()?;
        let Some((
            task_id,
            goal,
            constraints_json,
            assignee,
            workspace_id,
            project_id,
            status,
            created_at,
            updated_at,
        )) = row
        else {
            return Ok(None);
        };
        Ok(Some(TaskRecord {
            task_id: TaskId::from_str(&task_id).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            goal,
            constraints: serde_json::from_str(&constraints_json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            assignee_member_id: assignee.as_deref().and_then(|m| MemberId::from_str(m).ok()),
            workspace_id: workspace_id
                .as_deref()
                .and_then(|w| crate::foundation::ids::WorkspaceId::from_str(w).ok()),
            project_id: project_id
                .as_deref()
                .and_then(|p| crate::foundation::ids::ProjectId::from_str(p).ok()),
            status: parse_task_status(&status)?,
            created_at,
            updated_at,
        }))
    }

    pub fn require_task(&self, task_id: &TaskId) -> OfficeResult<TaskRecord> {
        self.get_task(task_id)?
            .ok_or_else(|| OfficeError::NotFound {
                entity: "task",
                id: task_id.to_string(),
            })
    }

    /// Paginated task list, oldest first.
    pub fn list_tasks(&self, offset: u64, limit: u32) -> OfficeResult<Vec<TaskRecord>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT task_id, goal, constraints_json, assignee_member_id, workspace_id,
                    project_id, status, created_at, updated_at
             FROM tasks ORDER BY created_at, task_id LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit as i64, offset as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?;
        let mut tasks = Vec::new();
        for row in rows {
            let (
                task_id,
                goal,
                constraints_json,
                assignee,
                workspace_id,
                project_id,
                status,
                created_at,
                updated_at,
            ) = row?;
            tasks.push(TaskRecord {
                task_id: TaskId::from_str(&task_id).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                goal,
                constraints: serde_json::from_str(&constraints_json).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                assignee_member_id: assignee.as_deref().and_then(|m| MemberId::from_str(m).ok()),
                workspace_id: workspace_id
                    .as_deref()
                    .and_then(|w| crate::foundation::ids::WorkspaceId::from_str(w).ok()),
                project_id: project_id
                    .as_deref()
                    .and_then(|p| crate::foundation::ids::ProjectId::from_str(p).ok()),
                status: parse_task_status(&status)?,
                created_at,
                updated_at,
            });
        }
        Ok(tasks)
    }

    // -- Layered launch lifecycle -------------------------------------------

    /// Begin an execution for a task: the launch intent and its foundation
    /// execution record (status `requested` — the intent layer) are created
    /// atomically. `request_key` is the caller's idempotency token — a retry
    /// with the same key replays the existing intent instead of creating a
    /// second execution.
    pub fn begin_execution(
        &self,
        task_id: &TaskId,
        attribution: &AttributionSnapshot,
        request_key: impl Into<String>,
    ) -> OfficeResult<ExecutionStart> {
        self.require_task(task_id)?;
        let request_key = request_key.into();
        if request_key.trim().is_empty() {
            return Err(OfficeError::Validation(
                "launch request_key must not be empty".into(),
            ));
        }
        if let Some(existing) = self.intent_by_request_key(&request_key)? {
            return Ok(ExecutionStart::Replayed(existing));
        }

        let now = utc_now();
        let intent_id = format!("intent-{}", uuid::Uuid::new_v4().simple());
        let execution = ExecutionRecord::start(task_id.clone(), attribution.clone());
        let tx = self.store.transaction()?;
        insert_execution_in(&tx, &execution)?;
        let insert = tx.execute(
            "INSERT INTO launch_intents(intent_id, task_id, request_key, member_id, state, execution_id, intended_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                intent_id,
                task_id.as_str(),
                request_key,
                attribution.member_id.as_str(),
                IntentState::Intended.as_str(),
                execution.execution_id.as_str(),
                now,
            ],
        );
        if let Err(rusqlite::Error::SqliteFailure(err, message)) = insert {
            if err.code == rusqlite::ErrorCode::ConstraintViolation {
                // A racing dispatch won this request key: drop our
                // half-made execution (the transaction rolls back) and
                // replay the winner's recorded intent.
                drop(tx);
                if let Some(existing) = self.intent_by_request_key(&request_key)? {
                    return Ok(ExecutionStart::Replayed(existing));
                }
            }
            return Err(OfficeError::Validation(format!(
                "launch intent insert failed: {}",
                message.unwrap_or_default()
            )));
        }
        tx.commit()?;
        Ok(ExecutionStart::Created(
            self.require_intent_by_request_key(&request_key)?,
        ))
    }

    /// Confirm the launch: the process exists and its pid is known. The
    /// foundation execution moves to `running`; the pid and a start marker
    /// are recorded so a later pid cannot be mistaken for this process.
    pub fn confirm_launch(
        &self,
        request_key: &str,
        pid: i64,
        pid_start_marker: impl Into<String>,
    ) -> OfficeResult<LaunchIntent> {
        let pid_start_marker = pid_start_marker.into();
        let mut intent = self.require_intent_by_request_key(request_key)?;
        match intent.state {
            IntentState::Intended => {}
            other => {
                return Err(OfficeError::Validation(format!(
                    "intent `{}` is {}; only an intended launch can be confirmed",
                    intent.intent_id,
                    other.as_str()
                )));
            }
        }
        let execution_id = intent
            .execution_id
            .clone()
            .expect("an intended intent always carries its execution");
        let now = utc_now();
        let tx = self.store.transaction()?;
        tx.execute(
            "UPDATE launch_intents
             SET state = 'confirmed', pid = ?2, pid_start_marker = ?3, confirmed_at = ?4
             WHERE request_key = ?1",
            rusqlite::params![request_key, pid, pid_start_marker, now],
        )?;
        tx.execute(
            "UPDATE office_executions SET status = 'running', updated_at = ?2
             WHERE execution_id = ?1 AND status = 'requested'",
            rusqlite::params![execution_id.as_str(), now],
        )?;
        tx.commit()?;
        intent.state = IntentState::Confirmed;
        intent.pid = Some(pid);
        intent.pid_start_marker = Some(pid_start_marker);
        intent.confirmed_at = Some(now);
        Ok(intent)
    }

    /// Record the honest "the host died before the pid was written" state.
    /// The execution stays `requested` — an uncertain start is never
    /// reported as success or as a clean failure.
    pub fn mark_unresolved(&self, request_key: &str) -> OfficeResult<LaunchIntent> {
        let mut intent = self.require_intent_by_request_key(request_key)?;
        if intent.state != IntentState::Intended {
            return Err(OfficeError::Validation(format!(
                "intent `{}` is {}; only an intended launch can become unresolved",
                intent.intent_id,
                intent.state.as_str()
            )));
        }
        self.store.connection().execute(
            "UPDATE launch_intents SET state = 'unresolved' WHERE request_key = ?1",
            [request_key],
        )?;
        intent.state = IntentState::Unresolved;
        Ok(intent)
    }

    /// Record a process exit. This is a process fact: the intent becomes
    /// `exited`, the foundation execution keeps its process_exit, and neither
    /// the task nor the execution is completed by this call.
    pub fn record_exit(&self, request_key: &str, exit_code: i32) -> OfficeResult<LaunchIntent> {
        let mut intent = self.require_intent_by_request_key(request_key)?;
        if intent.state != IntentState::Confirmed {
            return Err(OfficeError::Validation(format!(
                "intent `{}` is {}; only a confirmed launch can exit",
                intent.intent_id,
                intent.state.as_str()
            )));
        }
        let now = utc_now();
        let tx = self.store.transaction()?;
        tx.execute(
            "UPDATE launch_intents SET state = 'exited', exited_at = ?2, exit_code = ?3
             WHERE request_key = ?1",
            rusqlite::params![request_key, now, exit_code],
        )?;
        if let Some(execution_id) = &intent.execution_id {
            // Zero exit code records the process end only: the execution is
            // `stopped` (process ended, outcome pending), never `completed`.
            let new_status = if exit_code == 0 {
                ExecutionStatus::Stopped
            } else {
                ExecutionStatus::Failed
            };
            tx.execute(
                "UPDATE office_executions
                 SET status = ?2, process_exit = ?3, updated_at = ?4
                 WHERE execution_id = ?1 AND status <> 'completed'",
                rusqlite::params![execution_id.as_str(), new_status.as_str(), exit_code, now],
            )?;
        }
        tx.commit()?;
        intent.state = IntentState::Exited;
        intent.exited_at = Some(now);
        intent.exit_code = Some(exit_code);
        Ok(intent)
    }

    pub fn intent_by_request_key(&self, request_key: &str) -> OfficeResult<Option<LaunchIntent>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT intent_id, task_id, request_key, member_id, state, execution_id, pid,
                    pid_start_marker, intended_at, confirmed_at, exited_at, exit_code
             FROM launch_intents WHERE request_key = ?1",
        )?;
        let row = stmt.query_row([request_key], map_intent).optional()?;
        Ok(row)
    }

    fn require_intent_by_request_key(&self, request_key: &str) -> OfficeResult<LaunchIntent> {
        self.intent_by_request_key(request_key)?
            .ok_or_else(|| OfficeError::NotFound {
                entity: "launch intent",
                id: request_key.to_string(),
            })
    }

    /// All intents of a task, oldest first (history view).
    pub fn intents_for_task(&self, task_id: &TaskId) -> OfficeResult<Vec<LaunchIntent>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT intent_id, task_id, request_key, member_id, state, execution_id, pid,
                    pid_start_marker, intended_at, confirmed_at, exited_at, exit_code
             FROM launch_intents WHERE task_id = ?1 ORDER BY intended_at, intent_id",
        )?;
        let rows = stmt.query_map([task_id.as_str()], map_intent)?;
        let mut intents = Vec::new();
        for row in rows {
            intents.push(row?);
        }
        Ok(intents)
    }

    // -- Outcomes and results -----------------------------------------------

    /// Complete a task. The only path to `done`: an appended outcome event
    /// carrying evidence. Process exits and results never complete a task.
    /// The outcome append and the status flip share one transaction (QA
    /// round: the two writes were previously separate).
    pub fn complete_task(
        &self,
        task_id: &TaskId,
        evidence: impl Into<String>,
        actor: impl Into<String>,
    ) -> OfficeResult<()> {
        self.append_outcome(task_id, OutcomeKind::Completed, evidence, actor)
            .map(|_| ())
    }

    /// Append an outcome event (completion / correction / withdrawal /
    /// reopening) with its reason. The ledger insert and the implied task
    /// status transition commit atomically.
    pub fn append_outcome(
        &self,
        task_id: &TaskId,
        kind: OutcomeKind,
        evidence: impl Into<String>,
        actor: impl Into<String>,
    ) -> OfficeResult<TaskOutcome> {
        self.require_task(task_id)?;
        let evidence = evidence.into();
        let actor = actor.into();
        if evidence.trim().is_empty() {
            return Err(OfficeError::Validation(
                "outcome evidence must not be empty".into(),
            ));
        }
        let now = utc_now();
        let tx = self.store.transaction()?;
        tx.execute(
            "INSERT INTO task_outcomes(task_id, kind, evidence, actor, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![task_id.as_str(), kind.as_str(), evidence, actor, now],
        )?;
        // Every kind implies a status transition — including Completed —
        // and it commits atomically with the ledger row.
        let new_status = match kind {
            OutcomeKind::Corrected => "in_progress",
            OutcomeKind::Withdrawn => "cancelled",
            OutcomeKind::Reopened => "open",
            OutcomeKind::Completed => "done",
        };
        tx.execute(
            "UPDATE tasks SET status = ?2, updated_at = ?3 WHERE task_id = ?1",
            rusqlite::params![task_id.as_str(), new_status, now],
        )?;
        tx.commit()?;
        Ok(TaskOutcome {
            seq: self.store.connection().last_insert_rowid(),
            task_id: task_id.clone(),
            kind,
            evidence,
            actor,
            recorded_at: now,
        })
    }

    pub fn outcomes_for_task(&self, task_id: &TaskId) -> OfficeResult<Vec<TaskOutcome>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT seq, task_id, kind, evidence, actor, recorded_at
             FROM task_outcomes WHERE task_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([task_id.as_str()], |row| {
            let kind: String = row.get(2)?;
            let kind = match kind.as_str() {
                "completed" => OutcomeKind::Completed,
                "corrected" => OutcomeKind::Corrected,
                "withdrawn" => OutcomeKind::Withdrawn,
                "reopened" => OutcomeKind::Reopened,
                other => panic!("task_outcomes.kind holds an unknown value `{other}`"),
            };
            Ok((
                row.get::<_, i64>(0)?,
                TaskId::from_str(&row.get::<_, String>(1)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                kind,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        let mut outcomes = Vec::new();
        for row in rows {
            let (seq, task_id, kind, evidence, actor, recorded_at) = row?;
            outcomes.push(TaskOutcome {
                seq,
                task_id,
                kind,
                evidence,
                actor,
                recorded_at,
            });
        }
        Ok(outcomes)
    }

    /// Record a task result. Duplicates (same dedup key) are reported, not
    /// re-inserted. Results never change task status.
    pub fn record_result(
        &self,
        task_id: &TaskId,
        execution_id: Option<ExecutionId>,
        source: ResultSource,
        kind: impl Into<String>,
        dedup_key: impl Into<String>,
        payload: serde_json::Value,
    ) -> OfficeResult<Result<TaskResult, String>> {
        self.require_task(task_id)?;
        let dedup_key = dedup_key.into();
        let existing: Option<String> = self
            .store
            .connection()
            .query_row(
                "SELECT result_id FROM task_results WHERE dedup_key = ?1",
                [&dedup_key],
                |row| row.get(0),
            )
            .optional()?;
        if existing.is_some() {
            return Ok(Err(dedup_key));
        }
        let result = TaskResult {
            result_id: format!("result-{}", uuid::Uuid::new_v4().simple()),
            task_id: task_id.clone(),
            execution_id,
            source,
            kind: kind.into(),
            payload,
            recorded_at: utc_now(),
        };
        self.store.connection().execute(
            "INSERT INTO task_results(result_id, task_id, execution_id, source, kind, dedup_key, payload, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                result.result_id,
                result.task_id.as_str(),
                result.execution_id.as_ref().map(|e| e.as_str()),
                result.source.as_str(),
                result.kind,
                dedup_key,
                serde_json::to_string(&result.payload)?,
                result.recorded_at,
            ],
        )?;
        Ok(Ok(result))
    }

    pub fn results_for_task(&self, task_id: &TaskId) -> OfficeResult<Vec<TaskResult>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT result_id, task_id, execution_id, source, kind, payload, recorded_at
             FROM task_results WHERE task_id = ?1 ORDER BY recorded_at, result_id",
        )?;
        let rows = stmt.query_map([task_id.as_str()], |row| {
            let source: String = row.get(3)?;
            let source = match source.as_str() {
                "process" => ResultSource::Process,
                "qa" => ResultSource::Qa,
                "pr_ci" => ResultSource::PrCi,
                "user" => ResultSource::User,
                other => panic!("task_results.source holds an unknown value `{other}`"),
            };
            Ok((
                row.get::<_, String>(0)?,
                TaskId::from_str(&row.get::<_, String>(1)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                row.get::<_, Option<String>>(2)?,
                source,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        let mut results = Vec::new();
        for row in rows {
            let (result_id, task_id, execution_id, source, kind, payload, recorded_at) = row?;
            results.push(TaskResult {
                result_id,
                task_id,
                execution_id: execution_id
                    .as_deref()
                    .and_then(|e| ExecutionId::from_str(e).ok()),
                source,
                kind,
                payload: serde_json::from_str(&payload).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        5,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                recorded_at,
            });
        }
        Ok(results)
    }

    // -- Handoff brief -------------------------------------------------------

    /// Generate and persist a handoff brief from persisted facts: goal,
    /// constraints, attempts (launch intents), failures, deliverables
    /// (user/QA results) and open items. Survives restarts.
    pub fn generate_brief(&self, task_id: &TaskId) -> OfficeResult<serde_json::Value> {
        let task = self.require_task(task_id)?;
        let intents = self.intents_for_task(task_id)?;
        let results = self.results_for_task(task_id)?;
        let outcomes = self.outcomes_for_task(task_id)?;

        let attempts: Vec<_> = intents
            .iter()
            .map(|i| {
                json!({
                    "state": i.state.as_str(),
                    "member": i.member_id.as_str(),
                    "exit_code": i.exit_code,
                    "pid": i.pid,
                    "unresolved": i.state == IntentState::Unresolved,
                })
            })
            .collect();
        let failures: Vec<_> = intents
            .iter()
            .filter(|i| i.exit_code.is_some_and(|c| c != 0))
            .map(|i| json!({"intent": i.intent_id, "exit_code": i.exit_code}))
            .collect();
        let deliverables: Vec<_> = results
            .iter()
            .filter(|r| matches!(r.source, ResultSource::User | ResultSource::Qa))
            .map(|r| json!({"source": r.source.as_str(), "kind": r.kind, "payload": r.payload}))
            .collect();
        let open_items: Vec<_> = {
            let mut items = Vec::new();
            if task.status != TaskStatus::Done && task.status != TaskStatus::Cancelled {
                items.push(json!({"open": "task is not done", "status": task.status.as_str()}));
            }
            for intent in &intents {
                if intent.state == IntentState::Unresolved {
                    items.push(json!({"open": "launch intent pending reconciliation", "intent": intent.intent_id}));
                }
            }
            items
        };

        let brief = json!({
            "goal": task.goal,
            "constraints": task.constraints,
            "status": task.status.as_str(),
            "attempts": attempts,
            "failures": failures,
            "deliverables": deliverables,
            "open_items": open_items,
            "outcomes": outcomes.iter().map(|o| json!({
                "kind": o.kind.as_str(),
                "evidence": o.evidence,
                "actor": o.actor,
            })).collect::<Vec<_>>(),
        });

        let brief = match &self.redactor {
            Some(redactor) => redactor.scrub_json(&brief),
            None => brief,
        };
        let brief_json = serde_json::to_string(&brief)?;
        self.store.connection().execute(
            "INSERT INTO task_briefs(brief_id, task_id, brief_json, generated_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                format!("brief-{}", uuid::Uuid::new_v4().simple()),
                task_id.as_str(),
                brief_json,
                utc_now(),
            ],
        )?;
        Ok(brief)
    }

    /// The latest persisted brief for a task, if any (restart-safe read).
    pub fn latest_brief(&self, task_id: &TaskId) -> OfficeResult<Option<serde_json::Value>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT brief_json FROM task_briefs WHERE task_id = ?1
             ORDER BY generated_at DESC LIMIT 1",
        )?;
        let row: Option<String> = stmt
            .query_row([task_id.as_str()], |row| row.get(0))
            .optional()?;
        row.map(|text| serde_json::from_str(&text).map_err(Into::into))
            .transpose()
    }
}

fn parse_task_status(status: &str) -> OfficeResult<TaskStatus> {
    match status {
        "open" => Ok(TaskStatus::Open),
        "in_progress" => Ok(TaskStatus::InProgress),
        "done" => Ok(TaskStatus::Done),
        "cancelled" => Ok(TaskStatus::Cancelled),
        other => Err(OfficeError::Validation(format!(
            "tasks.status holds an unknown value `{other}`"
        ))),
    }
}

fn map_intent(row: &rusqlite::Row<'_>) -> rusqlite::Result<LaunchIntent> {
    let state: String = row.get(4)?;
    let state = match state.as_str() {
        "intended" => IntentState::Intended,
        "confirmed" => IntentState::Confirmed,
        "exited" => IntentState::Exited,
        "unresolved" => IntentState::Unresolved,
        other => panic!("launch_intents.state holds an unknown value `{other}`"),
    };
    Ok(LaunchIntent {
        intent_id: row.get(0)?,
        task_id: TaskId::from_str(&row.get::<_, String>(1)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?,
        request_key: row.get(2)?,
        member_id: MemberId::from_str(&row.get::<_, String>(3)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
        })?,
        state,
        execution_id: row
            .get::<_, Option<String>>(5)?
            .as_deref()
            .and_then(|e| ExecutionId::from_str(e).ok()),
        pid: row.get(6)?,
        pid_start_marker: row.get(7)?,
        intended_at: row.get(8)?,
        confirmed_at: row.get(9)?,
        exited_at: row.get(10)?,
        exit_code: row.get::<_, Option<i64>>(11)?.map(|c| c as i32),
    })
}

/// Insert an execution inside a caller-owned transaction.
fn insert_execution_in(
    tx: &rusqlite::Transaction<'_>,
    execution: &ExecutionRecord,
) -> OfficeResult<()> {
    tx.execute(
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
