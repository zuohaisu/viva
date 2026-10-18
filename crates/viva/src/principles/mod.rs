//! Member principles: owner approval, versions, and rollback (issue #120).
//!
//! A principle is a member-scoped rule ("how I should work"). Digest only
//! summarizes experience excerpts into candidates. Nothing here is approved
//! until the owner runs `approve`. There is no scheduler and no model call.
//!
//! Pending candidates that have waited 4 weeks (28 days) expire on the next
//! digest, including when Viva was stopped for longer than that. Expired
//! candidates are not approved text.
//!
//! Approved bodies live in this domain's tables. This module never writes
//! `knowledge_items` or a `self_model_candidate` — a principle is not a
//! hypothesis about the member.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use unicode_normalization::UnicodeNormalization;

use crate::foundation::error::{Error, Result};
use crate::foundation::events::{self, NewEvent};
use crate::foundation::ids::{ExecutionId, MemberId, utc_now};
use crate::foundation::store::{DOMAIN_PRINCIPLES, MigrationRegistry, Store};

/// Four weeks, measured as 28 days. Expiry is checked when digest runs.
pub const REVIEW_WINDOW_DAYS: i64 = 28;

pub const HARNESS_PI: &str = "pi";
pub const HARNESS_CLAUDE: &str = "claude";

pub fn register_migrations(registry: MigrationRegistry) -> MigrationRegistry {
    registry.register(
        DOMAIN_PRINCIPLES,
        1,
        "principles v1 approval and versions",
        PRINCIPLES_V1_SQL,
    )
}

pub const PRINCIPLES_V1_SQL: &str = r#"
CREATE TABLE principle_experiences (
    experience_id        TEXT PRIMARY KEY,
    member_id            TEXT NOT NULL,
    execution_id         TEXT NOT NULL,
    statement            TEXT NOT NULL CHECK (length(trim(statement)) > 0),
    normalized_statement TEXT NOT NULL CHECK (length(trim(normalized_statement)) > 0),
    recorded_at          TEXT NOT NULL,
    UNIQUE (member_id, execution_id, normalized_statement)
);

CREATE TABLE principle_candidates (
    candidate_id            TEXT PRIMARY KEY,
    member_id               TEXT NOT NULL,
    statement               TEXT NOT NULL CHECK (length(trim(statement)) > 0),
    normalized_statement    TEXT NOT NULL CHECK (length(trim(normalized_statement)) > 0),
    source_execution_ids    TEXT NOT NULL,
    dedup_key               TEXT NOT NULL,
    status                  TEXT NOT NULL
                            CHECK (status IN ('pending', 'approved', 'rejected', 'expired')),
    replaces_principle_id   TEXT,
    replaces_candidate_id   TEXT,
    proposed_at             TEXT NOT NULL,
    resolved_at             TEXT,
    rejection_reason        TEXT,
    UNIQUE (member_id, dedup_key),
    CHECK (status <> 'rejected' OR length(trim(rejection_reason)) > 0)
);

CREATE TABLE principles (
    principle_id         TEXT PRIMARY KEY,
    member_id            TEXT NOT NULL,
    candidate_id         TEXT NOT NULL REFERENCES principle_candidates(candidate_id),
    source_execution_ids TEXT NOT NULL,
    current_version      INTEGER NOT NULL CHECK (current_version >= 1),
    created_at           TEXT NOT NULL
);

CREATE TABLE principle_versions (
    principle_id         TEXT NOT NULL REFERENCES principles(principle_id),
    version              INTEGER NOT NULL CHECK (version >= 1),
    body                 TEXT NOT NULL CHECK (length(trim(body)) > 0),
    write_kind           TEXT NOT NULL CHECK (write_kind IN ('approve', 'edit', 'rollback')),
    source_execution_ids TEXT NOT NULL,
    operator             TEXT NOT NULL CHECK (length(trim(operator)) > 0),
    recorded_at          TEXT NOT NULL,
    rollback_from_version INTEGER,
    PRIMARY KEY (principle_id, version),
    CHECK (write_kind <> 'rollback' OR rollback_from_version IS NOT NULL)
);

CREATE TABLE principle_audit (
    seq                  INTEGER PRIMARY KEY AUTOINCREMENT,
    action               TEXT NOT NULL,
    member_id            TEXT NOT NULL,
    candidate_id         TEXT,
    principle_id         TEXT,
    version              INTEGER,
    actor                TEXT NOT NULL,
    reason               TEXT,
    source_execution_ids TEXT NOT NULL,
    recorded_at          TEXT NOT NULL,
    detail               TEXT NOT NULL DEFAULT ''
);

CREATE INDEX principle_candidates_member_status
    ON principle_candidates(member_id, status, proposed_at);
CREATE INDEX principle_audit_member ON principle_audit(member_id, seq);
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateStatus {
    Pending,
    Approved,
    Rejected,
    Expired,
}

impl CandidateStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "approved" => Ok(Self::Approved),
            "rejected" => Ok(Self::Rejected),
            "expired" => Ok(Self::Expired),
            other => Err(Error::Validation(format!(
                "principle candidate status `{other}` is unknown"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipleWriteKind {
    Approve,
    Edit,
    Rollback,
}

impl PrincipleWriteKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Edit => "edit",
            Self::Rollback => "rollback",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "approve" => Ok(Self::Approve),
            "edit" => Ok(Self::Edit),
            "rollback" => Ok(Self::Rollback),
            other => Err(Error::Validation(format!(
                "principle version write_kind `{other}` is unknown"
            ))),
        }
    }
}

/// Who is asking. The local CLI is the owner unless it names a member actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrincipleActor {
    Owner,
    Member(MemberId),
}

impl PrincipleActor {
    pub fn is_owner(&self) -> bool {
        matches!(self, Self::Owner)
    }

