//! Harness launch combinations (V09, issue #18).
//!
//! Viva does not host the agent loop: a member's conversational harness
//! (Pi, or any other installed agent CLI) runs in a Viva terminal with its
//! own UI. This module is the *generic, explicit* launch combination entry
//! the workbench (V14) uses: a harness is named, its argv is explicit, its
//! working location is absolute, and its member context travels in
//! environment variables the harness extension reads. There is no field
//! anywhere that accepts a joined shell string, and there is no fallback
//! that silently swaps an unavailable harness for another one.
//!
//! Pi specialization lives in [`pi`]: it knows the extension path, the
//! default binary name and how to probe availability honestly.

pub mod pi;

use std::path::PathBuf;

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::MemberId;

/// One harness launch combination. `name` is display/selection data only —
/// it never hard-codes a member identity (`Viva ≠ Samuel`: member names are
/// configuration, and Samuel may use any harness).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessSpec {
    /// Which harness this launches (e.g. "pi", "codex"). Selection data,
    /// not identity.
    pub name: String,
    /// Explicit program + arguments. argv[0] must name a program; no
    /// element may be a joined shell command.
    pub argv: Vec<String>,
    /// Absolute working location for the session (task worktree or chosen
    /// project checkout).
    pub cwd: PathBuf,
    /// The member whose session this is, when launched for a member.
    pub member_id: Option<MemberId>,
    /// Extra environment (office context: member identity, task id, and —
    /// only when the office holds a live grant — the grant reference).
    pub env: Vec<(String, String)>,
}

impl HarnessSpec {
    pub fn new(
        name: impl Into<String>,
        argv: Vec<String>,
        cwd: impl Into<PathBuf>,
    ) -> OfficeResult<Self> {
        let spec = Self {
            name: name.into(),
            argv,
            cwd: cwd.into(),
            member_id: None,
            env: Vec::new(),
        };
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> OfficeResult<()> {
        if self.name.trim().is_empty() {
            return Err(OfficeError::Validation(
                "harness name must not be empty".into(),
            ));
        }
        let Some(program) = self.argv.first() else {
            return Err(OfficeError::Validation(
                "harness argv must name a program; empty argv is rejected".into(),
            ));
        };
        if program.trim().is_empty() {
            return Err(OfficeError::Validation(
                "harness argv[0] must be a program, not an empty string".into(),
            ));
        }
        if !self.cwd.is_absolute() {
            return Err(OfficeError::Validation(format!(
                "harness cwd must be absolute: {}",
                self.cwd.display()
            )));
        }
        Ok(())
    }

    #[must_use]
    pub fn for_member(mut self, member: MemberId) -> Self {
        self.member_id = Some(member);
        self
    }

    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_argv_relative_cwd_and_blank_name() {
        let err = HarnessSpec::new("pi", vec![], std::env::temp_dir()).expect_err("empty argv");
        assert!(err.to_string().contains("argv"), "got: {err}");

        let err = HarnessSpec::new(
            "pi",
            vec!["pi".into()],
            std::path::Path::new("relative/dir"),
        )
        .expect_err("relative cwd");
        assert!(err.to_string().contains("absolute"), "got: {err}");

        let err = HarnessSpec::new("  ", vec!["pi".into()], std::env::temp_dir())
            .expect_err("blank name");
        assert!(err.to_string().contains("name"), "got: {err}");
    }

    #[test]
    fn accepts_an_explicit_combination() {
        let spec = HarnessSpec::new(
            "pi",
            vec![
                "pi".into(),
                "--extension".into(),
                "/repo/extensions/pi/viva-office.ts".into(),
            ],
            std::env::temp_dir().join("wt-1"),
        )
        .expect("spec")
        .for_member(MemberId::new())
        .with_env("VIVA_OFFICE_MEMBER_ID", "mem-1");
        assert_eq!(spec.name, "pi");
        assert!(spec.member_id.is_some());
        assert_eq!(spec.env.len(), 1);
    }
}
