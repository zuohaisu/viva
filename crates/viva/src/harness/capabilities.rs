//! Cross-harness capability matrix (M1 S6, issues #27 / #19).
//!
//! Each row names one launch combination Viva can start or hand off to.
//! Cells are **evidence labels**, not promises: `tested` means automated
//! and/or documented real checks exist in this repo; `unsupported` means
//! Viva deliberately does not claim the capability; `unknown` means no
//! honest evidence yet (often pending real-tool QA on a member machine).

use serde::{Deserialize, Serialize};

use super::pi::{Availability, PiHarness};

/// How much evidence Viva has for one matrix cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityEvidence {
    Tested,
    Unsupported,
    Unknown,
}

impl CapabilityEvidence {
    pub fn as_str(self) -> &'static str {
        match self {
            CapabilityEvidence::Tested => "tested",
            CapabilityEvidence::Unsupported => "unsupported",
            CapabilityEvidence::Unknown => "unknown",
        }
    }
}

/// One harness row across the M1 exit matrix dimensions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessCapabilityRow {
    pub harness: String,
    pub launch: CapabilityEvidence,
    pub interactive: CapabilityEvidence,
    pub status_report: CapabilityEvidence,
    pub handoff: CapabilityEvidence,
    pub native_resume: CapabilityEvidence,
    pub fork: CapabilityEvidence,
    pub notes: String,
}

/// Full matrix plus live Pi probe (when requested).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessMatrix {
    pub generated_at: String,
    pub rows: Vec<HarnessCapabilityRow>,
    pub pi_probe: Option<Availability>,
}

fn static_rows() -> Vec<HarnessCapabilityRow> {
    let mut rows: Vec<HarnessCapabilityRow> = crate::agents::DETECTABLE_AGENTS
        .iter()
        .map(|&agent| {
            let (status, handoff, notes) = if agent == "pi" {
                (
                    CapabilityEvidence::Tested,
                    CapabilityEvidence::Tested,
                    "Viva launches pi --extension viva.ts with member/task env (V09). \
                     Fork/rename conversation tools ship in extensions/pi/conversations/ \
                     but are not loaded in production viva.ts — use handoff. \
                     Pi-native session resume is Pi-owned, not Viva-owned.",
                )
            } else {
                (
                    CapabilityEvidence::Unknown,
                    CapabilityEvidence::Tested,
                    "Bare argv launch and workbench handoff brief (no Viva extension). \
                     Controlled status reports need hook integration — not wired.",
                )
            };
            HarnessCapabilityRow {
                harness: agent.into(),
                launch: CapabilityEvidence::Unknown,
                interactive: CapabilityEvidence::Tested,
                status_report: status,
                handoff,
                native_resume: CapabilityEvidence::Unknown,
                fork: CapabilityEvidence::Unsupported,
                notes: notes.into(),
            }
        })
        .collect();
    rows.push(HarnessCapabilityRow {
        harness: "user_shell".into(),
        launch: CapabilityEvidence::Tested,
        interactive: CapabilityEvidence::Tested,
        status_report: CapabilityEvidence::Unsupported,
        handoff: CapabilityEvidence::Unsupported,
        native_resume: CapabilityEvidence::Unsupported,
        fork: CapabilityEvidence::Unsupported,
        notes: "Plain SHELL at worktree cwd (o/Enter). Not an agent harness.".into(),
    });
    rows
}

/// Harness names that support workbench handoff (`h` picker) per the matrix.
pub fn handoff_targets() -> Vec<String> {
    static_rows()
        .into_iter()
        .filter(|r| r.handoff == CapabilityEvidence::Tested && r.harness != "user_shell")
        .map(|r| r.harness)
        .collect()
}

/// Build the matrix, optionally probing Pi availability on this machine.
pub fn build(probe_pi: bool) -> HarnessMatrix {
    let pi_harness = PiHarness::for_installation();
    let pi_probe = probe_pi.then(|| pi_harness.availability());
    let mut rows = static_rows();
    if let Some(row) = rows.iter_mut().find(|r| r.harness == "pi") {
        row.launch = pi_launch_evidence(&pi_harness, pi_probe.as_ref());
        if matches!(pi_probe, Some(Availability::Unavailable { .. })) {
            row.notes
                .push_str(" This machine: pi probe failed — launch stays unavailable here.");
        }
    }
    for row in rows.iter_mut() {
        if row.harness == "pi" {
            continue;
        }
        if row.harness == "user_shell" {
            continue;
        }
        row.launch = binary_launch_evidence(&row.harness);
    }
    HarnessMatrix {
        generated_at: crate::foundation::ids::utc_now(),
        rows,
        pi_probe,
    }
}

fn pi_launch_evidence(harness: &PiHarness, probe: Option<&Availability>) -> CapabilityEvidence {
    if !harness.extension_path.is_file() {
        return CapabilityEvidence::Unsupported;
    }
    match probe {
        Some(Availability::Available) => CapabilityEvidence::Tested,
        Some(Availability::Unavailable { .. }) => CapabilityEvidence::Unknown,
        None => CapabilityEvidence::Unknown,
    }
}

fn binary_launch_evidence(program: &str) -> CapabilityEvidence {
    match std::process::Command::new(program)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(status) if status.success() => CapabilityEvidence::Tested,
        _ => CapabilityEvidence::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_includes_pi_and_documents_fork_honestly() {
        let m = build(false);
        let pi = m.rows.iter().find(|r| r.harness == "pi").expect("pi row");
        assert_eq!(pi.fork, CapabilityEvidence::Unsupported);
        assert!(pi.notes.contains("conversations"));
    }

    #[test]
    fn handoff_targets_exclude_user_shell() {
        let targets = handoff_targets();
        assert!(targets.iter().any(|t| t == "pi"));
        assert!(!targets.iter().any(|t| t == "user_shell"));
    }
}
