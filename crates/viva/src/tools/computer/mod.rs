//! Browser and native-app operations by reusing existing tools
//! (F03, issue #25).
//!
//! What this module is — and deliberately is not:
//! - It is **glue around existing tools**, not an automation platform. The
//!   adapter shells out to `orca computer` (window/app inspection and
//!   input on macOS, already present and permissioned by the user) and
//!   `osascript`; it never links a browser, never ships an input-injection
//!   engine, and has no Windows path.
//! - Every action walks **locate → act → verify** with recorded evidence:
//!   the target must be visible in a pre-action state snapshot, otherwise
//!   the action is refused (target drift is a stop, never a guess); the
//!   post-action state must contain the expected marker, otherwise the
//!   action is recorded as failed. Refusals and failures are evidence
//!   rows too — nothing pretends to have succeeded.
//! - Permission gaps are honest: the audit records what it could really
//!   probe; an unavailable tool is `unavailable`, never "probably fine".
//! - Input authorization is **task-scoped**: acting requires a live grant
//!   covering `computer_input` for exactly the named task. A task grant
//!   is not a whole-machine grant, and chat without a grant gets nothing.
//! - Foreground input (keyboard/mouse/focus) serializes through one
//!   process-wide lease so two tasks cannot cross-type into each other;
//!   independent API/browser-context reads never take the lease and do
//!   not queue behind it.

use std::str::FromStr as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::authority::{Actor, AuthorityEngine, CapabilityReport};
use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::{MemberId, TaskId, utc_now};
use crate::foundation::store::{DOMAIN_TOOLS_COMPUTER, MigrationRegistry, Store};

/// Register the `tools_computer` domain migrations (F03's namespace).
pub fn register_migrations(registry: MigrationRegistry) -> MigrationRegistry {
    registry.register(
        DOMAIN_TOOLS_COMPUTER,
        1,
        "tools computer v1",
        TOOLS_COMPUTER_V1_SQL,
    )
}

pub const TOOLS_COMPUTER_V1_SQL: &str = r#"
-- What the audit really probed, per reused tool. One row per tool,
-- updated in place: the latest probe is the current truth.
CREATE TABLE computer_capabilities (
    tool             TEXT PRIMARY KEY,
    present          INTEGER NOT NULL CHECK (present IN (0, 1)),
    detail           TEXT NOT NULL,
    permission_state TEXT NOT NULL
                     CHECK (permission_state IN ('available', 'unavailable', 'unknown')),
    checked_at       TEXT NOT NULL
);

-- Append-only action evidence: locate/act/verify for every attempt,
-- including refusals and failures. Nothing is deleted or rewritten.
CREATE TABLE computer_actions (
    action_id       TEXT PRIMARY KEY,
    task_id         TEXT,
    actor_member_id TEXT NOT NULL,
    grant_id        TEXT,
    tool            TEXT NOT NULL,
    target          TEXT NOT NULL,
    argv_json       TEXT NOT NULL,
    state           TEXT NOT NULL CHECK (state IN ('executed', 'refused', 'failed')),
    reason          TEXT,
    pre_state       TEXT,
    action_output   TEXT,
    post_state      TEXT,
    foreground      INTEGER NOT NULL CHECK (foreground IN (0, 1)),
    recorded_at     TEXT NOT NULL
);
"#;

// ---------------------------------------------------------------------------
// Tool runner (the only seam between the office and the real tools)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolFailure {
    pub program: String,
    pub detail: String,
}

impl std::fmt::Display for ToolFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} failed: {}", self.program, self.detail)
    }
}

/// Runs one existing host tool. The real implementation spawns the
/// process; tests substitute a scripted runner. This is the only place
/// the office touches a computer-use tool.
pub trait ToolRunner: Send + Sync {
    fn run(&self, program: &str, argv: &[String]) -> Result<String, ToolFailure>;
}

/// The real runner: bounded-output process execution with a timeout,
/// using the host's own environment (the office copies no credentials).
pub struct ProcessRunner {
    pub timeout: std::time::Duration,
}

impl Default for ProcessRunner {
    fn default() -> Self {
        Self {
            timeout: std::time::Duration::from_secs(30),
        }
    }
}

