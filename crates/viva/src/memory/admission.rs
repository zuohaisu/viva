//! Temporal-model §5 admission gate for memory candidates (issue #116).
//!
//! Only candidates that pass at least one criterion become `admitted` facts.
//! Dynamic facts from authoritative systems are rejected or must be rewritten
//! as decisions, not state snapshots.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionSignal {
    Recurrence,
    Surprise,
    Emphasis,
    HighRederivationCost,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdmissionDecision {
    Admit { signals: Vec<AdmissionSignal> },
    Reject { reasons: Vec<String> },
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AdmissionHints {
    /// Caller asserted Haisu (or owner) explicitly asked to remember this.
    pub explicit_emphasis: bool,
    /// Prior proposed or admitted candidates with similar normalized content.
    pub similar_prior_count: usize,
    /// Model or caller marked this as conflicting with existing belief.
    pub marked_surprise: bool,
}

/// Returns whether `content` looks like a dynamic fact (issue/PR/task state)
/// rather than an explanation or decision.
pub fn looks_like_dynamic_fact(content: &str) -> Option<String> {
    let lower = content.to_lowercase();
    let patterns = [
        ("pr ", "merged", "PR merge status"),
        ("pull request", "merged", "PR merge status"),
        ("issue ", "closed", "issue status"),
        ("issue ", "open", "issue status"),
        ("task ", "done", "task status"),
        ("execution ", "completed", "execution status"),
        ("grant ", "revoked", "grant status"),
    ];
    for (a, b, label) in patterns {
        if lower.contains(a) && lower.contains(b) {
            return Some(format!(
                "dynamic fact ({label}): store the decision, not live object state"
            ));
        }
    }
    if lower.contains("已合并") && (lower.contains("pr") || lower.contains("pull")) {
        return Some(
            "dynamic fact (PR merge status): store the decision, not live object state".into(),
        );
    }
    None
}

fn has_emphasis_markers(content: &str, source: &str) -> bool {
    let blob = format!("{content}\n{source}").to_lowercase();
    const MARKERS: &[&str] = &[
        "记住",
        "务必记住",
        "强调",
        "remember this",
        "keep in mind",
        "do not forget",
        "important:",
        "emphasis:",
        "haisu said remember",
    ];
    MARKERS.iter().any(|m| blob.contains(m))
}

pub fn evaluate_admission(content: &str, source: &str, hints: AdmissionHints) -> AdmissionDecision {
    if let Some(reason) = looks_like_dynamic_fact(content) {
        return AdmissionDecision::Reject {
            reasons: vec![reason],
        };
    }
    let trimmed = content.trim();
    if trimmed.len() < 12 {
        return AdmissionDecision::Reject {
            reasons: vec!["content too thin to justify durable memory".into()],
        };
    }

    let mut signals = Vec::new();
    if hints.explicit_emphasis || has_emphasis_markers(content, source) {
        signals.push(AdmissionSignal::Emphasis);
    }
    if hints.similar_prior_count >= 1 {
        signals.push(AdmissionSignal::Recurrence);
    }
    if hints.marked_surprise {
        signals.push(AdmissionSignal::Surprise);
    }
    // Long, structured notes are costly to re-derive from scratch.
    if trimmed.len() >= 400 || trimmed.matches('\n').count() >= 4 {
        signals.push(AdmissionSignal::HighRederivationCost);
    }

    if signals.is_empty() {
        return AdmissionDecision::Reject {
            reasons: vec![
                "admission gate: no recurrence, surprise, emphasis, or high re-derivation cost"
                    .into(),
            ],
        };
    }

    AdmissionDecision::Admit { signals }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_pr_merged_status_snapshot() {
        let d = evaluate_admission(
            "PR #42 has been merged into main",
            "session summary",
            AdmissionHints::default(),
        );
        assert!(matches!(d, AdmissionDecision::Reject { .. }));
    }

    #[test]
    fn admits_explicit_emphasis() {
        let d = evaluate_admission(
            "We chose independent QA before merge for TFHS delivery",
            "chat",
            AdmissionHints {
                explicit_emphasis: true,
                ..Default::default()
            },
        );
        assert!(matches!(d, AdmissionDecision::Admit { .. }));
    }
}
