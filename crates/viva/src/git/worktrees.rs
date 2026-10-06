//! Task worktree isolation and Git-native worktree discovery
//! (V08, issue #17 + 2026-09-28 supplement).
//!
//! Rules encoded here:
//! - Task worktrees branch from the **latest remote default ref** (`git
//!   fetch` first, then `origin/<default>`), never from a stale local main.
//! - Protected refs are never checked out, created, or written; the
//!   protected set is explicit configuration.
//! - One writable checkout per branch, office-wide: a branch already
//!   checked out in any worktree (including a user's own) is refused.
//!   Creation within the office is mutually exclusive (DB UNIQUE +
//!   in-process lock).
//! - Deletion/prune/cleanup never happens implicitly: no function in this
//!   module removes a worktree — release only marks the registry row.
//! - Existing worktrees (including ones a user created inside Orca) can be
//!   **discovered and explicitly adopted**; discovery shows source, branch,
//!   dirty state and occupancy but never takes over processes, deletes, or
//!   touches Orca's private metadata.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::Mutex;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::{TaskId, WorktreeId, utc_now};
use crate::foundation::store::{DOMAIN_GIT, Store};
use crate::git::cli::{CliRunner, GitFailure, TreeState, current_branch, head_sha, tree_state};

/// Register the `git` domain migrations (V08's namespace).
pub fn register_migrations(
    registry: crate::foundation::store::MigrationRegistry,
) -> crate::foundation::store::MigrationRegistry {
    registry.register(DOMAIN_GIT, 1, "git v1", GIT_V1_SQL)
}

pub const GIT_V1_SQL: &str = r#"
CREATE TABLE task_worktrees (
    worktree_id   TEXT PRIMARY KEY,
    task_id       TEXT NOT NULL,
    repo_root     TEXT NOT NULL,
    worktree_path TEXT NOT NULL,
    branch        TEXT NOT NULL,
    base_sha      TEXT NOT NULL,
    source        TEXT NOT NULL CHECK (source IN ('created', 'adopted')),
    created_at    TEXT NOT NULL,
    released_at   TEXT,
    UNIQUE(repo_root, branch)
);

-- GitHub evidence rows: each bound to a task and the head SHA it was
-- true for; stale flags are set when the head moves on.
CREATE TABLE github_evidence (
    evidence_id TEXT PRIMARY KEY,
    task_id     TEXT NOT NULL,
    worktree_id TEXT,
    kind        TEXT NOT NULL CHECK (kind IN ('pr', 'issue', 'checks')),
    subject     TEXT NOT NULL,
    head_sha    TEXT,
    state_json  TEXT NOT NULL,
    fetched_at  TEXT NOT NULL,
    stale       INTEGER NOT NULL DEFAULT 0 CHECK (stale IN (0, 1))
);
"#;

/// Refs the office must never check out or write. Names are branch names
/// (`refs/heads/` stripped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedRefs {
    pub branches: Vec<String>,
}

impl ProtectedRefs {
    /// The office default: the repository default branch names (`main`,
    /// `master`) are always protected. Callers may extend this set, never
    /// shrink it.
    pub fn new(branches: Vec<String>) -> Self {
        let mut me = Self { branches };
        for required in ["main", "master"] {
            if !me.branches.iter().any(|b| b == required) {
                me.branches.push(required.to_string());
            }
        }
        me
    }

    /// Add caller-specific protected branches to the set.
    pub fn with_extra(mut self, extra: Vec<String>) -> Self {
        for branch in extra {
            if !self.branches.contains(&branch) {
                self.branches.push(branch);
            }
        }
        self
    }

    pub fn is_protected(&self, branch: &str) -> bool {
        let name = branch.strip_prefix("refs/heads/").unwrap_or(branch);
        self.branches.iter().any(|b| b == name)
    }
}

