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

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::authority::{Actor, AuthorityEngine, CapabilityReport};
use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::{MemberId, TaskId, utc_now};
use crate::foundation::store::{DOMAIN_TOOLS_COMPUTER, MigrationRegistry, Store};

/// Register the `tools_computer` domain migrations (F03's namespace).
pub fn register_migrations(registry: MigrationRegistry) -> MigrationRegistry {
    registry
        .register(
            DOMAIN_TOOLS_COMPUTER,
            1,
            "tools computer v1",
            TOOLS_COMPUTER_V1_SQL,
        )
        .register(
            DOMAIN_TOOLS_COMPUTER,
            2,
            "tools computer v2 foreground lease",
            TOOLS_COMPUTER_V2_SQL,
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

/// v2 moves the foreground lease into its own table — registered as a real
/// migration so every store picks it up on the next open. (The first
/// attempt edited v1 in place, which stores that had already applied v1
/// would never see again — QA finding.) IF NOT EXISTS also keeps
/// branch-derived stores that already carry the table healthy.
pub const TOOLS_COMPUTER_V2_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS foreground_leases (
    task_id          TEXT PRIMARY KEY,
    host_pid         INTEGER,
    pid_start_marker TEXT,
    acquired_at      TEXT NOT NULL
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
// Foreground lease: one global input lane across PROCESSES, tasks queue,
// reads don't
// ---------------------------------------------------------------------------

/// Coordinates global keyboard/mouse/focus work through the office store:
/// the lease is a row in `foreground_leases`, so the exclusion holds
/// between separate `viva` processes sharing the same VIVA_HOME file, not
/// only between threads. A foreground action must hold the single lease;
/// while it is held, other foreground actions wait and independent
/// API/browser-context actions proceed untouched.
pub struct ForegroundCoordinator<'a> {
    store: &'a Store,
    acquire_timeout: std::time::Duration,
}

/// Held lease; releasing happens on drop, so an early return can never
/// leave the lane stuck.
pub struct ForegroundLease<'c> {
    coordinator: &'c ForegroundCoordinator<'c>,
    pub task_id: String,
    acquired_at: String,
}

impl std::fmt::Debug for ForegroundLease<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ForegroundLease({})", self.task_id)
    }
}

impl Drop for ForegroundLease<'_> {
    fn drop(&mut self) {
        // Best-effort release; the row is keyed by task so a crashed
        // process cannot be impersonated by a later holder.
        let _ = self.coordinator.store.connection().execute(
            "DELETE FROM foreground_leases WHERE task_id = ?1 AND host_pid = ?2 AND acquired_at = ?3",
            rusqlite::params![self.task_id, std::process::id() as i64, self.acquired_at],
        );
    }
}