    pub fn label(&self) -> String {
        match self {
            Self::Owner => "owner".to_string(),
            Self::Member(id) => format!("member:{}", id.as_str()),
        }
    }

    fn member(&self) -> Option<&MemberId> {
        match self {
            Self::Owner => None,
            Self::Member(id) => Some(id),
        }
    }
}

/// One experience excerpt digest can turn into part of a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExperienceExcerpt {
    pub execution_id: ExecutionId,
    pub statement: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipleCandidate {
    pub candidate_id: String,
    pub member_id: MemberId,
    pub statement: String,
    pub normalized_statement: String,
    pub source_execution_ids: Vec<ExecutionId>,
    pub dedup_key: String,
    pub status: CandidateStatus,
    pub replaces_principle_id: Option<String>,
    pub replaces_candidate_id: Option<String>,
    pub proposed_at: String,
    pub resolved_at: Option<String>,
    pub rejection_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedDuplicate {
    pub candidate_id: String,
    pub status: CandidateStatus,
    pub dedup_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestReport {
    pub member_id: MemberId,
    pub proposed: Vec<PrincipleCandidate>,
    pub skipped: Vec<SkippedDuplicate>,
    pub expired: Vec<PrincipleCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipleVersion {
    pub principle_id: String,
    pub version: u32,
    pub body: String,
    pub write_kind: PrincipleWriteKind,
    pub source_execution_ids: Vec<ExecutionId>,
    pub operator: String,
    pub recorded_at: String,
    pub rollback_from_version: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipleCurrent {
    pub principle_id: String,
    pub member_id: MemberId,
    pub candidate_id: String,
    pub current_version: u32,
    pub source_execution_ids: Vec<ExecutionId>,
    pub version: PrincipleVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovedPrinciple {
    pub principle_id: String,
    pub member_id: MemberId,
    pub version: u32,
    pub body: String,
    pub source_execution_ids: Vec<ExecutionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionFiles {
    pub member_id: MemberId,
    pub pi_path: std::path::PathBuf,
    pub claude_path: std::path::PathBuf,
    pub principles: Vec<ApprovedPrinciple>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub seq: i64,
    pub action: String,
    pub member_id: MemberId,
    pub candidate_id: Option<String>,
    pub principle_id: Option<String>,
    pub version: Option<u32>,
    pub actor: String,
    pub reason: Option<String>,
    pub source_execution_ids: Vec<ExecutionId>,
    pub recorded_at: String,
    pub detail: String,
}

pub struct PrincipleStore<'a> {
    store: &'a Store,
}

impl<'a> PrincipleStore<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// Summarize `batch` into candidates and expire stale pending ones.
    ///
    /// The same normalized statement plus the same sorted execution ids is
    /// not proposed again when any record of that key already exists.
    /// A different statement on that same execution set is a new candidate
    /// and records which principle (or, failing that, which candidate) it
    /// would replace. Replacement is a citation, not an automatic withdraw.
    pub fn digest(
        &self,
        member: &MemberId,
        batch: &[ExperienceExcerpt],
        now_text: &str,
        actor: &PrincipleActor,
    ) -> Result<DigestReport> {
        let now = parse_rfc3339(now_text)?;
        let groups = group_excerpts(batch)?;
        let tx = self.store.transaction()?;
        for excerpt in batch {
            let statement = display_statement(&excerpt.statement);
            let normalized = normalize_statement(&excerpt.statement);
            if normalized.is_empty() {
                continue;
            }
            tx.execute(
                "INSERT OR IGNORE INTO principle_experiences(
                    experience_id, member_id, execution_id, statement,
                    normalized_statement, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    format!("pexp-{}", uuid::Uuid::new_v4().simple()),
                    member.as_str(),
                    excerpt.execution_id.as_str(),
                    statement,
                    normalized,
                    now_text,
                ],
            )?;
        }

        let mut proposed = Vec::new();
        let mut skipped = Vec::new();
        for (normalized, group) in groups {
            let sources_json = encode_ids(&group.ids)?;
            let key = dedup_key(&normalized, &group.ids);
            if let Some(existing) = find_by_key(&tx, member, &key)? {
                skipped.push(SkippedDuplicate {
                    candidate_id: existing.candidate_id,
                    status: existing.status,
                    dedup_key: key,
                });
                continue;
            }
            let (replaces_principle, replaces_candidate) =
                find_replaces(&tx, member, &sources_json, &normalized)?;
            let candidate_id = format!("pcand-{}", uuid::Uuid::new_v4().simple());
            tx.execute(
                "INSERT INTO principle_candidates(
                    candidate_id, member_id, statement, normalized_statement,
                    source_execution_ids, dedup_key, status,
                    replaces_principle_id, replaces_candidate_id,
                    proposed_at, resolved_at, rejection_reason)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8, ?9, NULL, NULL)",
                rusqlite::params![
                    candidate_id,
                    member.as_str(),
                    group.statement,
                    normalized,
                    sources_json,
                    key,
                    replaces_principle,
                    replaces_candidate,
                    now_text,
                ],
            )?;
            let detail =
                replace_detail(replaces_principle.as_deref(), replaces_candidate.as_deref());
            record(
                &tx,
                &AuditWrite {
                    action: "propose",
                    member,
                    candidate_id: Some(&candidate_id),
                    principle_id: None,
                    version: None,
                    actor,
                    reason: None,
                    sources: &group.ids,
                    detail: &detail,
                    subject_type: "principle_candidate",
                    subject_id: &candidate_id,
                    recorded_at: now_text,
                },
            )?;
            let candidate = load_candidate(&tx, &candidate_id)?.ok_or_else(|| {
                Error::Validation("principle candidate disappeared after insert".into())
            })?;
            proposed.push(candidate);
        }

        let expired = expire_pending(&tx, member, now, now_text, actor)?;
        tx.commit()?;
        Ok(DigestReport {
            member_id: member.clone(),
            proposed,
            skipped,
            expired,
        })
    }

    pub fn approve(
        &self,
        candidate_id: &str,
        body_override: Option<&str>,
        actor: &PrincipleActor,
    ) -> Result<PrincipleCurrent> {
        let preview = load_candidate(self.store.connection(), candidate_id)?;
        self.gate_candidate(actor, "approve", "approve", candidate_id, preview.as_ref())?;
        let preview = preview.ok_or_else(|| Error::NotFound {
            entity: "principle_candidate",
            id: candidate_id.to_string(),
        })?;
        let body = match body_override {
            Some(text) => display_statement(text),
            None => preview.statement.clone(),
        };
        if body.trim().is_empty() {
            return Err(Error::Validation(
                "approved principle body must not be empty".into(),
            ));
        }
        let now_text = utc_now();
        let tx = self.store.transaction()?;
        let candidate = require_approvable(&tx, candidate_id)?;
        let principle_id = format!("prin-{}", uuid::Uuid::new_v4().simple());
        let sources_json = encode_ids(&candidate.source_execution_ids)?;
        tx.execute(
            "INSERT INTO principles(
                principle_id, member_id, candidate_id, source_execution_ids,
                current_version, created_at)
             VALUES (?1, ?2, ?3, ?4, 1, ?5)",
            rusqlite::params![
                principle_id,
                candidate.member_id.as_str(),
                candidate.candidate_id,
                sources_json,
                now_text,
            ],
        )?;
        insert_version(
            &tx,
            &NewVersion {
                principle_id: &principle_id,
                version: 1,
                body: &body,
                write_kind: PrincipleWriteKind::Approve,
                sources_json: &sources_json,
                operator: "owner",
                recorded_at: &now_text,
                rollback_from: None,
            },
        )?;
        let updated = tx.execute(
            "UPDATE principle_candidates
             SET status = 'approved', resolved_at = ?2
             WHERE candidate_id = ?1 AND status = 'pending'",
            rusqlite::params![candidate_id, now_text],
        )?;
        if updated != 1 {
            return Err(Error::Validation("candidate was no longer pending".into()));
        }
        let detail = if normalize_statement(&body) == candidate.normalized_statement {
            "approved".to_string()
        } else {
            "approved with edited body".to_string()
        };
        record(
            &tx,
            &AuditWrite {
                action: "approve",
                member: &candidate.member_id,
                candidate_id: Some(candidate_id),
                principle_id: Some(&principle_id),
                version: Some(1),
                actor,
                reason: None,
                sources: &candidate.source_execution_ids,
                detail: &detail,
                subject_type: "principle",
                subject_id: &principle_id,
                recorded_at: &now_text,
            },
        )?;
        let current = load_current(&tx, &principle_id)?;
        tx.commit()?;
        Ok(current)
    }

    pub fn reject(
        &self,
        candidate_id: &str,
        reason: &str,
        actor: &PrincipleActor,
    ) -> Result<PrincipleCandidate> {
        let preview = load_candidate(self.store.connection(), candidate_id)?;
        self.gate_candidate(actor, "reject", "reject", candidate_id, preview.as_ref())?;
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(Error::Validation("rejection needs a reason".into()));
        }
        preview.ok_or_else(|| Error::NotFound {
            entity: "principle_candidate",
            id: candidate_id.to_string(),
        })?;
        let now_text = utc_now();
        let tx = self.store.transaction()?;
        let candidate = require_pending(&tx, candidate_id, "rejected")?;
        let updated = tx.execute(
            "UPDATE principle_candidates
             SET status = 'rejected', resolved_at = ?2, rejection_reason = ?3
             WHERE candidate_id = ?1 AND status = 'pending'",
            rusqlite::params![candidate_id, now_text, reason],
        )?;
        if updated != 1 {
            return Err(Error::Validation("candidate was no longer pending".into()));
        }
        record(
            &tx,
            &AuditWrite {
                action: "reject",
                member: &candidate.member_id,
                candidate_id: Some(candidate_id),
                principle_id: None,
                version: None,
                actor,
                reason: Some(reason),
                sources: &candidate.source_execution_ids,
                detail: "rejected",
                subject_type: "principle_candidate",
                subject_id: candidate_id,
                recorded_at: &now_text,
            },
        )?;
        let updated_row = load_candidate(&tx, candidate_id)?.ok_or_else(|| Error::NotFound {
            entity: "principle_candidate",
            id: candidate_id.to_string(),
        })?;
        tx.commit()?;
        Ok(updated_row)
    }

    /// Change a pending candidate's statement. Does not approve it.
    pub fn edit_candidate(
        &self,
        candidate_id: &str,
        body: &str,
        actor: &PrincipleActor,
    ) -> Result<PrincipleCandidate> {
        let preview = load_candidate(self.store.connection(), candidate_id)?;
        self.gate_candidate(actor, "edit", "edit", candidate_id, preview.as_ref())?;
        let statement = display_statement(body);
        let normalized = normalize_statement(body);
        if normalized.is_empty() {
            return Err(Error::Validation(
                "principle statement must not be empty".into(),
            ));
        }
        preview.ok_or_else(|| Error::NotFound {
            entity: "principle_candidate",
            id: candidate_id.to_string(),
        })?;
        let now_text = utc_now();
        let tx = self.store.transaction()?;
        let candidate = require_pending(&tx, candidate_id, "edited")?;
        let key = dedup_key(&normalized, &candidate.source_execution_ids);
        if let Some(existing) = find_by_key(&tx, &candidate.member_id, &key)? {
            if existing.candidate_id != candidate.candidate_id {
                return Err(Error::Validation(
                    "an existing candidate already uses this statement and source executions"
                        .into(),
                ));
            }
        }
        tx.execute(
            "UPDATE principle_candidates
             SET statement = ?2, normalized_statement = ?3, dedup_key = ?4
             WHERE candidate_id = ?1 AND status = 'pending'",
            rusqlite::params![candidate_id, statement, normalized, key],
        )?;
        record(
            &tx,
            &AuditWrite {
                action: "edit",
                member: &candidate.member_id,
                candidate_id: Some(candidate_id),
                principle_id: None,
                version: None,
                actor,
                reason: None,
                sources: &candidate.source_execution_ids,
                detail: "candidate statement updated",
                subject_type: "principle_candidate",
                subject_id: candidate_id,
                recorded_at: &now_text,
            },
        )?;
        let updated = load_candidate(&tx, candidate_id)?.ok_or_else(|| Error::NotFound {
            entity: "principle_candidate",
            id: candidate_id.to_string(),
        })?;
        tx.commit()?;
        Ok(updated)
    }

    /// Append a new approved body. Older versions stay.
    pub fn edit_principle(
        &self,
        principle_id: &str,
        body: &str,
        actor: &PrincipleActor,
    ) -> Result<PrincipleCurrent> {
        let preview = load_principle_row(self.store.connection(), principle_id)?;
        self.gate_principle(
            actor,
            &PrincipleGate {
                action: "edit",
                verb: "edit",
                principle_id,
                version: None,
                preview: preview.as_ref(),
                fallback_member: None,
            },
        )?;
        let body = display_statement(body);
        if body.is_empty() {
            return Err(Error::Validation("principle body must not be empty".into()));
        }
        let preview = preview.ok_or_else(|| Error::NotFound {
            entity: "principle",
            id: principle_id.to_string(),
        })?;
        let now_text = utc_now();
        let tx = self.store.transaction()?;
        let current = load_current(&tx, principle_id).map_err(|_| Error::NotFound {
            entity: "principle",
            id: principle_id.to_string(),
        })?;
        let next = current.current_version + 1;
        let sources_json = encode_ids(&current.source_execution_ids)?;
        insert_version(
            &tx,
            &NewVersion {
                principle_id,
                version: next,
                body: &body,
                write_kind: PrincipleWriteKind::Edit,
                sources_json: &sources_json,
                operator: "owner",
                recorded_at: &now_text,
                rollback_from: None,
            },
        )?;
        tx.execute(
            "UPDATE principles SET current_version = ?2 WHERE principle_id = ?1",
            rusqlite::params![principle_id, next as i64],
        )?;
        record(
            &tx,
            &AuditWrite {
                action: "edit",
                member: &preview.member_id,
                candidate_id: Some(&preview.candidate_id),
                principle_id: Some(principle_id),
                version: Some(next),
                actor,
                reason: None,
                sources: &current.source_execution_ids,
                detail: "principle body updated",
                subject_type: "principle",
                subject_id: principle_id,
                recorded_at: &now_text,
            },
        )?;
        let updated = load_current(&tx, principle_id)?;
        tx.commit()?;
        Ok(updated)
    }

    /// Copy `version` forward as the new current body. Newer rows stay readable.
    pub fn rollback(
        &self,
        member: &MemberId,
        principle_id: &str,
        version: u32,
        actor: &PrincipleActor,
    ) -> Result<PrincipleCurrent> {
        let preview = load_principle_row(self.store.connection(), principle_id)?;
        self.gate_principle(
            actor,
            &PrincipleGate {
                action: "rollback",
                verb: "roll back",
                principle_id,
                version: Some(version),
                preview: preview.as_ref(),
                fallback_member: Some(member),
            },
        )?;
        let preview = preview.ok_or_else(|| Error::NotFound {
            entity: "principle",
            id: principle_id.to_string(),
        })?;
        if &preview.member_id != member {
            return Err(Error::Validation(
                "principle belongs to another member".into(),
            ));
        }
        let now_text = utc_now();
        let tx = self.store.transaction()?;
        let current = load_current(&tx, principle_id)?;
        if version == current.current_version {
            return Err(Error::Validation(
                "rollback target is already the current version".into(),
            ));
        }
        let target = load_version(&tx, principle_id, version)?.ok_or_else(|| Error::NotFound {
            entity: "principle_version",
            id: format!("{principle_id}@{version}"),
        })?;
        let next = current.current_version + 1;
        let sources_json = encode_ids(&current.source_execution_ids)?;
        insert_version(
            &tx,
            &NewVersion {
                principle_id,
                version: next,
                body: &target.body,
                write_kind: PrincipleWriteKind::Rollback,
                sources_json: &sources_json,
                operator: "owner",
                recorded_at: &now_text,
                rollback_from: Some(version),
            },
        )?;
        tx.execute(
            "UPDATE principles SET current_version = ?2 WHERE principle_id = ?1",
            rusqlite::params![principle_id, next as i64],
        )?;
        let detail = format!("restored version {version}");
        record(
            &tx,
            &AuditWrite {
                action: "rollback",
                member,
                candidate_id: Some(&current.candidate_id),
                principle_id: Some(principle_id),
                version: Some(next),
                actor,
                reason: None,
                sources: &current.source_execution_ids,
                detail: &detail,
                subject_type: "principle",
                subject_id: principle_id,
                recorded_at: &now_text,
            },
        )?;
        let updated = load_current(&tx, principle_id)?;
        tx.commit()?;
        Ok(updated)
    }

    /// Current approved bodies only. Pending, rejected, and expired are absent.
    pub fn approved_current(&self, member: &MemberId) -> Result<Vec<ApprovedPrinciple>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT p.principle_id, p.member_id, p.current_version, v.body, p.source_execution_ids
             FROM principles p
             JOIN principle_versions v
               ON v.principle_id = p.principle_id AND v.version = p.current_version
             WHERE p.member_id = ?1
             ORDER BY p.principle_id",
        )?;
        let rows = stmt.query_map([member.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (principle_id, member_raw, version, body, sources_json) = row?;
            out.push(ApprovedPrinciple {
                principle_id,
                member_id: MemberId::from_str_id(&member_raw)?,
                version: version as u32,
                body,
                source_execution_ids: decode_ids(&sources_json)?,
            });
        }
        Ok(out)
    }

    pub fn version(&self, principle_id: &str, version: u32) -> Result<Option<PrincipleVersion>> {
        load_version(self.store.connection(), principle_id, version)
    }

    pub fn audit(&self, member: &MemberId) -> Result<Vec<AuditRecord>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT seq, action, member_id, candidate_id, principle_id, version,
                    actor, reason, source_execution_ids, recorded_at, detail
             FROM principle_audit WHERE member_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([member.as_str()], |row| {
            Ok(AuditRaw {
                seq: row.get(0)?,
                action: row.get(1)?,
                member_id: row.get(2)?,
                candidate_id: row.get(3)?,
                principle_id: row.get(4)?,
                version: row.get(5)?,
                actor: row.get(6)?,
                reason: row.get(7)?,
                sources_json: row.get(8)?,
                recorded_at: row.get(9)?,
                detail: row.get(10)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(finish_audit(row?)?);
        }
        Ok(out)
    }

    /// Write the Pi and Claude Code projections from the same approved rows.
    /// Files land under `out_dir/pi` and `out_dir/claude`, never as `CLAUDE.md`
    /// or `AGENTS.md`.
    pub fn write_projections(&self, member: &MemberId, out_dir: &Path) -> Result<ProjectionFiles> {
        let principles = self.approved_current(member)?;
        let pi_path = out_dir.join(HARNESS_PI).join("principles.md");
        let claude_path = out_dir.join(HARNESS_CLAUDE).join("principles.md");
        write_projection_file(&pi_path, &render_projection(HARNESS_PI, &principles))?;
        write_projection_file(
            &claude_path,
            &render_projection(HARNESS_CLAUDE, &principles),
        )?;
        Ok(ProjectionFiles {
            member_id: member.clone(),
            pi_path,
            claude_path,
            principles,
        })
    }

    fn gate_candidate(
        &self,
        actor: &PrincipleActor,
        action: &str,
        verb: &str,
        candidate_id: &str,
        preview: Option<&PrincipleCandidate>,
    ) -> Result<()> {
        if actor.is_owner() {
            return Ok(());
        }
        let member = preview
            .map(|candidate| candidate.member_id.clone())
            .or_else(|| actor.member().cloned())
            .ok_or_else(|| Error::Validation("principle actor is missing a member id".into()))?;
        let sources = preview
            .map(|candidate| candidate.source_execution_ids.clone())
            .unwrap_or_default();
        self.write_denial(
            actor,
            &DenialWrite {
                action,
                verb,
                member: &member,
                candidate_id: Some(candidate_id),
                principle_id: None,
                version: None,
                sources: &sources,
                subject_type: "principle_candidate",
                subject_id: candidate_id,
            },
        )
    }

    fn gate_principle(&self, actor: &PrincipleActor, gate: &PrincipleGate<'_>) -> Result<()> {
        if actor.is_owner() {
            return Ok(());
        }
        let member = gate
            .preview
            .map(|row| row.member_id.clone())
            .or_else(|| gate.fallback_member.cloned())
            .or_else(|| actor.member().cloned())
            .ok_or_else(|| Error::Validation("principle actor is missing a member id".into()))?;
        let sources = gate
            .preview
            .map(|row| row.source_execution_ids.clone())
            .unwrap_or_default();
        let candidate_owned = gate.preview.map(|row| row.candidate_id.clone());
        self.write_denial(
            actor,
            &DenialWrite {
                action: gate.action,
                verb: gate.verb,
                member: &member,
                candidate_id: candidate_owned.as_deref(),
                principle_id: Some(gate.principle_id),
                version: gate.version,
                sources: &sources,
                subject_type: "principle",
                subject_id: gate.principle_id,
            },
        )
    }

    fn write_denial(&self, actor: &PrincipleActor, denial: &DenialWrite<'_>) -> Result<()> {
        let now_text = utc_now();
        let tx = self.store.transaction()?;
        record(
            &tx,
            &AuditWrite {
                action: "denied",
                member: denial.member,
                candidate_id: denial.candidate_id,
                principle_id: denial.principle_id,
                version: denial.version,
                actor,
                reason: None,
                sources: denial.sources,
                detail: denial.action,
                subject_type: denial.subject_type,
                subject_id: denial.subject_id,
                recorded_at: &now_text,
            },
        )?;
        tx.commit()?;
        Err(Error::Validation(format!(
            "only the owner can {} a principle",
            denial.verb
        )))
    }
}

struct PrincipleGate<'a> {
    action: &'a str,
    verb: &'a str,
    principle_id: &'a str,
    version: Option<u32>,
    preview: Option<&'a PrincipleRow>,
    fallback_member: Option<&'a MemberId>,
}