// ---------------------------------------------------------------------------
// Registry rows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskWorktreeRecord {
    pub worktree_id: WorktreeId,
    pub task_id: TaskId,
    pub repo_root: PathBuf,
    pub worktree_path: PathBuf,
    pub branch: String,
    pub base_sha: String,
    /// `created` (office allocated) or `adopted` (pre-existing, explicitly
    /// selected — e.g. a worktree the user once created inside Orca).
    pub source: WorktreeSource,
    pub created_at: String,
    pub released_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeSource {
    Created,
    Adopted,
}

impl WorktreeSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            WorktreeSource::Created => "created",
            WorktreeSource::Adopted => "adopted",
        }
    }
}

/// One discovered worktree as git reports it, with office annotations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredWorktree {
    pub path: PathBuf,
    pub head_sha: Option<String>,
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    pub locked: Option<String>,
    pub prunable: Option<String>,
    /// Working-tree state (dirty/conflicts are facts to show, never to fix).
    pub tree: TreeState,
    /// Set when a task in this office holds this worktree.
    pub occupied_by_task: Option<TaskId>,
    /// How the office claims it (created/adopted), when it does.
    pub office_source: Option<WorktreeSource>,
}

pub struct WorktreeService<'a> {
    store: &'a Store,
    runner: CliRunner,
    protected: ProtectedRefs,
    /// Per-repo in-process creation lock (cross-process safety comes from
    /// the UNIQUE(repo_root, branch) constraint).
    creation_locks: Mutex<HashSet<PathBuf>>,
}

impl<'a> WorktreeService<'a> {
    pub fn new(store: &'a Store, protected: ProtectedRefs) -> Self {
        Self {
            store,
            runner: CliRunner::default(),
            protected,
            creation_locks: Mutex::new(HashSet::new()),
        }
    }

    // -- Discovery -----------------------------------------------------------

    /// Git-native discovery of every worktree of `repo_root`, annotated
    /// with dirty state and office occupancy. Read-only.
    pub fn discover(&self, repo_root: &Path) -> OfficeResult<Vec<DiscoveredWorktree>> {
        let out = self
            .runner
            .git_ok(repo_root, &["worktree", "list", "--porcelain"])?;
        let mut found = Vec::new();
        let mut current: Option<DiscoveredWorktree> = None;
        for line in out.stdout.lines() {
            if let Some(path) = line.strip_prefix("worktree ") {
                if let Some(prev) = current.take() {
                    found.push(prev);
                }
                current = Some(DiscoveredWorktree {
                    path: PathBuf::from(path),
                    head_sha: None,
                    branch: None,
                    bare: false,
                    detached: false,
                    locked: None,
                    prunable: None,
                    tree: TreeState::Clean,
                    occupied_by_task: None,
                    office_source: None,
                });
            } else if let Some(sha) = line.strip_prefix("HEAD ") {
                if let Some(entry) = current.as_mut() {
                    entry.head_sha = Some(sha.to_string());
                }
            } else if let Some(branch) = line.strip_prefix("branch ") {
                if let Some(entry) = current.as_mut() {
                    entry.branch = Some(
                        branch
                            .strip_prefix("refs/heads/")
                            .unwrap_or(branch)
                            .to_string(),
                    );
                }
            } else if line == "bare" {
                if let Some(entry) = current.as_mut() {
                    entry.bare = true;
                }
            } else if line == "detached" {
                if let Some(entry) = current.as_mut() {
                    entry.detached = true;
                }
            } else if let Some(locked) = line.strip_prefix("locked") {
                if let Some(entry) = current.as_mut() {
                    entry.locked = Some(locked.trim_start().to_string());
                }
            } else if let Some(prunable) = line.strip_prefix("prunable")
                && let Some(entry) = current.as_mut()
            {
                entry.prunable = Some(prunable.trim_start().to_string());
            }
        }
        if let Some(prev) = current.take() {
            found.push(prev);
        }

        // Annotate: tree state (missing for prunable/deleted dirs is fine).
        for entry in &mut found {
            if !entry.bare {
                entry.tree = tree_state(&self.runner, &entry.path);
            }
            if let Some(branch) = entry.branch.clone() {
                let claim = self.office_claim(repo_root, &branch)?;
                entry.occupied_by_task = claim.as_ref().map(|(task, _)| task.clone());
                entry.office_source = claim.map(|(_, source)| source);
            }
        }
        Ok(found)
    }

