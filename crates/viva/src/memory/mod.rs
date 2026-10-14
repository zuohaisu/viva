//! Native memory storage (issue #130): facts, FTS5 trigram, minimal HRR,
//! and Viva's per-member link / usage / exit layer.
//!
//! Legacy Hermes rows stay in `memory_links` as `legacy_hermes` and never
//! participate in retrieval. A thin Python adapter remains for `status`
//! only until the second migration slice (#131).

mod entity;
mod hash;
pub mod hrr;
pub mod migrate;
mod retrieval;

use std::path::PathBuf;
use std::process::Command;

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
}

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
    pub source: String,
    pub status: LinkStatus,
    pub created_at: String,
    pub archived_at: Option<String>,
    pub archived_reason: Option<String>,
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
    pub content: String,
    pub score: f64,
    pub trust: f64,
    pub member_id: MemberId,
    pub project_id: Option<ProjectId>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterConfig {
    pub python: PathBuf,
    pub adapter_script: PathBuf,
    pub agent_dir: PathBuf,
    pub db_path: PathBuf,
}

impl AdapterConfig {
    pub fn from_env() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let env_path = |key: &str, default: &str| -> PathBuf {
            std::env::var(key)
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(default.replace('~', &home)))
        };
        let agent_dir = env_path("VIVA_HERMES_AGENT", "~/.hermes/hermes-agent");
        let python = match std::env::var("VIVA_MEMORY_PYTHON") {
            Ok(pinned) => PathBuf::from(pinned),
            Err(_) => {
                let venv_python = agent_dir.join("venv").join("bin").join("python");
                if venv_python.is_file() {
                    venv_python
                } else {
                    PathBuf::from("python3")
                }
            }
        };
        Self {
            python,
            adapter_script: resolve_adapter_script(),
            agent_dir,
            db_path: std::env::var_os("VIVA_MEMORY_DB")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    if std::env::var("VIVA_HOME").is_ok_and(|v| !v.is_empty()) {
                        crate::foundation::paths::viva_home(None).join("memory_store.db")
                    } else {
                        PathBuf::from(home).join(".hermes/memory_store.db")
                    }
                }),
        }
    }
}

