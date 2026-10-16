//! Bounded recall filters (#116): scope and dialog-state parameters before FTS.

use serde::{Deserialize, Serialize};

use crate::foundation::ids::{MemberId, ProjectId};

/// Default recall posture matches issue #118's "do not relax" default: personal
/// memories stay out unless the caller explicitly widens visibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallFilter {
    pub include_personal: bool,
    pub include_project: bool,
    pub include_team: bool,
    pub include_superseded: bool,
    pub include_retracted: bool,
}

impl Default for RecallFilter {
    fn default() -> Self {
        Self {
            include_personal: false,
            include_project: true,
            include_team: true,
            include_superseded: false,
            include_retracted: false,
        }
    }
}

impl RecallFilter {
    /// Personal recall for the owning member (legacy F04 behavior).
    pub fn personal_member() -> Self {
        Self {
            include_personal: true,
            include_project: true,
            include_team: true,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkScope {
    Personal,
    Project,
    Team,
}

impl LinkScope {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkScope::Personal => "personal",
            LinkScope::Project => "project",
            LinkScope::Team => "team",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "project" => LinkScope::Project,
            "team" => LinkScope::Team,
            _ => LinkScope::Personal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KnowledgeKind {
    PersonalMemory,
    ProjectKnowledge,
    TeamKnowledge,
}

impl KnowledgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            KnowledgeKind::PersonalMemory => "personal_memory",
            KnowledgeKind::ProjectKnowledge => "project_knowledge",
            KnowledgeKind::TeamKnowledge => "team_knowledge",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "project_knowledge" => KnowledgeKind::ProjectKnowledge,
            "team_knowledge" => KnowledgeKind::TeamKnowledge,
            _ => KnowledgeKind::PersonalMemory,
        }
    }

    pub fn link_scope(self) -> LinkScope {
        match self {
            KnowledgeKind::PersonalMemory => LinkScope::Personal,
            KnowledgeKind::ProjectKnowledge => LinkScope::Project,
            KnowledgeKind::TeamKnowledge => LinkScope::Team,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactLifecycle {
    Active,
    Superseded,
    Archived,
    Retracted,
}

impl FactLifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            FactLifecycle::Active => "active",
            FactLifecycle::Superseded => "superseded",
            FactLifecycle::Archived => "archived",
            FactLifecycle::Retracted => "retracted",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "superseded" => FactLifecycle::Superseded,
            "archived" => FactLifecycle::Archived,
            "retracted" => FactLifecycle::Retracted,
            _ => FactLifecycle::Active,
        }
    }

    pub fn recallable_default(self) -> bool {
        matches!(self, FactLifecycle::Active)
    }
}

pub fn scope_allowed(
    filter: &RecallFilter,
    scope: LinkScope,
    viewer: &MemberId,
    link_member: &MemberId,
    project: Option<&ProjectId>,
    link_project: Option<&ProjectId>,
) -> bool {
    match scope {
        LinkScope::Personal => filter.include_personal && link_member == viewer,
        LinkScope::Project => {
            filter.include_project && project.is_some() && link_project == project
        }
        LinkScope::Team => filter.include_team,
    }
}