    /// Explicitly adopt an existing worktree for a task. Adoption records
    /// the choice; it never starts processes, deletes anything, or edits
    /// foreign metadata. Refuses protected branches.
    pub fn adopt_existing(
        &mut self,
        repo_root: &Path,
        worktree_path: &Path,
        task_id: &TaskId,
    ) -> OfficeResult<TaskWorktreeRecord> {
        if !worktree_path.exists() {
            return Err(OfficeError::Validation(format!(
                "cannot adopt a missing worktree: {}",
                worktree_path.display()
            )));
        }
        let branch = current_branch(&self.runner, worktree_path)?.ok_or_else(|| {
            OfficeError::Validation(
                "cannot adopt a detached-HEAD worktree for a task; give it a branch first".into(),
            )
        })?;
        if self.protected.is_protected(&branch) {
            return Err(OfficeError::Validation(format!(
                "`{branch}` is a protected ref; tasks never adopt a writable protected checkout"
            )));
        }
        // The branch must not have ANOTHER writable checkout besides the
        // worktree being adopted itself. Paths are compared canonically
        // (git reports resolved paths; the caller may pass symlinked ones).
        let target =
            std::fs::canonicalize(worktree_path).unwrap_or_else(|_| worktree_path.to_path_buf());
        if self.discover(repo_root)?.iter().any(|w| {
            w.branch.as_deref() == Some(branch.as_str())
                && std::fs::canonicalize(&w.path).unwrap_or_else(|_| w.path.clone()) != target
        }) {
            return Err(OfficeError::Validation(format!(
                "branch `{branch}` is already checked out elsewhere — one writable checkout per branch"
            )));
        }
        let record = TaskWorktreeRecord {
            worktree_id: WorktreeId::new(),
            task_id: task_id.clone(),
            repo_root: repo_root.to_path_buf(),
            worktree_path: worktree_path.to_path_buf(),
            branch: branch.clone(),
            base_sha: head_sha(&self.runner, worktree_path)?,
            source: WorktreeSource::Adopted,
            created_at: utc_now(),
            released_at: None,
        };
        self.insert_record(&record)?;
        Ok(record)
    }

    // -- Creation --------------------------------------------------------------

    /// Create an isolated worktree for a task: fetch the latest remote
    /// state, then branch from `origin/<default>`. Refuses protected branch
    /// names and shared writable checkouts. The default branch is the
    /// remote's, never a possibly stale local main.
    pub fn create_task_worktree(
        &mut self,
        repo_root: &Path,
        base_dir: &Path,
        task_id: &TaskId,
        task_branch: &str,
    ) -> OfficeResult<TaskWorktreeRecord> {
        if task_branch.trim().is_empty() || task_branch.contains(char::is_whitespace) {
            return Err(OfficeError::Validation(format!(
                "`{task_branch}` is not a valid branch name"
            )));
        }
        if self.protected.is_protected(task_branch) {
            return Err(OfficeError::Validation(format!(
                "`{task_branch}` is a protected ref; task worktrees must use a task branch"
            )));
        }
        // Mutual exclusion: one creation per repo at a time (in-process),
        // one branch per repo across processes (DB UNIQUE below).
        let _guard = self.lock_repo(repo_root);
        if self.branch_checked_out_anywhere(repo_root, task_branch)? {
            return Err(OfficeError::Validation(format!(
                "branch `{task_branch}` already has a writable checkout — one checkout per branch"
            )));
        }

        // Safe remote update: fetch only. Never pull/merge in any checkout.
        self.runner
            .git(repo_root, &["fetch", "--prune", "origin"])
            .map_err(|failure| match failure {
                GitFailure::AuthMissing { detail } => OfficeError::Validation(format!(
                    "git authentication missing while fetching (configure credentials for the user; the office stores none): {detail}"
                )),
                GitFailure::Network { detail } => OfficeError::Validation(format!(
                    "network unreachable while fetching; no task worktree was created: {detail}"
                )),
                other => OfficeError::Validation(other.to_string()),
            })?;

        let default_branch = self.remote_default_branch(repo_root)?;
        let base = format!("origin/{default_branch}");
        let base_sha = self
            .runner
            .git_ok(repo_root, &["rev-parse", &base])?
            .stdout
            .trim()
            .to_string();

        let worktree_path = base_dir.join(task_branch);
        self.runner.git_ok(
            repo_root,
            &[
                "worktree",
                "add",
                "-b",
                task_branch,
                worktree_path.to_string_lossy().as_ref(),
                &base,
            ],
        )?;

        let record = TaskWorktreeRecord {
            worktree_id: WorktreeId::new(),
            task_id: task_id.clone(),
            repo_root: repo_root.to_path_buf(),
            worktree_path,
            branch: task_branch.to_string(),
            base_sha,
            source: WorktreeSource::Created,
            created_at: utc_now(),
            released_at: None,
        };
        self.insert_record(&record)?;
        Ok(record)
    }