struct DenialWrite<'a> {
    action: &'a str,
    verb: &'a str,
    member: &'a MemberId,
    candidate_id: Option<&'a str>,
    principle_id: Option<&'a str>,
    version: Option<u32>,
    sources: &'a [ExecutionId],
    subject_type: &'a str,
    subject_id: &'a str,
}

/// Projection text for one harness. The principle bodies match the other
/// harness; only the first-line label differs.
pub fn render_projection(harness: &str, approved: &[ApprovedPrinciple]) -> String {
    let mut body = String::new();
    if approved.is_empty() {
        body.push_str("(no approved principles)\n");
    } else {
        for item in approved {
            body.push_str(&format!("## {}\n", item.principle_id));
            body.push_str(&format!("version: {}\n", item.version));
            body.push_str("sources:");
            for id in &item.source_execution_ids {
                body.push(' ');
                body.push_str(id.as_str());
            }
            body.push_str("\n\n");
            body.push_str(item.body.trim_end());
            body.push_str("\n\n");
        }
    }
    format!(
        "<!-- viva approved-principle projection for {harness}; pending, rejected, and expired candidates are omitted -->\n{body}"
    )
}

fn display_statement(input: &str) -> String {
    let nfc: String = input.nfc().collect();
    collapse_whitespace(&nfc)
}