fn resolve_adapter_script() -> PathBuf {
    let relative = PathBuf::from("extensions/pi/memory/memory_adapter.py");
    if let Ok(from_env) = std::env::var("VIVA_MEMORY_ADAPTER") {
        return PathBuf::from(from_env);
    }
    if let Ok(exe) = std::env::current_exe() {
        for dir in exe.ancestors().skip(1) {
            let candidate = dir.join(&relative);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    let dev_checkout = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../extensions/pi/memory/memory_adapter.py");
    if dev_checkout.is_file() {
        return dev_checkout;
    }
    relative
}

pub struct MemoryService<'a> {
    store: &'a Store,
    adapter: AdapterConfig,
}

impl<'a> MemoryService<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self {
            store,
            adapter: AdapterConfig::from_env(),
        }
    }

    pub fn with_adapter(store: &'a Store, adapter: AdapterConfig) -> Self {
        Self { store, adapter }
    }

    pub fn adapter_config(&self) -> &AdapterConfig {
        &self.adapter
    }

    pub fn probe_adapter_status(&self) -> Result<serde_json::Value> {
        let answer = self.run_adapter(json!({ "command": "status" }))?;
        Ok(json!({
            "ok": answer.ok,
            "state": answer.state,
            "error": answer.error,
            "hrr": answer.hrr,
            "facts_total": answer.facts_total,
        }))
    }

    pub fn remember(
        &self,
        member: &MemberId,
        project: Option<&ProjectId>,
        content: &str,
        source: &str,
        category: &str,
        tags: &str,
    ) -> Result<MemoryLink> {
        if content.trim().is_empty() {
            return Err(Error::Validation("memory content must not be empty".into()));
        }
        if source.trim().is_empty() {
            return Err(Error::Validation(
                "memory writes require a source (who wrote this, from which conversation/task)"
                    .into(),
            ));
        }
        let hash = hash::content_hash(content);
        if let Some(existing_id) = self.fact_id_for_hash(&hash)? {
            return self.ensure_native_link(existing_id, member, project, source);
        }
        let fact_id = format!("nf-{}", uuid::Uuid::new_v4().simple());
        let entities = entity::extract_entities(content);
        let vector = hrr::encode_fact(content, &entities, HRR_DIM);
        let blob = hrr::phases_to_bytes(&vector, HRR_DIM);
        let now = utc_now();
        let conn = self.store.connection();
        conn.execute(
            "INSERT INTO memory_facts(fact_id, content, content_hash, category, tags,
                                      trust_score, created_at, updated_at, hrr_vector)
             VALUES (?1, ?2, ?3, ?4, ?5, 1.0, ?6, ?6, ?7)",
            rusqlite::params![fact_id, content, hash, category, tags, now, blob],
        )?;
        self.attach_entities(&fact_id, &entities, &now)?;
        self.insert_native_link(&fact_id, member, project, source)
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
        if query.trim().is_empty() {
            return Err(Error::Validation(
                "memory search query must not be empty".into(),
            ));
        }
        let limit = 25usize;
        let candidates = self.scoped_candidates(viewer, project, query, limit * 3)?;
        let ranked = retrieval::rerank(query, candidates, limit);
        self.pack_recall(viewer, project, query, ranked, max_total_bytes)
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
        let scoped = self.scoped_fact_rows(viewer, project)?;
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
        self.pack_recall(viewer, project, entity, ranked, max_total_bytes)
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
        query: &str,
        ranked: Vec<(retrieval::CandidateFact, f64)>,
        max_total_bytes: usize,
    ) -> Result<MemorySearch> {
        let mut facts = Vec::new();
        let mut used_bytes = 0usize;
        for (hit, score) in ranked {
            let Some(link) = self.link_for(&hit.fact_id, viewer)? else {
                continue;
            };
            if link.status != LinkStatus::Active || link.project_id != project.cloned() {
                continue;
            }
            if link.storage_origin != StorageOrigin::Native {
                continue;
            }
            let size = hit.content.len();
            if used_bytes + size > max_total_bytes {
                continue;
            }
            used_bytes += size;
            self.record_usage(&hit.fact_id, viewer, query)?;
            facts.push(RecalledFact {
                fact_id: hit.fact_id,
                content: hit.content,
                score,
                trust: hit.trust_score,
                member_id: link.member_id,
                project_id: link.project_id,
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
    ) -> Result<Vec<retrieval::CandidateFact>> {
        let scoped = self.scoped_fact_rows(viewer, project)?;
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
                    for (c, r) in out.iter_mut().zip(raw_ranks.iter()) {
                        c.fts_rank = r / max_rank;
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
    ) -> Result<Vec<retrieval::CandidateFact>> {
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
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
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

    fn ensure_native_link(
        &self,
        fact_id: String,
        member: &MemberId,
        project: Option<&ProjectId>,
        source: &str,
    ) -> Result<MemoryLink> {
        if let Some(existing) = self.link_for(&fact_id, member)? {
            if existing.storage_origin == StorageOrigin::Native {
                if existing.project_id != project.cloned() {
                    return Err(Error::Validation(
                        "memory link project does not match the existing association".into(),
                    ));
                }
                return Ok(existing);
            }
        }
        self.insert_native_link(&fact_id, member, project, source)
    }

    fn insert_native_link(
        &self,
        fact_id: &str,
        member: &MemberId,
        project: Option<&ProjectId>,
        source: &str,
    ) -> Result<MemoryLink> {
        let link = MemoryLink {
            link_id: format!("mlink-{}", uuid::Uuid::new_v4().simple()),
            fact_id: fact_id.to_string(),
            storage_origin: StorageOrigin::Native,
            member_id: member.clone(),
            project_id: project.cloned(),
            source: source.to_string(),
            status: LinkStatus::Active,
            created_at: utc_now(),
            archived_at: None,
            archived_reason: None,
        };
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

    fn record_usage(&self, fact_id: &str, member: &MemberId, query: &str) -> Result<()> {
        self.store.connection().execute(
            "INSERT INTO memory_usages(fact_ref, member_id, query, used_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![fact_id, member.as_str(), query, utc_now()],
        )?;
        Ok(())
    }

    fn run_adapter(&self, request: serde_json::Value) -> Result<AdapterAnswer> {
        let script = &self.adapter.adapter_script;
        if !script.is_file() {
            return Ok(unavailable_answer(format!(
                "memory adapter script not found at {}",
                script.display()
            )));
        }
        let command = request
            .get("command")
            .and_then(|c| c.as_str())
            .unwrap_or_default()
            .to_string();
        let args: Vec<String> = vec![
            script.display().to_string(),
            command.clone(),
            "--agent-dir".into(),
            self.adapter.agent_dir.display().to_string(),
            "--db".into(),
            self.adapter.db_path.display().to_string(),
        ];
        if command == "status" {
            // status only
        } else {
            return Err(Error::Validation(format!(
                "native memory no longer invokes adapter command `{command}`"
            )));
        }
        let output = match Command::new(&self.adapter.python).args(&args).output() {
            Ok(output) => output,
            Err(err) => {
                return Ok(unavailable_answer(format!(
                    "memory adapter could not run ({}): {err}",
                    self.adapter.python.display()
                )));
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        match serde_json::from_str::<AdapterAnswer>(stdout.trim()) {
            Ok(answer) => Ok(answer),
            Err(err) => Ok(unavailable_answer(format!(
                "memory adapter answer unparseable: {err}; stdout: {}",
                stdout.chars().take(512).collect::<String>()
            ))),
        }
    }
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
    })
}

fn parse_link_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryLink> {
    use std::str::FromStr as _;
    let member: String = row.get(3)?;
    let project: Option<String> = row.get(4)?;
    let status: String = row.get(6)?;
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
        source: row.get(5)?,
        status: match status.as_str() {
            "active" => LinkStatus::Active,
            "archived" => LinkStatus::Archived,
            other => panic!("memory_links.status holds `{other}`"),
        },
        created_at: row.get(7)?,
        archived_at: row.get(8)?,
        archived_reason: row.get(9)?,
    })
}

#[derive(Debug, Deserialize)]
struct AdapterAnswer {
    ok: bool,
    state: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    hrr: Option<String>,
    #[serde(default)]
    facts_total: Option<i64>,
}

fn unavailable_answer(reason: String) -> AdapterAnswer {
    serde_json::from_value(json!({
        "ok": false,
        "state": "unavailable",
        "error": reason,
    }))
    .expect("static unavailable shape")
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
            service
                .remember(
                    &member,
                    None,
                    &format!("needle marker fact number {i} for scope test"),
                    "src",
                    "general",
                    "",
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