    /// The remote's default branch: `refs/remotes/origin/HEAD` when set,
    /// else the first existing of main/master on the remote.
    pub fn remote_default_branch(&self, repo_root: &Path) -> OfficeResult<String> {
        if let Ok(out) = self.runner.git(
            repo_root,
            &["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"],
        ) {
            let name = out.stdout.trim();
            if let Some(branch) = name.strip_prefix("origin/") {
                return Ok(branch.to_string());
            }
        }
        for candidate in ["main", "master"] {
            let reference = format!("origin/{candidate}");
            if self
                .runner
                .git(repo_root, &["rev-parse", "--verify", "--quiet", &reference])
                .is_ok()
            {
                return Ok(candidate.to_string());
            }
        }
        Err(OfficeError::Validation(
            "cannot determine the remote default branch; set origin/HEAD or fetch origin".into(),
        ))
    }

    /// Release a task worktree from the registry. **No deletion happens** —
    /// releasing only records that the office no longer claims it; removing
    /// the directory requires a human request naming the exact target.
    pub fn release(&self, worktree_id: &WorktreeId) -> OfficeResult<()> {
        let n = self.store.connection().execute(
            "UPDATE task_worktrees SET released_at = ?2
             WHERE worktree_id = ?1 AND released_at IS NULL",
            rusqlite::params![worktree_id.as_str(), utc_now()],
        )?;
        if n == 0 {
            return Err(OfficeError::NotFound {
                entity: "task worktree",
                id: worktree_id.to_string(),
            });
        }
        Ok(())
    }

