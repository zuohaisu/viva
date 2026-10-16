//! Session-end curation: turn sequences → proposed candidates (#116).
//!
//! Experience ingestion from harness adapters lands in #115; this module
//! accepts the same turn shape so curators can run once experience exists.

use serde::{Deserialize, Serialize};

use super::{KnowledgeKind, MemoryService};
use crate::foundation::error::{Error, Result};
use crate::foundation::ids::{MemberId, ProjectId};
use crate::memory::admission::AdmissionDecision;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionTurn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CuratedProposal {
    pub summary: String,
    pub content: String,
    pub source: String,
    pub knowledge_kind: KnowledgeKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CurationOutcome {
    pub proposed: usize,
    pub admitted: usize,
    pub rejected: usize,
    pub candidate_ids: Vec<String>,
}

/// Heuristic curator: split user/assistant turns into candidate chunks.
/// A production path may replace this with a cheap model; admission still
/// gates every write.
pub fn proposals_from_turns(turns: &[SessionTurn]) -> Vec<CuratedProposal> {
    let mut out = Vec::new();
    for turn in turns {
        let body = turn.content.trim();
        if body.len() < 40 {
            continue;
        }
        let summary: String = body.chars().take(120).collect();
        let source = format!("session curation ({})", turn.role.trim());
        out.push(CuratedProposal {
            summary,
            content: body.to_string(),
            source,
            knowledge_kind: KnowledgeKind::PersonalMemory,
        });
    }
    out
}

impl<'a> MemoryService<'a> {
    pub fn curate_session(
        &self,
        member: &MemberId,
        project: Option<&ProjectId>,
        turns: &[SessionTurn],
        provenance_json: &str,
        emphasis: bool,
    ) -> Result<CurationOutcome> {
        if turns.is_empty() {
            return Err(Error::Validation("curation needs at least one turn".into()));
        }
        let proposals = proposals_from_turns(turns);
        let mut outcome = CurationOutcome {
            proposed: 0,
            admitted: 0,
            rejected: 0,
            candidate_ids: Vec::new(),
        };
        for proposal in proposals {
            let remember = self.remember_with_options(
                member,
                project,
                &proposal.content,
                &proposal.source,
                "general",
                "",
                proposal.knowledge_kind,
                emphasis,
                Some(provenance_json),
                Some(&proposal.summary),
            )?;
            outcome.proposed += 1;
            outcome.candidate_ids.push(remember.candidate_id);
            match remember.decision {
                AdmissionDecision::Admit { .. } => outcome.admitted += 1,
                AdmissionDecision::Reject { .. } => outcome.rejected += 1,
            }
        }
        Ok(outcome)
    }
}
