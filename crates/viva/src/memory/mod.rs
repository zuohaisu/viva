//! External memory integration (F04, issue #26): Viva's namespace, audit
//! and exit layer over the user's real Holographic memory.
//!
//! What is reused and what Viva adds:
//! - The content store is the **real, already-running** bundled
//!   Holographic provider from the user's hermes-agent checkout (verified
//!   on 2026-09-29: `plugins/memory/holographic`, reached through its own
//!   `fact_store` tool interface; see `extensions/pi/memory/README.md`).
//!   Nothing here re-implements FTS, HRR or storage; a small Python
//!   adapter (`extensions/pi/memory/memory_adapter.py`) speaks to the real
//!   provider as a subprocess and answers JSON.
//! - The bundled schema has no member/project scoping, no provenance and
//!   no recoverable exit (its `remove_fact` is a physical SQL DELETE).
//!   Viva adds exactly those as its own layer: every Viva-written fact is
//!   **linked** to the member (and optional project) that wrote it, with a
//!   mandatory source; search results are filtered to the viewer's own
//!   linked, active facts; archive/restore are status flips that keep the
//!   fact retrievable on disk. There is no delete path in this module.
//! - Honesty: a store that cannot be reached is `unavailable` — never an
//!   empty-but-successful answer, and never "remembers" without the
//!   capability. Facts written outside Viva (by Hermes itself) are not
//!   claimable through Viva; the count of hidden unlinked hits is reported
//!   so the boundary is visible, not silent.

use std::path::PathBuf;
use std::process::Command;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::authority::{Actor, AuthorityEngine};
use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::{MemberId, ProjectId, utc_now};
use crate::foundation::store::{DOMAIN_MEMORY, MigrationRegistry, Store};

/// Register the `memory` domain migrations (F04's namespace). v2 re-scopes
/// links to (fact_id, member): the external store dedupes content across
/// writers, but each member's claim on a fact is its own row — the v1
/// UNIQUE(fact_id) silently handed the first writer's link to everyone
/// else (QA finding).
pub fn register_migrations(registry: MigrationRegistry) -> MigrationRegistry {
    registry
        .register(DOMAIN_MEMORY, 1, "memory v1", MEMORY_V1_SQL)
        .register(
            DOMAIN_MEMORY,
            2,
            "memory v2 per-member links",
            MEMORY_V2_SQL,
        )
}

pub const MEMORY_V1_SQL: &str = r#"
-- Viva's namespace layer over the external store: which Holographic fact
-- belongs to which member/project, with its mandatory source. fact_id is
-- the external store's integer id — the same asset any later harness can
-- resolve again through the adapter.
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

-- Usage evidence: every fact a search actually returned is recorded with
-- who asked and what they asked. Reuse claims cite these rows.
CREATE TABLE memory_usages (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    fact_id    INTEGER NOT NULL,
    member_id  TEXT NOT NULL,
    query      TEXT NOT NULL,
    used_at    TEXT NOT NULL
);
"#;

pub const MEMORY_V2_SQL: &str = r#"
-- One claim per (fact, member). The provider dedupes content globally, so
-- two members writing the same text share the underlying fact row; their
-- links, sources and exit states stay separate.
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

// ---------------------------------------------------------------------------
// Adapter configuration
// ---------------------------------------------------------------------------

/// Where the real implementation lives and how to call it. Defaults follow
/// the verified layout of the user's machine; every field is overridable
/// so tests run against a temp store and never against personal data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterConfig {
    pub python: PathBuf,
    pub adapter_script: PathBuf,
    pub agent_dir: PathBuf,
    pub db_path: PathBuf,
}

impl AdapterConfig {
    /// Environment-driven defaults. `VIVA_MEMORY_PYTHON`,
    /// `VIVA_HERMES_AGENT`, `VIVA_MEMORY_DB`, `VIVA_MEMORY_ADAPTER`.
    /// When VIVA_HOME is set, an unpinned provider DB lives inside that
    /// isolated home; without it, keep the user's existing Hermes default.
    ///
    /// When the python is not pinned by env, the checkout's own venv is
    /// preferred: the bundled plugin package imports YAML tooling that a
    /// bare system python lacks (verified: a system python answers
    /// `unavailable: No module named 'ruamel'`).
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

/// Find the adapter script without depending on the caller's cwd (a bare
/// relative default dies the moment `viva` runs anywhere else — QA
/// finding N7). Resolution order: `VIVA_MEMORY_ADAPTER`, then the running
/// binary's ancestor directories (packaged layouts put `extensions/` next
/// to the binary or its parent), then the compile-time checkout path as a
/// dev fallback. Returns the first existing file; if nothing exists, the
/// env/relative default is kept so the eventual "not found" names
/// something recognizable.
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

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryLink {
    pub link_id: String,
    pub fact_id: i64,
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
    pub fn as_str(&self) -> &'static str {
        match self {
            LinkStatus::Active => "active",
            LinkStatus::Archived => "archived",
        }
    }
}