    pub fn record(&self, worktree_id: &WorktreeId) -> OfficeResult<Option<TaskWorktreeRecord>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT worktree_id, task_id, repo_root, worktree_path, branch, base_sha,
                    source, created_at, released_at
             FROM task_worktrees WHERE worktree_id = ?1",
        )?;
        let row = stmt
            .query_row([worktree_id.as_str()], map_record)
            .optional()?;
        Ok(row)
    }

    /// All registry records, including released (history preserved).
    pub fn all_records(&self) -> OfficeResult<Vec<TaskWorktreeRecord>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT worktree_id, task_id, repo_root, worktree_path, branch, base_sha,
                    source, created_at, released_at
             FROM task_worktrees ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], map_record)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    // -- Internals --------------------------------------------------------------

    fn lock_repo(&self, repo_root: &Path) -> RepoLock<'_> {
        loop {
            {
                let mut set = self.creation_locks.lock().expect("creation locks");
                if set.insert(repo_root.to_path_buf()) {
                    return RepoLock {
                        locks: &self.creation_locks,
                        path: repo_root.to_path_buf(),
                    };
                }
            }
            // Another creation in this office holds the repo; wait for it.
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Whether any worktree of `repo_root` (git-discovered) has `branch`
    /// checked out — including ones the office never created.
    fn branch_checked_out_anywhere(&self, repo_root: &Path, branch: &str) -> OfficeResult<bool> {
        Ok(self
            .discover(repo_root)?
            .iter()
            .any(|w| w.branch.as_deref() == Some(branch)))
    }

    /// The office's live claim on a branch: which task holds it and via
    /// which source (created vs adopted).
    fn office_claim(
        &self,
        repo_root: &Path,
        branch: &str,
    ) -> OfficeResult<Option<(TaskId, WorktreeSource)>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT task_id, source FROM task_worktrees
             WHERE repo_root = ?1 AND branch = ?2 AND released_at IS NULL LIMIT 1",
        )?;
        let row: Option<(String, String)> = stmt
            .query_row(
                rusqlite::params![repo_root.to_string_lossy().as_ref(), branch],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(task, source)| {
            (
                TaskId::from_str(&task).expect("registry task ids are well-formed"),
                match source.as_str() {
                    "created" => WorktreeSource::Created,
                    "adopted" => WorktreeSource::Adopted,
                    other => panic!("task_worktrees.source holds an unknown value `{other}`"),
                },
            )
        }))
    }

    /// Bounded working-tree diff against HEAD for a worktree (V14 need:
    /// real diff, bounded, aligned with head). Read-only.
    pub fn worktree_diff(&self, worktree_path: &Path, max_bytes: usize) -> OfficeResult<String> {
        let out = self
            .runner
            .git_ok(worktree_path, &["--no-pager", "diff", "HEAD", "--"])?;
        Ok(crate::git::cli::bounded_output(
            out.stdout.as_bytes(),
            max_bytes,
        ))
    }

    fn insert_record(&self, record: &TaskWorktreeRecord) -> OfficeResult<()> {
        let inserted = self.store.connection().execute(
            "INSERT INTO task_worktrees(worktree_id, task_id, repo_root, worktree_path,
                                         branch, base_sha, source, created_at, released_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                record.worktree_id.as_str(),
                record.task_id.as_str(),
                record.repo_root.to_string_lossy().as_ref(),
                record.worktree_path.to_string_lossy().as_ref(),
                record.branch,
                record.base_sha,
                record.source.as_str(),
                record.created_at,
                record.released_at,
            ],
        );
        if let Err(rusqlite::Error::SqliteFailure(err, message)) = inserted {
            if err.code == rusqlite::ErrorCode::ConstraintViolation {
                return Err(OfficeError::Validation(format!(
                    "another task already claims this repo/branch ({})",
                    message.unwrap_or_default()
                )));
            }
            return Err(rusqlite::Error::SqliteFailure(err, message).into());
        }
        Ok(())
    }
}

/// Held for the duration of one repo's worktree creation; removes the key
/// on drop so a failed creation never wedges the next one.
struct RepoLock<'a> {
    locks: &'a Mutex<HashSet<PathBuf>>,
    path: PathBuf,
}

impl Drop for RepoLock<'_> {
    fn drop(&mut self) {
        self.locks
            .lock()
            .expect("creation locks")
            .remove(&self.path);
    }
}

fn map_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskWorktreeRecord> {
    let source: String = row.get(6)?;
    Ok(TaskWorktreeRecord {
        worktree_id: WorktreeId::from_str(&row.get::<_, String>(0)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        task_id: TaskId::from_str(&row.get::<_, String>(1)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?,
        repo_root: PathBuf::from(row.get::<_, String>(2)?),
        worktree_path: PathBuf::from(row.get::<_, String>(3)?),
        branch: row.get(4)?,
        base_sha: row.get(5)?,
        source: match source.as_str() {
            "created" => WorktreeSource::Created,
            "adopted" => WorktreeSource::Adopted,
            other => panic!("task_worktrees.source holds an unknown value `{other}`"),
        },
        created_at: row.get(7)?,
        released_at: row.get(8)?,
    })
}
