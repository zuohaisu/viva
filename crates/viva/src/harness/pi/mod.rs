//! The Pi harness: the default conversational host for a configured
//! coordinating member (V09, issue #18).
//!
//! Division of labor (ADR 0011): Viva owns members/tasks/worktrees/terminals
//! and the terminal process; Pi owns its chat UI, model calls, tool loop and
//! compaction. This module builds the *launch combination* — explicit argv
//! plus the office-context environment the extension reads — and probes
//! availability honestly. It never starts a fallback harness when Pi is
//! missing: an unavailable provider is an error, not a swap.
//!
//! Identity rules encoded here:
//! - The member is whatever configuration says. The coordinating member is
//!   resolved by the caller (office configuration); `Samuel` is never
//!   hard-coded in this crate.
//! - Member identity travels as `VIVA_OFFICE_MEMBER_ID`; display name and
//!   model binding are captured at launch as configuration facts. Changing
//!   a binding later never rewrites identity or history.
//! - The dispatch grant, if any, is per-task and comes from the office's
//!   own grant records. Plain chat never receives an office-wide grant.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::harness::HarnessSpec;
use crate::members::{MemberBinding, MemberRecord};
use crate::tasks::TaskRecord;

/// Environment contract between the Rust harness and the TS extension.
/// Documented once, here and in `extensions/pi/README.md`.
pub const ENV_MEMBER_ID: &str = "VIVA_OFFICE_MEMBER_ID";
pub const ENV_MEMBER_NAME: &str = "VIVA_OFFICE_MEMBER_NAME";
pub const ENV_TASK_ID: &str = "VIVA_OFFICE_TASK_ID";
pub const ENV_GRANT_ID: &str = "VIVA_OFFICE_GRANT_ID";
/// Overrides the `viva` binary the extension calls (tests use a fake).
pub const ENV_VIVA_BIN: &str = "VIVA_BIN";

/// Default probe timeout for `<pi> --version`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Where the shipped extension lives, relative to a repository checkout.
pub const EXTENSION_RELATIVE_PATH: &str = "extensions/pi/viva-office.ts";

#[derive(Debug, Clone)]
pub struct PiHarness {
    /// The pi binary to launch. Configurable so tests can point at a fake.
    pub pi_binary: String,
    /// Absolute path to the shipped TypeScript extension.
    pub extension_path: PathBuf,
}

impl PiHarness {
    /// The harness for a repository checkout: the extension ships with the
    /// repo (V13 later ships it with the installation).
    pub fn for_repo(repo_root: &Path) -> Self {
        Self {
            pi_binary: "pi".into(),
            extension_path: repo_root.join(EXTENSION_RELATIVE_PATH),
        }
    }

    /// Build the launch combination for one member's interactive session.
    /// `task` attaches the current task context (brief and handoff target);
    /// `grant` is the office's live per-task grant reference — passing one
    /// is an office decision recorded at dispatch, never a chat capability.
    pub fn launch(
        &self,
        member: &MemberRecord,
        binding: Option<&MemberBinding>,
        task: Option<&TaskRecord>,
        cwd: &Path,
        grant: Option<&crate::foundation::ids::GrantId>,
    ) -> OfficeResult<HarnessSpec> {
        if !self.extension_path.is_absolute() {
            return Err(OfficeError::Validation(format!(
                "pi extension path must be absolute: {}",
                self.extension_path.display()
            )));
        }
        let mut spec = HarnessSpec::new(
            "pi",
            vec![
                self.pi_binary.clone(),
                "--extension".into(),
                self.extension_path.to_string_lossy().into_owned(),
            ],
            cwd,
        )?
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
        if let Some(binding) = binding {
            // The binding is a launch-time configuration snapshot (shown to
            // the member for orientation). It is not identity: identity is
            // the member id.
            spec = spec.with_env("VIVA_OFFICE_MODEL_BINDING", binding.model_binding.clone());
        }
        Ok(spec)
    }