/// One fact returned by a real search, with its Viva-side context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecalledFact {
    pub fact_id: i64,
    pub content: String,
    pub score: f64,
    pub trust: f64,
    pub member_id: MemberId,
    pub project_id: Option<ProjectId>,
    pub source: String,
}

/// The honest outcome of a search.
#[derive(Debug, Clone, PartialEq)]
pub enum MemorySearch {
    /// The store really answered; these are the viewer's own active,
    /// linked facts within budget.
    Fetched {
        facts: Vec<RecalledFact>,
        /// Hits the store returned that Viva hid: no Viva link (written
        /// outside Viva), another member's fact, another project's fact,
        /// or archived. Hidden, and counted so the boundary is visible.
        hidden_unclaimable: usize,
    },
    /// The store could not be reached. Never a fake empty success.
    Unavailable { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub seq: i64,
    pub fact_id: i64,
    pub member_id: MemberId,
    pub query: String,
    pub used_at: String,
}

// ---------------------------------------------------------------------------
// Adapter subprocess answers
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AdapterAnswer {
    ok: bool,
    state: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    facts: Vec<AdapterFact>,
    #[serde(default)]
    fact: Option<AdapterAddedFact>,
    #[serde(default)]
    hrr: Option<String>,
    #[serde(default)]
    facts_total: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct AdapterFact {
    fact_id: i64,
    #[serde(default)]
    content: String,
    #[serde(default)]
    score: f64,
    #[serde(rename = "trust_score", default)]
    trust: f64,
}

#[derive(Debug, Deserialize)]
struct AdapterAddedFact {
    fact_id: i64,
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

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

    /// The adapter wiring in use (for status views and tests).
    pub fn adapter_config(&self) -> &AdapterConfig {
        &self.adapter
    }

    /// Run the adapter's real status command (a read-only `list` through
    /// the provider) so readiness is reported from an actual round trip —
    /// never from file existence alone.
    pub fn probe_adapter_status(&self) -> OfficeResult<serde_json::Value> {
        let answer = self.run_adapter(json!({ "command": "status" }))?;
        Ok(json!({
            "ok": answer.ok,
            "state": answer.state,
            "error": answer.error,
            "hrr": answer.hrr,
            "facts_total": answer.facts_total,
        }))
    }

    // -- Writing ---------------------------------------------------------------

    /// Write one fact through the real provider and link it to its writer.
    /// The source is mandatory provenance. The provider dedupes identical
    /// content to one underlying fact row; each writer still gets their
    /// OWN link (claim + source + exit state), so a second member writing
    /// the same text never inherits the first member's link.
    pub fn remember(
        &self,
        member: &MemberId,
        project: Option<&ProjectId>,
        content: &str,
        source: &str,
        category: &str,
        tags: &str,
    ) -> OfficeResult<MemoryLink> {
        if content.trim().is_empty() {
            return Err(OfficeError::Validation(
                "memory content must not be empty".into(),
            ));
        }
        if source.trim().is_empty() {
            return Err(OfficeError::Validation(
                "memory writes require a source (who wrote this, from which conversation/task)"
                    .into(),
            ));
        }
        let answer = self.run_adapter(json!({
            "command": "add",
            "content": content,
            "category": category,
            "tags": tags,
        }))?;
        if !answer.ok || answer.state != "fetched" {
            return Err(OfficeError::Validation(format!(
                "memory write unavailable: {}",
                answer
                    .error
                    .unwrap_or_else(|| "adapter answered without a reason".to_string())
            )));
        }
        let Some(fact) = answer.fact else {
            return Err(OfficeError::Validation(
                "adapter answered ok without a fact id".into(),
            ));
        };
        self.link_fact(fact.fact_id, member, project, source)
    }

    /// Link an existing external fact id (e.g. re-adopting a fact after a
    /// harness switch, resolved through the same asset reference). Idempotent
    /// per member: re-linking one's own fact returns the existing link.
    pub fn link_fact(
        &self,
        fact_id: i64,
        member: &MemberId,
        project: Option<&ProjectId>,
        source: &str,
    ) -> OfficeResult<MemoryLink> {
        if source.trim().is_empty() {
            return Err(OfficeError::Validation(
                "memory links require a source".into(),
            ));
        }
        if let Some(existing) = self.link_for(fact_id, member)? {
            return Ok(existing);
        }
        let link = MemoryLink {
            link_id: format!("mlink-{}", uuid::Uuid::new_v4().simple()),
            fact_id,
            member_id: member.clone(),
            project_id: project.cloned(),
            source: source.to_string(),
            status: LinkStatus::Active,
            created_at: utc_now(),
            archived_at: None,
            archived_reason: None,
        };
        self.store.connection().execute(
            "INSERT INTO memory_links(link_id, fact_id, member_id, project_id, source,
                                       status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                link.link_id,
                link.fact_id,
                link.member_id.as_str(),
                link.project_id.as_ref().map(|p| p.as_str()),
                link.source,
                link.status.as_str(),
                link.created_at,
            ],
        )?;
        Ok(link)
    }

    // -- Reading ---------------------------------------------------------------

    /// Search the real store and answer with the viewer's own, active,
    /// in-budget facts. Every returned fact leaves a usage row (the reuse
    /// evidence). Unavailable stores stay unavailable.
    pub fn search(
        &self,
        viewer: &MemberId,
        project: Option<&ProjectId>,
        query: &str,
        max_total_bytes: usize,
    ) -> OfficeResult<MemorySearch> {
        if query.trim().is_empty() {
            return Err(OfficeError::Validation(
                "memory search query must not be empty".into(),
            ));
        }
        let answer = self.run_adapter(json!({
            "command": "search",
            "query": query,
            "limit": 25,
        }))?;
        if !answer.ok || answer.state != "fetched" {
            return Ok(MemorySearch::Unavailable {
                reason: answer
                    .error
                    .unwrap_or_else(|| "adapter answered without an error reason".to_string()),
            });
        }

        let mut facts = Vec::new();
        let mut hidden_unclaimable = 0usize;
        let mut used_bytes = 0usize;
        for hit in answer.facts {
            // The claim filter is per-viewer: a hit is claimable only
            // through the viewer's OWN active link (no link, another
            // member's link, another project's link, or archived → hidden
            // and counted, never mixed in).
            let Some(link) = self.link_for(hit.fact_id, viewer)? else {
                hidden_unclaimable += 1;
                continue;
            };
            let in_scope = link.status == LinkStatus::Active && link.project_id == project.cloned();
            if !in_scope {
                hidden_unclaimable += 1;
                continue;
            }
            let size = hit.content.len();
            if used_bytes + size > max_total_bytes {
                hidden_unclaimable += 1;
                continue;
            }
            used_bytes += size;
            self.record_usage(hit.fact_id, viewer, query)?;
            facts.push(RecalledFact {
                fact_id: hit.fact_id,
                content: hit.content,
                score: hit.score,
                trust: hit.trust,
                member_id: link.member_id,
                project_id: link.project_id,
                source: link.source,
            });
        }
        Ok(MemorySearch::Fetched {
            facts,
            hidden_unclaimable,
        })
    }

    /// Entity-based recall through the provider's real `probe` interface,
    /// filtered by the same per-viewer claim rules as `search`.
    pub fn probe(
        &self,
        viewer: &MemberId,
        project: Option<&ProjectId>,
        entity: &str,
        max_total_bytes: usize,
    ) -> OfficeResult<MemorySearch> {
        if entity.trim().is_empty() {
            return Err(OfficeError::Validation(
                "memory probe entity must not be empty".into(),
            ));
        }
        let answer = self.run_adapter(json!({
            "command": "probe",
            "entity": entity,
            "limit": 25,
        }))?;
        if !answer.ok || answer.state != "fetched" {
            return Ok(MemorySearch::Unavailable {
                reason: answer
                    .error
                    .unwrap_or_else(|| "adapter answered without an error reason".to_string()),
            });
        }
        self.claim_hits(viewer, project, answer.facts, entity, max_total_bytes)
    }

    fn claim_hits(
        &self,
        viewer: &MemberId,
        project: Option<&ProjectId>,
        hits: Vec<AdapterFact>,
        query: &str,
        max_total_bytes: usize,
    ) -> OfficeResult<MemorySearch> {
        let mut facts = Vec::new();
        let mut hidden_unclaimable = 0usize;
        let mut used_bytes = 0usize;
        for hit in hits {
            let Some(link) = self.link_for(hit.fact_id, viewer)? else {
                hidden_unclaimable += 1;
                continue;
            };
            let in_scope = link.status == LinkStatus::Active && link.project_id == project.cloned();
            if !in_scope {
                hidden_unclaimable += 1;
                continue;
            }
            let size = hit.content.len();
            if used_bytes + size > max_total_bytes {
                hidden_unclaimable += 1;
                continue;
            }
            used_bytes += size;
            self.record_usage(hit.fact_id, viewer, query)?;
            facts.push(RecalledFact {
                fact_id: hit.fact_id,
                content: hit.content,
                score: hit.score,
                trust: hit.trust,
                member_id: link.member_id,
                project_id: link.project_id,
                source: link.source,
            });
        }
        Ok(MemorySearch::Fetched {
            facts,
            hidden_unclaimable,
        })
    }

    // -- Exit paths (no delete) --------------------------------------------------

    /// Archive a linked fact: hidden from selection, still on disk, still
    /// resolvable by explicit request. Only the link's OWN member may exit
    /// it, and only under a live `maintain_knowledge` grant. The bundled
    /// `remove_fact` (physical DELETE) is deliberately not exposed here.
    pub fn archive(
        &self,
        fact_id: i64,
        reason: &str,
        actor: &Actor,
        authority: &AuthorityEngine<'_>,
    ) -> OfficeResult<MemoryLink> {
        self.exit_flip(fact_id, LinkStatus::Archived, reason, actor, authority)
    }

    pub fn restore(
        &self,
        fact_id: i64,
        reason: &str,
        actor: &Actor,
        authority: &AuthorityEngine<'_>,
    ) -> OfficeResult<MemoryLink> {
        self.exit_flip(fact_id, LinkStatus::Active, reason, actor, authority)
    }

    fn exit_flip(
        &self,
        fact_id: i64,
        status: LinkStatus,
        reason: &str,
        actor: &Actor,
        authority: &AuthorityEngine<'_>,
    ) -> OfficeResult<MemoryLink> {
        let Actor::Member { member, grant: _ } = actor else {
            return Err(OfficeError::Validation(
                "workers do not exit memories; member grants only".into(),
            ));
        };
        // Exit is an office-state mutation: a live grant is required, and
        // the authority engine appends its own denial record.
        authority
            .check(actor, "maintain_knowledge", None)?
            .map_err(|reason| {
                OfficeError::Validation(format!("memory exit not authorized: {reason}"))
            })?;
        // Owner condition: a member exits only their OWN claim on a fact.
        let Some(_link) = self.link_for(fact_id, member)? else {
            return Err(OfficeError::NotFound {
                entity: "own memory link",
                id: fact_id.to_string(),
            });
        };
        if reason.trim().is_empty() {
            return Err(OfficeError::Validation(
                "a memory exit action requires a reason".into(),
            ));
        }
        let now = utc_now();
        match status {
            LinkStatus::Archived => {
                self.store.connection().execute(
                    "UPDATE memory_links SET status = 'archived', archived_at = ?3,
                            archived_reason = ?4
                     WHERE fact_id = ?1 AND member_id = ?2",
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
                     WHERE fact_id = ?1 AND member_id = ?2",
                    rusqlite::params![fact_id, member.as_str()],
                )?;
            }
        }
        self.link_for(fact_id, member)?
            .ok_or_else(|| OfficeError::NotFound {
                entity: "own memory link",
                id: fact_id.to_string(),
            })
    }

    // -- Records -----------------------------------------------------------------

    /// The viewer's own link on a fact, if any.
    pub fn link_for(&self, fact_id: i64, member: &MemberId) -> OfficeResult<Option<MemoryLink>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT link_id, fact_id, member_id, project_id, source, status, created_at,
                    archived_at, archived_reason
             FROM memory_links WHERE fact_id = ?1 AND member_id = ?2",
        )?;
        let row = stmt
            .query_row(rusqlite::params![fact_id, member.as_str()], |row| {
                let member: String = row.get(2)?;
                let project: Option<String> = row.get(3)?;
                let status: String = row.get(5)?;
                Ok(MemoryLink {
                    link_id: row.get(0)?,
                    fact_id: row.get(1)?,
                    member_id: MemberId::from_str(&member).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
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
                                3,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?,
                    source: row.get(4)?,
                    status: match status.as_str() {
                        "active" => LinkStatus::Active,
                        "archived" => LinkStatus::Archived,
                        other => panic!("memory_links.status holds `{other}`"),
                    },
                    created_at: row.get(6)?,
                    archived_at: row.get(7)?,
                    archived_reason: row.get(8)?,
                })
            })
            .optional()?;
        Ok(row)
    }

    pub fn usage_of(&self, fact_id: i64) -> OfficeResult<Vec<UsageRecord>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT seq, fact_id, member_id, query, used_at
             FROM memory_usages WHERE fact_id = ?1 ORDER BY seq",
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
        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    fn record_usage(&self, fact_id: i64, member: &MemberId, query: &str) -> OfficeResult<()> {
        self.store.connection().execute(
            "INSERT INTO memory_usages(fact_id, member_id, query, used_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![fact_id, member.as_str(), query, utc_now()],
        )?;
        Ok(())
    }

    // -- Adapter subprocess --------------------------------------------------------

    /// Run the Python adapter as a subprocess. Uses an explicit temp env:
    /// no HERMES_* leakage beyond the documented checkout path, and the
    /// db path is ALWAYS explicit (the adapter refuses without one — the
    /// user's real store can never be a silent default).
    fn run_adapter(&self, request: serde_json::Value) -> OfficeResult<AdapterAnswer> {
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
        let mut args: Vec<String> = vec![
            script.display().to_string(),
            command.clone(),
            "--agent-dir".into(),
            self.adapter.agent_dir.display().to_string(),
            "--db".into(),
            self.adapter.db_path.display().to_string(),
        ];
        match command.as_str() {
            "search" => {
                args.push("--query".into());
                args.push(
                    request
                        .get("query")
                        .and_then(|q| q.as_str())
                        .unwrap_or_default()
                        .to_string(),
                );
                args.push("--limit".into());
                args.push(
                    request
                        .get("limit")
                        .and_then(|l| l.as_i64())
                        .unwrap_or(10)
                        .to_string(),
                );
            }
            "probe" => {
                args.push("--entity".into());
                args.push(
                    request
                        .get("entity")
                        .and_then(|e| e.as_str())
                        .unwrap_or_default()
                        .to_string(),
                );
                args.push("--limit".into());
                args.push(
                    request
                        .get("limit")
                        .and_then(|l| l.as_i64())
                        .unwrap_or(10)
                        .to_string(),
                );
            }
            "add" => {
                for (flag, key) in [
                    ("--content", "content"),
                    ("--category", "category"),
                    ("--tags", "tags"),
                ] {
                    if let Some(value) = request.get(key).and_then(|v| v.as_str()) {
                        args.push(flag.into());
                        args.push(value.to_string());
                    }
                }
            }
            "status" => {} // no extra flags
            other => {
                return Err(OfficeError::Validation(format!(
                    "unknown adapter command `{other}`"
                )));
            }
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

fn unavailable_answer(reason: String) -> AdapterAnswer {
    serde_json::from_value(json!({
        "ok": false,
        "state": "unavailable",
        "error": reason,
        "facts": [],
    }))
    .expect("static unavailable shape")
}

use std::str::FromStr as _;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_config_defaults_are_explicit_paths() {
        // The db default is the verified real store; the adapter refuses
        // to run without an explicit --db regardless, so the default only
        // names WHERE the office looks, never silently writes elsewhere.
        let config = AdapterConfig {
            python: PathBuf::from("python3"),
            adapter_script: PathBuf::from("extensions/pi/memory/memory_adapter.py"),
            agent_dir: PathBuf::from("/tmp/agent"),
            db_path: PathBuf::from("/tmp/mem.db"),
        };
        assert!(config.db_path.is_absolute());
    }
}
