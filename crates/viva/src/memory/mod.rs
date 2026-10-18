//! Native memory storage (issue #130): facts, FTS5 trigram, minimal HRR,
//! and Viva's per-member link / usage / exit layer.
//!
//! Legacy Hermes rows stay in `memory_links` as `legacy_hermes` and never
//! participate in retrieval (#131 removed the Python Hermes adapter).

mod admission;
mod curation;
mod entity;
mod hash;
pub mod hrr;
pub mod migrate;
pub mod migrate_v4;
mod recall;
mod retrieval;

pub use admission::{AdmissionDecision, AdmissionHints, evaluate_admission};
pub use curation::{CurationOutcome, SessionTurn, proposals_from_turns};
pub use recall::{FactLifecycle, KnowledgeKind, LinkScope, RecallFilter};

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::authority::{Actor, AuthorityEngine};
use crate::foundation::error::{Error, Result};
use crate::foundation::ids::{MemberId, ProjectId, utc_now};
use crate::foundation::store::{DOMAIN_MEMORY, MigrationRegistry, Store};

pub use hrr::{HRR_DIM, encode_fact as encode_fact_hrr, encode_text as encode_text_hrr};

pub fn register_migrations(registry: MigrationRegistry) -> MigrationRegistry {
    registry
        .register(DOMAIN_MEMORY, 1, "memory v1", MEMORY_V1_SQL)
        .register(
            DOMAIN_MEMORY,
            2,
            "memory v2 per-member links",
            MEMORY_V2_SQL,
        )
        .register_fn(
            DOMAIN_MEMORY,
            3,
            "memory v3 native facts fts hrr",
            migrate::apply_memory_v3,
        )
        .register_fn(
            DOMAIN_MEMORY,
            4,
            "memory v4 curation candidates lifecycle",
            migrate_v4::apply_memory_v4,
        )
        .register(DOMAIN_MEMORY, 5, "memory v5 dialog state", MEMORY_V5_SQL)
}

/// Explicit dialog state for one execution (issue #117). No inference.
pub const MEMORY_V5_SQL: &str = r#"
CREATE TABLE dialog_states (
    execution_id   TEXT PRIMARY KEY,
    session_id     TEXT,
    member_id      TEXT NOT NULL,
    domain         TEXT NOT NULL CHECK (domain IN ('work', 'personal')),
    project_id     TEXT,
    interlocutor   TEXT,
    activity       TEXT,
    since_at       TEXT NOT NULL,
    written_by     TEXT NOT NULL CHECK (written_by IN ('command', 'haisu')),
    recorded_at    TEXT NOT NULL
);
"#;

pub const MEMORY_V1_SQL: &str = r#"
CREATE TABLE memory_links (
    link_id         TEXT PRIMARY KEY,
    fact_id         INTEGER NOT NULL UNIQUE,
    member_id       TEXT NOT NULL,
    project_id      TEXT,
    source          TEXT NOT NULL CHECK (length(trim(source)) > 0),
    status          TEXT NOT NULL DEFAULT 'active'
                    CHECK (status IN ('active', 'archived')),
    created_at      TEXT NOT NULL,
    archived_at     TEXT,
    archived_reason TEXT
);

CREATE TABLE memory_usages (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    fact_id    INTEGER NOT NULL,
    member_id  TEXT NOT NULL,
    query      TEXT NOT NULL,
    used_at    TEXT NOT NULL
);
"#;

pub const MEMORY_V2_SQL: &str = r#"
CREATE TABLE memory_links_v2 (
    link_id         TEXT PRIMARY KEY,
    fact_id         INTEGER NOT NULL,
    member_id       TEXT NOT NULL,
    project_id      TEXT,
    source          TEXT NOT NULL CHECK (length(trim(source)) > 0),
    status          TEXT NOT NULL DEFAULT 'active'
                    CHECK (status IN ('active', 'archived')),
    created_at      TEXT NOT NULL,
    archived_at     TEXT,
    archived_reason TEXT,
    UNIQUE (fact_id, member_id)
);
INSERT INTO memory_links_v2
    SELECT link_id, fact_id, member_id, project_id, source, status,
           created_at, archived_at, archived_reason
    FROM memory_links;
DROP TABLE memory_links;
ALTER TABLE memory_links_v2 RENAME TO memory_links;
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageOrigin {
    Native,
    LegacyHermes,
}

