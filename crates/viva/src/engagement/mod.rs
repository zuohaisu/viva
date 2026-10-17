//! Engagement state for one execution (issue #118, ADR-0013 Decision 5).
//!
//! The state lives on this execution, not on the member. Explicit switch
//! wins, then entry rules, then a judgment model. A result that is unknown,
//! below the configured confidence, or from an unavailable model is stored
//! as pending with no domain. Recall and opening projection use that state
//! only to narrow visibility. Switching back to work does not erase harness
//! transcripts that were already produced.

mod judge;

pub use judge::{
    FixtureModel, FixtureReply, HttpModel, JudgmentDecision, JudgmentFailure, JudgmentModel,
    JudgmentRequest, ScriptModel, model_from_settings, parse_judgment_body,
};

use std::sync::Arc;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::foundation::error::{Error, Result};
use crate::foundation::events::{self, NewEvent};
use crate::foundation::ids::{ExecutionId, MemberId, ProjectId, TaskId, utc_now};
use crate::foundation::records::{self, ExecutionRecord};
use crate::foundation::store::{DOMAIN_ENGAGEMENT, MigrationRegistry, Store};
use crate::knowledge::SkillSetService;
use crate::memory::{MemorySearch, MemoryService, RecallFilter, VisibleMemory};

pub fn register_migrations(registry: MigrationRegistry) -> MigrationRegistry {
    registry.register(
        DOMAIN_ENGAGEMENT,
        1,
        "engagement state v1",
        ENGAGEMENT_V1_SQL,
    )
}

pub const ENGAGEMENT_V1_SQL: &str = r#"
CREATE TABLE engagement_settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE engagement_states (
    execution_id   TEXT PRIMARY KEY,
    member_id      TEXT NOT NULL,
    status         TEXT NOT NULL CHECK (status IN ('adopted', 'pending')),
    domain         TEXT CHECK (domain IN ('work', 'personal')),
    project_id     TEXT,
    interlocutor   TEXT,
    activity       TEXT,
    since          TEXT NOT NULL,
    source         TEXT CHECK (source IS NULL OR source IN ('explicit', 'entry', 'model')),
    confidence     REAL,
    model_binding  TEXT,
    pending_reason TEXT CHECK (
        pending_reason IS NULL
        OR pending_reason IN ('below_threshold', 'model_unavailable', 'unknown')
    ),
    updated_at     TEXT NOT NULL,
    CHECK (
        (
            status = 'adopted'
            AND domain IS NOT NULL
            AND pending_reason IS NULL
            AND source IS NOT NULL
        )
        OR
        (
            status = 'pending'
            AND domain IS NULL
            AND project_id IS NULL
            AND interlocutor IS NULL
            AND activity IS NULL
            AND pending_reason IS NOT NULL
            AND confidence IS NULL
        )
    )
);

CREATE TABLE engagement_experiences (
    seq            INTEGER PRIMARY KEY AUTOINCREMENT,
    execution_id   TEXT NOT NULL,
    kind           TEXT NOT NULL CHECK (kind IN ('adopted', 'corrected', 'pending')),
    source         TEXT,
    previous_json  TEXT,
    next_json      TEXT NOT NULL,
    fields_sent_json TEXT NOT NULL DEFAULT '[]',
    model_binding  TEXT,
    recorded_at    TEXT NOT NULL
);

CREATE INDEX engagement_experiences_execution
    ON engagement_experiences(execution_id, seq);

INSERT INTO engagement_settings(key, value) VALUES
    ('confidence_threshold', '0.8'),
    ('judgment_model', 'jev'),
    ('judgment_endpoint', ''),
    ('judgment_script', '');
"#;

const PERSONAL_PHRASES: &[&str] = &["聊点私事"];
const WORK_PHRASES: &[&str] = &["聊点工作"];