fn normalize_statement(input: &str) -> String {
    display_statement(input)
        .chars()
        .map(|ch| {
            if ch.is_ascii_uppercase() {
                ch.to_ascii_lowercase()
            } else {
                ch
            }
        })
        .collect()
}

fn collapse_whitespace(input: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for ch in input.trim().chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch);
        }
    }
    out
}

fn dedup_key(normalized: &str, ids: &[ExecutionId]) -> String {
    let mut key = String::new();
    key.push_str(normalized);
    key.push('\n');
    for (index, id) in ids.iter().enumerate() {
        if index > 0 {
            key.push(',');
        }
        key.push_str(id.as_str());
    }
    key
}

struct Group {
    statement: String,
    ids: Vec<ExecutionId>,
}

fn group_excerpts(batch: &[ExperienceExcerpt]) -> Result<Vec<(String, Group)>> {
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for excerpt in batch {
        let normalized = normalize_statement(&excerpt.statement);
        if normalized.is_empty() {
            return Err(Error::Validation(
                "principle statement must not be empty".into(),
            ));
        }
        let group = groups.entry(normalized).or_insert_with(|| Group {
            statement: display_statement(&excerpt.statement),
            ids: Vec::new(),
        });
        group.ids.push(excerpt.execution_id.clone());
    }
    let mut out = Vec::new();
    for (normalized, mut group) in groups {
        group.ids = canonical_ids(&group.ids)?;
        out.push((normalized, group));
    }
    Ok(out)
}