impl StorageOrigin {
    fn as_str(self) -> &'static str {
        match self {
            StorageOrigin::Native => "native",
            StorageOrigin::LegacyHermes => "legacy_hermes",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "legacy_hermes" => StorageOrigin::LegacyHermes,
            _ => StorageOrigin::Native,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryLink {
    pub link_id: String,
    pub fact_id: String,
    pub storage_origin: StorageOrigin,
    pub member_id: MemberId,
    pub project_id: Option<ProjectId>,
    pub link_scope: LinkScope,
    pub source: String,
    pub status: LinkStatus,
    pub created_at: String,
    pub archived_at: Option<String>,
    pub archived_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RememberResult {
    pub candidate_id: String,
    pub candidate_status: String,
    pub decision: Option<AdmissionDecision>,
    pub link: Option<MemoryLink>,
    pub fact_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkStatus {
    Active,
    Archived,
}

impl LinkStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkStatus::Active => "active",
            LinkStatus::Archived => "archived",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecalledFact {
    pub fact_id: String,
    pub summary: String,
    pub content: String,
    pub score: f64,
    pub trust: f64,
    pub member_id: MemberId,
    pub project_id: Option<ProjectId>,
    pub knowledge_kind: String,
    pub lifecycle_status: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MemorySearch {
    Fetched {
        facts: Vec<RecalledFact>,
        hidden_unclaimable: usize,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub seq: i64,
    pub fact_id: String,
    pub member_id: MemberId,
    pub query: String,
    pub used_at: String,
}

pub struct MemoryService<'a> {
    store: &'a Store,
}

impl<'a> MemoryService<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// Read-only snapshot for `viva memory status` — native facts in the Viva store.
    pub fn native_status(&self) -> Result<serde_json::Value> {
        let conn = self.store.connection();
        let facts_total: i64 =
            conn.query_row("SELECT COUNT(*) FROM memory_facts", [], |r| r.get(0))?;
        let legacy_hermes_links: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_links WHERE storage_origin = 'legacy_hermes'",
            [],
            |r| r.get(0),
        )?;
        let native_links: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_links WHERE storage_origin = 'native'",
            [],
            |r| r.get(0),
        )?;
        Ok(json!({
            "ok": true,
            "state": "ready",
            "storage": "native",
            "facts_total": facts_total,
            "legacy_hermes_links": legacy_hermes_links,
            "native_links": native_links,
            "hrr": {
                "dim": HRR_DIM,
                "rerank_weights": {
                    "fts": retrieval::FTS_WEIGHT,
                    "jaccard": retrieval::JACCARD_WEIGHT,
                    "hrr": retrieval::HRR_WEIGHT,
                }
            }
        }))
    }

    /// Creates a `proposed` candidate only (issue #116). Call [`Self::admit_candidate`]
    /// to run the admission gate and materialize an admitted fact.
    pub fn remember(
        &self,
        member: &MemberId,
        project: Option<&ProjectId>,
        content: &str,
        source: &str,
        category: &str,
        tags: &str,
    ) -> Result<RememberResult> {
        self.remember_with_options(
            member,
            project,
            content,
            source,
            category,
            tags,
            KnowledgeKind::PersonalMemory,
            false,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn remember_with_options(
        &self,
        member: &MemberId,
        project: Option<&ProjectId>,
        content: &str,
        source: &str,
        category: &str,
        tags: &str,
        kind: KnowledgeKind,
        explicit_emphasis: bool,
        provenance_json: Option<&str>,
        summary: Option<&str>,
    ) -> Result<RememberResult> {
        if content.trim().is_empty() {
            return Err(Error::Validation("memory content must not be empty".into()));
        }
        if source.trim().is_empty() {
            return Err(Error::Validation(
                "memory writes require a source (who wrote this, from which conversation/task)"
                    .into(),
            ));
        }
        if matches!(kind, KnowledgeKind::ProjectKnowledge) && project.is_none() {
            return Err(Error::Validation(
                "project_knowledge requires --project".into(),
            ));
        }

        let summary = summary
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| content.chars().take(160).collect());
        let provenance = provenance_json.unwrap_or("{}");
        let v4 = self.memory_v4_enabled()?;
        let candidate_id = format!("mc-{}", uuid::Uuid::new_v4().simple());
        let now = utc_now();

        if v4 {
            let hints_json = serde_json::json!({
                "explicit_emphasis": explicit_emphasis,
                "category": category,
                "tags": tags,
            })
            .to_string();
            self.store.connection().execute(
                "INSERT INTO memory_candidates(candidate_id, member_id, project_id, knowledge_kind,
                    summary, content, source, provenance_json, status, admission_json,
                    fact_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'proposed', ?9, NULL, ?10, ?10)",
                rusqlite::params![
                    candidate_id,
                    member.as_str(),
                    project.map(|p| p.as_str()),
                    kind.as_str(),
                    summary,
                    content,
                    source,
                    provenance,
                    hints_json,
                    now,
                ],
            )?;
            return Ok(RememberResult {
                candidate_id,
                candidate_status: "proposed".into(),
                decision: None,
                link: None,
                fact_id: None,
            });
        }

        // Pre-v4 stores: direct write (no candidate ledger).
        let hash = hash::content_hash(content);
        let link_scope = kind.link_scope();
        if let Some(existing_id) = self.fact_id_for_hash(&hash)? {
            let link = self.ensure_native_link_scoped(
                existing_id.clone(),
                member,
                project,
                source,
                link_scope,
                kind,
            )?;
            return Ok(RememberResult {
                candidate_id,
                candidate_status: "admitted".into(),
                decision: Some(AdmissionDecision::Admit {
                    signals: vec![admission::AdmissionSignal::Emphasis],
                }),
                fact_id: Some(existing_id),
                link: Some(link),
            });
        }

        let fact_id = format!("nf-{}", uuid::Uuid::new_v4().simple());
        let entities = entity::extract_entities(content);
        let vector = hrr::encode_fact(content, &entities, HRR_DIM);
        let blob = hrr::phases_to_bytes(&vector, HRR_DIM);
        let conn = self.store.connection();
        conn.execute(
            "INSERT INTO memory_facts(fact_id, content, content_hash, category, tags,
                                      trust_score, created_at, updated_at, hrr_vector)
             VALUES (?1, ?2, ?3, ?4, ?5, 1.0, ?6, ?6, ?7)",
            rusqlite::params![fact_id, content, hash, category, tags, now, blob],
        )?;
        self.attach_entities(&fact_id, &entities, &now)?;
        let link = self.insert_native_link_scoped(&fact_id, member, project, source, link_scope)?;
        Ok(RememberResult {
            candidate_id,
            candidate_status: "admitted".into(),
            decision: Some(AdmissionDecision::Admit {
                signals: vec![admission::AdmissionSignal::Emphasis],
            }),
            fact_id: Some(fact_id),
            link: Some(link),
        })
    }

    /// Run the admission gate on a `proposed` candidate and materialize facts when admitted.
    pub fn admit_candidate(&self, candidate_id: &str) -> Result<RememberResult> {
        if !self.memory_v4_enabled()? {
            return Err(Error::Validation(
                "admit_candidate requires memory v4 schema".into(),
            ));
        }
        let row = self.load_proposed_candidate(candidate_id)?;
        let hints_value: serde_json::Value =
            serde_json::from_str(&row.hints_json).unwrap_or(json!({}));
        let explicit_emphasis = hints_value
            .get("explicit_emphasis")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let category = hints_value
            .get("category")
            .and_then(|v| v.as_str())
            .unwrap_or("general");
        let tags = hints_value
            .get("tags")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let similar = self.count_similar_candidates_for_member(&row.content, &row.member_id)?;
        let decision = evaluate_admission(
            &row.content,
            &row.source,
            AdmissionHints {
                explicit_emphasis,
                similar_prior_count: similar,
                marked_surprise: false,
            },
        );

        let (status, admission_json) = match &decision {
            AdmissionDecision::Admit { signals } => (
                "admitted",
                serde_json::to_string(signals).unwrap_or_else(|_| "[]".into()),
            ),
            AdmissionDecision::Reject { reasons } => (
                "rejected",
                serde_json::to_string(reasons).unwrap_or_else(|_| "[]".into()),
            ),
        };

        if let AdmissionDecision::Reject { .. } = decision {
            self.store.connection().execute(
                "UPDATE memory_candidates SET status = ?2, admission_json = ?3, updated_at = ?4
                 WHERE candidate_id = ?1",
                rusqlite::params![candidate_id, status, admission_json, utc_now()],
            )?;
            return Ok(RememberResult {
                candidate_id: candidate_id.to_string(),
                candidate_status: status.to_string(),
                decision: Some(decision),
                link: None,
                fact_id: None,
            });
        }

        let member = row.member_id;
        let project = row.project_id.as_ref();
        let kind = row.kind;
        let hash = hash::content_hash(&row.content);
        let link_scope = kind.link_scope();
        let now = utc_now();

        let (fact_id, link) = if let Some(existing_id) = self.fact_id_for_hash(&hash)? {
            let link = self.ensure_native_link_scoped(
                existing_id.clone(),
                &member,
                project,
                &row.source,
                link_scope,
                kind,
            )?;
            if link.link_scope == link_scope {
                self.upgrade_fact_kind(&existing_id, kind)?;
            }
            (existing_id, link)
        } else {
            let fact_id = format!("nf-{}", uuid::Uuid::new_v4().simple());
            let entities = entity::extract_entities(&row.content);
            let vector = hrr::encode_fact(&row.content, &entities, HRR_DIM);
            let blob = hrr::phases_to_bytes(&vector, HRR_DIM);
            self.store.connection().execute(
                "INSERT INTO memory_facts(fact_id, content, content_hash, category, tags,
                    trust_score, created_at, updated_at, hrr_vector, summary, knowledge_kind,
                    lifecycle_status, importance_reason, provenance_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1.0, ?6, ?6, ?7, ?8, ?9, 'active', ?10, ?11)",
                rusqlite::params![
                    fact_id,
                    row.content,
                    hash,
                    category,
                    tags,
                    now,
                    blob,
                    row.summary,
                    kind.as_str(),
                    admission_json,
                    row.provenance_json,
                ],
            )?;
            self.attach_entities(&fact_id, &entities, &now)?;
            let link = self.insert_native_link_scoped(
                &fact_id,
                &member,
                project,
                &row.source,
                link_scope,
            )?;
            (fact_id, link)
        };

        self.store.connection().execute(
            "UPDATE memory_candidates SET status = 'admitted', admission_json = ?2,
                    fact_id = ?3, updated_at = ?4
             WHERE candidate_id = ?1",
            rusqlite::params![candidate_id, admission_json, fact_id, utc_now()],
        )?;

        Ok(RememberResult {
            candidate_id: candidate_id.to_string(),
            candidate_status: "admitted".into(),
            decision: Some(decision),
            fact_id: Some(fact_id),
            link: Some(link),
        })
    }

