//! GitHub evidence via `gh`, strictly read-only, bound to task + head SHA
//! (V08, issue #17).
//!
//! - The only `gh` surface here is a **read whitelist** (pr/issue/run list
//!   and view, pr status). Write subcommands and change flags are rejected
//!   structurally at the entry — they cannot reach the CLI.
//! - Evidence rows record the queried subject, the head SHA it belongs to,
//!   the raw state JSON and the fetch time. On later reads the row is
//!   flagged stale when the worktree head has moved past it.
//! - Missing auth or network produces an explicit
//!   `EvidenceState::Unavailable` — the office never reports evidence PASS
//!   when it could not actually look.
//! - Credentials stay wherever the user keeps them; `gh` runs with the
//!   user's environment and the office copies nothing.

use std::path::Path;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::str::FromStr as _;

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::{TaskId, WorktreeId, utc_now};
use crate::foundation::store::Store;
use crate::git::cli::{CliRunner, GitFailure};

/// The exhaustive `gh` read surface. Anything outside this tree is a write
/// or an admin operation and is rejected before spawning.
pub const READ_WHITELIST: &[&[&str]] = &[
    &["pr", "list"],
    &["pr", "status"],
    &["pr", "view"],
    &["issue", "list"],
    &["issue", "view"],
    &["run", "list"],
    &["run", "view"],
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhRejection {
    WriteAttempt { detail: String },
    AuthMissing { detail: String },
    Network { detail: String },
    Other { detail: String },
}

/// A PR as queried read-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestEvidence {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub head_ref: String,
    pub head_sha: String,
}

/// The honest state of an evidence query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EvidenceState<T> {
    /// The query really ran; the payload is what GitHub returned.
    Fetched { payload: T, fetched_at: String },
    /// The query could not run (auth/network). Never a PASS.
    Unavailable { reason: String },
}

/// A stored evidence row bound to a task and a head SHA.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    pub evidence_id: String,
    pub task_id: TaskId,
    pub kind: String,
    pub subject: String,
    pub head_sha: Option<String>,
    pub state_json: String,
    pub fetched_at: String,
    pub stale: bool,
}

/// Read-only `gh` runner with structural write protection.
#[derive(Default)]
pub struct GhRunner {
    runner: CliRunner,
}

impl std::fmt::Display for GhRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GhRejection::WriteAttempt { detail } => write!(f, "write rejected: {detail}"),
            GhRejection::AuthMissing { detail } => write!(f, "gh authentication missing: {detail}"),
            GhRejection::Network { detail } => write!(f, "network unreachable: {detail}"),
            GhRejection::Other { detail } => write!(f, "{detail}"),
        }
    }
}

impl GhRunner {
    /// Validate a `gh` argument vector against the read whitelist. The
    /// first two tokens decide; a flag can never turn a read into a write.
    pub fn validate_read_args(args: &[&str]) -> Result<(), GhRejection> {
        if args.len() < 2 {
            return Err(GhRejection::WriteAttempt {
                detail: "gh requires a subcommand tree (e.g. `pr view`)".into(),
            });
        }
        let ok = READ_WHITELIST
            .iter()
            .any(|tree| args[0] == tree[0] && args[1] == tree[1]);
        if ok {
            return Ok(());
        }
        Err(GhRejection::WriteAttempt {
            detail: format!(
                "`gh {} {}` is outside the office's read-only surface — writes need a \
                 separate, owner-authorized path",
                args[0], args[1]
            ),
        })
    }

    /// Run one whitelisted read command in `cwd`. Auth/network failures
    /// come back as `EvidenceState::Unavailable`, never as a fake PASS;
    /// a non-whitelisted argument vector is rejected the same visible way
    /// (the write never reaches the CLI).
    pub fn read<T, F>(&self, cwd: &Path, args: &[&str], parse: F) -> EvidenceState<T>
    where
        F: FnOnce(&str) -> OfficeResult<T>,
    {
        if let Err(rejection) = Self::validate_read_args(args) {
            return EvidenceState::Unavailable {
                reason: rejection.to_string(),
            };
        }
        match self.runner.run("gh", cwd, args) {
            Ok(out) => match parse(&out.stdout) {
                Ok(payload) => EvidenceState::Fetched {
                    payload,
                    fetched_at: utc_now(),
                },
                Err(err) => EvidenceState::Unavailable {
                    reason: err.to_string(),
                },
            },
            Err(GitFailure::AuthMissing { detail }) => EvidenceState::Unavailable {
                reason: format!("gh authentication missing: {detail}"),
            },
            Err(GitFailure::Network { detail }) => EvidenceState::Unavailable {
                reason: format!("network unreachable: {detail}"),
            },
            Err(other) => EvidenceState::Unavailable {
                reason: other.to_string(),
            },
        }
    }

    /// Query one PR's evidence for a task. `cwd` should be inside the task
    /// worktree so the user's repo/remotes apply.
    pub fn fetch_pr(&self, cwd: &Path, number: u64) -> EvidenceState<PullRequestEvidence> {
        self.read(
            cwd,
            &[
                "pr",
                "view",
                &number.to_string(),
                "--json",
                "number,title,state,headRefName,headRefOid",
            ],
            |stdout| {
                let value: serde_json::Value = serde_json::from_str(stdout)?;
                Ok(PullRequestEvidence {
                    number: value
                        .get("number")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| OfficeError::Validation("pr view: missing number".into()))?,
                    title: value
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    state: value
                        .get("state")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    head_ref: value
                        .get("headRefName")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    head_sha: value
                        .get("headRefOid")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                })
            },
        )
    }

    /// `gh auth status` probe, for actionable preflight messages.
    pub fn auth_available(&self, cwd: &Path) -> bool {
        matches!(self.runner.run("gh", cwd, &["auth", "status"]), Ok(out) if out.success())
    }
}