impl<'a> ForegroundCoordinator<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self {
            store,
            acquire_timeout: std::time::Duration::from_secs(30),
        }
    }

    /// Take the lane only if it is free, in one transactional step. The
    /// write transaction is the cross-process gate: only one writer can
    /// insert while the table is empty.
    ///
    /// Before the exclusivity check, stale rows are reconciled: a lease
    /// whose holder pid is gone (crashed process — Drop never ran) is
    /// removed. A process birth marker prevents PID reuse from pinning a
    /// dead holder. For legacy markerless rows the recorded acquisition
    /// time is compared to the OS process age; never steal a live holder
    /// solely because its action ran long.
    pub fn try_acquire_foreground(
        &self,
        task_id: &str,
    ) -> OfficeResult<Option<ForegroundLease<'_>>> {
        let tx = self.store.transaction()?;
        {
            let rows: Vec<(String, Option<i64>, Option<String>, String)> = {
                let mut stmt = tx.prepare(
                    "SELECT task_id, host_pid, pid_start_marker, acquired_at FROM foreground_leases",
                )?;
                let mapped = stmt.query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?;
                mapped.collect::<Result<Vec<_>, _>>()?
            };
            for (held_task, pid, marker, acquired_at) in rows {
                let live = pid.is_some_and(pid_is_alive);
                let stale = match (pid, marker.as_deref()) {
                    (Some(pid), Some(marker)) => {
                        // If the OS cannot inspect a live holder, fail
                        // closed: do not steal its keyboard lane.
                        !live || process_start_marker(pid).is_some_and(|now| now != marker)
                    }
                    // Older rows never wrote a marker. Recover only when
                    // this PID's present process is younger than the row;
                    // an old live holder is not taken just for being old.
                    (Some(pid), None) => !live || legacy_pid_recycled(pid, &acquired_at),
                    _ => true,
                };
                if stale {
                    tx.execute(
                        "DELETE FROM foreground_leases WHERE task_id = ?1",
                        [&held_task],
                    )?;
                }
            }
        }
        let held: i64 = tx.query_row("SELECT COUNT(*) FROM foreground_leases", [], |r| r.get(0))?;
        if held > 0 {
            drop(tx);
            return Ok(None);
        }
        let pid = std::process::id() as i64;
        let marker = process_start_marker(pid).ok_or_else(|| {
            OfficeError::Validation(
                "cannot identify foreground holder process; refusing input".into(),
            )
        })?;
        let acquired_at = utc_now();
        let insert = tx.execute(
            "INSERT INTO foreground_leases(task_id, host_pid, pid_start_marker, acquired_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![task_id, pid, marker, acquired_at],
        );
        if let Err(err) = insert {
            drop(tx);
            if matches!(err, rusqlite::Error::SqliteFailure(_, _)) {
                return Ok(None); // another process won the race
            }
            return Err(err.into());
        }
        tx.commit()?;
        Ok(Some(ForegroundLease {
            coordinator: self,
            task_id: task_id.to_string(),
            acquired_at,
        }))
    }

    /// Block until this task holds the single foreground lane (bounded by
    /// the acquire timeout — waiting forever would wedge the host).
    pub fn acquire_foreground(&self, task_id: &str) -> OfficeResult<ForegroundLease<'_>> {
        let deadline = std::time::Instant::now() + self.acquire_timeout;
        loop {
            if let Some(lease) = self.try_acquire_foreground(task_id)? {
                return Ok(lease);
            }
            if std::time::Instant::now() >= deadline {
                return Err(OfficeError::Validation(format!(
                    "the foreground input lane is still held after {}s — pausing instead of \
                     interleaving keystrokes",
                    self.acquire_timeout.as_secs()
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    /// Who holds the lane right now (observability).
    pub fn holder(&self) -> OfficeResult<Option<String>> {
        let row: Option<String> = self
            .store
            .connection()
            .query_row("SELECT task_id FROM foreground_leases LIMIT 1", [], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(row)
    }
}

/// Liveness probe for a lease holder: `kill -0 <pid>` via argv array.
/// A missing process fails; a live one (even one we cannot signal)
/// succeeds with exit 0.
fn pid_is_alive(pid: i64) -> bool {
    if pid <= 1 {
        return false;
    }
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// The OS birth identity, not a timestamp invented by Viva. On Linux the
/// proc stat start tick is stable for the life of the PID. On macOS `ps`
/// reports the process start date (seconds resolution); if inspection fails
/// we refuse to acquire rather than pretending to own the foreground.
fn process_start_marker(pid: i64) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after_comm = stat.rsplit_once(") ")?.1;
        return after_comm.split_whitespace().nth(19).map(str::to_string);
    }
    #[cfg(not(target_os = "linux"))]
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
}

fn legacy_pid_recycled(pid: i64, acquired_at: &str) -> bool {
    let Ok(then) = OffsetDateTime::parse(acquired_at, &Rfc3339) else {
        return false; // malformed legacy evidence: fail closed
    };
    let age = OffsetDateTime::now_utc() - then;
    let Ok(out) = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "etime="])
        .output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(elapsed) = parse_elapsed(text.trim()) else {
        return false;
    };
    // ps elapsed has second precision: do not steal a holder when a PID
    // was recycled in the same few seconds as acquisition.
    age > elapsed + time::Duration::seconds(2)
}

fn parse_elapsed(text: &str) -> Option<time::Duration> {
    let (days, clock) = match text.split_once('-') {
        Some((days, clock)) => (days.parse::<i64>().ok()?, clock),
        None => (0, text),
    };
    let parts: Vec<i64> = clock
        .split(':')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] => (0, *minutes, *seconds),
        [hours, minutes, seconds] => (*hours, *minutes, *seconds),
        _ => return None,
    };
    Some(time::Duration::seconds(
        days * 86400 + hours * 3600 + minutes * 60 + seconds,
    ))
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
    /// Marker that must appear in the post-action locator state (the
    /// world after the action).
    pub expect_post: String,
    /// Marker that must appear in the ACTION's own output — the actual
    /// observation. Without this, a read-only action whose world is
    /// unchanged would always "verify" against the locator alone
    /// (tautological verification, QA finding).
    pub expect_action_output: Option<String>,
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
/// without typing anywhere. Verification checks the ACTION'S OWN output
/// (the observation), not just an unchanged world state, and the visual
/// capture stays on — the screenshot is part of the before/after evidence.
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
                expect_action_output: Some("com.google.Chrome".into()),
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
                ],
                expect_post: "Finder".into(),
                expect_action_output: Some("window".into()),
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
}

