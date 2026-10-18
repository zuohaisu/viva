//! Claude Code launch for a projected member session (issue #114).
//!
//! The process is pointed at the same projection directory and the same
//! `soul.md` Pi reads. It does not write `CLAUDE.md` into the work directory.
//! `--bare` skips the work directory's own instruction-file discovery.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::foundation::error::{Error, Result};
use crate::harness::pi::{ENV_MEMBER_ID, ENV_MEMBER_NAME, ENV_MODEL_BINDING, ENV_TASK_ID};
use crate::harness::{ENV_EXECUTION_ID, ENV_PROJECTION_DIR, ENV_SOUL_PATH, HarnessSpec};
use crate::members::{MemberBinding, MemberRecord};
use crate::tasks::TaskRecord;

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The one projection Claude Code reads. Same files Pi is given.
pub struct ClaudeProjection<'a> {
    pub soul_path: &'a Path,
    pub projection_dir: &'a Path,
    pub execution_id: &'a str,
}

#[derive(Debug, Clone)]
pub struct ClaudeCodeHarness {
    pub binary: String,
}

impl ClaudeCodeHarness {
    pub fn for_installation() -> Self {
        Self {
            binary: std::env::var("VIVA_CLAUDE_BINARY").unwrap_or_else(|_| "claude".into()),
        }
    }

    /// `projection.soul_path` is the member's canonical file. The projection
    /// directory holds the symlink, the skill directory, and principles.
    pub fn launch(
        &self,
        member: &MemberRecord,
        binding: Option<&MemberBinding>,
        task: Option<&TaskRecord>,
        cwd: &Path,
        projection: &ClaudeProjection<'_>,
    ) -> Result<HarnessSpec> {
        if !cwd.is_absolute() {
            return Err(Error::Validation("claude-code cwd must be absolute".into()));
        }
        if !projection.soul_path.is_absolute() || !projection.projection_dir.is_absolute() {
            return Err(Error::Validation(
                "claude-code reads the absolute soul.md and projection directory".into(),
            ));
        }
        let soul = projection.soul_path.to_string_lossy().into_owned();
        let projection_dir = projection.projection_dir.to_string_lossy().into_owned();
        let mut spec = HarnessSpec::new(
            "claude-code",
            vec![
                self.binary.clone(),
                "--bare".into(),
                "--append-system-prompt-file".into(),
                soul,
                "--plugin-dir".into(),
                projection_dir.clone(),
                "--add-dir".into(),
                projection_dir,
                "--system-prompt-snapshot".into(),
                "off".into(),
            ],
            cwd,
        )?
        .for_member(member.member_id.clone())
        .with_env(ENV_MEMBER_ID, member.member_id.to_string())
        .with_env(ENV_MEMBER_NAME, member.display_name.clone())
        .with_env(ENV_SOUL_PATH, projection.soul_path.to_string_lossy())
        .with_env(
            ENV_PROJECTION_DIR,
            projection.projection_dir.to_string_lossy(),
        )
        .with_env(ENV_EXECUTION_ID, projection.execution_id);
        if let Some(task) = task {
            spec = spec.with_env(ENV_TASK_ID, task.task_id.to_string());
        }
        if let Some(binding) = binding {
            spec = spec.with_env(ENV_MODEL_BINDING, binding.model_binding.clone());
        }
        Ok(spec)
    }

    pub fn availability(&self) -> crate::harness::pi::Availability {
        use crate::harness::pi::Availability;
        let mut child = match Command::new(&self.binary)
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                return Availability::Unavailable {
                    reason: format!("`{}` could not be started: {err}", self.binary),
                };
            }
        };
        let deadline = Instant::now() + PROBE_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if status.success() {
                        return Availability::Available;
                    }
                    return Availability::Unavailable {
                        reason: format!("`{} --version` exited with {status}", self.binary),
                    };
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Availability::Unavailable {
                            reason: format!(
                                "`{} --version` did not answer within {PROBE_TIMEOUT:?}",
                                self.binary
                            ),
                        };
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(err) => {
                    return Availability::Unavailable {
                        reason: format!("probe failed: {err}"),
                    };
                }
            }
        }
    }
}
