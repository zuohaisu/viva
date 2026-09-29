//! Configurable delivery workflows over Tasks (F01, issue #23).
//!
//! Semantics:
//! - A workflow is **data**: an ordered list of steps, each naming a role,
//!   the evidence required to pass it, a retry budget, and explicit
//!   pass/fail transitions. Two shapes ship as tested configuration — a
//!   delivery shape (implement → verify → independent review → authorized
//!   delivery, with a bounded fix step off the happy path) and a read-only
//!   review shape — proving the Dev→QA ordering is configuration, never
//!   code.
//! - Advancement is driven by real recorded results only. A passing result
//!   must attach evidence covering every requirement of the step, and any
//!   evidence carrying a head SHA must name the run's exact head — a CI or
//!   verification result only ever applies to the commit it was produced
//!   on, never to a head that moved on.
//! - Failures consume the attempted step's retry budget. While budget
//!   remains the run routes by the step's `on_fail` (default: retry the
//!   same step); when the budget is exhausted the run **pauses** with the
//!   reason. Pause is an honest state — it is never rewritten into a pass.
//! - The retired ticket pipeline stays retired: there is no ticket, run
//!   queue or verdict object here. A run is bound to exactly one task, at
//!   most one run is active per task, and the run never closes the task —
//!   completion is still an appended task outcome with evidence.
//! - Protected actions can never enter a workflow: a config naming
//!   `merge_pull_request`, `approve_pull_request` or any other protected
//!   action is rejected at registration, so developer/QA members can never
//!   self-approve or self-merge through a workflow. A delivery step cannot
//!   pass on a promise either: its authorization evidence must cite a live
//!   owner grant that covers `deliver_pr` (checked against the authority
//!   engine, denials appended).

use std::str::FromStr as _;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::authority::{Actor, AuthorityEngine, PROTECTED_ACTIONS};
use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::{GrantId, MemberId, TaskId, utc_now};
use crate::foundation::store::{DOMAIN_WORKFLOWS, MigrationRegistry, Store};

/// Register the `workflows` domain migrations (F01's namespace; runs
/// reference the `tasks` slice without altering it).
pub fn register_migrations(registry: MigrationRegistry) -> MigrationRegistry {
    registry.register(DOMAIN_WORKFLOWS, 1, "workflows v1", WORKFLOWS_V1_SQL)
}

pub const WORKFLOWS_V1_SQL: &str = r#"
CREATE TABLE workflow_configs (
    config_id    TEXT PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE,
    config_json  TEXT NOT NULL,
    created_at   TEXT NOT NULL
);