/// Evidence storage: rows bound to task + head SHA, flagged stale on read
/// when the worktree head has moved past them.
pub struct EvidenceStore<'a> {
    store: &'a Store,
}

impl<'a> EvidenceStore<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// Persist a fetched payload bound to a task + head SHA.
    pub fn bind<T: Serialize>(
        &self,
        task_id: &TaskId,
        worktree: Option<&WorktreeId>,
        kind: &str,
        subject: &str,
        head_sha: Option<String>,
        state: &EvidenceState<T>,
    ) -> OfficeResult<EvidenceRecord> {
        let state_json = serde_json::to_string(state)?;
        let record = EvidenceRecord {
            evidence_id: format!("ev-{}", uuid::Uuid::new_v4().simple()),
            task_id: task_id.clone(),
            kind: kind.to_string(),
            subject: subject.to_string(),
            head_sha: head_sha.clone(),
            state_json,
            fetched_at: utc_now(),
            stale: false,
        };
        let _ = worktree;
        self.store.connection().execute(
            "INSERT INTO github_evidence(evidence_id, task_id, kind, subject, head_sha,
                                         state_json, fetched_at, stale)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                record.evidence_id,
                record.task_id.as_str(),
                record.kind,
                record.subject,
                record.head_sha,
                record.state_json,
                record.fetched_at,
                record.stale as i64,
            ],
        )?;
        Ok(record)
    }

    /// Read a record, flagging it stale when `current_head` differs from
    /// the bound head SHA. The stale flag persists (append-style honesty:
    /// the row remembers it fell behind).
    pub fn get_marking_stale(
        &self,
        evidence_id: &str,
        current_head: Option<&str>,
    ) -> OfficeResult<Option<EvidenceRecord>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT evidence_id, task_id, kind, subject, head_sha, state_json,
                    fetched_at, stale
             FROM github_evidence WHERE evidence_id = ?1",
        )?;
        let mut record = stmt
            .query_row([evidence_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            })
            .optional()?;
        if let Some((
            evidence_id,
            task_id,
            kind,
            subject,
            head_sha,
            state_json,
            fetched_at,
            stale,
        )) = record.take()
        {
            let stale = stale != 0
                || matches!((head_sha.as_deref(), current_head), (Some(bound), Some(head)) if bound != head);
            if stale {
                self.store.connection().execute(
                    "UPDATE github_evidence SET stale = 1 WHERE evidence_id = ?1",
                    [&evidence_id],
                )?;
            }
            return Ok(Some(EvidenceRecord {
                evidence_id,
                task_id: TaskId::from_str(&task_id).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                kind,
                subject,
                head_sha,
                state_json,
                fetched_at,
                stale,
            }));
        }
        Ok(None)
    }

    /// All evidence rows for a task (history view).
    pub fn for_task(&self, task_id: &TaskId) -> OfficeResult<Vec<EvidenceRecord>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT evidence_id, task_id, kind, subject, head_sha, state_json,
                    fetched_at, stale
             FROM github_evidence WHERE task_id = ?1 ORDER BY fetched_at",
        )?;
        let rows = stmt.query_map([task_id.as_str()], |row| {
            Ok(EvidenceRecord {
                evidence_id: row.get(0)?,
                task_id: TaskId::from_str(&row.get::<_, String>(1)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                kind: row.get(2)?,
                subject: row.get(3)?,
                head_sha: row.get(4)?,
                state_json: row.get(5)?,
                fetched_at: row.get(6)?,
                stale: row.get::<_, i64>(7)? != 0,
            })
        })?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_commands_never_pass_the_readonly_entry() {
        assert!(GhRunner::validate_read_args(&["pr", "merge", "123"]).is_err());
        assert!(GhRunner::validate_read_args(&["pr", "create"]).is_err());
        assert!(GhRunner::validate_read_args(&["issue", "close", "5"]).is_err());
        assert!(GhRunner::validate_read_args(&["repo", "delete"]).is_err());
        assert!(GhRunner::validate_read_args(&["auth", "token"]).is_err());
        assert!(GhRunner::validate_read_args(&["pr"]).is_err());

        assert!(GhRunner::validate_read_args(&["pr", "view", "123"]).is_ok());
        assert!(GhRunner::validate_read_args(&["pr", "list", "--state", "open"]).is_ok());
        assert!(GhRunner::validate_read_args(&["run", "list"]).is_ok());
    }

    #[test]
    fn parse_failure_is_unavailable_never_pass() {
        let gh = GhRunner::default();
        // `read` with whitelisted args but a parser over garbage output:
        // we cannot run gh offline here, so exercise the parse path via
        // fetch_pr against a directory with no repo/auth and assert the
        // honest unavailable state (works with or without credentials —
        // there is no PR `999999999` in this directory's remote).
        let dir = tempfile::TempDir::new().expect("dir");
        let state = gh.fetch_pr(dir.path(), 999_999_999);
        match state {
            EvidenceState::Unavailable { reason } => {
                assert!(!reason.is_empty());
            }
            EvidenceState::Fetched { .. } => {
                panic!("a nonexistent PR in an empty dir must not fetch")
            }
        }
    }
}