fn canonical_ids(ids: &[ExecutionId]) -> Result<Vec<ExecutionId>> {
    if ids.is_empty() {
        return Err(Error::Validation(
            "a principle candidate needs at least one source execution".into(),
        ));
    }
    let mut out = ids.to_vec();
    out.sort();
    out.dedup();
    Ok(out)
}

fn encode_ids(ids: &[ExecutionId]) -> Result<String> {
    let texts: Vec<&str> = ids.iter().map(|id| id.as_str()).collect();
    Ok(serde_json::to_string(&texts)?)
}

fn decode_ids(raw: &str) -> Result<Vec<ExecutionId>> {
    let texts: Vec<String> = serde_json::from_str(raw)?;
    texts
        .iter()
        .map(|text| ExecutionId::from_str_id(text))
        .collect()
}

fn parse_rfc3339(text: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(text, &Rfc3339)
        .map_err(|err| Error::Validation(format!("timestamp `{text}` is not RFC3339: {err}")))
}

fn review_window_elapsed(proposed_at: &str, now: OffsetDateTime) -> Result<bool> {
    let proposed = parse_rfc3339(proposed_at)?;
    Ok(now - proposed >= time::Duration::days(REVIEW_WINDOW_DAYS))
}

struct AuditWrite<'a> {
    action: &'a str,
    member: &'a MemberId,
    candidate_id: Option<&'a str>,
    principle_id: Option<&'a str>,
    version: Option<u32>,
    actor: &'a PrincipleActor,
    reason: Option<&'a str>,
    sources: &'a [ExecutionId],
    detail: &'a str,
    subject_type: &'a str,
    subject_id: &'a str,
    recorded_at: &'a str,
}