/// A skill whose knowledge tags include this exact tag is personal-domain
/// and is dropped unless the adopted state is personal.
pub const PERSONAL_SKILL_TAG: &str = "personal";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngagementDomain {
    Work,
    Personal,
}

impl EngagementDomain {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::Personal => "personal",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "work" => Ok(Self::Work),
            "personal" => Ok(Self::Personal),
            other => Err(Error::Validation(format!(
                "engagement domain `{other}` must be work or personal"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngagementStatus {
    Adopted,
    Pending,
    Unknown,
}

impl EngagementStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Adopted => "adopted",
            Self::Pending => "pending",
            Self::Unknown => "unknown",
        }
    }

    fn parse_stored(value: &str) -> Result<Self> {
        match value {
            "adopted" => Ok(Self::Adopted),
            "pending" => Ok(Self::Pending),
            other => Err(Error::Validation(format!(
                "engagement status `{other}` is not stored"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngagementSource {
    Explicit,
    Entry,
    Model,
}

impl EngagementSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Entry => "entry",
            Self::Model => "model",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "explicit" => Ok(Self::Explicit),
            "entry" => Ok(Self::Entry),
            "model" => Ok(Self::Model),
            other => Err(Error::Validation(format!(
                "engagement source `{other}` is unknown"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingReason {
    BelowThreshold,
    ModelUnavailable,
    Unknown,
}

impl PendingReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::BelowThreshold => "below_threshold",
            Self::ModelUnavailable => "model_unavailable",
            Self::Unknown => "unknown",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "below_threshold" => Ok(Self::BelowThreshold),
            "model_unavailable" => Ok(Self::ModelUnavailable),
            "unknown" => Ok(Self::Unknown),
            other => Err(Error::Validation(format!(
                "pending reason `{other}` is unknown"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngagementView {
    pub execution_id: ExecutionId,
    pub member_id: MemberId,
    pub status: EngagementStatus,
    pub domain: Option<EngagementDomain>,
    pub project_id: Option<ProjectId>,
    pub interlocutor: Option<String>,
    pub activity: Option<String>,
    pub since: Option<String>,
    pub source: Option<EngagementSource>,
    pub confidence: Option<f64>,
    pub model_binding: Option<String>,
    pub pending_reason: Option<PendingReason>,
}

impl EngagementView {
    pub fn recall_filter(&self) -> RecallFilter {
        let include_personal = self.status == EngagementStatus::Adopted
            && self.domain == Some(EngagementDomain::Personal);
        RecallFilter {
            include_personal,
            ..RecallFilter::default()
        }
    }

    fn unknown(execution_id: ExecutionId, member_id: MemberId) -> Self {
        Self {
            execution_id,
            member_id,
            status: EngagementStatus::Unknown,
            domain: None,
            project_id: None,
            interlocutor: None,
            activity: None,
            since: None,
            source: None,
            confidence: None,
            model_binding: None,
            pending_reason: Some(PendingReason::Unknown),
        }
    }
}

/// Adopt when confidence is finite and greater than or equal to the
/// configured threshold. The threshold is an argument; callers load it.
pub fn confidence_meets_threshold(confidence: f64, threshold: f64) -> bool {
    confidence.is_finite()
        && threshold.is_finite()
        && (0.0..=1.0).contains(&confidence)
        && confidence >= threshold
}

/// Explicit phrases win over entry rules. Both personal and work in one
/// utterance is not explicit — the resolver continues to the next level.
pub fn explicit_domain_from_utterance(utterance: &str) -> Option<EngagementDomain> {
    let personal =
        contains_phrase(utterance, PERSONAL_PHRASES) || has_state_command(utterance, "personal");
    let work = contains_phrase(utterance, WORK_PHRASES) || has_state_command(utterance, "work");
    match (personal, work) {
        (true, false) => Some(EngagementDomain::Personal),
        (false, true) => Some(EngagementDomain::Work),
        _ => None,
    }
}

fn contains_phrase(text: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|phrase| text.contains(phrase))
}

fn has_state_command(text: &str, domain: &str) -> bool {
    let tokens: Vec<String> = text
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(|ch: char| !ch.is_ascii_alphanumeric())
                .to_ascii_lowercase()
        })
        .collect();
    tokens
        .windows(2)
        .any(|pair| pair[0] == "state" && pair[1] == domain)
}

#[derive(Debug, Clone)]
pub struct ResolveInput {
    pub execution_id: ExecutionId,
    pub repository: Option<String>,
    pub task_id: Option<TaskId>,
    pub entry: Option<String>,
    pub utterance: Option<String>,
    pub explicit_domain: Option<EngagementDomain>,
    pub project_id: Option<ProjectId>,
    pub interlocutor: Option<String>,
    pub activity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolveOutcome {
    pub view: EngagementView,
    pub model_called: bool,
    pub experience_seq: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngagementExperience {
    pub seq: i64,
    pub execution_id: ExecutionId,
    pub kind: String,
    pub source: Option<String>,
    pub previous: Value,
    pub next: Value,
    pub fields_sent: Vec<String>,
    pub model_binding: Option<String>,
    pub recorded_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectedKnowledge {
    pub item_id: String,
    pub title: String,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectedSkill {
    pub skill_id: String,
    pub personal: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpeningProjection {
    pub engagement: EngagementView,
    pub knowledge: Vec<ProjectedKnowledge>,
    pub memories: Vec<VisibleMemory>,
    pub skills: Vec<ProjectedSkill>,
}

#[derive(Debug, Clone)]
pub struct EngagementConfig {
    pub confidence_threshold: f64,
    pub judgment_model: String,
    pub judgment_endpoint: String,
    pub judgment_script: String,
}

impl EngagementConfig {
    pub fn load(store: &Store) -> Result<Self> {
        let threshold_raw = setting(store, "confidence_threshold")?;
        let confidence_threshold: f64 = threshold_raw.parse().map_err(|_| {
            Error::Validation(format!(
                "engagement.confidence_threshold `{threshold_raw}` is not a number"
            ))
        })?;
        if !confidence_threshold.is_finite() || !(0.0..=1.0).contains(&confidence_threshold) {
            return Err(Error::Validation(format!(
                "engagement.confidence_threshold `{threshold_raw}` must be within 0..=1"
            )));
        }
        if confidence_threshold == 0.0 {
            return Err(Error::Validation(
                "engagement.confidence_threshold must be greater than 0".into(),
            ));
        }
        Ok(Self {
            confidence_threshold,
            judgment_model: setting(store, "judgment_model")?,
            judgment_endpoint: setting(store, "judgment_endpoint")?,
            judgment_script: setting(store, "judgment_script")?,
        })
    }
}

pub struct EngagementService<'a> {
    store: &'a Store,
    model: Arc<dyn JudgmentModel>,
}

impl<'a> EngagementService<'a> {
    pub fn new(store: &'a Store, model: Arc<dyn JudgmentModel>) -> Self {
        Self { store, model }
    }

    pub fn from_settings(store: &'a Store) -> Result<Self> {
        let config = EngagementConfig::load(store)?;
        let model = model_from_settings(
            &config.judgment_model,
            &config.judgment_endpoint,
            &config.judgment_script,
        );
        Ok(Self::new(store, model))
    }

    pub fn set_confidence_threshold(&self, value: f64) -> Result<()> {
        if !value.is_finite() || value <= 0.0 || value > 1.0 {
            return Err(Error::Validation(
                "engagement.confidence_threshold must be within (0, 1]".into(),
            ));
        }
        self.store.connection().execute(
            "UPDATE engagement_settings SET value = ?1 WHERE key = 'confidence_threshold'",
            rusqlite::params![value.to_string()],
        )?;
        Ok(())
    }

    pub fn get(&self, execution_id: &ExecutionId) -> Result<EngagementView> {
        let execution = self.require_execution(execution_id)?;
        Ok(self.load_row(execution_id)?.unwrap_or_else(|| {
            EngagementView::unknown(execution_id.clone(), execution.attribution.member_id)
        }))
    }

    pub fn resolve(&self, input: ResolveInput) -> Result<ResolveOutcome> {
        let execution = self.require_execution(&input.execution_id)?;
        let member_id = execution.attribution.member_id.clone();
        if let Some(task) = &input.task_id
            && task != &execution.task_id
        {
            return Err(Error::Validation(format!(
                "entry task `{task}` does not match execution `{}` task `{}`",
                execution.execution_id, execution.task_id
            )));
        }
        let config = EngagementConfig::load(self.store)?;
        let previous = self.load_row(&input.execution_id)?;
        let explicit = input.explicit_domain.or_else(|| {
            input
                .utterance
                .as_deref()
                .and_then(explicit_domain_from_utterance)
        });

        let (draft, model_called) = if let Some(domain) = explicit {
            (
                Draft::adopted(
                    domain,
                    EngagementSource::Explicit,
                    input.project_id.clone(),
                    blank_to_none(input.interlocutor.clone()),
                    blank_to_none(input.activity.clone()),
                    None,
                    None,
                ),
                false,
            )
        } else if entry_is_repository_task(&input, &execution) {
            let project = input
                .project_id
                .clone()
                .or_else(|| execution.attribution.project_id.clone());
            (
                Draft::adopted(
                    EngagementDomain::Work,
                    EngagementSource::Entry,
                    project,
                    None,
                    None,
                    None,
                    None,
                ),
                false,
            )
        } else {
            let request = JudgmentRequest {
                execution_id: input.execution_id.as_str().to_string(),
                member_id: member_id.as_str().to_string(),
                repository: blank_to_none(input.repository.clone()),
                task_id: input.task_id.as_ref().map(|id| id.as_str().to_string()),
                entry: blank_to_none(input.entry.clone()),
                utterance: blank_to_none(input.utterance.clone()),
            };
            (self.judge(&request, config.confidence_threshold)?, true)
        };

        let view = draft.view(input.execution_id.clone(), member_id);
        let experience_seq = self.persist(&view, previous.as_ref(), &draft.fields_sent)?;
        Ok(ResolveOutcome {
            view,
            model_called,
            experience_seq,
        })
    }

    pub fn experiences(&self, execution_id: &ExecutionId) -> Result<Vec<EngagementExperience>> {
        self.require_execution(execution_id)?;
        let mut stmt = self.store.connection().prepare(
            "SELECT seq, execution_id, kind, source, previous_json, next_json,
                    fields_sent_json, model_binding, recorded_at
             FROM engagement_experiences
             WHERE execution_id = ?1
             ORDER BY seq",
        )?;
        let rows = stmt.query_map([execution_id.as_str()], |row| {
            let execution_id: String = row.get(1)?;
            let previous: Option<String> = row.get(4)?;
            let next: String = row.get(5)?;
            let fields: String = row.get(6)?;
            Ok(EngagementExperience {
                seq: row.get(0)?,
                execution_id: ExecutionId::from_str_stored(&execution_id)?,
                kind: row.get(2)?,
                source: row.get(3)?,
                previous: parse_json_cell(previous.as_deref())?,
                next: parse_json_cell(Some(&next))?,
                fields_sent: serde_json::from_str(&fields).map_err(|err| {
                    rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Text,
                        Box::new(err),
                    )
                })?,
                model_binding: row.get(7)?,
                recorded_at: row.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn recall(&self, execution_id: &ExecutionId, query: &str) -> Result<EngagementRecall> {
        let view = self.get(execution_id)?;
        let filter = view.recall_filter();
        let search = MemoryService::new(self.store).search_filtered(
            &view.member_id,
            view.project_id.as_ref(),
            query,
            16 * 1024,
            &filter,
        )?;
        Ok(EngagementRecall {
            engagement: view,
            include_personal: filter.include_personal,
            search,
        })
    }

    /// Opening projection for this execution. Personal knowledge, personal
    /// memories, and personal-tagged skills stay out unless the adopted
    /// domain is personal. Missing state uses the restricted filter and
    /// writes nothing.
    pub fn opening_projection(&self, execution_id: &ExecutionId) -> Result<OpeningProjection> {
        let view = self.get(execution_id)?;
        let filter = view.recall_filter();
        let knowledge = self.project_knowledge(&view, &filter)?;
        let memories = MemoryService::new(self.store).list_visible(
            &view.member_id,
            view.project_id.as_ref(),
            &filter,
        )?;
        let skills = self.project_skills(&view, &filter)?;
        Ok(OpeningProjection {
            engagement: view,
            knowledge,
            memories,
            skills,
        })
    }

    fn judge(&self, request: &JudgmentRequest, threshold: f64) -> Result<Draft> {
        let binding = self.model.binding_name().to_string();
        match self.model.judge(request) {
            Ok(decision) => {
                if !decision.confidence.is_finite() || !(0.0..=1.0).contains(&decision.confidence) {
                    return Ok(Draft::pending(
                        PendingReason::ModelUnavailable,
                        Some(binding),
                        decision.fields_sent,
                    ));
                }
                if confidence_meets_threshold(decision.confidence, threshold) {
                    let project = decision
                        .project_id
                        .as_deref()
                        .and_then(ProjectId::from_str_loose);
                    Ok(Draft::adopted(
                        decision.domain,
                        EngagementSource::Model,
                        project,
                        blank_to_none(decision.interlocutor),
                        blank_to_none(decision.activity),
                        Some(decision.confidence),
                        Some(binding),
                    )
                    .with_fields(decision.fields_sent))
                } else {
                    Ok(Draft::pending(
                        PendingReason::BelowThreshold,
                        Some(binding),
                        decision.fields_sent,
                    ))
                }
            }
            Err(failure) => Ok(Draft::pending(
                PendingReason::ModelUnavailable,
                Some(failure.binding),
                failure.fields_sent,
            )),
        }
    }

    fn persist(
        &self,
        view: &EngagementView,
        previous: Option<&EngagementView>,
        fields_sent: &[String],
    ) -> Result<Option<i64>> {
        if previous.is_some_and(|prior| same_determination(prior, view)) {
            return Ok(None);
        }
        let kind = experience_kind(previous, view);
        let previous_json = previous.map(snapshot).transpose()?;
        let next_json = snapshot(view)?;
        let fields_json = serde_json::to_string(fields_sent)?;
        let tx = self.store.transaction()?;
        tx.execute(
            "INSERT INTO engagement_states(
                execution_id, member_id, status, domain, project_id, interlocutor, activity,
                since, source, confidence, model_binding, pending_reason, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(execution_id) DO UPDATE SET
                member_id = excluded.member_id,
                status = excluded.status,
                domain = excluded.domain,
                project_id = excluded.project_id,
                interlocutor = excluded.interlocutor,
                activity = excluded.activity,
                since = excluded.since,
                source = excluded.source,
                confidence = excluded.confidence,
                model_binding = excluded.model_binding,
                pending_reason = excluded.pending_reason,
                updated_at = excluded.updated_at",
            rusqlite::params![
                view.execution_id.as_str(),
                view.member_id.as_str(),
                view.status.as_str(),
                view.domain.map(EngagementDomain::as_str),
                view.project_id.as_ref().map(|id| id.as_str()),
                view.interlocutor,
                view.activity,
                view.since,
                view.source.map(EngagementSource::as_str),
                view.confidence,
                view.model_binding,
                view.pending_reason.map(PendingReason::as_str),
                utc_now(),
            ],
        )?;
        tx.execute(
            "INSERT INTO engagement_experiences(
                execution_id, kind, source, previous_json, next_json, fields_sent_json,
                model_binding, recorded_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                view.execution_id.as_str(),
                kind,
                view.source.map(EngagementSource::as_str),
                previous_json,
                next_json,
                fields_json,
                view.model_binding,
                utc_now(),
            ],
        )?;
        let seq = tx.last_insert_rowid();
        events::append_in(
            &tx,
            NewEvent {
                domain: DOMAIN_ENGAGEMENT,
                kind: "engagement.state_changed".into(),
                subject_type: "execution".into(),
                subject_id: view.execution_id.as_str().to_string(),
                origin: "engagement".into(),
                payload: json!({
                    "kind": kind,
                    "status": view.status.as_str(),
                    "domain": view.domain.map(EngagementDomain::as_str),
                    "source": view.source.map(EngagementSource::as_str),
                    "pending_reason": view.pending_reason.map(PendingReason::as_str),
                    "fields_sent": fields_sent,
                    "model_binding": view.model_binding,
                }),
            },
        )?;
        tx.commit()?;
        Ok(Some(seq))
    }

    fn require_execution(&self, execution_id: &ExecutionId) -> Result<ExecutionRecord> {
        records::get_execution(self.store, execution_id)?.ok_or_else(|| Error::NotFound {
            entity: "execution",
            id: execution_id.to_string(),
        })
    }

    fn load_row(&self, execution_id: &ExecutionId) -> Result<Option<EngagementView>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT member_id, status, domain, project_id, interlocutor, activity, since,
                    source, confidence, model_binding, pending_reason
             FROM engagement_states WHERE execution_id = ?1",
        )?;
        let row = stmt
            .query_row([execution_id.as_str()], |row| {
                let member_id: String = row.get(0)?;
                let status: String = row.get(1)?;
                let domain: Option<String> = row.get(2)?;
                let project_id: Option<String> = row.get(3)?;
                let source: Option<String> = row.get(7)?;
                let pending: Option<String> = row.get(10)?;
                Ok(EngagementView {
                    execution_id: execution_id.clone(),
                    member_id: MemberId::from_str_stored(&member_id)?,
                    status: EngagementStatus::parse_stored(&status).map_err(sql_err)?,
                    domain: domain
                        .as_deref()
                        .map(EngagementDomain::parse)
                        .transpose()
                        .map_err(sql_err)?,
                    project_id: project_id
                        .as_deref()
                        .map(ProjectId::from_str_stored)
                        .transpose()?,
                    interlocutor: row.get(4)?,
                    activity: row.get(5)?,
                    since: row.get(6)?,
                    source: source
                        .as_deref()
                        .map(EngagementSource::parse)
                        .transpose()
                        .map_err(sql_err)?,
                    confidence: row.get(8)?,
                    model_binding: row.get(9)?,
                    pending_reason: pending
                        .as_deref()
                        .map(PendingReason::parse)
                        .transpose()
                        .map_err(sql_err)?,
                })
            })
            .optional()?;
        Ok(row)
    }

    fn project_knowledge(
        &self,
        view: &EngagementView,
        filter: &RecallFilter,
    ) -> Result<Vec<ProjectedKnowledge>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT item_id, title, scope, owner_member_id, project_id
             FROM knowledge_items
             WHERE status = 'active'
             ORDER BY created_at, item_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (item_id, title, scope, owner, project) = row?;
            if scope == "skill" {
                continue;
            }
            let visible = match scope.as_str() {
                "personal" => {
                    filter.include_personal && owner.as_deref() == Some(view.member_id.as_str())
                }
                "project" => {
                    filter.include_project
                        && view.project_id.as_ref().map(|id| id.as_str()) == project.as_deref()
                        && project.is_some()
                }
                "team" => filter.include_team,
                _ => false,
            };
            if visible {
                out.push(ProjectedKnowledge {
                    item_id,
                    title,
                    scope,
                });
            }
        }
        Ok(out)
    }

    fn project_skills(
        &self,
        view: &EngagementView,
        filter: &RecallFilter,
    ) -> Result<Vec<ProjectedSkill>> {
        let memberships = SkillSetService::new(self.store).list_member_skills(&view.member_id)?;
        let mut out = Vec::new();
        for membership in memberships {
            let Some(skill) =
                crate::knowledge::KnowledgeRegistry::new(self.store).skill(&membership.skill_id)?
            else {
                continue;
            };
            if !skill.enabled {
                continue;
            }
            let personal = knowledge_item_is_personal(self.store, &skill.item_id)?;
            if personal && !filter.include_personal {
                continue;
            }
            out.push(ProjectedSkill {
                skill_id: membership.skill_id,
                personal,
            });
        }
        Ok(out)
    }
}

#[derive(Debug, Clone)]
pub struct EngagementRecall {
    pub engagement: EngagementView,
    pub include_personal: bool,
    pub search: MemorySearch,
}

struct Draft {
    status: EngagementStatus,
    domain: Option<EngagementDomain>,
    project_id: Option<ProjectId>,
    interlocutor: Option<String>,
    activity: Option<String>,
    source: Option<EngagementSource>,
    confidence: Option<f64>,
    model_binding: Option<String>,
    pending_reason: Option<PendingReason>,
    fields_sent: Vec<String>,
}

impl Draft {
    fn adopted(
        domain: EngagementDomain,
        source: EngagementSource,
        project_id: Option<ProjectId>,
        interlocutor: Option<String>,
        activity: Option<String>,
        confidence: Option<f64>,
        model_binding: Option<String>,
    ) -> Self {
        Self {
            status: EngagementStatus::Adopted,
            domain: Some(domain),
            project_id,
            interlocutor,
            activity,
            source: Some(source),
            confidence,
            model_binding,
            pending_reason: None,
            fields_sent: Vec::new(),
        }
    }

    fn with_fields(mut self, fields_sent: Vec<String>) -> Self {
        self.fields_sent = fields_sent;
        self
    }

    fn pending(
        reason: PendingReason,
        model_binding: Option<String>,
        fields_sent: Vec<String>,
    ) -> Self {
        Self {
            status: EngagementStatus::Pending,
            domain: None,
            project_id: None,
            interlocutor: None,
            activity: None,
            source: None,
            confidence: None,
            model_binding,
            pending_reason: Some(reason),
            fields_sent,
        }
    }

    fn view(&self, execution_id: ExecutionId, member_id: MemberId) -> EngagementView {
        EngagementView {
            execution_id,
            member_id,
            status: self.status,
            domain: self.domain,
            project_id: self.project_id.clone(),
            interlocutor: self.interlocutor.clone(),
            activity: self.activity.clone(),
            since: Some(utc_now()),
            source: self.source,
            confidence: self.confidence,
            model_binding: self.model_binding.clone(),
            pending_reason: self.pending_reason,
        }
    }
}

fn entry_is_repository_task(input: &ResolveInput, execution: &ExecutionRecord) -> bool {
    let Some(repository) = input.repository.as_deref().map(str::trim) else {
        return false;
    };
    if repository.is_empty() {
        return false;
    }
    match &input.task_id {
        Some(task) => task == &execution.task_id,
        None => input
            .entry
            .as_deref()
            .is_some_and(|entry| entry.eq_ignore_ascii_case("task")),
    }
}

fn same_determination(prior: &EngagementView, next: &EngagementView) -> bool {
    prior.status == next.status
        && prior.domain == next.domain
        && prior.project_id == next.project_id
        && prior.interlocutor == next.interlocutor
        && prior.activity == next.activity
        && prior.source == next.source
        && prior.pending_reason == next.pending_reason
        && prior.model_binding == next.model_binding
        && confidence_same(prior.confidence, next.confidence)
}

fn confidence_same(left: Option<f64>, right: Option<f64>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(a), Some(b)) => a.to_bits() == b.to_bits(),
        _ => false,
    }
}

fn experience_kind(previous: Option<&EngagementView>, next: &EngagementView) -> &'static str {
    if next.status == EngagementStatus::Pending {
        return "pending";
    }
    if previous.is_some_and(|prior| prior.status == EngagementStatus::Adopted) {
        "corrected"
    } else {
        "adopted"
    }
}

fn snapshot(view: &EngagementView) -> Result<String> {
    Ok(serde_json::to_string(&json!({
        "status": view.status.as_str(),
        "domain": view.domain.map(EngagementDomain::as_str),
        "project_id": view.project_id.as_ref().map(|id| id.as_str()),
        "interlocutor": view.interlocutor,
        "activity": view.activity,
        "source": view.source.map(EngagementSource::as_str),
        "confidence": view.confidence,
        "model_binding": view.model_binding,
        "pending_reason": view.pending_reason.map(PendingReason::as_str),
    }))?)
}

fn blank_to_none(value: Option<String>) -> Option<String> {
    value.and_then(|text| {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn setting(store: &Store, key: &str) -> Result<String> {
    store
        .connection()
        .query_row(
            "SELECT value FROM engagement_settings WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| Error::Validation(format!("engagement setting `{key}` is missing")))
}

fn knowledge_item_is_personal(store: &Store, item_id: &str) -> Result<bool> {
    let (scope, tags): (String, String) = store.connection().query_row(
        "SELECT scope, tags_json FROM knowledge_items WHERE item_id = ?1",
        [item_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if scope == "personal" {
        return Ok(true);
    }
    let parsed: Vec<String> =
        serde_json::from_str(&tags).unwrap_or_else(|_| vec!["personal".into()]);
    Ok(parsed.iter().any(|tag| tag == PERSONAL_SKILL_TAG))
}

fn parse_json_cell(raw: Option<&str>) -> rusqlite::Result<Value> {
    match raw {
        None => Ok(Value::Null),
        Some(text) => serde_json::from_str(text).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
        }),
    }
}

fn sql_err(err: Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
}

trait IdParse {
    fn from_str_stored(value: &str) -> rusqlite::Result<Self>
    where
        Self: Sized;
}

impl IdParse for ExecutionId {
    fn from_str_stored(value: &str) -> rusqlite::Result<Self> {
        value.parse().map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
        })
    }
}

impl IdParse for MemberId {
    fn from_str_stored(value: &str) -> rusqlite::Result<Self> {
        value.parse().map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
        })
    }
}

impl IdParse for ProjectId {
    fn from_str_stored(value: &str) -> rusqlite::Result<Self> {
        value.parse().map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
        })
    }
}

trait ProjectLoose {
    fn from_str_loose(value: &str) -> Option<Self>
    where
        Self: Sized;
}

impl ProjectLoose for ProjectId {
    fn from_str_loose(value: &str) -> Option<Self> {
        value.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_phrase_wins_only_when_one_domain_is_named() {
        assert_eq!(
            explicit_domain_from_utterance("我们聊点私事吧"),
            Some(EngagementDomain::Personal)
        );
        assert_eq!(
            explicit_domain_from_utterance("state work"),
            Some(EngagementDomain::Work)
        );
        assert_eq!(explicit_domain_from_utterance("先聊点私事再聊点工作"), None);
    }

    #[test]
    fn threshold_comparison_adopts_equal_confidence() {
        assert!(confidence_meets_threshold(0.4, 0.4));
        assert!(!confidence_meets_threshold(0.39, 0.4));
        assert!(!confidence_meets_threshold(f64::NAN, 0.4));
    }

    #[test]
    fn migration_seeds_the_initial_threshold_text() {
        assert!(ENGAGEMENT_V1_SQL.contains("('confidence_threshold', '0.8')"));
    }
}