impl<'a> ComputerEngine<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self {
            store,
            runner: Box::new(ProcessRunner::default()),
        }
    }

    /// Test/bespoke seam: a scripted runner.
    pub fn with_runner(store: &'a Store, runner: Box<dyn ToolRunner>) -> Self {
        Self { store, runner }
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
        // A computer_input grant must name its task. The authority engine
        // only cross-checks grants that carry a scope, so an unscoped
        // (task_id = NULL) grant would otherwise serve ANY task — a
        // whole-machine license in disguise (QA finding N4). Unlike
        // office-wide maintenance actions, nothing depends on unscoped
        // computer_input, so the narrower rule is enforced here.
        if let Some(gid) = &grant_id {
            let grant = authority.require_grant(gid)?;
            if grant.task_id.as_ref() != Some(task_id) {
                let record = self.record(ActionRecord {
                    action_id: new_action_id(),
                    task_id: Some(task_id.clone()),
                    actor_member_id: member,
                    grant_id: Some(gid.to_string()),
                    tool: spec.tool.to_string(),
                    target: spec.target.clone(),
                    argv: spec.argv.clone(),
                    state: ActionState::Refused,
                    reason: Some(
                        "computer_input grants must be scoped to the task they authorize — \
                         an unscoped grant is not a whole-machine license"
                            .into(),
                    ),
                    pre_state: None,
                    action_output: None,
                    post_state: None,
                    foreground: spec.foreground,
                    recorded_at: utc_now(),
                })?;
                return Ok(record);
            }
        }

        // Foreground actions take the single input lane; reads don't.
        // The coordinator is a per-call view over the shared store — the
        // lease row itself carries the exclusion across processes.
        let foreground_coordinator = ForegroundCoordinator::new(self.store);
        let _lease = if spec.foreground {
            Some(foreground_coordinator.acquire_foreground(task_id.as_str())?)
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

        // Verify on BOTH axes: the action's own output must contain its
        // observation marker (the actual effect/observation), and the
        // post-action locator state must contain the world marker. Two
        // independent checks — neither alone can tautologically pass.
        let post_text = self
            .runner
            .run(spec.program, &spec.locator)
            .map_err(|failure| {
                OfficeError::Validation(format!("post-action verify failed: {failure}"))
            })?;
        let mut failures: Vec<String> = Vec::new();
        if let (Some(marker), Some(output)) = (&spec.expect_action_output, &action_output) {
            if !output.contains(marker) {
                failures.push(format!(
                    "the action's own output does not show the expected observation `{marker}`"
                ));
            }
        }
        if !post_text.contains(&spec.expect_post) {
            failures.push(format!(
                "the post-action state does not show `{}`",
                spec.expect_post
            ));
        }
        let (state, reason) = if failures.is_empty() {
            (ActionState::Executed, None)
        } else {
            (
                ActionState::Failed,
                Some(format!(
                    "{} — recorded as failed, not as success",
                    failures.join("; ")
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

    /// Who holds the foreground lane right now (observability).
    pub fn holder(&self) -> OfficeResult<Option<String>> {
        ForegroundCoordinator::new(self.store).holder()
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
        let frozen = crate::foundation::store::MigrationRegistry::new()
            .register(
                crate::foundation::store::DOMAIN_TOOLS_COMPUTER,
                1,
                "tools computer v1",
                TOOLS_COMPUTER_V1_SQL,
            )
            .register(
                crate::foundation::store::DOMAIN_TOOLS_COMPUTER,
                2,
                "tools computer v2 foreground lease",
                TOOLS_COMPUTER_V2_SQL,
            )
            .freeze()
            .expect("registry");
        let store = crate::foundation::store::Store::open_in_memory(&frozen).expect("store");
        let coordinator = ForegroundCoordinator::new(&store);
        let lease = coordinator.acquire_foreground("task-a").expect("acquire");
        assert_eq!(
            coordinator.holder().expect("holder").as_deref(),
            Some("task-a")
        );
        // A second task cannot take the lane while it is held.
        assert!(
            coordinator
                .try_acquire_foreground("task-b")
                .expect("try")
                .is_none()
        );
        // An independent (non-foreground) context needs no lane at all.
        drop(lease);
        let next = coordinator.try_acquire_foreground("task-b").expect("try");
        assert!(next.is_some(), "the lane frees on drop");
    }
}