struct PrincipleRow {
    principle_id: String,
    member_id: MemberId,
    candidate_id: String,
    source_execution_ids: Vec<ExecutionId>,
    current_version: u32,
}

fn record(tx: &rusqlite::Transaction<'_>, write: &AuditWrite<'_>) -> Result<()> {
    let sources_json = encode_ids(write.sources)?;
    tx.execute(
        "INSERT INTO principle_audit(
            action, member_id, candidate_id, principle_id, version, actor,
            reason, source_execution_ids, recorded_at, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            write.action,
            write.member.as_str(),
            write.candidate_id,
            write.principle_id,
            write.version.map(|version| version as i64),
            write.actor.label(),
            write.reason,
            sources_json,
            write.recorded_at,
            write.detail,
        ],
    )?;
    let kind = match write.action {
        "propose" => "principle_proposed",
        "approve" => "principle_approved",
        "reject" => "principle_rejected",
        "edit" => "principle_edited",
        "expire" => "principle_expired",
        "rollback" => "principle_rolled_back",
        "denied" => "principle_denied",
        other => {
            return Err(Error::Validation(format!(
                "unknown principle audit action {other}"
            )));
        }
    };
    let source_list: Vec<&str> = write.sources.iter().map(|id| id.as_str()).collect();
    events::append_in(
        tx,
        NewEvent {
            domain: DOMAIN_PRINCIPLES,
            kind: kind.to_string(),
            subject_type: write.subject_type.to_string(),
            subject_id: write.subject_id.to_string(),
            origin: "principles".to_string(),
            payload: json!({
                "action": write.action,
                "member_id": write.member.as_str(),
                "candidate_id": write.candidate_id,
                "principle_id": write.principle_id,
                "version": write.version,
                "actor": write.actor.label(),
                "reason": write.reason,
                "source_execution_ids": source_list,
                "detail": write.detail,
            }),
        },
    )?;
    Ok(())
}

fn replace_detail(principle_id: Option<&str>, candidate_id: Option<&str>) -> String {
    match (principle_id, candidate_id) {
        (Some(id), _) => format!("replaces principle {id}"),
        (None, Some(id)) => format!("replaces candidate {id}"),
        _ => String::new(),
    }
}

fn find_by_key(
    conn: &rusqlite::Connection,
    member: &MemberId,
    key: &str,
) -> Result<Option<PrincipleCandidate>> {
    let id: Option<String> = conn
        .query_row(
            "SELECT candidate_id FROM principle_candidates
             WHERE member_id = ?1 AND dedup_key = ?2",
            rusqlite::params![member.as_str(), key],
            |row| row.get(0),
        )
        .optional()?;
    match id {
        Some(id) => load_candidate(conn, &id),
        None => Ok(None),
    }
}

