//! Knowledge and skills: ownership, provenance, selection and exit
//! mechanisms (V11, issue #20).
//!
//! Semantics:
//! - Knowledge entries require provenance — no source, no entry. Entries
//!   are scoped personal / project / team / skill; a personal entry never
//!   enters another member's task context.
//! - SKILL.md (and any body file) is the **content authority**; SQLite
//!   keeps only the index, status and references. Bodies live in ordinary
//!   files and are never duplicated into the database.
//! - Exit paths (archive / withdraw / supersede) are appended lifecycle
//!   events with reasons; history and the body file survive. Archived or
//!   withdrawn entries leave the default selection but remain findable by
//!   explicit request.
//! - Raw events never become memories here: "reuse" is reported only when
//!   a real execution's usage row exists. Nothing in this module writes
//!   "learned" automatically.
//! - External memory references (e.g. Holographic handles) may be
//!   unresolved: they are stored with an explicit `unavailable` state and
//!   surfaced as such — never as content.

use std::path::PathBuf;
use std::str::FromStr as _;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::{ExecutionId, MemberId, ProjectId, utc_now};
use crate::foundation::store::{DOMAIN_KNOWLEDGE, MigrationRegistry, Store};

/// Register the `knowledge` domain migrations (V11's namespace).
pub fn register_migrations(registry: MigrationRegistry) -> MigrationRegistry {
    registry.register(DOMAIN_KNOWLEDGE, 1, "knowledge v1", KNOWLEDGE_V1_SQL)
}

pub const KNOWLEDGE_V1_SQL: &str = r#"
CREATE TABLE knowledge_items (
    item_id        TEXT PRIMARY KEY,
    title          TEXT NOT NULL CHECK (length(trim(title)) > 0),
    scope          TEXT NOT NULL CHECK (scope IN ('personal', 'project', 'team', 'skill')),
    owner_member_id TEXT,
    project_id     TEXT,
    tags_json      TEXT NOT NULL DEFAULT '[]',
    body_path      TEXT NOT NULL,
    provenance     TEXT NOT NULL CHECK (length(trim(provenance)) > 0),
    status         TEXT NOT NULL DEFAULT 'active'
                   CHECK (status IN ('active', 'archived', 'withdrawn', 'superseded')),
    superseded_by  TEXT REFERENCES knowledge_items(item_id),
    external_ref   TEXT,
    external_state TEXT NOT NULL DEFAULT 'none'
                   CHECK (external_state IN ('none', 'unavailable', 'linked', 'unresolvable')),
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    CHECK (scope <> 'personal' OR owner_member_id IS NOT NULL),
    CHECK ((status = 'superseded') = (superseded_by IS NOT NULL))
);

-- Append-only lifecycle ledger: every archive/withdraw/restore/correct
-- keeps its reason and actor. Nothing is ever deleted here.
CREATE TABLE knowledge_lifecycle (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    item_id     TEXT NOT NULL REFERENCES knowledge_items(item_id),
    action      TEXT NOT NULL CHECK (action IN ('archived', 'withdrawn', 'restored', 'corrected', 'superseded')),
    reason      TEXT NOT NULL,
    actor       TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);

-- Usage evidence: reuse is claimed only through rows appended here by a
-- real execution's report. No other code path may insert them.
CREATE TABLE knowledge_usage (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    item_id      TEXT NOT NULL REFERENCES knowledge_items(item_id),
    execution_id TEXT NOT NULL,
    used_at      TEXT NOT NULL,
    note         TEXT NOT NULL DEFAULT ''
);

-- Skill registrations. `entry_path` points at SKILL.md — the content
-- authority on disk; enablement is office state only.
CREATE TABLE skills (
    skill_id      TEXT PRIMARY KEY,
    item_id       TEXT NOT NULL REFERENCES knowledge_items(item_id),
    entry_path    TEXT NOT NULL,
    enabled       INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    registered_at TEXT NOT NULL,
    disabled_at   TEXT
);
"#;

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Personal,
    Project,
    Team,
    Skill,
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Personal => "personal",
            Scope::Project => "project",
            Scope::Team => "team",
            Scope::Skill => "skill",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeStatus {
    Active,
    Archived,
    Withdrawn,
    Superseded,
}

