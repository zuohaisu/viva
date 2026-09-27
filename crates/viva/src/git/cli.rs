//! Git CLI runner: argv-array command execution with timeouts, bounded
//! output capture and honest failure classification (V08, issue #17).
//!
//! - Commands are always argv arrays (paths with spaces work; there is no
//!   shell string anywhere).
//! - Git itself is the engine; this runner wraps `git` and `gh` binaries
//!   with the user's real credential environment — the office never copies
//!   or stores credentials.
//! - Failures classify into actionable states (missing repo, dirty tree,
//!   merge conflicts, missing auth, network); raw stderr travels with the
//!   error so the caller can act.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::foundation::error::{OfficeError, OfficeResult};

// TreeState derives Serialize/Deserialize so discovered state can travel
// through the workbench projection layer.

/// Hard bound on captured stdout/stderr per command (bounded logging).
pub const MAX_CAPTURED_OUTPUT: usize = 1024 * 1024;

/// One executed command's captured result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOutput {
    pub argv: Vec<String>,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Why a git operation failed, classified for actionable handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitFailure {
    /// The working tree has modifications (never discard them).
    Dirty { path: PathBuf, detail: String },
    /// The working tree has unresolved merge conflicts.
    Conflicts { path: PathBuf, detail: String },
    /// Authentication is missing for the requested remote operation.
    AuthMissing { detail: String },
    /// The network/remote is unreachable.
    Network { detail: String },
    /// No git repository at the path.
    NotARepository { path: PathBuf },
    /// Anything else, with the raw output attached.
    Other { detail: String },
}

impl std::fmt::Display for GitFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitFailure::Dirty { path, .. } => {
                write!(
                    f,
                    "working tree has uncommitted changes at {} — they are preserved, never discarded",
                    path.display()
                )
            }
            GitFailure::Conflicts { path, .. } => {
                write!(
                    f,
                    "unresolved merge conflicts at {} — resolve them before continuing",
                    path.display()
                )
            }
            GitFailure::AuthMissing { detail } => write!(f, "git authentication missing: {detail}"),
            GitFailure::Network { detail } => write!(f, "git remote unreachable: {detail}"),
            GitFailure::NotARepository { path } => {
                write!(f, "no git repository at {}", path.display())
            }
            GitFailure::Other { detail } => write!(f, "{detail}"),
        }
    }
}

/// Runs `git` (or `gh`) with the office's rules. `gh` subcommand
/// whitelisting lives in `crate::git::evidence`.
pub struct CliRunner {
    pub timeout: Duration,
}

impl Default for CliRunner {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
        }
    }
}

impl CliRunner {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    /// Run one command in `cwd`. On failure the output classifies into a
    /// [`GitFailure`].
    pub fn run(
        &self,
        program: &str,
        cwd: &Path,
        args: &[&str],
    ) -> Result<CommandOutput, GitFailure> {
        let argv: Vec<String> = std::iter::once(program.to_string())
            .chain(args.iter().map(|a| a.to_string()))
            .collect();
        let mut child = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| GitFailure::Other {
                detail: format!("cannot launch `{program}`: {err}"),
            })?;

        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed() >= self.timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(GitFailure::Other {
                        detail: format!("`{program} {}` timed out", args.join(" ")),
                    });
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(err) => {
                    return Err(GitFailure::Other {
                        detail: format!("`{program}` wait failed: {err}"),
                    });
                }
            }
        }
        let output = child.wait_with_output().map_err(|err| GitFailure::Other {
            detail: format!("`{program}` output capture failed: {err}"),
        })?;
        let stdout = bounded_lossy(&output.stdout);
        let stderr = bounded_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);
        let out = CommandOutput {
            argv,
            exit_code,
            stdout,
            stderr,
        };
        if out.success() {
            return Ok(out);
        }
        Err(classify(program, cwd, &out))
    }

    /// Run git, returning captured output or an error.
    pub fn git(&self, cwd: &Path, args: &[&str]) -> Result<CommandOutput, GitFailure> {
        self.run("git", cwd, args)
    }

    /// Run git expecting success; map failures into the office error type.
    pub fn git_ok(&self, cwd: &Path, args: &[&str]) -> OfficeResult<CommandOutput> {
        self.git(cwd, args)
            .map_err(|f| OfficeError::Validation(f.to_string()))
    }
}

fn bounded_lossy(bytes: &[u8]) -> String {
    let truncated = bytes.len() > MAX_CAPTURED_OUTPUT;
    let mut end = bytes.len().min(MAX_CAPTURED_OUTPUT);
    // Boundaries matter only for valid UTF-8 slices of a possibly-invalid
    // byte stream; from_utf8_lossy handles the rest.
    while end > 0 && bytes[end - 1] & 0xC0 == 0x80 {
        end -= 1;
    }
    let mut text = String::from_utf8_lossy(&bytes[..end]).into_owned();
    if truncated {
        text.push_str("… [output truncated]");
    }
    text
}