fn find_replaces(
    conn: &rusqlite::Connection,
    member: &MemberId,
    sources_json: &str,
    normalized: &str,
) -> Result<(Option<String>, Option<String>)> {
    let principle_id: Option<String> = conn
        .query_row(
            "SELECT p.principle_id
             FROM principles p
             JOIN principle_candidates c ON c.candidate_id = p.candidate_id
             WHERE p.member_id = ?1
               AND c.source_execution_ids = ?2
               AND c.normalized_statement <> ?3
             ORDER BY p.created_at DESC
             LIMIT 1",
            rusqlite::params![member.as_str(), sources_json, normalized],
            |row| row.get(0),
        )
        .optional()?;
    if principle_id.is_some() {
        return Ok((principle_id, None));
    }
    let candidate_id: Option<String> = conn
        .query_row(
            "SELECT candidate_id FROM principle_candidates
             WHERE member_id = ?1
               AND source_execution_ids = ?2
               AND normalized_statement <> ?3
             ORDER BY proposed_at DESC
             LIMIT 1",
            rusqlite::params![member.as_str(), sources_json, normalized],
            |row| row.get(0),
        )
        .optional()?;
    Ok((None, candidate_id))
}

fn expire_pending(
    tx: &rusqlite::Transaction<'_>,
    member: &MemberId,
    now: OffsetDateTime,
    now_text: &str,
    actor: &PrincipleActor,
) -> Result<Vec<PrincipleCandidate>> {
    let mut stmt = tx.prepare(
        "SELECT candidate_id FROM principle_candidates
         WHERE member_id = ?1 AND status = 'pending'
         ORDER BY proposed_at",
    )?;
    let ids: Vec<String> = stmt
        .query_map([member.as_str()], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    let mut expired = Vec::new();
    for candidate_id in ids {
        let candidate = load_candidate(tx, &candidate_id)?.ok_or_else(|| Error::NotFound {
            entity: "principle_candidate",
            id: candidate_id.clone(),
        })?;
        if !review_window_elapsed(&candidate.proposed_at, now)? {
            continue;
        }
        let updated = tx.execute(
            "UPDATE principle_candidates
             SET status = 'expired', resolved_at = ?2
             WHERE candidate_id = ?1 AND status = 'pending'",
            rusqlite::params![candidate.candidate_id, now_text],
        )?;
        if updated != 1 {
            continue;
        }
        record(
            tx,
            &AuditWrite {
                action: "expire",
                member,
                candidate_id: Some(&candidate.candidate_id),
                principle_id: None,
                version: None,
                actor,
                reason: None,
                sources: &candidate.source_execution_ids,
                detail: "unreviewed for 4 weeks",
                subject_type: "principle_candidate",
                subject_id: &candidate.candidate_id,
                recorded_at: now_text,
            },
        )?;
        let mut updated_row = candidate;
        updated_row.status = CandidateStatus::Expired;
        updated_row.resolved_at = Some(now_text.to_string());
        expired.push(updated_row);
    }
    Ok(expired)
}

fn require_approvable(
    conn: &rusqlite::Connection,
    candidate_id: &str,
) -> Result<PrincipleCandidate> {
    let candidate = load_candidate(conn, candidate_id)?.ok_or_else(|| Error::NotFound {
        entity: "principle_candidate",
        id: candidate_id.to_string(),
    })?;
    match candidate.status {
        CandidateStatus::Pending => Ok(candidate),
        CandidateStatus::Expired => Err(Error::Validation(
            "expired candidates cannot be approved".into(),
        )),
        CandidateStatus::Rejected => Err(Error::Validation(
            "rejected candidates cannot be approved".into(),
        )),
        CandidateStatus::Approved => Err(Error::Validation("candidate is already approved".into())),
    }
}

fn require_pending(
    conn: &rusqlite::Connection,
    candidate_id: &str,
    verb: &str,
) -> Result<PrincipleCandidate> {
    let candidate = load_candidate(conn, candidate_id)?.ok_or_else(|| Error::NotFound {
        entity: "principle_candidate",
        id: candidate_id.to_string(),
    })?;
    if candidate.status != CandidateStatus::Pending {
        return Err(Error::Validation(format!(
            "only a pending candidate can be {verb} (status is {})",
            candidate.status.as_str()
        )));
    }
    Ok(candidate)
}

struct NewVersion<'a> {
    principle_id: &'a str,
    version: u32,
    body: &'a str,
    write_kind: PrincipleWriteKind,
    sources_json: &'a str,
    operator: &'a str,
    recorded_at: &'a str,
    rollback_from: Option<u32>,
}

fn insert_version(conn: &rusqlite::Connection, row: &NewVersion<'_>) -> Result<()> {
    conn.execute(
        "INSERT INTO principle_versions(
            principle_id, version, body, write_kind, source_execution_ids,
            operator, recorded_at, rollback_from_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            row.principle_id,
            row.version as i64,
            row.body,
            row.write_kind.as_str(),
            row.sources_json,
            row.operator,
            row.recorded_at,
            row.rollback_from.map(|version| version as i64),
        ],
    )?;
    Ok(())
}