impl KnowledgeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            KnowledgeStatus::Active => "active",
            KnowledgeStatus::Archived => "archived",
            KnowledgeStatus::Withdrawn => "withdrawn",
            KnowledgeStatus::Superseded => "superseded",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalState {
    None,
    /// An external memory handle is recorded but not verified/resolvable —
    /// surfaced honestly as unavailable, never as content.
    Unavailable,
    Linked,
    Unresolvable,
}

impl ExternalState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExternalState::None => "none",
            ExternalState::Unavailable => "unavailable",
            ExternalState::Linked => "linked",
            ExternalState::Unresolvable => "unresolvable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeItem {
    pub item_id: String,
    pub title: String,
    pub scope: Scope,
    pub owner_member_id: Option<MemberId>,
    pub project_id: Option<ProjectId>,
    pub tags: Vec<String>,
    /// Path of the authoritative body file (ordinary file, not the DB).
    pub body_path: PathBuf,
    /// Where this knowledge came from. Required — no provenance, no entry.
    pub provenance: String,
    pub status: KnowledgeStatus,
    pub superseded_by: Option<String>,
    pub external_ref: Option<String>,
    pub external_state: ExternalState,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LifecycleEvent {
    pub seq: i64,
    pub item_id: String,
    pub action: String,
    pub reason: String,
    pub actor: String,
    pub recorded_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub seq: i64,
    pub item_id: String,
    pub execution_id: ExecutionId,
    pub used_at: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillRegistration {
    pub skill_id: String,
    pub item_id: String,
    pub entry_path: PathBuf,
    pub enabled: bool,
    pub registered_at: String,
    pub disabled_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

/// Whose context knowledge is being selected for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionContext {
    /// The member whose task/context is asking (personal entries of other
    /// members never match).
    pub viewer_member_id: MemberId,
    pub project_id: Option<ProjectId>,
}

/// One selected entry as handed to a task brief or agent context. When the
/// body is unresolved (external reference), `body` is None and the reason
/// travels along.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectedKnowledge {
    pub item_id: String,
    pub title: String,
    pub scope: Scope,
    pub body: Option<String>,
    pub body_unavailable_reason: Option<String>,
    pub external_ref: Option<String>,
    pub external_state: ExternalState,
    pub provenance: String,
    pub body_path: PathBuf,
}