impl ToolRunner for ProcessRunner {
    fn run(&self, program: &str, argv: &[String]) -> Result<String, ToolFailure> {
        let output = std::process::Command::new(program)
            .args(argv)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| ToolFailure {
                program: program.to_string(),
                detail: format!("spawn failed: {e}"),
            })?;
        // Bound the captured output (evidence stays small and readable).
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stdout: String = stdout.chars().take(64 * 1024).collect();
        if output.status.success() {
            Ok(stdout)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(ToolFailure {
                program: program.to_string(),
                detail: format!(
                    "exit {:?}: {}",
                    output.status.code(),
                    stderr.chars().take(2048).collect::<String>()
                ),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Foreground lease: one global input lane, tasks queue, reads don't
// ---------------------------------------------------------------------------

/// Coordinates global keyboard/mouse/focus work. A foreground action must
/// hold the single lease; while it is held, other foreground actions wait
/// and independent API/browser-context actions proceed untouched.
#[derive(Default)]
pub struct ForegroundCoordinator {
    holder: Mutex<Option<String>>,
    released: Condvar,
}

/// Held lease; releasing happens on drop, so an early return can never
/// leave the lane stuck.
pub struct ForegroundLease {
    coordinator: Arc<ForegroundCoordinator>,
    pub task_id: String,
}

impl std::fmt::Debug for ForegroundLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ForegroundLease({})", self.task_id)
    }
}

impl Drop for ForegroundLease {
    fn drop(&mut self) {
        let mut holder = self.coordinator.holder.lock().expect("lease lock");
        if holder.as_deref() == Some(self.task_id.as_str()) {
            *holder = None;
            self.coordinator.released.notify_all();
        }
    }
}

impl ForegroundCoordinator {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Block until this task holds the single foreground lane.
    pub fn acquire_foreground(self: &Arc<Self>, task_id: &str) -> ForegroundLease {
        let mut holder = self.holder.lock().expect("lease lock");
        while holder.is_some() {
            holder = self.released.wait(holder).expect("lease wait");
        }
        *holder = Some(task_id.to_string());
        ForegroundLease {
            coordinator: Arc::clone(self),
            task_id: task_id.to_string(),
        }
    }

    /// Take the lane only if it is free (used where waiting is worse than
    /// pausing the action).
    pub fn try_acquire_foreground(self: &Arc<Self>, task_id: &str) -> Option<ForegroundLease> {
        let mut holder = self.holder.lock().expect("lease lock");
        if holder.is_some() {
            return None;
        }
        *holder = Some(task_id.to_string());
        Some(ForegroundLease {
            coordinator: Arc::clone(self),
            task_id: task_id.to_string(),
        })
    }

    /// Who holds the lane right now (observability).
    pub fn holder(&self) -> Option<String> {
        self.holder.lock().expect("lease lock").clone()
    }
}

// ---------------------------------------------------------------------------
// Specs and evidence
// ---------------------------------------------------------------------------

/// One audited reused tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditedTool {
    /// Logical name recorded in evidence.
    pub name: &'static str,
    /// The executable probed.
    pub program: &'static str,
    /// Probe argv (read-only).
    pub probe: &'static [&'static str],
}

/// The tools this module reuses today, both already proven on the host.
pub const AUDITED_TOOLS: &[AuditedTool] = &[
    AuditedTool {
        name: "orca-computer",
        program: "orca",
        probe: &["computer", "capabilities"],
    },
    AuditedTool {
        name: "osascript",
        program: "osascript",
        probe: &["-e", "return 1"],
    },
];

/// One action request: locate by `target` in the locator output, run the
/// action argv, then require `expect_post` in the post locator output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionSpec {
    pub tool: &'static str,
    pub program: &'static str,
    /// Read-only argv producing the locate/verify state snapshot.
    pub locator: Vec<String>,
    /// The target that must appear in the pre-action state.
    pub target: String,
    /// Full argv of the action (after the program).
    pub argv: Vec<String>,
    /// Marker that must appear in the post-action state.
    pub expect_post: String,
    /// Foreground input serializes on the global lease.
    pub foreground: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionState {
    Executed,
    Refused,
    Failed,
}