    pub fn link_fact(
        &self,
        fact_id: &str,
        member: &MemberId,
        project: Option<&ProjectId>,
        source: &str,
    ) -> Result<MemoryLink> {
        if source.trim().is_empty() {
            return Err(Error::Validation("memory links require a source".into()));
        }
        if self.is_legacy_hermes_ref(fact_id)? {
            return Err(Error::Validation(
                "cannot link legacy Hermes fact ids; native fact ids only".into(),
            ));
        }
        if !self.native_fact_exists(fact_id)? {
            return Err(Error::NotFound {
                entity: "native memory fact",
                id: fact_id.to_string(),
            });
        }
        if let Some(existing) = self.link_for(fact_id, member)? {
            if existing.storage_origin == StorageOrigin::LegacyHermes {
                return Err(Error::Validation(
                    "cannot link legacy Hermes fact ids; native fact ids only".into(),
                ));
            }
            if existing.project_id != project.cloned() {
                return Err(Error::Validation(
                    "memory link project does not match the existing association".into(),
                ));
            }
            return Ok(existing);
        }
        self.insert_native_link(fact_id, member, project, source)
    }

    pub fn search(
        &self,
        viewer: &MemberId,
        project: Option<&ProjectId>,
        query: &str,
        max_total_bytes: usize,
    ) -> Result<MemorySearch> {
        self.search_filtered(
            viewer,
            project,
            query,
            max_total_bytes,
            &RecallFilter::personal_member(),
        )
    }

    pub fn search_filtered(
        &self,
        viewer: &MemberId,
        project: Option<&ProjectId>,
        query: &str,
        max_total_bytes: usize,
        filter: &RecallFilter,
    ) -> Result<MemorySearch> {
        if query.trim().is_empty() {
            return Err(Error::Validation(
                "memory search query must not be empty".into(),
            ));
        }
        let limit = 25usize;
        let candidates = self.scoped_candidates(viewer, project, query, limit * 3, filter)?;
        let ranked = retrieval::rerank(query, candidates, limit);
        self.pack_recall(viewer, project, ranked, max_total_bytes, filter)
    }