    /// Honest availability probe: run `<pi> --version` and report what
    /// actually happened. A missing or failing binary is `Unavailable` —
    /// the caller must surface that, never substitute another harness.
    pub fn availability(&self) -> Availability {
        let mut child = match Command::new(&self.pi_binary)
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                return Availability::Unavailable {
                    reason: format!("`{}` could not be started: {err}", self.pi_binary),
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
                        reason: format!("`{} --version` exited with {status}", self.pi_binary),
                    };
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Availability::Unavailable {
                            reason: format!(
                                "`{} --version` did not answer within {PROBE_TIMEOUT:?}",
                                self.pi_binary
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

/// What the availability probe actually saw. Rendered for display; never
/// coerced into a pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Availability {
    Available,
    Unavailable { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::ids::MemberId;

    fn member() -> MemberRecord {
        MemberRecord {
            member_id: MemberId::new(),
            display_name: "Samuel".into(),
            created_at: crate::foundation::ids::utc_now(),
        }
    }

    fn binding(member_id: &MemberId) -> MemberBinding {
        MemberBinding {
            member_id: member_id.clone(),
            role: "coordinator".into(),
            model_binding: "glm-5.3-flash".into(),
            tools: vec!["pi-cli".into()],
            updated_at: crate::foundation::ids::utc_now(),
        }
    }

    #[test]
    fn launch_combination_is_explicit_and_context_is_injected() {
        let repo = std::env::temp_dir().join("viva-pi-launch-test");
        let harness = PiHarness::for_repo(&repo);
        let m = member();
        let task = crate::tasks::TaskRecord {
            task_id: crate::foundation::ids::TaskId::new(),
            goal: "write the office loop".into(),
            constraints: vec![],
            assignee_member_id: Some(m.member_id.clone()),
            workspace_id: None,
            project_id: None,
            status: crate::tasks::TaskStatus::Open,
            created_at: crate::foundation::ids::utc_now(),
            updated_at: crate::foundation::ids::utc_now(),
        };
        let grant = crate::foundation::ids::GrantId::new();
        let spec = harness
            .launch(
                &m,
                Some(&binding(&m.member_id)),
                Some(&task),
                &repo,
                Some(&grant),
            )
            .expect("launch");

        // Explicit argv: program, extension flag, extension path. No shell
        // string anywhere in the shape.
        assert_eq!(spec.argv[0], "pi");
        assert_eq!(spec.argv[1], "--extension");
        assert!(spec.argv[2].ends_with(EXTENSION_RELATIVE_PATH));

        // Context injection: identity, configuration snapshot, task and
        // grant references.
        let env = |key: &str| {
            spec.env
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(env(ENV_MEMBER_ID).as_deref(), Some(m.member_id.as_str()));
        assert_eq!(env(ENV_MEMBER_NAME).as_deref(), Some("Samuel"));
        assert_eq!(env(ENV_TASK_ID).as_deref(), Some(task.task_id.as_str()));
        assert_eq!(env(ENV_GRANT_ID).as_deref(), Some(grant.as_str()));
        assert_eq!(
            env("VIVA_OFFICE_MODEL_BINDING").as_deref(),
            Some("glm-5.3-flash")
        );
        assert_eq!(spec.member_id.as_ref(), Some(&m.member_id));
    }

    #[test]
    fn plain_chat_has_no_grant_reference() {
        // Without a task and without a grant, the environment carries
        // identity only — chat never inherits office authority.
        let repo = std::env::temp_dir();
        let harness = PiHarness::for_repo(&repo);
        let m = member();
        let spec = harness.launch(&m, None, None, &repo, None).expect("launch");
        assert!(!spec.env.iter().any(|(k, _)| k == ENV_GRANT_ID));
        assert!(!spec.env.iter().any(|(k, _)| k == ENV_TASK_ID));
    }

    #[test]
    fn missing_pi_is_reported_not_swapped() {
        let harness = PiHarness {
            pi_binary: "/nonexistent/viva-pi-should-not-exist".into(),
            extension_path: std::env::temp_dir()
                .join("viva-pi-launch-test")
                .join(EXTENSION_RELATIVE_PATH),
        };
        match harness.availability() {
            Availability::Unavailable { reason } => {
                assert!(reason.contains("could not be started"), "got: {reason}");
            }
            other => panic!("a missing binary must be Unavailable, got {other:?}"),
        }
    }

    #[test]
    fn a_working_fake_pi_is_available() {
        let dir = tempfile::TempDir::new().expect("dir");
        let fake = dir.path().join("fake-pi");
        std::fs::write(
            &fake,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo \"pi 1.2.3\"; exit 0; fi\nexit 1\n",
        )
        .expect("script");
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let harness = PiHarness {
            pi_binary: fake.to_string_lossy().into_owned(),
            extension_path: dir.path().join("ext.ts"),
        };
        assert_eq!(harness.availability(), Availability::Available);
    }

    #[test]
    fn a_failing_fake_pi_is_unavailable_with_reason() {
        let dir = tempfile::TempDir::new().expect("dir");
        let fake = dir.path().join("fake-pi-fail");
        std::fs::write(&fake, "#!/bin/sh\nexit 3\n").expect("script");
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let harness = PiHarness {
            pi_binary: fake.to_string_lossy().into_owned(),
            extension_path: dir.path().join("ext.ts"),
        };
        match harness.availability() {
            Availability::Unavailable { reason } => {
                assert!(reason.contains("exited with"), "got: {reason}");
            }
            other => panic!("a failing probe must be Unavailable, got {other:?}"),
        }
    }
}