impl SelectedKnowledge {
    pub fn bytes(&self) -> usize {
        self.body.as_deref().map(str::len).unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionOptions {
    /// Maximum total body bytes returned. Selection stops before the
    /// budget is exceeded — related material is chosen, never full-loaded.
    pub max_total_bytes: usize,
    /// Explicitly include archived/withdrawn/superseded entries.
    pub include_inactive: bool,
}

impl Default for SelectionOptions {
    fn default() -> Self {
        Self {
            max_total_bytes: 8 * 1024,
            include_inactive: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

pub struct KnowledgeRegistry<'a> {
    store: &'a Store,
}

impl<'a> KnowledgeRegistry<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// Record a knowledge entry. Provenance is mandatory: where it came
    /// from, who wrote it, which conversation/source produced it.
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        title: impl Into<String>,
        scope: Scope,
        body_path: impl Into<PathBuf>,
        provenance: impl Into<String>,
        owner_member_id: Option<MemberId>,
        project_id: Option<ProjectId>,
        tags: Vec<String>,
        external_ref: Option<String>,
        external_state: ExternalState,
    ) -> OfficeResult<KnowledgeItem> {
        let title = title.into();
        let provenance = provenance.into();
        let body_path = body_path.into();
        if title.trim().is_empty() {
            return Err(OfficeError::Validation(
                "knowledge title must not be empty".into(),
            ));
        }
        if provenance.trim().is_empty() {
            return Err(OfficeError::Validation(
                "knowledge requires provenance (source/origin); an entry without provenance is rejected".into(),
            ));
        }
        if !body_path.is_absolute() {
            return Err(OfficeError::Validation(format!(
                "knowledge body path must be absolute: {}",
                body_path.display()
            )));
        }
        if scope == Scope::Personal && owner_member_id.is_none() {
            return Err(OfficeError::Validation(
                "personal knowledge must name its owner member".into(),
            ));
        }
        if external_state == ExternalState::Unavailable && external_ref.is_none() {
            return Err(OfficeError::Validation(
                "an unavailable external reference must record what the reference is".into(),
            ));
        }
        let now = utc_now();
        let item = KnowledgeItem {
            item_id: format!("know-{}", uuid::Uuid::new_v4().simple()),
            title,
            scope,
            owner_member_id,
            project_id,
            tags,
            body_path,
            provenance,
            status: KnowledgeStatus::Active,
            superseded_by: None,
            external_ref,
            external_state,
            created_at: now.clone(),
            updated_at: now,
        };
        self.store.connection().execute(
            "INSERT INTO knowledge_items(item_id, title, scope, owner_member_id, project_id,
                                         tags_json, body_path, provenance, status,
                                         superseded_by, external_ref, external_state,
                                         created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![
                item.item_id,
                item.title,
                item.scope.as_str(),
                item.owner_member_id.as_ref().map(|m| m.as_str()),
                item.project_id.as_ref().map(|p| p.as_str()),
                serde_json::to_string(&item.tags)?,
                item.body_path.to_string_lossy().as_ref(),
                item.provenance,
                item.status.as_str(),
                item.superseded_by,
                item.external_ref,
                item.external_state.as_str(),
                item.created_at,
                item.updated_at,
            ],
        )?;
        Ok(item)
    }

    /// Append-only exit actions. Every action carries a reason and actor;
    /// the body file is never touched.
    pub fn archive(
        &self,
        item_id: &str,
        reason: impl Into<String>,
        actor: impl Into<String>,
    ) -> OfficeResult<LifecycleEvent> {
        self.exit_action(
            item_id,
            "archived",
            KnowledgeStatus::Archived,
            None,
            reason,
            actor,
        )
    }

    pub fn withdraw(
        &self,
        item_id: &str,
        reason: impl Into<String>,
        actor: impl Into<String>,
    ) -> OfficeResult<LifecycleEvent> {
        self.exit_action(
            item_id,
            "withdrawn",
            KnowledgeStatus::Withdrawn,
            None,
            reason,
            actor,
        )
    }

    /// Supersede one entry by another (correction chain preserved).
    pub fn supersede(
        &self,
        item_id: &str,
        by_item_id: &str,
        reason: impl Into<String>,
        actor: impl Into<String>,
    ) -> OfficeResult<LifecycleEvent> {
        self.exit_action(
            item_id,
            "superseded",
            KnowledgeStatus::Superseded,
            Some(by_item_id),
            reason,
            actor,
        )
    }

    fn exit_action(
        &self,
        item_id: &str,
        action: &str,
        new_status: KnowledgeStatus,
        superseded_by: Option<&str>,
        reason: impl Into<String>,
        actor: impl Into<String>,
    ) -> OfficeResult<LifecycleEvent> {
        self.require_item(item_id)?;
        let reason = reason.into();
        let actor = actor.into();
        if reason.trim().is_empty() {
            return Err(OfficeError::Validation(
                "a lifecycle action requires a reason".into(),
            ));
        }
        let now = utc_now();
        let tx = self.store.transaction()?;
        match superseded_by {
            Some(by) => {
                tx.execute(
                    "UPDATE knowledge_items SET status = ?2, superseded_by = ?3, updated_at = ?4
                     WHERE item_id = ?1",
                    rusqlite::params![item_id, new_status.as_str(), by, now],
                )?;
            }
            None => {
                tx.execute(
                    "UPDATE knowledge_items SET status = ?2, updated_at = ?3 WHERE item_id = ?1",
                    rusqlite::params![item_id, new_status.as_str(), now],
                )?;
            }
        }
        tx.execute(
            "INSERT INTO knowledge_lifecycle(item_id, action, reason, actor, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![item_id, action, reason, actor, now],
        )?;
        tx.commit()?;
        Ok(LifecycleEvent {
            seq: self.store.connection().last_insert_rowid(),
            item_id: item_id.to_string(),
            action: action.to_string(),
            reason,
            actor,
            recorded_at: now,
        })
    }

    /// Bring an archived entry back (explicit request; recorded).
    pub fn restore(
        &self,
        item_id: &str,
        reason: impl Into<String>,
        actor: impl Into<String>,
    ) -> OfficeResult<LifecycleEvent> {
        self.require_item(item_id)?;
        let reason = reason.into();
        let actor = actor.into();
        let now = utc_now();
        let tx = self.store.transaction()?;
        tx.execute(
            "UPDATE knowledge_items SET status = 'active', updated_at = ?2 WHERE item_id = ?1",
            rusqlite::params![item_id, now],
        )?;
        tx.execute(
            "INSERT INTO knowledge_lifecycle(item_id, action, reason, actor, recorded_at)
             VALUES (?1, 'restored', ?2, ?3, ?4)",
            rusqlite::params![item_id, reason, actor, now],
        )?;
        tx.commit()?;
        Ok(LifecycleEvent {
            seq: self.store.connection().last_insert_rowid(),
            item_id: item_id.to_string(),
            action: "restored".into(),
            reason,
            actor,
            recorded_at: now,
        })
    }

    pub fn lifecycle(&self, item_id: &str) -> OfficeResult<Vec<LifecycleEvent>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT seq, item_id, action, reason, actor, recorded_at
             FROM knowledge_lifecycle WHERE item_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([item_id], |row| {
            Ok(LifecycleEvent {
                seq: row.get(0)?,
                item_id: row.get(1)?,
                action: row.get(2)?,
                reason: row.get(3)?,
                actor: row.get(4)?,
                recorded_at: row.get(5)?,
            })
        })?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    fn require_item(&self, item_id: &str) -> OfficeResult<KnowledgeItem> {
        self.get_item(item_id)?
            .ok_or_else(|| OfficeError::NotFound {
                entity: "knowledge item",
                id: item_id.to_string(),
            })
    }

    pub fn get_item(&self, item_id: &str) -> OfficeResult<Option<KnowledgeItem>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT item_id, title, scope, owner_member_id, project_id, tags_json, body_path,
                    provenance, status, superseded_by, external_ref, external_state,
                    created_at, updated_at
             FROM knowledge_items WHERE item_id = ?1",
        )?;
        let row = stmt.query_row([item_id], map_item).optional()?;
        Ok(row)
    }