    pub fn probe(
        &self,
        viewer: &MemberId,
        project: Option<&ProjectId>,
        entity: &str,
        max_total_bytes: usize,
    ) -> Result<MemorySearch> {
        if entity.trim().is_empty() {
            return Err(Error::Validation(
                "memory probe entity must not be empty".into(),
            ));
        }
        let filter = RecallFilter::personal_member();
        let scoped = self.scoped_fact_rows(viewer, project, &filter)?;
        let mut scored: Vec<(retrieval::CandidateFact, f64)> = Vec::new();
        for row in scoped {
            if let Some(sim) = retrieval::probe_score(entity, &row) {
                let score = retrieval::shift_similarity(sim) * row.trust_score;
                scored.push((row, score));
            }
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(25);
        let ranked = scored
            .into_iter()
            .map(|(mut c, score)| {
                c.fts_rank = score;
                (c, score)
            })
            .collect();
        self.pack_recall(viewer, project, ranked, max_total_bytes, &filter)
    }

    /// Record real execution usage (`used_in`); retrieval alone must not write this.
    pub fn record_execution_usage(
        &self,
        fact_id: &str,
        member: &MemberId,
        execution_id: &str,
        note: &str,
    ) -> Result<()> {
        if !self.memory_v4_enabled()? {
            return Err(Error::Validation(
                "execution usage requires memory v4 schema".into(),
            ));
        }
        self.store.connection().execute(
            "INSERT INTO memory_execution_usages(fact_ref, member_id, execution_id, note, used_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![fact_id, member.as_str(), execution_id, note, utc_now()],
        )?;
        Ok(())
    }

    pub fn archive(
        &self,
        fact_id: &str,
        reason: &str,
        actor: &Actor,
        authority: &AuthorityEngine<'_>,
    ) -> Result<MemoryLink> {
        self.exit_flip(fact_id, LinkStatus::Archived, reason, actor, authority)
    }

    pub fn restore(
        &self,
        fact_id: &str,
        reason: &str,
        actor: &Actor,
        authority: &AuthorityEngine<'_>,
    ) -> Result<MemoryLink> {
        self.exit_flip(fact_id, LinkStatus::Active, reason, actor, authority)
    }

    fn exit_flip(
        &self,
        fact_id: &str,
        status: LinkStatus,
        reason: &str,
        actor: &Actor,
        authority: &AuthorityEngine<'_>,
    ) -> Result<MemoryLink> {
        let Actor::Member { member, grant: _ } = actor else {
            return Err(Error::Validation(
                "workers do not exit memories; member grants only".into(),
            ));
        };
        authority
            .check(actor, "maintain_knowledge", None)?
            .map_err(|reason| Error::Validation(format!("memory exit not authorized: {reason}")))?;
        let Some(link) = self.link_for(fact_id, member)? else {
            return Err(Error::NotFound {
                entity: "own memory link",
                id: fact_id.to_string(),
            });
        };
        if link.storage_origin == StorageOrigin::LegacyHermes {
            return Err(Error::Validation(
                "legacy Hermes links are not mutable through native exit paths".into(),
            ));
        }
        if reason.trim().is_empty() {
            return Err(Error::Validation(
                "a memory exit action requires a reason".into(),
            ));
        }
        let now = utc_now();
        match status {
            LinkStatus::Archived => {
                self.store.connection().execute(
                    "UPDATE memory_links SET status = 'archived', archived_at = ?3,
                            archived_reason = ?4
                     WHERE fact_ref = ?1 AND member_id = ?2 AND storage_origin = 'native'",
                    rusqlite::params![
                        fact_id,
                        member.as_str(),
                        now,
                        format!("{reason} (by {})", member.as_str()),
                    ],
                )?;
            }
            LinkStatus::Active => {
                self.store.connection().execute(
                    "UPDATE memory_links SET status = 'active', archived_at = NULL,
                            archived_reason = NULL
                     WHERE fact_ref = ?1 AND member_id = ?2 AND storage_origin = 'native'",
                    rusqlite::params![fact_id, member.as_str()],
                )?;
            }
        }
        self.link_for(fact_id, member)?
            .ok_or_else(|| Error::NotFound {
                entity: "own memory link",
                id: fact_id.to_string(),
            })
    }

    pub fn link_for(&self, fact_id: &str, member: &MemberId) -> Result<Option<MemoryLink>> {
        if self.memory_v4_enabled()? {
            let mut stmt = self.store.connection().prepare(
                "SELECT link_id, fact_ref, storage_origin, member_id, project_id,
                        COALESCE(link_scope, 'personal'), source, status,
                        created_at, archived_at, archived_reason
                 FROM memory_links WHERE fact_ref = ?1 AND member_id = ?2
                 ORDER BY CASE storage_origin WHEN 'native' THEN 0 ELSE 1 END LIMIT 1",
            )?;
            let row = stmt
                .query_row(rusqlite::params![fact_id, member.as_str()], |row| {
                    parse_link_row_scoped(row)
                })
                .optional()?;
            return Ok(row);
        }
        let mut stmt = self.store.connection().prepare(
            "SELECT link_id, fact_ref, storage_origin, member_id, project_id, source, status,
                    created_at, archived_at, archived_reason
             FROM memory_links WHERE fact_ref = ?1 AND member_id = ?2
             ORDER BY CASE storage_origin WHEN 'native' THEN 0 ELSE 1 END LIMIT 1",
        )?;
        let row = stmt
            .query_row(rusqlite::params![fact_id, member.as_str()], |row| {
                parse_link_row(row)
            })
            .optional()?;
        Ok(row)
    }

    pub fn usage_of(&self, fact_id: &str) -> Result<Vec<UsageRecord>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT seq, fact_ref, member_id, query, used_at
             FROM memory_usages WHERE fact_ref = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([fact_id], |row| {
            let member: String = row.get(2)?;
            Ok(UsageRecord {
                seq: row.get(0)?,
                fact_id: row.get(1)?,
                member_id: MemberId::from_str(&member).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                query: row.get(3)?,
                used_at: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn pack_recall(
        &self,
        viewer: &MemberId,
        project: Option<&ProjectId>,
        ranked: Vec<(retrieval::CandidateFact, f64)>,
        max_total_bytes: usize,
        filter: &RecallFilter,
    ) -> Result<MemorySearch> {
        let mut facts = Vec::new();
        let mut used_bytes = 0usize;
        for (hit, score) in ranked {
            let link = self.best_link_for_recall(&hit.fact_id, viewer, project, filter)?;
            let Some(link) = link else {
                continue;
            };
            if link.status != LinkStatus::Active {
                continue;
            }
            if link.link_scope == LinkScope::Personal && link.project_id != project.cloned() {
                continue;
            }
            if link.storage_origin != StorageOrigin::Native {
                continue;
            }
            if !recall::scope_allowed(
                filter,
                link.link_scope,
                viewer,
                &link.member_id,
                project,
                link.project_id.as_ref(),
            ) {
                continue;
            }
            let lifecycle = FactLifecycle::parse(&hit.lifecycle_status);
            if lifecycle == FactLifecycle::Superseded && !filter.include_superseded {
                continue;
            }
            if lifecycle == FactLifecycle::Retracted && !filter.include_retracted {
                continue;
            }
            if lifecycle == FactLifecycle::Archived {
                continue;
            }
            let size = hit.content.len();
            if used_bytes + size > max_total_bytes {
                continue;
            }
            used_bytes += size;
            facts.push(RecalledFact {
                fact_id: hit.fact_id,
                summary: hit.summary.clone(),
                content: hit.content,
                score,
                trust: hit.trust_score,
                member_id: link.member_id,
                project_id: link.project_id,
                knowledge_kind: hit.knowledge_kind.clone(),
                lifecycle_status: hit.lifecycle_status.clone(),
                source: link.source,
            });
        }
        Ok(MemorySearch::Fetched {
            facts,
            hidden_unclaimable: 0,
        })
    }

    fn scoped_candidates(
        &self,
        viewer: &MemberId,
        project: Option<&ProjectId>,
        query: &str,
        candidate_limit: usize,
        filter: &RecallFilter,
    ) -> Result<Vec<retrieval::CandidateFact>> {
        let scoped = self.scoped_fact_rows(viewer, project, filter)?;
        if scoped.is_empty() {
            return Ok(Vec::new());
        }
        if query.chars().count() < 3 {
            let q = query.to_lowercase();
            let mut hits: Vec<retrieval::CandidateFact> = scoped
                .into_iter()
                .filter(|f| f.content.to_lowercase().contains(&q))
                .collect();
            for h in &mut hits {
                h.fts_rank = 1.0;
            }
            hits.truncate(candidate_limit);
            return Ok(hits);
        }
        let allowed: Vec<String> = scoped.iter().map(|f| f.fact_id.clone()).collect();
        let fts_query = retrieval::sanitize_fts_query(query);
        let placeholders = allowed.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!(
            "SELECT f.fact_id, f.content, f.category, f.tags, f.trust_score, f.hrr_vector,
                    fts.rank AS fts_rank_raw
             FROM memory_facts_fts fts
             JOIN memory_facts f ON f.rowid = fts.rowid
             WHERE memory_facts_fts MATCH ? AND f.fact_id IN ({placeholders})
               AND f.trust_score >= 0.3
             ORDER BY fts.rank LIMIT ?"
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(fts_query)];
        for id in &allowed {
            params.push(Box::new(id.clone()));
        }
        params.push(Box::new(candidate_limit as i64));
        let stmt = self.store.connection().prepare(&sql);
        let mut out = Vec::new();
        if let Ok(mut stmt) = stmt {
            let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
            if let Ok(rows) = stmt.query_map(param_refs.as_slice(), |row| {
                let rank: f64 = row.get(6)?;
                Ok((
                    retrieval::CandidateFact {
                        fact_id: row.get(0)?,
                        content: row.get(1)?,
                        category: row.get(2)?,
                        tags: row.get(3)?,
                        trust_score: row.get(4)?,
                        hrr_vector: row.get(5)?,
                        fts_rank: 0.0,
                        summary: String::new(),
                        knowledge_kind: "personal_memory".into(),
                        lifecycle_status: "active".into(),
                    },
                    rank.abs(),
                ))
            }) {
                let mut raw_ranks = Vec::new();
                for (c, rank) in rows.flatten() {
                    raw_ranks.push(rank);
                    out.push(c);
                }
                if !out.is_empty() {
                    let max_rank = raw_ranks.iter().copied().fold(1e-6f64, f64::max);
                    let meta: std::collections::HashMap<_, _> = scoped
                        .iter()
                        .map(|f| (f.fact_id.clone(), f.clone()))
                        .collect();
                    for (c, r) in out.iter_mut().zip(raw_ranks.iter()) {
                        c.fts_rank = r / max_rank;
                        if let Some(full) = meta.get(&c.fact_id) {
                            c.summary = full.summary.clone();
                            c.knowledge_kind = full.knowledge_kind.clone();
                            c.lifecycle_status = full.lifecycle_status.clone();
                        }
                    }
                    out.truncate(candidate_limit);
                    return Ok(out);
                }
            }
        }
        // FTS miss or malformed MATCH: deterministic substring fallback within scope.
        let q = query.to_lowercase();
        let mut hits: Vec<retrieval::CandidateFact> = scoped
            .into_iter()
            .filter(|f| f.content.to_lowercase().contains(&q))
            .collect();
        for h in &mut hits {
            h.fts_rank = 1.0;
        }
        hits.truncate(candidate_limit);
        Ok(hits)
    }

    fn scoped_fact_rows(
        &self,
        viewer: &MemberId,
        project: Option<&ProjectId>,
        filter: &RecallFilter,
    ) -> Result<Vec<retrieval::CandidateFact>> {
        if !self.memory_v4_enabled()? {
            let project_clause = if project.is_some() {
                " AND l.project_id = ?2 "
            } else {
                " AND l.project_id IS NULL "
            };
            let sql = format!(
                "SELECT f.fact_id, f.content, f.category, f.tags, f.trust_score, f.hrr_vector
                 FROM memory_links l
                 JOIN memory_facts f ON f.fact_id = l.fact_ref
                 WHERE l.member_id = ?1 AND l.status = 'active' AND l.storage_origin = 'native'
                 {project_clause}"
            );
            let mut stmt = self.store.connection().prepare(&sql)?;
            let rows = if let Some(p) = project {
                stmt.query_map(rusqlite::params![viewer.as_str(), p.as_str()], map_fact_row)?
            } else {
                stmt.query_map([viewer.as_str()], map_fact_row)?
            };
            return rows.collect::<Result<Vec<_>, _>>().map_err(Into::into);
        }

        let mut allowed_ids: Vec<String> = Vec::new();
        let mut stmt = self.store.connection().prepare(
            "SELECT l.link_id, l.fact_ref, l.storage_origin, l.member_id, l.project_id,
                    COALESCE(l.link_scope, 'personal'), l.source, l.status,
                    l.created_at, l.archived_at, l.archived_reason
             FROM memory_links l
             WHERE l.status = 'active' AND l.storage_origin = 'native'",
        )?;
        let rows = stmt.query_map([], parse_link_row_scoped)?;
        for link in rows.flatten() {
            if recall::scope_allowed(
                filter,
                link.link_scope,
                viewer,
                &link.member_id,
                project,
                link.project_id.as_ref(),
            ) {
                allowed_ids.push(link.fact_id);
            }
        }
        allowed_ids.sort();
        allowed_ids.dedup();
        if allowed_ids.is_empty() {
            return Ok(Vec::new());
        }

        let placeholders = allowed_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");
        let lifecycle_clause = if filter.include_superseded {
            "f.lifecycle_status IN ('active', 'superseded')"
        } else {
            "f.lifecycle_status = 'active'"
        };
        let sql = format!(
            "SELECT f.fact_id, f.content, f.category, f.tags, f.trust_score, f.hrr_vector,
                    COALESCE(f.summary, ''), COALESCE(f.knowledge_kind, 'personal_memory'),
                    COALESCE(f.lifecycle_status, 'active')
             FROM memory_facts f
             WHERE f.fact_id IN ({placeholders}) AND {lifecycle_clause}"
        );
        let mut stmt = self.store.connection().prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> = allowed_ids
            .iter()
            .map(|id| id as &dyn rusqlite::ToSql)
            .collect();
        let rows = stmt.query_map(param_refs.as_slice(), map_fact_row_v4)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Curation ledger from #116. Absent means recall is `not_ready`, not an empty hit.
    pub fn curation_ready(&self) -> Result<bool> {
        self.memory_v4_enabled()
    }

    fn memory_v4_enabled(&self) -> Result<bool> {
        let n: i64 = self.store.connection().query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'memory_candidates'",
            [],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    fn count_similar_candidates_for_member(
        &self,
        content: &str,
        member: &MemberId,
    ) -> Result<usize> {
        if !self.memory_v4_enabled()? {
            return Ok(0);
        }
        let needle = content.chars().take(80).collect::<String>().to_lowercase();
        if needle.len() < 20 {
            return Ok(0);
        }
        let mut stmt = self.store.connection().prepare(
            "SELECT content FROM memory_candidates
             WHERE member_id = ?1 AND status IN ('proposed', 'admitted')",
        )?;
        let rows = stmt.query_map([member.as_str()], |row| row.get::<_, String>(0))?;
        let mut count = 0usize;
        for row in rows.flatten() {
            if row.to_lowercase().contains(&needle) {
                count += 1;
            }
        }
        Ok(count)
    }

    fn load_proposed_candidate(&self, candidate_id: &str) -> Result<ProposedCandidate> {
        let mut stmt = self.store.connection().prepare(
            "SELECT candidate_id, member_id, project_id, knowledge_kind, summary, content,
                    source, provenance_json, admission_json
             FROM memory_candidates
             WHERE candidate_id = ?1 AND status = 'proposed'",
        )?;
        stmt.query_row([candidate_id], |row| {
            use std::str::FromStr as _;
            let member: String = row.get(1)?;
            let project: Option<String> = row.get(2)?;
            let kind: String = row.get(3)?;
            Ok(ProposedCandidate {
                member_id: MemberId::from_str(&member).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                project_id: project
                    .as_deref()
                    .map(ProjectId::from_str)
                    .transpose()
                    .map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                kind: KnowledgeKind::parse(&kind),
                summary: row.get(4)?,
                content: row.get(5)?,
                source: row.get(6)?,
                provenance_json: row.get(7)?,
                hints_json: row.get(8)?,
            })
        })
        .map_err(|_| Error::NotFound {
            entity: "proposed memory candidate",
            id: candidate_id.to_string(),
        })
    }

    fn upgrade_fact_kind(&self, fact_id: &str, kind: KnowledgeKind) -> Result<()> {
        if !self.memory_v4_enabled()? {
            return Ok(());
        }
        self.store.connection().execute(
            "UPDATE memory_facts SET knowledge_kind = ?2, updated_at = ?3 WHERE fact_id = ?1",
            rusqlite::params![fact_id, kind.as_str(), utc_now()],
        )?;
        Ok(())
    }

    fn link_scope_rank(scope: LinkScope) -> u8 {
        match scope {
            LinkScope::Personal => 0,
            LinkScope::Project => 1,
            LinkScope::Team => 2,
        }
    }

    fn best_link_for_recall(
        &self,
        fact_id: &str,
        viewer: &MemberId,
        project: Option<&ProjectId>,
        filter: &RecallFilter,
    ) -> Result<Option<MemoryLink>> {
        let mut links = Vec::new();
        if self.memory_v4_enabled()? {
            let mut stmt = self.store.connection().prepare(
                "SELECT link_id, fact_ref, storage_origin, member_id, project_id,
                        COALESCE(link_scope, 'personal'), source, status,
                        created_at, archived_at, archived_reason
                 FROM memory_links
                 WHERE fact_ref = ?1 AND storage_origin = 'native' AND status = 'active'",
            )?;
            for link in stmt.query_map([fact_id], parse_link_row_scoped)? {
                links.push(link?);
            }
        } else {
            let mut stmt = self.store.connection().prepare(
                "SELECT link_id, fact_ref, storage_origin, member_id, project_id, source, status,
                        created_at, archived_at, archived_reason
                 FROM memory_links
                 WHERE fact_ref = ?1 AND storage_origin = 'native' AND status = 'active'",
            )?;
            for link in stmt.query_map([fact_id], parse_link_row)? {
                links.push(link?);
            }
        }
        let mut best: Option<MemoryLink> = None;
        for link in links {
            if !recall::scope_allowed(
                filter,
                link.link_scope,
                viewer,
                &link.member_id,
                project,
                link.project_id.as_ref(),
            ) {
                continue;
            }
            let stop = link.link_scope == LinkScope::Personal && link.member_id == *viewer;
            best = Some(link);
            if stop {
                break;
            }
        }
        Ok(best)
    }

    fn fact_id_for_hash(&self, hash: &str) -> Result<Option<String>> {
        self.store
            .connection()
            .query_row(
                "SELECT fact_id FROM memory_facts WHERE content_hash = ?1",
                [hash],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    fn native_fact_exists(&self, fact_id: &str) -> Result<bool> {
        let n: i64 = self.store.connection().query_row(
            "SELECT COUNT(*) FROM memory_facts WHERE fact_id = ?1",
            [fact_id],
            |row| row.get(0),
        )?;
        Ok(n > 0)
    }

    fn is_legacy_hermes_ref(&self, fact_ref: &str) -> Result<bool> {
        if fact_ref.starts_with("nf-") {
            return Ok(false);
        }
        if fact_ref.chars().all(|c| c.is_ascii_digit()) {
            let n: i64 = self.store.connection().query_row(
                "SELECT COUNT(*) FROM memory_links WHERE fact_ref = ?1 AND storage_origin = 'legacy_hermes'",
                [fact_ref],
                |row| row.get(0),
            )?;
            return Ok(n > 0);
        }
        Ok(false)
    }

    fn ensure_native_link_scoped(
        &self,
        fact_id: String,
        member: &MemberId,
        project: Option<&ProjectId>,
        source: &str,
        link_scope: LinkScope,
        kind: KnowledgeKind,
    ) -> Result<MemoryLink> {
        if let Some(existing) = self.link_for(&fact_id, member)? {
            if existing.storage_origin == StorageOrigin::Native {
                if existing.project_id != project.cloned() {
                    return Err(Error::Validation(
                        "memory link project does not match the existing association".into(),
                    ));
                }
                if existing.link_scope == link_scope {
                    return Ok(existing);
                }
                if Self::link_scope_rank(link_scope) > Self::link_scope_rank(existing.link_scope) {
                    self.store.connection().execute(
                        "UPDATE memory_links SET link_scope = ?3
                         WHERE fact_ref = ?1 AND member_id = ?2 AND storage_origin = 'native'",
                        rusqlite::params![fact_id, member.as_str(), link_scope.as_str(),],
                    )?;
                    self.upgrade_fact_kind(&fact_id, kind)?;
                    return self
                        .link_for(&fact_id, member)?
                        .ok_or_else(|| Error::NotFound {
                            entity: "memory link",
                            id: fact_id.to_string(),
                        });
                }
                return Err(Error::Validation(format!(
                    "memory link scope {:?} cannot be narrowed to {:?}; archive the existing link first",
                    existing.link_scope, link_scope
                )));
            }
        }
        self.insert_native_link_scoped(&fact_id, member, project, source, link_scope)
    }

    fn insert_native_link(
        &self,
        fact_id: &str,
        member: &MemberId,
        project: Option<&ProjectId>,
        source: &str,
    ) -> Result<MemoryLink> {
        self.insert_native_link_scoped(fact_id, member, project, source, LinkScope::Personal)
    }

    fn insert_native_link_scoped(
        &self,
        fact_id: &str,
        member: &MemberId,
        project: Option<&ProjectId>,
        source: &str,
        link_scope: LinkScope,
    ) -> Result<MemoryLink> {
        let link = MemoryLink {
            link_id: format!("mlink-{}", uuid::Uuid::new_v4().simple()),
            fact_id: fact_id.to_string(),
            storage_origin: StorageOrigin::Native,
            member_id: member.clone(),
            project_id: project.cloned(),
            link_scope,
            source: source.to_string(),
            status: LinkStatus::Active,
            created_at: utc_now(),
            archived_at: None,
            archived_reason: None,
        };
        if self.memory_v4_enabled()? {
            self.store.connection().execute(
                "INSERT INTO memory_links(link_id, fact_ref, storage_origin, member_id, project_id,
                    link_scope, source, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    link.link_id,
                    link.fact_id,
                    link.storage_origin.as_str(),
                    link.member_id.as_str(),
                    link.project_id.as_ref().map(|p| p.as_str()),
                    link.link_scope.as_str(),
                    link.source,
                    link.status.as_str(),
                    link.created_at,
                ],
            )?;
        } else {
            self.store.connection().execute(
                "INSERT INTO memory_links(link_id, fact_ref, storage_origin, member_id, project_id,
                                           source, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    link.link_id,
                    link.fact_id,
                    link.storage_origin.as_str(),
                    link.member_id.as_str(),
                    link.project_id.as_ref().map(|p| p.as_str()),
                    link.source,
                    link.status.as_str(),
                    link.created_at,
                ],
            )?;
        }
        Ok(link)
    }

    fn attach_entities(&self, fact_id: &str, entities: &[String], now: &str) -> Result<()> {
        let conn = self.store.connection();
        for name in entities {
            let entity_id: i64 = if let Some(id) = conn
                .query_row(
                    "SELECT entity_id FROM memory_entities WHERE name = ?1",
                    [name],
                    |row| row.get(0),
                )
                .optional()?
            {
                id
            } else {
                conn.execute(
                    "INSERT INTO memory_entities(name, created_at) VALUES (?1, ?2)",
                    rusqlite::params![name, now],
                )?;
                conn.last_insert_rowid()
            };
            conn.execute(
                "INSERT OR IGNORE INTO memory_fact_entities(fact_id, entity_id) VALUES (?1, ?2)",
                rusqlite::params![fact_id, entity_id],
            )?;
        }
        Ok(())
    }
}

struct ProposedCandidate {
    member_id: MemberId,
    project_id: Option<ProjectId>,
    kind: KnowledgeKind,
    summary: String,
    content: String,
    source: String,
    provenance_json: String,
    hints_json: String,
}

fn map_fact_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<retrieval::CandidateFact> {
    Ok(retrieval::CandidateFact {
        fact_id: row.get(0)?,
        content: row.get(1)?,
        category: row.get(2)?,
        tags: row.get(3)?,
        trust_score: row.get(4)?,
        hrr_vector: row.get(5)?,
        fts_rank: 0.0,
        summary: String::new(),
        knowledge_kind: "personal_memory".into(),
        lifecycle_status: "active".into(),
    })
}

fn map_fact_row_v4(row: &rusqlite::Row<'_>) -> rusqlite::Result<retrieval::CandidateFact> {
    Ok(retrieval::CandidateFact {
        fact_id: row.get(0)?,
        content: row.get(1)?,
        category: row.get(2)?,
        tags: row.get(3)?,
        trust_score: row.get(4)?,
        hrr_vector: row.get(5)?,
        fts_rank: 0.0,
        summary: row.get(6)?,
        knowledge_kind: row.get(7)?,
        lifecycle_status: row.get(8)?,
    })
}

fn parse_link_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryLink> {
    parse_link_fields(row, false)
}

fn parse_link_row_scoped(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryLink> {
    parse_link_fields(row, true)
}

fn parse_link_fields(row: &rusqlite::Row<'_>, scoped: bool) -> rusqlite::Result<MemoryLink> {
    use std::str::FromStr as _;
    let member: String = row.get(3)?;
    let project: Option<String> = row.get(4)?;
    let (link_scope, source_idx, status_idx, created_idx) = if scoped {
        (LinkScope::parse(row.get::<_, String>(5)?.as_str()), 6, 7, 8)
    } else {
        (LinkScope::Personal, 5, 6, 7)
    };
    let status: String = row.get(status_idx)?;
    let origin: String = row.get(2)?;
    Ok(MemoryLink {
        link_id: row.get(0)?,
        fact_id: row.get(1)?,
        storage_origin: StorageOrigin::from_str(&origin),
        member_id: MemberId::from_str(&member).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
        })?,
        project_id: project
            .as_deref()
            .map(ProjectId::from_str)
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
        link_scope,
        source: row.get(source_idx)?,
        status: match status.as_str() {
            "active" => LinkStatus::Active,
            "archived" => LinkStatus::Archived,
            other => panic!("memory_links.status holds `{other}`"),
        },
        created_at: row.get(created_idx)?,
        archived_at: row.get(created_idx + 1)?,
        archived_reason: row.get(created_idx + 2)?,
    })
}

use std::str::FromStr as _;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::store::{DOMAIN_AUTHORITY, DOMAIN_FOUNDATION, FOUNDATION_V1_SQL};

    fn mem_store() -> Store {
        let registry = MigrationRegistry::new()
            .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
            .register(DOMAIN_MEMORY, 1, "memory v1", MEMORY_V1_SQL)
            .register(DOMAIN_MEMORY, 2, "memory v2", MEMORY_V2_SQL)
            .register_fn(DOMAIN_MEMORY, 3, "memory v3", migrate::apply_memory_v3)
            .register_fn(DOMAIN_MEMORY, 4, "memory v4", migrate_v4::apply_memory_v4)
            .register(
                DOMAIN_AUTHORITY,
                1,
                "authority v1",
                crate::authority::AUTHORITY_V1_SQL,
            )
            .freeze()
            .expect("registry");
        Store::open_in_memory(&registry).expect("store")
    }

    fn remember_test(service: &MemoryService<'_>, member: &MemberId, content: &str) -> Result<()> {
        let proposed = service.remember_with_options(
            member,
            None,
            content,
            "test",
            "general",
            "",
            KnowledgeKind::PersonalMemory,
            true,
            None,
            None,
        )?;
        service.admit_candidate(&proposed.candidate_id)?;
        Ok(())
    }

    #[test]
    fn legacy_links_survive_marked_hermes() {
        let store = mem_store();
        store
            .connection()
            .execute(
                "INSERT INTO memory_links(link_id, fact_ref, storage_origin, member_id, source, status, created_at)
                 VALUES ('l1', '99', 'legacy_hermes', 'm1', 's', 'active', 't')",
                [],
            )
            .unwrap();
        let n: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM memory_links WHERE storage_origin = 'legacy_hermes'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn filter_before_limit_does_not_drop_scoped_facts() {
        let store = mem_store();
        let service = MemoryService::new(&store);
        let member = MemberId::new();
        for i in 0..5 {
            remember_test(
                &service,
                &member,
                &format!("needle marker fact number {i} for scope test"),
            )
            .unwrap();
        }
        let result = service
            .search(&member, None, "needle marker", 64 * 1024)
            .unwrap();
        let MemorySearch::Fetched { facts, .. } = result else {
            panic!("fetched");
        };
        assert_eq!(facts.len(), 5, "scoped filter must not drop member facts");
    }
}