fn classify(program: &str, cwd: &Path, out: &CommandOutput) -> GitFailure {
    let combined = format!("{}{}", out.stdout, out.stderr);
    let lower = combined.to_lowercase();
    if program == "git" {
        if lower.contains("not a git repository") {
            return GitFailure::NotARepository {
                path: cwd.to_path_buf(),
            };
        }
        if lower.contains("could not read from remote repository")
            || lower.contains("authentication failed")
            || lower.contains("terminal prompts disabled")
        {
            return GitFailure::AuthMissing { detail: combined };
        }
        if lower.contains("could not resolve host") || lower.contains("connection refused") {
            return GitFailure::Network { detail: combined };
        }
    }
    GitFailure::Other {
        detail: format!(
            "`{} {}` failed ({}): {combined}",
            program,
            out.argv[1..].join(" "),
            out.exit_code
        ),
    }
}

/// Working-tree state of one checkout, probed read-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TreeState {
    Clean,
    /// Modifications present — the office never discards them.
    Dirty {
        entries: Vec<String>,
    },
    /// Unresolved merge conflicts.
    Conflicts {
        entries: Vec<String>,
    },
    /// The path does not exist (prunable worktree, deleted directory).
    Missing,
}

/// Probe `path`'s working tree state with `git status --porcelain` (NUL-free
/// parsing via line records; renamed entries use `->` which stays intact).
pub fn tree_state(runner: &CliRunner, path: &Path) -> TreeState {
    if !path.exists() {
        return TreeState::Missing;
    }
    match runner.git(path, &["status", "--porcelain"]) {
        Ok(out) => {
            let entries: Vec<String> = out
                .stdout
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_string)
                .collect();
            let conflicts: Vec<String> = entries
                .iter()
                .filter(|e| {
                    let code = e.get(..2).unwrap_or("");
                    code.contains('U') || code == "AA" || code == "DD"
                })
                .cloned()
                .collect();
            if !conflicts.is_empty() {
                TreeState::Conflicts { entries: conflicts }
            } else if entries.is_empty() {
                TreeState::Clean
            } else {
                TreeState::Dirty { entries }
            }
        }
        Err(GitFailure::NotARepository { .. }) => TreeState::Missing,
        Err(_) => TreeState::Missing,
    }
}

/// The checked-out branch of `path`, if any (`None` = detached HEAD).
pub fn current_branch(runner: &CliRunner, path: &Path) -> OfficeResult<Option<String>> {
    let out = runner.git_ok(path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = out.stdout.trim();
    Ok(if branch == "HEAD" || branch.is_empty() {
        None
    } else {
        Some(branch.to_string())
    })
}

/// The HEAD commit sha of `path`.
pub fn head_sha(runner: &CliRunner, path: &Path) -> OfficeResult<String> {
    let out = runner.git_ok(path, &["rev-parse", "HEAD"])?;
    Ok(out.stdout.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_with_spaces_reaches_git_intact() {
        let dir = tempfile::TempDir::new().expect("dir");
        let repo = dir.path().join("repo with spaces");
        std::fs::create_dir_all(&repo).expect("mkdir");
        let runner = CliRunner::default();
        runner
            .git_ok(&repo, &["init", "-q"])
            .expect("init in path with spaces");
        // A commitless repo has no HEAD revision; erroring is honest. The
        // point of this test is that the path with spaces reached git at all.
        assert!(
            runner
                .git(&repo, &["rev-parse", "--is-inside-work-tree"])
                .is_ok()
        );
    }

    #[test]
    fn tree_state_distinguishes_clean_dirty_and_missing() {
        let dir = tempfile::TempDir::new().expect("dir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        let runner = CliRunner::default();
        runner
            .git_ok(&repo, &["init", "-q", "-b", "main"])
            .expect("init");
        std::fs::write(repo.join("a.txt"), "hello").expect("write");
        runner.git_ok(&repo, &["add", "."]).expect("add");
        runner
            .git_ok(
                &repo,
                &[
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "user.name=t",
                    "commit",
                    "-qm",
                    "init",
                ],
            )
            .expect("commit");

        assert_eq!(tree_state(&runner, &repo), TreeState::Clean);

        std::fs::write(repo.join("a.txt"), "changed").expect("modify");
        match tree_state(&runner, &repo) {
            TreeState::Dirty { entries } => assert_eq!(entries.len(), 1),
            other => panic!("expected dirty, got {other:?}"),
        }

        assert_eq!(
            tree_state(&runner, &dir.path().join("missing")),
            TreeState::Missing
        );
    }
}