fn load_candidate(
    conn: &rusqlite::Connection,
    candidate_id: &str,
) -> Result<Option<PrincipleCandidate>> {
    let raw = conn
        .query_row(
            "SELECT candidate_id, member_id, statement, normalized_statement,
                    source_execution_ids, dedup_key, status, replaces_principle_id,
                    replaces_candidate_id, proposed_at, resolved_at, rejection_reason
             FROM principle_candidates WHERE candidate_id = ?1",
            [candidate_id],
            |row| {
                Ok(CandidateRaw {
                    candidate_id: row.get(0)?,
                    member_id: row.get(1)?,
                    statement: row.get(2)?,
                    normalized_statement: row.get(3)?,
                    sources_json: row.get(4)?,
                    dedup_key: row.get(5)?,
                    status: row.get(6)?,
                    replaces_principle_id: row.get(7)?,
                    replaces_candidate_id: row.get(8)?,
                    proposed_at: row.get(9)?,
                    resolved_at: row.get(10)?,
                    rejection_reason: row.get(11)?,
                })
            },
        )
        .optional()?;
    raw.map(finish_candidate).transpose()
}

struct CandidateRaw {
    candidate_id: String,
    member_id: String,
    statement: String,
    normalized_statement: String,
    sources_json: String,
    dedup_key: String,
    status: String,
    replaces_principle_id: Option<String>,
    replaces_candidate_id: Option<String>,
    proposed_at: String,
    resolved_at: Option<String>,
    rejection_reason: Option<String>,
}

fn finish_candidate(raw: CandidateRaw) -> Result<PrincipleCandidate> {
    Ok(PrincipleCandidate {
        candidate_id: raw.candidate_id,
        member_id: MemberId::from_str_id(&raw.member_id)?,
        statement: raw.statement,
        normalized_statement: raw.normalized_statement,
        source_execution_ids: decode_ids(&raw.sources_json)?,
        dedup_key: raw.dedup_key,
        status: CandidateStatus::parse(&raw.status)?,
        replaces_principle_id: raw.replaces_principle_id,
        replaces_candidate_id: raw.replaces_candidate_id,
        proposed_at: raw.proposed_at,
        resolved_at: raw.resolved_at,
        rejection_reason: raw.rejection_reason,
    })
}

fn load_principle_row(
    conn: &rusqlite::Connection,
    principle_id: &str,
) -> Result<Option<PrincipleRow>> {
    let raw = conn
        .query_row(
            "SELECT principle_id, member_id, candidate_id, source_execution_ids, current_version
             FROM principles WHERE principle_id = ?1",
            [principle_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((principle_id, member_raw, candidate_id, sources_json, version)) = raw else {
        return Ok(None);
    };
    Ok(Some(PrincipleRow {
        principle_id,
        member_id: MemberId::from_str_id(&member_raw)?,
        candidate_id,
        source_execution_ids: decode_ids(&sources_json)?,
        current_version: version as u32,
    }))
}

fn load_current(conn: &rusqlite::Connection, principle_id: &str) -> Result<PrincipleCurrent> {
    let row = load_principle_row(conn, principle_id)?.ok_or_else(|| Error::NotFound {
        entity: "principle",
        id: principle_id.to_string(),
    })?;
    let version = load_version(conn, principle_id, row.current_version)?.ok_or_else(|| {
        Error::Validation(format!(
            "principle {principle_id} points at missing version {}",
            row.current_version
        ))
    })?;
    Ok(PrincipleCurrent {
        principle_id: row.principle_id,
        member_id: row.member_id,
        candidate_id: row.candidate_id,
        current_version: row.current_version,
        source_execution_ids: row.source_execution_ids,
        version,
    })
}

fn load_version(
    conn: &rusqlite::Connection,
    principle_id: &str,
    version: u32,
) -> Result<Option<PrincipleVersion>> {
    let raw = conn
        .query_row(
            "SELECT principle_id, version, body, write_kind, source_execution_ids,
                    operator, recorded_at, rollback_from_version
             FROM principle_versions WHERE principle_id = ?1 AND version = ?2",
            rusqlite::params![principle_id, version as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                ))
            },
        )
        .optional()?;
    let Some((
        principle_id,
        version,
        body,
        write_kind,
        sources_json,
        operator,
        recorded_at,
        rollback_from,
    )) = raw
    else {
        return Ok(None);
    };
    Ok(Some(PrincipleVersion {
        principle_id,
        version: version as u32,
        body,
        write_kind: PrincipleWriteKind::parse(&write_kind)?,
        source_execution_ids: decode_ids(&sources_json)?,
        operator,
        recorded_at,
        rollback_from_version: rollback_from.map(|value| value as u32),
    }))
}

struct AuditRaw {
    seq: i64,
    action: String,
    member_id: String,
    candidate_id: Option<String>,
    principle_id: Option<String>,
    version: Option<i64>,
    actor: String,
    reason: Option<String>,
    sources_json: String,
    recorded_at: String,
    detail: String,
}

fn finish_audit(raw: AuditRaw) -> Result<AuditRecord> {
    Ok(AuditRecord {
        seq: raw.seq,
        action: raw.action,
        member_id: MemberId::from_str_id(&raw.member_id)?,
        candidate_id: raw.candidate_id,
        principle_id: raw.principle_id,
        version: raw.version.map(|value| value as u32),
        actor: raw.actor,
        reason: raw.reason,
        source_execution_ids: decode_ids(&raw.sources_json)?,
        recorded_at: raw.recorded_at,
        detail: raw.detail,
    })
}

fn write_projection_file(path: &Path, body: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, body.as_bytes())?;
    Ok(())
}

trait IdParse {
    fn from_str_id(text: &str) -> Result<Self>
    where
        Self: Sized;
}

impl IdParse for MemberId {
    fn from_str_id(text: &str) -> Result<Self> {
        use std::str::FromStr;
        Ok(MemberId::from_str(text)?)
    }
}

impl IdParse for ExecutionId {
    fn from_str_id(text: &str) -> Result<Self> {
        use std::str::FromStr;
        Ok(ExecutionId::from_str(text)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_folds_ascii_case_and_whitespace() {
        assert_eq!(
            normalize_statement("  Ask   Which\nRepository "),
            "ask which repository"
        );
        assert_eq!(
            display_statement("  Ask   Which\nRepository "),
            "Ask Which Repository"
        );
    }
}