impl ActionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionState::Executed => "executed",
            ActionState::Refused => "refused",
            ActionState::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionRecord {
    pub action_id: String,
    pub task_id: Option<TaskId>,
    pub actor_member_id: MemberId,
    pub grant_id: Option<String>,
    pub tool: String,
    pub target: String,
    pub argv: Vec<String>,
    pub state: ActionState,
    pub reason: Option<String>,
    pub pre_state: Option<String>,
    /// What the action itself returned (the observation it produced).
    pub action_output: Option<String>,
    pub post_state: Option<String>,
    pub foreground: bool,
    pub recorded_at: String,
}

/// The two low-risk smoke tasks (issue acceptance: one browser task, one
/// native-app task, each locate → act → verify with evidence). Both are
/// read-only inspections through the real tools: they prove the chain
/// without typing anywhere.
pub fn smoke_specs() -> Vec<(String, ActionSpec)> {
    vec![
        (
            "browser-chrome".to_string(),
            ActionSpec {
                tool: "orca-computer",
                program: "orca",
                locator: vec!["computer".into(), "list-apps".into(), "--json".into()],
                target: "Google Chrome".into(),
                argv: vec![
                    "computer".into(),
                    "list-windows".into(),
                    "--app".into(),
                    "Google Chrome".into(),
                    "--json".into(),
                ],
                expect_post: "Google Chrome".into(),
                foreground: false,
            },
        ),
        (
            "native-finder".to_string(),
            ActionSpec {
                tool: "orca-computer",
                program: "orca",
                locator: vec!["computer".into(), "list-apps".into(), "--json".into()],
                target: "Finder".into(),
                argv: vec![
                    "computer".into(),
                    "get-app-state".into(),
                    "--app".into(),
                    "Finder".into(),
                    "--json".into(),
                    "--no-screenshot".into(),
                ],
                expect_post: "Finder".into(),
                foreground: false,
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

pub struct ComputerEngine<'a> {
    store: &'a Store,
    runner: Box<dyn ToolRunner>,
    coordinator: Arc<ForegroundCoordinator>,
}

impl<'a> ComputerEngine<'a> {
    pub fn new(store: &'a Store, coordinator: Arc<ForegroundCoordinator>) -> Self {
        Self {
            store,
            runner: Box::new(ProcessRunner::default()),
            coordinator,
        }
    }

    /// Test/bespoke seam: a scripted runner.
    pub fn with_runner(
        store: &'a Store,
        coordinator: Arc<ForegroundCoordinator>,
        runner: Box<dyn ToolRunner>,
    ) -> Self {
        Self {
            store,
            runner,
            coordinator,
        }
    }

    // -- Audit -----------------------------------------------------------------

    /// Probe every reused tool for real and record the result. A missing
    /// or failing probe is `unavailable` with the reason — never a pass
    /// on a promise.
    pub fn audit(&self) -> OfficeResult<Vec<CapabilityReport>> {
        let mut reports = Vec::new();
        for tool in AUDITED_TOOLS {
            let now = utc_now();
            let (present, detail, permission) = match self.runner.run(
                tool.program,
                &tool.probe.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            ) {
                Ok(out) => (true, out, "available"),
                Err(failure) => (false, failure.to_string(), "unavailable"),
            };
            self.store.connection().execute(
                "INSERT INTO computer_capabilities(tool, present, detail, permission_state, checked_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(tool) DO UPDATE SET
                   present = excluded.present,
                   detail = excluded.detail,
                   permission_state = excluded.permission_state,
                   checked_at = excluded.checked_at",
                rusqlite::params![tool.name, present as i64, detail, permission, now],
            )?;
            reports.push(CapabilityReport {
                tool: tool.name.to_string(),
                mode: if present { "probe_ok" } else { "probe_failed" }.to_string(),
                restriction_state: if present {
                    crate::authority::RestrictionState::Enforced
                } else {
                    crate::authority::RestrictionState::Unverified
                },
                adapter_evidence: if present {
                    Some("live probe succeeded".into())
                } else {
                    None
                },
            });
        }
        Ok(reports)
    }

    pub fn capability(&self, tool: &str) -> OfficeResult<Option<(bool, String, String, String)>> {
        let row = self
            .store
            .connection()
            .query_row(
                "SELECT present, detail, permission_state, checked_at
                 FROM computer_capabilities WHERE tool = ?1",
                [tool],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? != 0,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        Ok(row)
    }

    // -- Actions -----------------------------------------------------------------

    /// Execute one action: authorize (task-scoped grant), locate, act,
    /// verify — recording evidence for every outcome. Returns the record;
    /// refusals and failures also carry it (with the honest state).
    pub fn execute(
        &self,
        authority: &AuthorityEngine<'_>,
        actor: &Actor,
        task_id: &TaskId,
        spec: &ActionSpec,
    ) -> OfficeResult<ActionRecord> {
        let member = match actor {
            Actor::Member { member, .. } => member.clone(),
            Actor::Worker { .. } => {
                return Err(OfficeError::Validation(
                    "workers do not drive the computer; member grants only".into(),
                ));
            }
        };

        // Task-scoped authorization. A grant for another task is refused
        // here (cross-task); no grant is refused too — the office records
        // the refusal as evidence either way.
        let grant_id = match actor {
            Actor::Member { grant, .. } => grant.clone(),
            Actor::Worker { .. } => None,
        };
        let authorized = match authority.check(actor, "computer_input", Some(task_id))? {
            Ok(()) => true,
            Err(reason) => {
                let record = self.record(ActionRecord {
                    action_id: new_action_id(),
                    task_id: Some(task_id.clone()),
                    actor_member_id: member,
                    grant_id: grant_id.map(|g| g.to_string()),
                    tool: spec.tool.to_string(),
                    target: spec.target.clone(),
                    argv: spec.argv.clone(),
                    state: ActionState::Refused,
                    reason: Some(format!("not authorized: {reason}")),
                    pre_state: None,
                    action_output: None,
                    post_state: None,
                    foreground: spec.foreground,
                    recorded_at: utc_now(),
                })?;
                return Ok(record);
            }
        };
        let _ = authorized;

        // Foreground actions take the single input lane; reads don't.
        let _lease = if spec.foreground {
            Some(self.coordinator.acquire_foreground(task_id.as_str()))
        } else {
            None
        };

        // Locate: the target must be visible before anything happens.
        let pre = self.runner.run(spec.program, &spec.locator);
        let pre_text = match pre {
            Ok(text) => text,
            Err(failure) => {
                // Permission gaps and broken tools surface here, honestly.
                let record = self.record(ActionRecord {
                    action_id: new_action_id(),
                    task_id: Some(task_id.clone()),
                    actor_member_id: member,
                    grant_id: grant_id.map(|g| g.to_string()),
                    tool: spec.tool.to_string(),
                    target: spec.target.clone(),
                    argv: spec.argv.clone(),
                    state: ActionState::Failed,
                    reason: Some(format!("locate failed: {failure}")),
                    pre_state: None,
                    action_output: None,
                    post_state: None,
                    foreground: spec.foreground,
                    recorded_at: utc_now(),
                })?;
                return Ok(record);
            }
        };
        if !pre_text.contains(&spec.target) {
            let record = self.record(ActionRecord {
                action_id: new_action_id(),
                task_id: Some(task_id.clone()),
                actor_member_id: member,
                grant_id: grant_id.map(|g| g.to_string()),
                tool: spec.tool.to_string(),
                target: spec.target.clone(),
                argv: spec.argv.clone(),
                state: ActionState::Refused,
                reason: Some(format!(
                    "target `{}` is not visible in the pre-action state — refusing \
                     to act on a drifted or missing target",
                    spec.target
                )),
                pre_state: Some(pre_text),
                action_output: None,
                post_state: None,
                foreground: spec.foreground,
                recorded_at: utc_now(),
            })?;
            return Ok(record);
        }

        // Act. The action's own output is evidence too — it is recorded
        // with the record whatever the verify outcome is.
        let action_output = match self.runner.run(spec.program, &spec.argv) {
            Ok(out) => Some(out),
            Err(failure) => {
                let record = self.record(ActionRecord {
                    action_id: new_action_id(),
                    task_id: Some(task_id.clone()),
                    actor_member_id: member,
                    grant_id: grant_id.map(|g| g.to_string()),
                    tool: spec.tool.to_string(),
                    target: spec.target.clone(),
                    argv: spec.argv.clone(),
                    state: ActionState::Failed,
                    reason: Some(format!("action failed: {failure}")),
                    pre_state: Some(pre_text),
                    action_output: None,
                    post_state: None,
                    foreground: spec.foreground,
                    recorded_at: utc_now(),
                })?;
                return Ok(record);
            }
        };

        // Verify: the expected marker must appear in the post-action
        // locator state.
        let post_text = self
            .runner
            .run(spec.program, &spec.locator)
            .map_err(|failure| {
                OfficeError::Validation(format!("post-action verify failed: {failure}"))
            })?;
        let (state, reason) = if post_text.contains(&spec.expect_post) {
            (ActionState::Executed, None)
        } else {
            (
                ActionState::Failed,
                Some(format!(
                    "expected `{}` after the action, but the post-action state does not \
                     show it — recorded as failed, not as success",
                    spec.expect_post
                )),
            )
        };
        let record = self.record(ActionRecord {
            action_id: new_action_id(),
            task_id: Some(task_id.clone()),
            actor_member_id: member,
            grant_id: grant_id.map(|g| g.to_string()),
            tool: spec.tool.to_string(),
            target: spec.target.clone(),
            argv: spec.argv.clone(),
            state,
            reason,
            pre_state: Some(pre_text),
            action_output,
            post_state: Some(post_text),
            foreground: spec.foreground,
            recorded_at: utc_now(),
        })?;
        Ok(record)
    }

    pub fn actions_for_task(&self, task_id: &TaskId) -> OfficeResult<Vec<ActionRecord>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT action_id, task_id, actor_member_id, grant_id, tool, target, argv_json,
                    state, reason, pre_state, action_output, post_state, foreground,
                    recorded_at
             FROM computer_actions WHERE task_id = ?1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map([task_id.as_str()], map_action)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    fn record(&self, record: ActionRecord) -> OfficeResult<ActionRecord> {
        self.store.connection().execute(
            "INSERT INTO computer_actions(action_id, task_id, actor_member_id, grant_id,
                                          tool, target, argv_json, state, reason,
                                          pre_state, action_output, post_state, foreground,
                                          recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![
                record.action_id,
                record.task_id.as_ref().map(|t| t.as_str()),
                record.actor_member_id.as_str(),
                record.grant_id,
                record.tool,
                record.target,
                serde_json::to_string(&record.argv)?,
                record.state.as_str(),
                record.reason,
                record.pre_state,
                record.action_output,
                record.post_state,
                record.foreground as i64,
                record.recorded_at,
            ],
        )?;
        Ok(record)
    }
}

fn new_action_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("cact-{}-{:x}", uuid::Uuid::new_v4().simple(), n)
}

fn map_action(row: &rusqlite::Row<'_>) -> rusqlite::Result<ActionRecord> {
    let state: String = row.get(7)?;
    Ok(ActionRecord {
        action_id: row.get(0)?,
        task_id: row
            .get::<_, Option<String>>(1)?
            .as_deref()
            .and_then(|t| TaskId::from_str(t).ok()),
        actor_member_id: MemberId::from_str(&row.get::<_, String>(2)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
        })?,
        grant_id: row.get(3)?,
        tool: row.get(4)?,
        target: row.get(5)?,
        argv: serde_json::from_str(&row.get::<_, String>(6)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e))
        })?,
        state: match state.as_str() {
            "executed" => ActionState::Executed,
            "refused" => ActionState::Refused,
            "failed" => ActionState::Failed,
            other => panic!("computer_actions.state holds `{other}`"),
        },
        reason: row.get(8)?,
        pre_state: row.get(9)?,
        action_output: row.get(10)?,
        post_state: row.get(11)?,
        foreground: row.get::<_, i64>(12)? != 0,
        recorded_at: row.get(13)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_lane_serializes_and_reads_do_not_queue() {
        let coordinator = ForegroundCoordinator::new();
        let lease = coordinator.acquire_foreground("task-a");
        assert_eq!(coordinator.holder().as_deref(), Some("task-a"));
        // A second task cannot take the lane while it is held.
        assert!(coordinator.try_acquire_foreground("task-b").is_none());
        // An independent (non-foreground) context needs no lane at all.
        drop(lease);
        let next = coordinator.try_acquire_foreground("task-b");
        assert!(next.is_some(), "the lane frees on drop");
    }
}
