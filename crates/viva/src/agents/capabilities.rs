//! First-version capability matrix for the seven detectable agent CLIs (#76).
//!
//! Values are honest: `verified` means this repo exercises the path;
//! `unverified` means wiring exists but is not proven here; `unknown` means
//! missing hook, tool, or credentials on this machine.

use serde::{Deserialize, Serialize};

/// How far Viva has proven a capability for one CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityLevel {
    /// Exercised in this repository's automated checks.
    Verified,
    /// Known integration path, not fully verified in CI on every machine.
    Unverified,
    /// Not wired or not provable without real installs / credentials.
    #[default]
    Unknown,
    /// Deliberately out of scope for the first matrix.
    Unsupported,
}

impl CapabilityLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            CapabilityLevel::Verified => "verified",
            CapabilityLevel::Unverified => "unverified",
            CapabilityLevel::Unknown => "unknown",
            CapabilityLevel::Unsupported => "unsupported",
        }
    }
}

/// Per-CLI capability notes for the workbench agent list (#76).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentCliProfile {
    pub tool: &'static str,
    pub controlled_report: CapabilityLevel,
    pub process_identification: CapabilityLevel,
    pub screen_inference: CapabilityLevel,
    pub note: &'static str,
}

/// The seven CLIs Viva detects in v1 (owner ruling, 2026-10-01).
pub fn capability_matrix() -> &'static [AgentCliProfile] {
    use CapabilityLevel::*;
    &[
        AgentCliProfile {
            tool: "pi",
            controlled_report: Verified,
            process_identification: Verified,
            screen_inference: Unverified,
            note: "Pi extension reports through the controlled channel in-repo.",
        },
        AgentCliProfile {
            tool: "codex",
            controlled_report: Unverified,
            process_identification: Verified,
            screen_inference: Unverified,
            note: "Hook-capable; controlled report not verified in CI here.",
        },
        AgentCliProfile {
            tool: "claude",
            controlled_report: Unverified,
            process_identification: Verified,
            screen_inference: Unverified,
            note: "Hook-capable; controlled report not verified in CI here.",
        },
        AgentCliProfile {
            tool: "codebuddy",
            controlled_report: Unknown,
            process_identification: Verified,
            screen_inference: Unverified,
            note: "Detection only until a controlled hook is wired.",
        },
        AgentCliProfile {
            tool: "qodercli",
            controlled_report: Unknown,
            process_identification: Verified,
            screen_inference: Unverified,
            note: "Detection only until a controlled hook is wired.",
        },
        AgentCliProfile {
            tool: "cline",
            controlled_report: Unknown,
            process_identification: Verified,
            screen_inference: Unverified,
            note: "Detection only until a controlled hook is wired.",
        },
        AgentCliProfile {
            tool: "hermes",
            controlled_report: Unknown,
            process_identification: Verified,
            screen_inference: Unverified,
            note: "Detection only until a controlled hook is wired.",
        },
    ]
}

pub fn profile_for(tool: &str) -> Option<&'static AgentCliProfile> {
    let key = tool.to_ascii_lowercase();
    capability_matrix().iter().find(|p| p.tool == key)
}

/// Label shown beside a session when no live controlled report is present.
pub fn controlled_report_label(tool: &str, has_controlled_report: bool) -> &'static str {
    if has_controlled_report {
        return "report:live";
    }
    match profile_for(tool).map(|p| p.controlled_report) {
        Some(CapabilityLevel::Verified) => "report:unverified",
        Some(CapabilityLevel::Unverified) => "report:unverified",
        Some(CapabilityLevel::Unknown) => "report:unknown",
        Some(CapabilityLevel::Unsupported) => "report:unsupported",
        None => "report:unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_covers_all_detectable_clis() {
        for tool in super::super::DETECTABLE_AGENTS {
            assert!(
                profile_for(tool).is_some(),
                "missing capability profile for {tool}"
            );
        }
    }

    #[test]
    fn labels_stay_honest_without_a_controlled_report() {
        assert_eq!(controlled_report_label("pi", false), "report:unverified");
        assert_eq!(controlled_report_label("codex", false), "report:unverified");
        assert_eq!(controlled_report_label("cline", false), "report:unknown");
        assert_eq!(controlled_report_label("pi", true), "report:live");
    }
}