-- One run per delivery attempt of a task. At most one run may be active
-- (running or paused) per task.
-- Layered-state semantics (mirrors V03's launch intents): run status and
-- current_step are PROCESS facts (where the workflow is), never delivery
-- facts. Nothing here closes, fails or reopens a task — the task's own
-- status changes only through appended task_outcomes, so the two layers
-- cannot compete for the same fact.
CREATE TABLE workflow_runs (
    run_id        TEXT PRIMARY KEY,
    task_id       TEXT NOT NULL REFERENCES tasks(task_id),
    config_id     TEXT NOT NULL REFERENCES workflow_configs(config_id),
    status        TEXT NOT NULL CHECK (status IN ('running', 'paused', 'completed', 'aborted')),
    current_step  INTEGER NOT NULL,
    head_sha      TEXT NOT NULL,
    pause_reason  TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
CREATE UNIQUE INDEX one_active_workflow_run_per_task
    ON workflow_runs(task_id) WHERE status IN ('running', 'paused');

-- Append-only step history: every attempt, pass or fail, keeps its actor,
-- evidence and head binding. Process records are recoverable from here.
CREATE TABLE workflow_step_records (
    record_id   TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL REFERENCES workflow_runs(run_id),
    step_index  INTEGER NOT NULL,
    step_id     TEXT NOT NULL,
    role        TEXT NOT NULL,
    actor_member_id TEXT NOT NULL,
    outcome     TEXT NOT NULL CHECK (outcome IN ('passed', 'failed')),
    evidence_json TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);
"#;

// ---------------------------------------------------------------------------
// Workflow configuration (data, not code)
// ---------------------------------------------------------------------------

/// A workflow configuration: ordered steps with roles, evidence
/// requirements, retry budgets and explicit transitions. Shapes are data —
/// nothing in the engine knows about "developer then QA".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowConfig {
    pub name: String,
    pub steps: Vec<WorkflowStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub step_id: String,
    /// The role that performs this step (e.g. `implementer`, `verifier`,
    /// `reviewer`, `fixer`). Free-form data; the engine never branches on
    /// a role value.
    pub role: String,
    pub title: String,
    /// Authority action this step implies, when it has one (e.g.
    /// `dispatch_delegated`). A protected action here is rejected at
    /// registration.
    pub action: Option<String>,
    /// Evidence a passing result must attach, one item per requirement.
    pub requires_evidence: Vec<EvidenceRequirement>,
    /// Retry budget: the maximum number of recorded attempts of this step
    /// within one run. Exhaustion pauses the run.
    pub max_attempts: u32,
    /// Step to move to when this step passes (default: next in order).
    pub on_pass: Option<String>,
    /// Step to route to when this step fails while budget remains
    /// (default: retry this step). Must differ from this step's id.
    pub on_fail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRequirement {
    pub kind: String,
    /// The evidence must carry the run's exact head SHA.
    pub require_head_sha: bool,
}

impl WorkflowConfig {
    /// The delivery shape: implement → verify → independent review →
    /// authorized delivery. The array order is the happy path; the bounded
    /// fix step lives OFF the path and is entered only by fail-routing
    /// from verify or review, returning to verification — fixed work
    /// re-earns its verification evidence.
    pub fn delivery_default() -> Self {
        Self {
            name: "delivery".into(),
            steps: vec![
                WorkflowStep {
                    step_id: "implement".into(),
                    role: "implementer".into(),
                    title: "Implement the change in the task worktree".into(),
                    action: Some("dispatch_delegated".into()),
                    requires_evidence: vec![EvidenceRequirement {
                        kind: "implementation".into(),
                        require_head_sha: true,
                    }],
                    max_attempts: 3,
                    on_pass: Some("verify".into()),
                    on_fail: None,
                },
                WorkflowStep {
                    step_id: "fix".into(),
                    role: "fixer".into(),
                    title: "Bounded fix pass; returns to verification".into(),
                    action: Some("dispatch_delegated".into()),
                    requires_evidence: vec![EvidenceRequirement {
                        kind: "fix".into(),
                        require_head_sha: true,
                    }],
                    max_attempts: 2,
                    on_pass: Some("verify".into()),
                    on_fail: None,
                },
                WorkflowStep {
                    step_id: "verify".into(),
                    role: "verifier".into(),
                    title: "Verify the change on the recorded head".into(),
                    action: Some("dispatch_delegated".into()),
                    requires_evidence: vec![EvidenceRequirement {
                        kind: "verification".into(),
                        require_head_sha: true,
                    }],
                    max_attempts: 2,
                    on_pass: None,
                    on_fail: Some("fix".into()),
                },
                WorkflowStep {
                    step_id: "review".into(),
                    role: "reviewer".into(),
                    title: "Independent review of the verified head".into(),
                    action: Some("read_execution".into()),
                    requires_evidence: vec![EvidenceRequirement {
                        kind: "review".into(),
                        require_head_sha: true,
                    }],
                    max_attempts: 2,
                    on_pass: None,
                    on_fail: Some("fix".into()),
                },
                WorkflowStep {
                    step_id: "deliver".into(),
                    role: "deliverer".into(),
                    title: "Open the PR under an owner grant (no merge)".into(),
                    action: Some("deliver_pr".into()),
                    requires_evidence: vec![
                        EvidenceRequirement {
                            kind: "delivery".into(),
                            require_head_sha: true,
                        },
                        EvidenceRequirement {
                            kind: "authorization".into(),
                            require_head_sha: false,
                        },
                    ],
                    max_attempts: 1,
                    on_pass: None,
                    on_fail: None,
                },
            ],
        }
    }

    /// The read-only review shape: triage → review → summary. No
    /// dispatch-class step, no fix routing — a different shape entirely,
    /// driven by the same engine.
    pub fn read_only_review_default() -> Self {
        Self {
            name: "read-only-review".into(),
            steps: vec![
                WorkflowStep {
                    step_id: "triage".into(),
                    role: "triager".into(),
                    title: "Scope the material to review".into(),
                    action: Some("read_task".into()),
                    requires_evidence: vec![EvidenceRequirement {
                        kind: "triage".into(),
                        require_head_sha: true,
                    }],
                    max_attempts: 2,
                    on_pass: None,
                    on_fail: None,
                },
                WorkflowStep {
                    step_id: "review".into(),
                    role: "reviewer".into(),
                    title: "Read-only review of the material".into(),
                    action: Some("read_execution".into()),
                    requires_evidence: vec![EvidenceRequirement {
                        kind: "review".into(),
                        require_head_sha: true,
                    }],
                    max_attempts: 2,
                    on_pass: None,
                    on_fail: None,
                },
                WorkflowStep {
                    step_id: "summarize".into(),
                    role: "reporter".into(),
                    title: "Record findings with evidence".into(),
                    action: None,
                    requires_evidence: vec![EvidenceRequirement {
                        kind: "summary".into(),
                        require_head_sha: false,
                    }],
                    max_attempts: 2,
                    on_pass: None,
                    on_fail: None,
                },
            ],
        }
    }

    fn validate(&self) -> OfficeResult<()> {
        if self.name.trim().is_empty() {
            return Err(OfficeError::Validation(
                "workflow config name must not be empty".into(),
            ));
        }
        if self.steps.is_empty() {
            return Err(OfficeError::Validation(
                "a workflow needs at least one step".into(),
            ));
        }
        let ids: Vec<&str> = self.steps.iter().map(|s| s.step_id.as_str()).collect();
        for step in &self.steps {
            if step.step_id.trim().is_empty() || step.role.trim().is_empty() {
                return Err(OfficeError::Validation(
                    "workflow steps need a step_id and a role".into(),
                ));
            }
            if step.max_attempts == 0 {
                return Err(OfficeError::Validation(format!(
                    "step `{}` needs a retry budget of at least 1",
                    step.step_id
                )));
            }
            if let Some(action) = &step.action {
                if PROTECTED_ACTIONS.contains(&action.as_str()) {
                    return Err(OfficeError::Validation(format!(
                        "step `{}` names protected action `{action}` — no workflow may \
                         carry a protected action (merge/approve are owner-only, never \
                         workflow steps)",
                        step.step_id
                    )));
                }
            }
            if ids.iter().filter(|id| **id == step.step_id).count() > 1 {
                return Err(OfficeError::Validation(format!(
                    "duplicate step id `{}`",
                    step.step_id
                )));
            }
            for (field, target) in [("on_pass", &step.on_pass), ("on_fail", &step.on_fail)] {
                if let Some(target) = target {
                    if !ids.contains(&target.as_str()) {
                        return Err(OfficeError::Validation(format!(
                            "step `{}` {field} targets unknown step `{target}`",
                            step.step_id
                        )));
                    }
                    if field == "on_fail" && target == &step.step_id {
                        return Err(OfficeError::Validation(format!(
                            "step `{}` on_fail must differ from itself",
                            step.step_id
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Evidence
// ---------------------------------------------------------------------------

/// One evidence item attached to a recorded step result. Any item carrying
/// a head SHA must name the run's exact head. Authorization evidence must
/// cite the owner grant that covers the delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepEvidence {
    pub kind: String,
    #[serde(default)]
    pub head_sha: Option<String>,
    #[serde(default)]
    pub note: String,
    /// The live owner grant authorizing a dispatch-class step. Checked
    /// against the authority engine at record time.
    #[serde(default)]
    pub grant_id: Option<String>,
    /// External references worth keeping (PR URL, CI run URL, …).
    #[serde(default)]
    pub references: Vec<String>,
}

impl StepEvidence {
    pub fn new(kind: impl Into<String>, note: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            head_sha: None,
            note: note.into(),
            grant_id: None,
            references: Vec::new(),
        }
    }

    pub fn with_head(mut self, head_sha: impl Into<String>) -> Self {
        self.head_sha = Some(head_sha.into());
        self
    }

    pub fn with_grant(mut self, grant_id: impl Into<String>) -> Self {
        self.grant_id = Some(grant_id.into());
        self
    }
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Paused,
    Completed,
    Aborted,
}

impl RunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Paused => "paused",
            RunStatus::Completed => "completed",
            RunStatus::Aborted => "aborted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub run_id: String,
    pub task_id: TaskId,
    pub config_id: String,
    pub status: RunStatus,
    /// Index into the config's steps.
    pub current_step: usize,
    pub head_sha: String,
    pub pause_reason: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// One recorded attempt of a step (append-only history).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRecord {
    pub record_id: String,
    pub run_id: String,
    pub step_index: usize,
    pub step_id: String,
    pub role: String,
    pub actor_member_id: MemberId,
    pub outcome: StepOutcome,
    pub evidence: Vec<StepEvidence>,
    pub recorded_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepOutcome {
    Passed,
    Failed,
}

impl StepOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            StepOutcome::Passed => "passed",
            StepOutcome::Failed => "failed",
        }
    }
}

/// What recording one result decided.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StepAdvance {
    /// The step passed; the run moved to the next step, or completed.
    Passed { next_step: Option<String> },
    /// The step failed; budget remains and the step retries itself.
    Retrying { attempts_left: u32 },
    /// The step failed; budget remains and the run routed to the
    /// configured step.
    RoutedTo { step_id: String },
    /// The step failed and the retry budget is exhausted: the run paused.
    Paused { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunHistory {
    pub run: WorkflowRun,
    pub config: WorkflowConfig,
    pub records: Vec<StepRecord>,
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

pub struct WorkflowEngine<'a> {
    store: &'a Store,
}

impl<'a> WorkflowEngine<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    // -- Configuration -------------------------------------------------------

    /// Register a workflow configuration. Protected actions are rejected
    /// here, structurally: no grant, no flag and no config can put a merge
    /// or an approval inside a workflow.
    pub fn register_config(&self, config: &WorkflowConfig) -> OfficeResult<String> {
        config.validate()?;
        let existing: Option<String> = self
            .store
            .connection()
            .query_row(
                "SELECT config_id FROM workflow_configs WHERE name = ?1",
                [config.name.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if existing.is_some() {
            return Err(OfficeError::Validation(format!(
                "workflow config `{}` is already registered",
                config.name
            )));
        }
        let config_id = format!("wfcfg-{}", uuid::Uuid::new_v4().simple());
        self.store.connection().execute(
            "INSERT INTO workflow_configs(config_id, name, config_json, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                config_id,
                config.name,
                serde_json::to_string(config)?,
                utc_now(),
            ],
        )?;
        Ok(config_id)
    }

    pub fn config_by_name(&self, name: &str) -> OfficeResult<Option<(String, WorkflowConfig)>> {
        let row: Option<(String, String)> = self
            .store
            .connection()
            .query_row(
                "SELECT config_id, config_json FROM workflow_configs WHERE name = ?1",
                [name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(config_id, json)| Ok((config_id, serde_json::from_str(&json)?)))
            .transpose()
    }

    // -- Run lifecycle ---------------------------------------------------------

    /// Start the run of a config against a task, bound to the exact head
    /// SHA the work will be verified on. One active run per task: a second
    /// start while a run is running or paused is refused — the task, not a
    /// queue, owns its delivery state.
    pub fn start_run(
        &self,
        task_id: &TaskId,
        config_name: &str,
        head_sha: impl Into<String>,
    ) -> OfficeResult<WorkflowRun> {
        self.require_task(task_id)?;
        let (config_id, _) =
            self.config_by_name(config_name)?
                .ok_or_else(|| OfficeError::NotFound {
                    entity: "workflow config",
                    id: config_name.to_string(),
                })?;
        let head_sha = validate_head_sha(head_sha)?;
        if let Some(active) = self.active_run_for_task(task_id)? {
            return Err(OfficeError::Validation(format!(
                "task `{}` already has an active workflow run `{}` ({}) — resolve it \
                 (complete or abort) before starting another",
                task_id,
                active.run_id,
                active.status.as_str()
            )));
        }
        let now = utc_now();
        let run = WorkflowRun {
            run_id: format!("wfrun-{}", uuid::Uuid::new_v4().simple()),
            task_id: task_id.clone(),
            config_id: config_id.clone(),
            status: RunStatus::Running,
            current_step: 0,
            head_sha,
            pause_reason: None,
            created_at: now.clone(),
            updated_at: now,
        };
        self.store.connection().execute(
            "INSERT INTO workflow_runs(run_id, task_id, config_id, status, current_step,
                                       head_sha, pause_reason, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                run.run_id,
                run.task_id.as_str(),
                run.config_id,
                run.status.as_str(),
                run.current_step as i64,
                run.head_sha,
                run.pause_reason,
                run.created_at,
                run.updated_at,
            ],
        )?;
        Ok(run)
    }

    /// The one active (running/paused) run of a task, if any.
    pub fn active_run_for_task(&self, task_id: &TaskId) -> OfficeResult<Option<WorkflowRun>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT run_id, task_id, config_id, status, current_step, head_sha,
                    pause_reason, created_at, updated_at
             FROM workflow_runs WHERE task_id = ?1 AND status IN ('running', 'paused')
             ORDER BY created_at LIMIT 1",
        )?;
        let row = stmt.query_row([task_id.as_str()], map_run).optional()?;
        Ok(row)
    }

    pub fn get_run(&self, run_id: &str) -> OfficeResult<Option<WorkflowRun>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT run_id, task_id, config_id, status, current_step, head_sha,
                    pause_reason, created_at, updated_at
             FROM workflow_runs WHERE run_id = ?1",
        )?;
        let row = stmt.query_row([run_id], map_run).optional()?;
        Ok(row)
    }

    fn require_run(&self, run_id: &str) -> OfficeResult<WorkflowRun> {
        self.get_run(run_id)?.ok_or_else(|| OfficeError::NotFound {
            entity: "workflow run",
            id: run_id.to_string(),
        })
    }

    /// Resume a paused run, optionally rebinding it to a new head (e.g.
    /// after a fix commit). Records keep their old head bindings — history
    /// shows exactly which head every earlier result belonged to. Nothing
    /// is replayed: the current step simply owes fresh evidence.
    pub fn resume_paused(
        &self,
        run_id: &str,
        new_head_sha: Option<String>,
    ) -> OfficeResult<WorkflowRun> {
        let mut run = self.require_run(run_id)?;
        if run.status != RunStatus::Paused {
            return Err(OfficeError::Validation(format!(
                "run `{}` is {}; only a paused run can be resumed",
                run_id,
                run.status.as_str()
            )));
        }
        let head_sha = match new_head_sha {
            Some(head) => validate_head_sha(head)?,
            None => run.head_sha.clone(),
        };
        self.store.connection().execute(
            "UPDATE workflow_runs SET status = 'running', head_sha = ?2, pause_reason = NULL,
                    updated_at = ?3
             WHERE run_id = ?1",
            rusqlite::params![run_id, head_sha, utc_now()],
        )?;
        run.status = RunStatus::Running;
        run.head_sha = head_sha;
        run.pause_reason = None;
        run.updated_at = utc_now();
        Ok(run)
    }

    /// Abort a run. Abortion is recorded; it completes nothing.
    pub fn abort_run(&self, run_id: &str, reason: impl Into<String>) -> OfficeResult<()> {
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(OfficeError::Validation(
                "aborting a run requires a reason".into(),
            ));
        }
        let run = self.require_run(run_id)?;
        if matches!(run.status, RunStatus::Completed | RunStatus::Aborted) {
            return Err(OfficeError::Validation(format!(
                "run `{}` is already {}",
                run_id,
                run.status.as_str()
            )));
        }
        self.store.connection().execute(
            "UPDATE workflow_runs SET status = 'aborted', pause_reason = ?2, updated_at = ?3
             WHERE run_id = ?1",
            rusqlite::params![run_id, reason, utc_now()],
        )?;
        Ok(())
    }

    // -- Recording results -------------------------------------------------

    /// Record one attempt of the run's current step. This is the only way
    /// a step moves: by an appended record whose evidence stands up to the
    /// step's requirements. A pass must cover every requirement; any
    /// evidence carrying a head SHA must name the run's exact head; and a
    /// passing step with an `authorization` requirement must cite a live
    /// owner grant covering `deliver_pr` (denials land in the audit log).
    pub fn record_step_result(
        &self,
        run_id: &str,
        actor: &MemberId,
        passed: bool,
        evidence: Vec<StepEvidence>,
    ) -> OfficeResult<StepAdvance> {
        let run = self.require_run(run_id)?;
        if run.status != RunStatus::Running {
            return Err(OfficeError::Validation(format!(
                "run `{}` is {}; results are recorded on running runs only",
                run_id,
                run.status.as_str()
            )));
        }
        let (_, config) =
            self.config_by_id(&run.config_id)?
                .ok_or_else(|| OfficeError::NotFound {
                    entity: "workflow config",
                    id: run.config_id.clone(),
                })?;
        let step = config.steps.get(run.current_step).ok_or_else(|| {
            OfficeError::Validation(format!(
                "run `{}` current step index {} is outside its config ({} steps) — \
                 the config changed after the run started",
                run_id,
                run.current_step,
                config.steps.len()
            ))
        })?;

        for item in &evidence {
            if let Some(head) = &item.head_sha {
                let head = validate_head_sha(head)?;
                if !same_commit(&head, &run.head_sha) {
                    return Err(OfficeError::Validation(format!(
                        "evidence for step `{}` was bound to head {head}, but run `{}` is \
                         bound to {} — results only apply to the head they were produced on",
                        step.step_id, run_id, run.head_sha
                    )));
                }
            }
        }

        if passed {
            self.enforce_requirements(actor, &run, step, &evidence)?;
        }

        let now = utc_now();
        let record = StepRecord {
            record_id: format!("wfrec-{}", uuid::Uuid::new_v4().simple()),
            run_id: run_id.to_string(),
            step_index: run.current_step,
            step_id: step.step_id.clone(),
            role: step.role.clone(),
            actor_member_id: actor.clone(),
            outcome: if passed {
                StepOutcome::Passed
            } else {
                StepOutcome::Failed
            },
            evidence: evidence.clone(),
            recorded_at: now.clone(),
        };
        let tx = self.store.transaction()?;
        tx.execute(
            "INSERT INTO workflow_step_records(record_id, run_id, step_index, step_id, role,
                                               actor_member_id, outcome, evidence_json,
                                               recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                record.record_id,
                record.run_id,
                record.step_index as i64,
                record.step_id,
                record.role,
                record.actor_member_id.as_str(),
                record.outcome.as_str(),
                serde_json::to_string(&record.evidence)?,
                record.recorded_at,
            ],
        )?;

        let advance = if passed {
            let next_index = match &step.on_pass {
                Some(target) => self.step_index_of(&config, target)?,
                None => run.current_step + 1,
            };
            let (status, next_step, pause_reason) = if next_index >= config.steps.len() {
                (RunStatus::Completed, None::<String>, None::<String>)
            } else {
                (
                    RunStatus::Running,
                    Some(config.steps[next_index].step_id.clone()),
                    None,
                )
            };
            tx.execute(
                "UPDATE workflow_runs SET status = ?2, current_step = ?3, pause_reason = ?4,
                        updated_at = ?5
                 WHERE run_id = ?1",
                rusqlite::params![
                    run_id,
                    status.as_str(),
                    next_index as i64,
                    pause_reason,
                    now,
                ],
            )?;
            StepAdvance::Passed { next_step }
        } else {
            let attempts_used = self.count_attempts_in_tx(&tx, run_id, run.current_step)?;
            if attempts_used >= step.max_attempts {
                let reason = format!(
                    "step `{}` exhausted its retry budget ({} attempts) — paused, not failed-over",
                    step.step_id, step.max_attempts
                );
                tx.execute(
                    "UPDATE workflow_runs SET status = 'paused', pause_reason = ?2,
                            updated_at = ?3
                     WHERE run_id = ?1",
                    rusqlite::params![run_id, reason, now],
                )?;
                StepAdvance::Paused { reason }
            } else if let Some(target) = &step.on_fail {
                let target_index = self.step_index_of(&config, target)?;
                tx.execute(
                    "UPDATE workflow_runs SET current_step = ?2, updated_at = ?3
                     WHERE run_id = ?1",
                    rusqlite::params![run_id, target_index as i64, now],
                )?;
                StepAdvance::RoutedTo {
                    step_id: target.clone(),
                }
            } else {
                StepAdvance::Retrying {
                    attempts_left: step.max_attempts - attempts_used,
                }
            }
        };
        tx.commit()?;
        Ok(advance)
    }

    /// A passing step must really stand on evidence: every requirement is
    /// covered by a matching item with the required head binding, and —
    /// structurally, independent of what the config declares — a step
    /// whose action is dispatch-class (a world-mutating action such as
    /// `deliver_pr`) can only pass with `authorization` evidence citing a
    /// live owner grant. An empty `requires_evidence` list can therefore
    /// never bypass authorization (QA finding: the grant check was keyed
    /// on an evidence kind a config could omit).
    fn enforce_requirements(
        &self,
        actor: &MemberId,
        run: &WorkflowRun,
        step: &WorkflowStep,
        evidence: &[StepEvidence],
    ) -> OfficeResult<()> {
        let needs_authorization = step
            .action
            .as_deref()
            .is_some_and(AuthorityEngine::is_dispatch_action);
        for requirement in &step.requires_evidence {
            let covered = evidence.iter().any(|item| item.kind == requirement.kind);
            if !covered {
                return Err(OfficeError::Validation(format!(
                    "step `{}` cannot pass without `{}` evidence — claims are not evidence",
                    step.step_id, requirement.kind
                )));
            }
            if requirement.require_head_sha {
                let bound = evidence
                    .iter()
                    .any(|item| item.kind == requirement.kind && item.head_sha.is_some());
                if !bound {
                    return Err(OfficeError::Validation(format!(
                        "step `{}` `{}` evidence must carry the head SHA it was produced on",
                        step.step_id, requirement.kind
                    )));
                }
            }
        }
        if needs_authorization {
            let action = step.action.as_deref().expect("checked above");
            let item = evidence
                .iter()
                .find(|item| item.kind == "authorization")
                .ok_or_else(|| {
                    OfficeError::Validation(format!(
                        "step `{}` performs the world-mutating action `{action}` — passing it \
                         requires authorization evidence citing the owner grant",
                        step.step_id
                    ))
                })?;
            let grant_id = item.grant_id.as_deref().ok_or_else(|| {
                OfficeError::Validation(format!(
                    "step `{}` authorization evidence must cite the owner grant id",
                    step.step_id
                ))
            })?;
            let grant_id = GrantId::from_str(grant_id).map_err(|err| {
                OfficeError::Validation(format!("authorization grant id invalid: {err}"))
            })?;
            let engine = AuthorityEngine::new(self.store);
            let decision = engine.check(
                &Actor::Member {
                    member: actor.clone(),
                    grant: Some(grant_id),
                },
                action,
                Some(&run.task_id),
            )?;
            if let Err(reason) = decision {
                return Err(OfficeError::Validation(format!(
                    "step `{}` authorization rejected: {reason}",
                    step.step_id
                )));
            }
        }
        Ok(())
    }

    // -- History ------------------------------------------------------------

    /// The full, recoverable process record: run, its config as data, and
    /// every appended attempt with its evidence and head bindings.
    pub fn run_history(&self, run_id: &str) -> OfficeResult<RunHistory> {
        let run = self.require_run(run_id)?;
        let (_, config) =
            self.config_by_id(&run.config_id)?
                .ok_or_else(|| OfficeError::NotFound {
                    entity: "workflow config",
                    id: run.config_id.clone(),
                })?;
        let mut stmt = self.store.connection().prepare(
            "SELECT record_id, run_id, step_index, step_id, role, actor_member_id,
                    outcome, evidence_json, recorded_at
             FROM workflow_step_records WHERE run_id = ?1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map([run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?;
        let mut records = Vec::new();
        for row in rows {
            let (
                record_id,
                run_id,
                step_index,
                step_id,
                role,
                actor,
                outcome,
                evidence_json,
                recorded_at,
            ) = row?;
            records.push(StepRecord {
                record_id,
                run_id,
                step_index: step_index as usize,
                step_id,
                role,
                actor_member_id: MemberId::from_str(&actor).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        5,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                outcome: match outcome.as_str() {
                    "passed" => StepOutcome::Passed,
                    "failed" => StepOutcome::Failed,
                    other => panic!("workflow_step_records.outcome holds `{other}`"),
                },
                evidence: serde_json::from_str(&evidence_json).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        7,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                recorded_at,
            });
        }
        Ok(RunHistory {
            run,
            config,
            records,
        })
    }

    // -- Helpers ------------------------------------------------------------

    fn config_by_id(&self, config_id: &str) -> OfficeResult<Option<(String, WorkflowConfig)>> {
        let row: Option<(String, String)> = self
            .store
            .connection()
            .query_row(
                "SELECT config_id, config_json FROM workflow_configs WHERE config_id = ?1",
                [config_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(id, json)| Ok((id, serde_json::from_str(&json)?)))
            .transpose()
    }

    fn step_index_of(&self, config: &WorkflowConfig, step_id: &str) -> OfficeResult<usize> {
        config
            .steps
            .iter()
            .position(|s| s.step_id == step_id)
            .ok_or_else(|| OfficeError::Validation(format!("unknown step `{step_id}`")))
    }

    fn count_attempts_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        run_id: &str,
        step_index: usize,
    ) -> OfficeResult<u32> {
        let n: i64 = tx.query_row(
            "SELECT COUNT(*) FROM workflow_step_records WHERE run_id = ?1 AND step_index = ?2",
            rusqlite::params![run_id, step_index as i64],
            |row| row.get(0),
        )?;
        Ok(n as u32)
    }

    fn require_task(&self, task_id: &TaskId) -> OfficeResult<()> {
        let exists: Option<String> = self
            .store
            .connection()
            .query_row(
                "SELECT task_id FROM tasks WHERE task_id = ?1",
                [task_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        exists.map(|_| ()).ok_or_else(|| OfficeError::NotFound {
            entity: "task",
            id: task_id.to_string(),
        })
    }
}

fn validate_head_sha(head_sha: impl Into<String>) -> OfficeResult<String> {
    let head_sha = head_sha.into();
    let trimmed = head_sha.trim();
    if trimmed.len() < 7 || trimmed.len() > 64 || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(OfficeError::Validation(format!(
            "`{trimmed}` is not a git head SHA (expected 7–64 hex characters)"
        )));
    }
    Ok(trimmed.to_string())
}

/// Two SHA strings name the same commit when one is a prefix of the other
/// (short vs full form); both must already be hex of sane length.
fn same_commit(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    long.get(..short.len()) == Some(short)
}

fn map_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowRun> {
    let status: String = row.get(3)?;
    Ok(WorkflowRun {
        run_id: row.get(0)?,
        task_id: TaskId::from_str(&row.get::<_, String>(1)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?,
        config_id: row.get(2)?,
        status: match status.as_str() {
            "running" => RunStatus::Running,
            "paused" => RunStatus::Paused,
            "completed" => RunStatus::Completed,
            "aborted" => RunStatus::Aborted,
            other => panic!("workflow_runs.status holds an unknown value `{other}`"),
        },
        current_step: row.get::<_, i64>(4)? as usize,
        head_sha: row.get(5)?,
        pause_reason: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_validation_rejects_garbage() {
        assert!(validate_head_sha("deadbeef").is_ok());
        assert!(validate_head_sha("abc").is_err());
        assert!(validate_head_sha("not a sha!").is_err());
    }
}
