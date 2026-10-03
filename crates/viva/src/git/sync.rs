//! Auto-sync policies (V15-4): the MAIN checkout of a project may
//! fast-forward automatically, but only when the user explicitly enabled
//! `auto_pull` — task worktrees are fetch-only, forever. Every outcome is
//! returned (never swallowed) so the caller can audit it.

use std::path::Path;

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::git::cli::CliRunner;

/// What one main-checkout sync actually did. Every variant is auditable
/// and honestly named: a skip names its reason, a failure carries the
/// git-side detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MainSyncOutcome {
    FastForward {
        from: String,
        to: String,
    },
    AlreadyUpToDate,
    SkippedDirty,
    /// The user-facing auto_pull switch is off: a deliberate SKIP (gate),
    /// not a failure — audit records must not overstate it.
    SkippedAutoPullDisabled,
    Failed(String),
}

/// Fetch a task worktree. Fetch never touches the working tree — this is
/// the only automatic thing a task worktree ever gets (V15-4 ruling).
pub fn fetch_worktree(worktree_path: &Path, runner: &CliRunner) -> OfficeResult<()> {
    runner
        .git_ok(worktree_path, &["fetch", "--prune", "origin"])
        .map(|_| ())
}

/// Fast-forward the main checkout: clean tree only, `--ff-only`, no
/// force, no merge. A dirty tree is a skip (with reason), never a stash.
pub fn sync_main_checkout(repo_root: &Path, runner: &CliRunner) -> OfficeResult<MainSyncOutcome> {
    let status = runner
        .git(repo_root, &["status", "--porcelain"])
        .map_err(|failure| {
            OfficeError::Validation(format!(
                "git status failed in {}: {failure}",
                repo_root.display()
            ))
        })?;
    if !status.stdout.trim().is_empty() {
        return Ok(MainSyncOutcome::SkippedDirty);
    }
    let before = crate::git::cli::head_sha(runner, repo_root)?;
    runner
        .git_ok(repo_root, &["pull", "--ff-only"])
        .map_err(|failure| {
            OfficeError::Validation(format!(
                "git pull --ff-only failed (the tree was left exactly as it was): {failure}"
            ))
        })?;
    let after = crate::git::cli::head_sha(runner, repo_root)?;
    if before == after {
        Ok(MainSyncOutcome::AlreadyUpToDate)
    } else {
        Ok(MainSyncOutcome::FastForward {
            from: before,
            to: after,
        })
    }
}

/// ahead/behind of a worktree's branch vs `origin/<branch>`. `None` when
/// the upstream ref does not exist (never fetched) or git fails — shown
/// as unknown, never guessed.
pub fn ahead_behind(
    worktree_path: &Path,
    branch: &str,
    runner: &CliRunner,
) -> OfficeResult<Option<(u32, u32)>> {
    let upstream = format!("origin/{branch}");
    let range = format!("{upstream}...HEAD");
    let out = match runner.git(
        worktree_path,
        &["rev-list", "--left-right", "--count", range.as_str()],
    ) {
        Ok(out) if out.success() => out,
        _ => return Ok(None),
    };
    let mut numbers = out.stdout.split_whitespace();
    let (Some(behind), Some(ahead), None) = (
        numbers.next().and_then(|n| n.parse::<u32>().ok()),
        numbers.next().and_then(|n| n.parse::<u32>().ok()),
        numbers.next(),
    ) else {
        return Ok(None);
    };
    Ok(Some((behind, ahead)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ahead_behind on a path that is not a repository is an honest
    /// unknown, never a guessed zero.
    #[test]
    fn ahead_behind_is_none_outside_a_repository() {
        let dir = tempfile::tempdir().expect("dir");
        let runner = CliRunner::default();
        let result = ahead_behind(dir.path(), "main", &runner).expect("no error");
        assert_eq!(result, None);
    }
}
