//! Claude Code launch combination (issue #117).
//!
//! The second harness named by the resident-container epic. It receives the
//! same execution and grant environment as Pi. Callback commands
//! (`viva memory recall|remember|state`, `viva skill`) read that environment;
//! a member id on the command line is not the caller.

use std::path::Path;

use crate::foundation::error::Result;
use crate::foundation::ids::{ExecutionId, GrantId};
use crate::harness::HarnessSpec;
use crate::harness::pi::{
    Availability, ENV_EXECUTION_ID, ENV_GRANT_ID, ENV_MEMBER_ID, ENV_MEMBER_NAME, ENV_TASK_ID,
    ENV_VIVA_BIN, probe_program,
};
use crate::members::MemberRecord;
use crate::tasks::TaskRecord;

#[derive(Debug, Clone)]
pub struct ClaudeHarness {
    pub claude_binary: String,
}

impl ClaudeHarness {
    pub fn for_installation() -> Self {
        Self {
            claude_binary: std::env::var("VIVA_CLAUDE_BINARY").unwrap_or_else(|_| "claude".into()),
        }
    }

    pub fn availability(&self) -> Availability {
        probe_program(&self.claude_binary)
    }

    /// Explicit argv plus the callback environment. No instruction file is
    /// written here (projection is #114) and nothing is joined into a shell
    /// string.
    pub fn launch(
        &self,
        member: &MemberRecord,
        task: Option<&TaskRecord>,
        cwd: &Path,
        grant: Option<&GrantId>,
        execution: Option<&ExecutionId>,
    ) -> Result<HarnessSpec> {
        let mut spec = HarnessSpec::new("claude", vec![self.claude_binary.clone()], cwd)?
            .for_member(member.member_id.clone())
            .with_env(ENV_MEMBER_ID, member.member_id.to_string())
            .with_env(ENV_MEMBER_NAME, member.display_name.clone())
            .with_env(
                ENV_VIVA_BIN,
                std::env::var(ENV_VIVA_BIN).unwrap_or_else(|_| "viva".into()),
            );
        if let Some(task) = task {
            spec = spec.with_env(ENV_TASK_ID, task.task_id.to_string());
        }
        if let Some(grant) = grant {
            spec = spec.with_env(ENV_GRANT_ID, grant.to_string());
        }
        if let Some(execution) = execution {
            spec = spec.with_env(ENV_EXECUTION_ID, execution.to_string());
        }
        Ok(spec)
    }
}

/// Same keys Pi injects for a callback session, so both harnesses call one CLI.
pub fn callback_env(
    member: &MemberRecord,
    task: &TaskRecord,
    grant: &GrantId,
    execution: &ExecutionId,
) -> Result<Vec<(String, String)>> {
    let spec = ClaudeHarness {
        claude_binary: "claude".into(),
    }
    .launch(
        member,
        Some(task),
        &std::env::temp_dir(),
        Some(grant),
        Some(execution),
    )?;
    Ok(spec.env)
}