    // -- Selection -------------------------------------------------------------

    /// Select knowledge for a task context: minimal relevant subset within
    /// a byte budget, scope-filtered (personal entries never leak across
    /// members), active-only unless explicitly requested otherwise.
    ///
    /// Project-scope semantics (QA round裁定, owner review pending): a
    /// project entry appears ONLY when the context names that project. A
    /// context without a project sees no project entries at all — a task
    /// outside any project never inherits another project's knowledge.
    ///
    /// Term matching scores title + tags; bodies are read from their
    /// authoritative files only for the chosen entries.
    pub fn select_for_context(
        &self,
        context: &SelectionContext,
        terms: &[&str],
        options: SelectionOptions,
    ) -> OfficeResult<Vec<SelectedKnowledge>> {
        let status_filter = if options.include_inactive {
            "1 = 1"
        } else {
            "status = 'active'"
        };
        let mut stmt = self.store.connection().prepare(&format!(
            "SELECT item_id, title, scope, owner_member_id, project_id, tags_json, body_path,
                    provenance, status, superseded_by, external_ref, external_state,
                    created_at, updated_at
             FROM knowledge_items
             WHERE {status_filter}
               AND (scope = 'team'
                    OR (scope = 'personal' AND owner_member_id = ?1)
                    OR (scope = 'project' AND project_id IS NOT NULL AND project_id = ?2)
                    OR scope = 'skill')
             ORDER BY created_at, item_id"
        ))?;
        let rows = stmt.query_map(
            rusqlite::params![
                context.viewer_member_id.as_str(),
                context.project_id.as_ref().map(|p| p.as_str()),
            ],
            map_item,
        )?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }

        // Score: term hits in title (weight 3) or tags (weight 2). Entries
        // with no query terms keep a neutral order.
        let mut scored: Vec<(usize, KnowledgeItem)> = items
            .into_iter()
            .map(|item| {
                let title = item.title.to_lowercase();
                let tags = item
                    .tags
                    .iter()
                    .map(|t| t.to_lowercase())
                    .collect::<Vec<_>>();
                let score = terms
                    .iter()
                    .map(|term| {
                        let term = term.to_lowercase();
                        let mut hits = 0;
                        if title.contains(&term) {
                            hits += 3;
                        }
                        if tags.iter().any(|tag| tag.contains(&term)) {
                            hits += 2;
                        }
                        hits
                    })
                    .sum();
                (score, item)
            })
            .collect();
        scored.sort_by_key(|(score, _item)| std::cmp::Reverse(*score));

        // Fill the budget with the best entries. The budget is a HARD
        // constraint: an entry that does not fit — including a single
        // oversized one — is never loaded, even if that leaves the result
        // empty (QA round Q4).
        let mut selected = Vec::new();
        let mut used = 0usize;
        for (_score, item) in scored {
            let entry = self.read_body_for(item)?;
            let size = entry.bytes();
            if used + size > options.max_total_bytes {
                continue;
            }
            used += size;
            selected.push(entry);
        }
        Ok(selected)
    }

    fn read_body_for(&self, item: KnowledgeItem) -> OfficeResult<SelectedKnowledge> {
        // External references that were never verified stay unavailable —
        // the office says so instead of inventing content.
        if item.external_state == ExternalState::Unavailable {
            return Ok(SelectedKnowledge {
                item_id: item.item_id,
                title: item.title,
                scope: item.scope,
                body: None,
                body_unavailable_reason: Some(format!(
                    "external memory reference not verified/resolvable: {}",
                    item.external_ref.clone().unwrap_or_default()
                )),
                external_ref: item.external_ref.clone(),
                external_state: item.external_state,
                provenance: item.provenance,
                body_path: item.body_path,
            });
        }
        let body = match std::fs::read_to_string(&item.body_path) {
            Ok(text) => Some(text),
            Err(err) => {
                return Ok(SelectedKnowledge {
                    item_id: item.item_id,
                    title: item.title,
                    scope: item.scope,
                    body: None,
                    body_unavailable_reason: Some(format!("body file unreadable: {err}")),
                    external_ref: item.external_ref.clone(),
                    external_state: item.external_state,
                    provenance: item.provenance,
                    body_path: item.body_path,
                });
            }
        };
        Ok(SelectedKnowledge {
            item_id: item.item_id,
            title: item.title,
            scope: item.scope,
            body,
            body_unavailable_reason: None,
            external_ref: item.external_ref.clone(),
            external_state: item.external_state,
            provenance: item.provenance,
            body_path: item.body_path,
        })
    }

    // -- Usage evidence ----------------------------------------------------------

    /// Record that a real execution actually used an entry. This is the
    /// only path that may append usage rows; reuse claims cite them.
    pub fn record_usage(
        &self,
        item_id: &str,
        execution_id: &ExecutionId,
        note: impl Into<String>,
    ) -> OfficeResult<UsageRecord> {
        self.require_item(item_id)?;
        let note = note.into();
        let now = utc_now();
        self.store.connection().execute(
            "INSERT INTO knowledge_usage(item_id, execution_id, used_at, note)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![item_id, execution_id.as_str(), now, note],
        )?;
        Ok(UsageRecord {
            seq: self.store.connection().last_insert_rowid(),
            item_id: item_id.to_string(),
            execution_id: execution_id.clone(),
            used_at: now,
            note,
        })
    }

    /// Usage evidence for an entry — the only honest basis for a reuse
    /// claim.
    pub fn usage_of(&self, item_id: &str) -> OfficeResult<Vec<UsageRecord>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT seq, item_id, execution_id, used_at, note
             FROM knowledge_usage WHERE item_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([item_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                ExecutionId::from_str(&row.get::<_, String>(2)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut records = Vec::new();
        for row in rows {
            let (seq, item_id, execution_id, used_at, note) = row?;
            records.push(UsageRecord {
                seq,
                item_id,
                execution_id,
                used_at,
                note,
            });
        }
        Ok(records)
    }

    // -- Skills -------------------------------------------------------------------

    /// Register a skill whose SKILL.md is the content authority. The file
    /// is read to verify it exists; it is never moved, rewritten or copied.
    pub fn register_skill(
        &self,
        title: impl Into<String>,
        entry_path: impl Into<PathBuf>,
        provenance: impl Into<String>,
        owner: Option<MemberId>,
        tags: Vec<String>,
    ) -> OfficeResult<SkillRegistration> {
        let entry_path = entry_path.into();
        if !entry_path.is_absolute() {
            return Err(OfficeError::Validation(format!(
                "skill entry path must be absolute: {}",
                entry_path.display()
            )));
        }
        if !entry_path.is_file() {
            return Err(OfficeError::Validation(format!(
                "skill entry file does not exist: {}",
                entry_path.display()
            )));
        }
        let item = self.record(
            title,
            Scope::Skill,
            entry_path.clone(),
            provenance,
            owner,
            None,
            tags,
            None,
            ExternalState::None,
        )?;
        let registration = SkillRegistration {
            skill_id: format!("skill-{}", uuid::Uuid::new_v4().simple()),
            item_id: item.item_id,
            entry_path,
            enabled: true,
            registered_at: utc_now(),
            disabled_at: None,
        };
        self.store.connection().execute(
            "INSERT INTO skills(skill_id, item_id, entry_path, enabled, registered_at, disabled_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                registration.skill_id,
                registration.item_id,
                registration.entry_path.to_string_lossy().as_ref(),
                registration.enabled as i64,
                registration.registered_at,
                registration.disabled_at,
            ],
        )?;
        Ok(registration)
    }

    /// Disable (or re-enable) a skill. Office state only — the SKILL.md
    /// file on disk is untouched.
    pub fn set_skill_enabled(&self, skill_id: &str, enabled: bool) -> OfficeResult<()> {
        let n = self.store.connection().execute(
            "UPDATE skills SET enabled = ?2, disabled_at = ?3 WHERE skill_id = ?1",
            rusqlite::params![
                skill_id,
                enabled as i64,
                if enabled { None } else { Some(utc_now()) }
            ],
        )?;
        if n == 0 {
            return Err(OfficeError::NotFound {
                entity: "skill",
                id: skill_id.to_string(),
            });
        }
        Ok(())
    }

    pub fn skill(&self, skill_id: &str) -> OfficeResult<Option<SkillRegistration>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT skill_id, item_id, entry_path, enabled, registered_at, disabled_at
             FROM skills WHERE skill_id = ?1",
        )?;
        let row = stmt.query_row([skill_id], map_skill).optional()?;
        Ok(row)
    }

    pub fn list_skills(&self, enabled_only: bool) -> OfficeResult<Vec<SkillRegistration>> {
        let sql = format!(
            "SELECT skill_id, item_id, entry_path, enabled, registered_at, disabled_at
             FROM skills {} ORDER BY registered_at",
            if enabled_only {
                "WHERE enabled = 1"
            } else {
                ""
            }
        );
        let mut stmt = self.store.connection().prepare(&sql)?;
        let rows = stmt.query_map([], map_skill)?;
        let mut skills = Vec::new();
        for row in rows {
            skills.push(row?);
        }
        Ok(skills)
    }

    // -- Export --------------------------------------------------------------------

    /// Export the domain for backup: entries with provenance, lifecycle
    /// history and usage evidence; bodies are read from their authoritative
    /// files (missing bodies exported as absent, honestly).
    pub fn export_json(&self) -> OfficeResult<serde_json::Value> {
        let mut stmt = self.store.connection().prepare(
            "SELECT item_id, title, scope, owner_member_id, project_id, tags_json, body_path,
                    provenance, status, superseded_by, external_ref, external_state,
                    created_at, updated_at
             FROM knowledge_items ORDER BY created_at",
        )?;
        let items: Vec<KnowledgeItem> = stmt
            .query_map([], map_item)?
            .collect::<Result<Vec<_>, _>>()?;
        let mut entries = Vec::new();
        for item in items {
            let body = std::fs::read_to_string(&item.body_path).ok();
            entries.push(json!({
                "item": item,
                "body_present": body.is_some(),
                "body": body,
                "lifecycle": self.lifecycle(&item.item_id)?,
                "usage": self.usage_of(&item.item_id)?,
            }));
        }
        Ok(json!({
            "entries": entries,
            "skills": self.list_skills(false)?,
        }))
    }
}

fn map_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<KnowledgeItem> {
    let scope: String = row.get(2)?;
    let status: String = row.get(8)?;
    let external_state: String = row.get(11)?;
    Ok(KnowledgeItem {
        item_id: row.get(0)?,
        title: row.get(1)?,
        scope: match scope.as_str() {
            "personal" => Scope::Personal,
            "project" => Scope::Project,
            "team" => Scope::Team,
            "skill" => Scope::Skill,
            other => panic!("knowledge_items.scope holds an unknown value `{other}`"),
        },
        owner_member_id: row
            .get::<_, Option<String>>(3)?
            .as_deref()
            .and_then(|m| MemberId::from_str(m).ok()),
        project_id: row
            .get::<_, Option<String>>(4)?
            .as_deref()
            .and_then(|p| ProjectId::from_str(p).ok()),
        tags: serde_json::from_str(&row.get::<_, String>(5)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
        })?,
        body_path: PathBuf::from(row.get::<_, String>(6)?),
        provenance: row.get(7)?,
        status: match status.as_str() {
            "active" => KnowledgeStatus::Active,
            "archived" => KnowledgeStatus::Archived,
            "withdrawn" => KnowledgeStatus::Withdrawn,
            "superseded" => KnowledgeStatus::Superseded,
            other => panic!("knowledge_items.status holds an unknown value `{other}`"),
        },
        superseded_by: row.get(9)?,
        external_ref: row.get(10)?,
        external_state: match external_state.as_str() {
            "none" => ExternalState::None,
            "unavailable" => ExternalState::Unavailable,
            "linked" => ExternalState::Linked,
            "unresolvable" => ExternalState::Unresolvable,
            other => panic!("knowledge_items.external_state holds an unknown value `{other}`"),
        },
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}

fn map_skill(row: &rusqlite::Row<'_>) -> rusqlite::Result<SkillRegistration> {
    Ok(SkillRegistration {
        skill_id: row.get(0)?,
        item_id: row.get(1)?,
        entry_path: PathBuf::from(row.get::<_, String>(2)?),
        enabled: row.get::<_, i64>(3)? != 0,
        registered_at: row.get(4)?,
        disabled_at: row.get(5)?,
    })
}
