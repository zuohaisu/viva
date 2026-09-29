//! Runtime knowledge review and repository maintenance proposals
//! (F02, issue #24).
//!
//! Semantics:
//! - Maintenance runs only while the office is open: every scan is recorded
//!   under an explicit [`MaintenanceSession`] that the host (workbench)
//!   opens on start and ends on shutdown. There is no daemon and no timer
//!   here; closing the TUI ends the session.
//! - Scans **propose**, they never destroy. Evidence-backed proposals are
//!   appended with a stable dedup key, so re-running a review reproduces no
//!   duplicates; changing facts produce a new proposal, honestly.
//! - Exits stay reversible and traceable: knowledge archiving goes through
//!   the knowledge registry's lifecycle (reason + actor appended, body file
//!   untouched, restore path intact); skill disabling flips office state
//!   only and never touches SKILL.md.
//! - Authorization comes from the user: executing a proposal requires a
//!   live grant covering `maintain_knowledge` (an ACT_* office-wide grant —
//!   maintenance is not task-scoped, so a task-scoped grant is refused).
//! - There is **no delete** in this module, no worktree removal, and no
//!   remote/branch prune: worktree and cleanliness findings are proposals
//!   for human review only. After a restart the pending proposals are
//!   restored as records and nothing replays — execution always needs a
//!   fresh, authorized call.

use std::path::Path;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::authority::{Actor, AuthorityEngine};
use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::utc_now;
use crate::foundation::store::{DOMAIN_MAINTENANCE, MigrationRegistry, Store};
use crate::git::cli::CliRunner;
use crate::git::worktrees::TaskWorktreeRecord;
use crate::knowledge::KnowledgeRegistry;

/// Register the `maintenance` domain migrations (F02's namespace).
pub fn register_migrations(registry: MigrationRegistry) -> MigrationRegistry {
    registry.register(DOMAIN_MAINTENANCE, 1, "maintenance v1", MAINTENANCE_V1_SQL)
}

pub const MAINTENANCE_V1_SQL: &str = r#"
-- Review sessions: proof that a scan ran under a live office host. The
-- host opens a session on start and ends it on shutdown; no session means
-- no maintenance ran.
CREATE TABLE maintenance_sessions (
    session_id  TEXT PRIMARY KEY,
    host_pid    INTEGER NOT NULL,
    started_at  TEXT NOT NULL,
    ended_at    TEXT
);

-- One review run within a session, with its honest outcome counts.
CREATE TABLE maintenance_scans (
    scan_id     TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL REFERENCES maintenance_sessions(session_id),
    kind        TEXT NOT NULL,
    started_at  TEXT NOT NULL,
    finished_at TEXT,
    proposed    INTEGER,
    duplicates  INTEGER
);

-- Evidence-backed proposals with a stable dedup key: re-running a review
-- cannot manufacture duplicates. Nothing here is ever deleted; resolved
-- proposals keep who resolved them and why.
CREATE TABLE maintenance_proposals (
    proposal_id      TEXT PRIMARY KEY,
    session_id       TEXT NOT NULL REFERENCES maintenance_sessions(session_id),
    scan_id          TEXT NOT NULL REFERENCES maintenance_scans(scan_id),
    dedup_key        TEXT NOT NULL UNIQUE,
    kind             TEXT NOT NULL CHECK (kind IN
                         ('stale_knowledge', 'idle_skill', 'worktree_review', 'repo_cleanliness')),
    subject          TEXT NOT NULL,
    evidence         TEXT NOT NULL CHECK (length(trim(evidence)) > 0),
    suggested_action TEXT NOT NULL CHECK (suggested_action IN
                         ('archive_knowledge', 'disable_skill', 'human_review')),
    status           TEXT NOT NULL DEFAULT 'open'
                     CHECK (status IN ('open', 'executed', 'dismissed')),
    created_at       TEXT NOT NULL,
    resolved_at      TEXT,
    resolved_by      TEXT,
    resolution_note  TEXT
);
"#;

// ---------------------------------------------------------------------------
// Proposals
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalKind {
    StaleKnowledge,
    IdleSkill,
    WorktreeReview,
    RepoCleanliness,
}

impl ProposalKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProposalKind::StaleKnowledge => "stale_knowledge",
            ProposalKind::IdleSkill => "idle_skill",
            ProposalKind::WorktreeReview => "worktree_review",
            ProposalKind::RepoCleanliness => "repo_cleanliness",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestedAction {
    /// Low-risk, reversible exit through the knowledge lifecycle.
    ArchiveKnowledge,
    /// Reversible office-state change (the skill's files are untouched).
    DisableSkill,
    /// A human must name the exact action; maintenance never performs it.
    HumanReview,
}

impl SuggestedAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            SuggestedAction::ArchiveKnowledge => "archive_knowledge",
            SuggestedAction::DisableSkill => "disable_skill",
            SuggestedAction::HumanReview => "human_review",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub proposal_id: String,
    pub session_id: String,
    pub dedup_key: String,
    pub kind: ProposalKind,
    pub subject: String,
    pub evidence: String,
    pub suggested_action: SuggestedAction,
    pub status: ProposalStatus,
    pub created_at: String,
    pub resolved_at: Option<String>,
    pub resolved_by: Option<String>,
    pub resolution_note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Open,
    Executed,
    Dismissed,
}

impl ProposalStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProposalStatus::Open => "open",
            ProposalStatus::Executed => "executed",
            ProposalStatus::Dismissed => "dismissed",
        }
    }
}

/// One review run's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReport {
    pub scan_id: String,
    pub kind: &'static str,
    /// Newly appended proposals.
    pub proposed: Vec<Proposal>,
    /// Findings whose dedup key already had an open/resolved proposal:
    /// re-runs do not duplicate them.
    pub already_proposed: usize,
    /// Honest notes about what the scan could not check (e.g. git
    /// unavailable) — absence of proposals is not silently "all clean".
    pub notes: Vec<String>,
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A live maintenance window, owned by the office host. Closing the office
/// ends the window; a restart opens a new one and only restores records.
pub struct MaintenanceSession {
    session_id: String,
}

impl MaintenanceSession {
    /// Open a session for this process. The host calls this when the office
    /// becomes active.
    pub fn open(store: &Store) -> OfficeResult<MaintenanceSession> {
        let session = MaintenanceSession {
            session_id: format!("maint-{}", uuid::Uuid::new_v4().simple()),
        };
        store.connection().execute(
            "INSERT INTO maintenance_sessions(session_id, host_pid, started_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![session.session_id, std::process::id() as i64, utc_now()],
        )?;
        Ok(session)
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Close the window. Pending proposals stay exactly as they are —
    /// restoring them after a restart is a read, never a replay.
    pub fn end(self, store: &Store) -> OfficeResult<()> {
        store.connection().execute(
            "UPDATE maintenance_sessions SET ended_at = ?2 WHERE session_id = ?1",
            rusqlite::params![self.session_id, utc_now()],
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

pub struct MaintenanceService<'a> {
    store: &'a Store,
}

impl<'a> MaintenanceService<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    // -- Knowledge review ----------------------------------------------------

    /// Review knowledge and skills: active items whose last update is older
    /// than `stale_after_days` (or whose timestamps are unparseable — those
    /// get an honest human-review proposal) and enabled skills with no
    /// recorded usage. Idempotent: re-running finds the same facts and
    /// adds nothing.
    pub fn review_knowledge(
        &self,
        session: &MaintenanceSession,
        registry: &KnowledgeRegistry<'_>,
        stale_after_days: u32,
    ) -> OfficeResult<ScanReport> {
        let scan_id = self.open_scan(session, "knowledge_review")?;
        let mut proposed = Vec::new();
        let mut already = 0usize;

        let cutoff = (OffsetDateTime::now_utc() - time::Duration::days(stale_after_days as i64))
            .format(&Rfc3339)
            .map_err(|e| OfficeError::Validation(format!("cutoff format: {e}")))?;

        for item in registry_export_items(registry)? {
            if item.status != crate::knowledge::KnowledgeStatus::Active {
                continue;
            }
            match OffsetDateTime::parse(&item.updated_at, &Rfc3339) {
                Ok(updated) => {
                    let updated = updated
                        .format(&Rfc3339)
                        .map_err(|e| OfficeError::Validation(format!("timestamp format: {e}")))?;
                    if updated.as_str() > cutoff.as_str() {
                        continue;
                    }
                    let evidence = format!(
                        "active since {} with no update since {} (stale threshold \
                         {stale_after_days} days); provenance: {}",
                        item.created_at, item.updated_at, item.provenance
                    );
                    match self.propose(
                        session,
                        &scan_id,
                        NewProposal {
                            kind: ProposalKind::StaleKnowledge,
                            dedup_key: format!("stale_knowledge:{}", item.item_id),
                            subject: item.item_id.clone(),
                            evidence,
                            suggested_action: SuggestedAction::ArchiveKnowledge,
                        },
                    )? {
                        Some(p) => proposed.push(p),
                        None => already += 1,
                    }
                }
                // Unparseable timestamps: propose for human review instead
                // of silently skipping or guessing staleness.
                Err(_) => {
                    let evidence = format!(
                        "updated_at `{}` is not parseable as RFC 3339 — staleness \
                         unknown; provenance: {}",
                        item.updated_at, item.provenance
                    );
                    match self.propose(
                        session,
                        &scan_id,
                        NewProposal {
                            kind: ProposalKind::StaleKnowledge,
                            dedup_key: format!("stale_knowledge_unparseable:{}", item.item_id),
                            subject: item.item_id.clone(),
                            evidence,
                            suggested_action: SuggestedAction::HumanReview,
                        },
                    )? {
                        Some(p) => proposed.push(p),
                        None => already += 1,
                    }
                }
            }
        }

        // Enabled skills without a single usage record: candidates for
        // disabling (reversible office state), never for deletion.
        for skill in registry.list_skills(true)? {
            let usage = registry.usage_of(&skill.item_id)?;
            if !usage.is_empty() {
                continue;
            }
            let evidence = format!(
                "skill {} is enabled but has no usage record; entry {} — disabling keeps \
                 the SKILL.md untouched and reversible",
                skill.skill_id,
                skill.entry_path.display()
            );
            match self.propose(
                session,
                &scan_id,
                NewProposal {
                    kind: ProposalKind::IdleSkill,
                    dedup_key: format!("idle_skill:{}", skill.skill_id),
                    subject: skill.skill_id.clone(),
                    evidence,
                    suggested_action: SuggestedAction::DisableSkill,
                },
            )? {
                Some(p) => proposed.push(p),
                None => already += 1,
            }
        }

        self.close_scan(&scan_id, proposed.len(), already)?;
        Ok(ScanReport {
            scan_id,
            kind: "knowledge_review",
            proposed,
            already_proposed: already,
            notes: Vec::new(),
        })
    }

    // -- Worktree review -------------------------------------------------------

    /// Review active task worktrees. The outcome is always a proposal for
    /// human review: maintenance has no worktree removal and no prune —
    /// releasing a worktree stays a human-named action.
    pub fn review_worktrees(
        &self,
        session: &MaintenanceSession,
        records: &[TaskWorktreeRecord],
    ) -> OfficeResult<ScanReport> {
        let scan_id = self.open_scan(session, "worktree_review")?;
        let mut proposed = Vec::new();
        let mut already = 0usize;
        for record in records {
            if record.released_at.is_some() {
                continue;
            }
            let evidence = format!(
                "worktree {} on branch {} for task {} at {} created {} is still active — \
                 decide explicitly whether to keep or release it (release is human-named)",
                record.worktree_id,
                record.branch,
                record.task_id,
                record.worktree_path.display(),
                record.created_at,
            );
            match self.propose(
                session,
                &scan_id,
                NewProposal {
                    kind: ProposalKind::WorktreeReview,
                    dedup_key: format!("worktree_review:{}", record.worktree_id),
                    subject: record.worktree_id.to_string(),
                    evidence,
                    suggested_action: SuggestedAction::HumanReview,
                },
            )? {
                Some(p) => proposed.push(p),
                None => already += 1,
            }
        }
        self.close_scan(&scan_id, proposed.len(), already)?;
        Ok(ScanReport {
            scan_id,
            kind: "worktree_review",
            proposed,
            already_proposed: already,
            notes: Vec::new(),
        })
    }

    // -- Repository cleanliness --------------------------------------------------

    /// One read-only `git status --porcelain` pass over `repo_root`.
    /// Findings become a human-review proposal whose dedup key is the
    /// dirty state itself: the same state proposes once, a changed state
    /// proposes again. Git failure is reported, never papered over.
    pub fn review_repo(
        &self,
        session: &MaintenanceSession,
        repo_root: &Path,
    ) -> OfficeResult<ScanReport> {
        let scan_id = self.open_scan(session, "repo_review")?;
        let output = CliRunner::default().run("git", repo_root, &["status", "--porcelain"]);

        let mut proposed = Vec::new();
        let mut already = 0usize;
        let mut note: Option<String> = None;

        match output {
            Ok(out) if out.success() => {
                let status = out.stdout.trim().to_string();
                if status.is_empty() {
                    note = Some("working tree clean — nothing to propose".into());
                } else {
                    let key = format!("repo_cleanliness:{:x}", md5_of(&status));
                    let evidence = format!(
                        "`git status --porcelain` lists working-tree leftovers:\n{status}\n\
                         — a human decides; maintenance removes nothing"
                    );
                    match self.propose(
                        session,
                        &scan_id,
                        NewProposal {
                            kind: ProposalKind::RepoCleanliness,
                            dedup_key: key,
                            subject: repo_root.display().to_string(),
                            evidence,
                            suggested_action: SuggestedAction::HumanReview,
                        },
                    )? {
                        Some(p) => proposed.push(p),
                        None => already += 1,
                    }
                }
            }
            Ok(out) => {
                note = Some(format!(
                    "git status failed (exit {}): {} — no proposals were made",
                    out.exit_code, out.stderr
                ));
            }
            Err(failure) => {
                note = Some(format!(
                    "git status could not run: {failure} — no proposals were made"
                ));
            }
        }

        self.close_scan(&scan_id, proposed.len(), already)?;
        Ok(ScanReport {
            scan_id,
            kind: "repo_review",
            proposed,
            already_proposed: already,
            notes: note.into_iter().collect(),
        })
    }

    // -- Resolution ---------------------------------------------------------------

    /// Every open proposal, oldest first — the restart view. Reading this
    /// executes nothing.
    pub fn open_proposals(&self) -> OfficeResult<Vec<Proposal>> {
        self.proposals_by_status("open")
    }

    /// Execute one open proposal under a live grant. Knowledge exits go
    /// through the reversible lifecycle (reason recorded, body file kept,
    /// restore available); skill disabling flips office state only.
    /// `human_review` proposals are refused: maintenance does not prune
    /// worktrees, delete files or touch remotes — that action stays with a
    /// human who names it.
    pub fn execute_proposal(
        &self,
        proposal_id: &str,
        actor: &Actor,
        authority: &AuthorityEngine<'_>,
        registry: &KnowledgeRegistry<'_>,
        note: impl Into<String>,
    ) -> OfficeResult<Proposal> {
        let proposal = self.require_proposal(proposal_id)?;
        if proposal.status != ProposalStatus::Open {
            return Err(OfficeError::Validation(format!(
                "proposal `{proposal_id}` is already {}",
                proposal.status.as_str()
            )));
        }
        if proposal.suggested_action == SuggestedAction::HumanReview {
            return Err(OfficeError::Validation(
                "this proposal is for human review only — maintenance never prunes \
                 worktrees, deletes files or touches remotes; a human must name the \
                 exact action"
                    .into(),
            ));
        }
        // Office-wide mutation needs an office-wide live grant; a
        // task-scoped grant is refused by the cross-task check on purpose.
        authority
            .check(actor, "maintain_knowledge", None)?
            .map_err(|reason| {
                OfficeError::Validation(format!("maintenance not authorized: {reason}"))
            })?;

        let note = note.into();
        let now = utc_now();
        let actor_str = match actor {
            Actor::Member { member, .. } => format!("member:{}", member.as_str()),
            Actor::Worker { credential_id } => format!("worker:{credential_id}"),
        };
        match proposal.suggested_action {
            SuggestedAction::ArchiveKnowledge => {
                let reason = format!("maintenance: {}; evidence: {}", note, proposal.evidence);
                registry.archive(&proposal.subject, reason, &actor_str)?;
            }
            SuggestedAction::DisableSkill => {
                registry.set_skill_enabled(&proposal.subject, false)?;
            }
            SuggestedAction::HumanReview => unreachable!("refused above"),
        }
        self.store.connection().execute(
            "UPDATE maintenance_proposals SET status = 'executed', resolved_at = ?2,
                    resolved_by = ?3, resolution_note = ?4
             WHERE proposal_id = ?1",
            rusqlite::params![proposal_id, now, actor_str, note],
        )?;
        self.get_proposal(proposal_id)?
            .ok_or_else(|| OfficeError::NotFound {
                entity: "proposal",
                id: proposal_id.to_string(),
            })
    }

    /// Dismiss a proposal without executing it (decision recorded).
    /// Dismissal silences a finding, so it is an office-state mutation too:
    /// it requires a live `maintain_knowledge` grant like execution does,
    /// and the denial is appended when refused.
    pub fn dismiss_proposal(
        &self,
        proposal_id: &str,
        actor: &Actor,
        authority: &AuthorityEngine<'_>,
        note: impl Into<String>,
    ) -> OfficeResult<Proposal> {
        let proposal = self.require_proposal(proposal_id)?;
        if proposal.status != ProposalStatus::Open {
            return Err(OfficeError::Validation(format!(
                "proposal `{proposal_id}` is already {}",
                proposal.status.as_str()
            )));
        }
        authority
            .check(actor, "maintain_knowledge", None)?
            .map_err(|reason| {
                OfficeError::Validation(format!("maintenance dismissal not authorized: {reason}"))
            })?;
        let note = note.into();
        let actor_str = match actor {
            Actor::Member { member, .. } => format!("member:{}", member.as_str()),
            Actor::Worker { credential_id } => format!("worker:{credential_id}"),
        };
        self.store.connection().execute(
            "UPDATE maintenance_proposals SET status = 'dismissed', resolved_at = ?2,
                    resolved_by = ?3, resolution_note = ?4
             WHERE proposal_id = ?1",
            rusqlite::params![proposal_id, utc_now(), actor_str, note],
        )?;
        self.get_proposal(proposal_id)?
            .ok_or_else(|| OfficeError::NotFound {
                entity: "proposal",
                id: proposal_id.to_string(),
            })
    }

    pub fn get_proposal(&self, proposal_id: &str) -> OfficeResult<Option<Proposal>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT proposal_id, session_id, dedup_key, kind, subject, evidence,
                    suggested_action, status, created_at, resolved_at, resolved_by,
                    resolution_note
             FROM maintenance_proposals WHERE proposal_id = ?1",
        )?;
        let row = stmt.query_row([proposal_id], map_proposal).optional()?;
        Ok(row)
    }

    fn require_proposal(&self, proposal_id: &str) -> OfficeResult<Proposal> {
        self.get_proposal(proposal_id)?
            .ok_or_else(|| OfficeError::NotFound {
                entity: "proposal",
                id: proposal_id.to_string(),
            })
    }

    fn proposals_by_status(&self, status: &str) -> OfficeResult<Vec<Proposal>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT proposal_id, session_id, dedup_key, kind, subject, evidence,
                    suggested_action, status, created_at, resolved_at, resolved_by,
                    resolution_note
             FROM maintenance_proposals WHERE status = ?1 ORDER BY created_at, rowid",
        )?;
        let rows = stmt.query_map([status], map_proposal)?;
        let mut proposals = Vec::new();
        for row in rows {
            proposals.push(row?);
        }
        Ok(proposals)
    }

    // -- Internals -------------------------------------------------------------

    fn open_scan(&self, session: &MaintenanceSession, kind: &str) -> OfficeResult<String> {
        let scan_id = format!("scan-{}", uuid::Uuid::new_v4().simple());
        self.store.connection().execute(
            "INSERT INTO maintenance_scans(scan_id, session_id, kind, started_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![scan_id, session.session_id, kind, utc_now()],
        )?;
        Ok(scan_id)
    }

    fn close_scan(&self, scan_id: &str, proposed: usize, already: usize) -> OfficeResult<()> {
        self.store.connection().execute(
            "UPDATE maintenance_scans SET finished_at = ?2, proposed = ?3, duplicates = ?4
             WHERE scan_id = ?1",
            rusqlite::params![scan_id, utc_now(), proposed as i64, already as i64],
        )?;
        Ok(())
    }

    fn propose(
        &self,
        session: &MaintenanceSession,
        scan_id: &str,
        new: NewProposal,
    ) -> OfficeResult<Option<Proposal>> {
        // Dedup rule: a proposal is unique per (kind, subject, generation).
        // The generation counts EXECUTED proposals for the subject — an
        // executed exit removes the subject from the scan population, so
        // the condition re-proposing means a genuinely new cycle (e.g. a
        // disabled skill was re-enabled and is idle again). A DISMISSED
        // proposal is a standing human decision and keeps silencing its
        // subject (generation unchanged) — re-runs never nag.
        let generation: i64 = self.store.connection().query_row(
            "SELECT COUNT(*) FROM maintenance_proposals
             WHERE kind = ?1 AND subject = ?2 AND status = 'executed'",
            rusqlite::params![new.kind.as_str(), new.subject],
            |row| row.get(0),
        )?;
        let dedup_key = format!("{}:{:04}", new.dedup_key, generation);
        let existing: Option<String> = self
            .store
            .connection()
            .query_row(
                "SELECT proposal_id FROM maintenance_proposals WHERE dedup_key = ?1",
                [&dedup_key],
                |row| row.get(0),
            )
            .optional()?;
        if existing.is_some() {
            return Ok(None);
        }
        let proposal = Proposal {
            proposal_id: format!("prop-{}", uuid::Uuid::new_v4().simple()),
            session_id: session.session_id.clone(),
            dedup_key,
            kind: new.kind,
            subject: new.subject,
            evidence: new.evidence,
            suggested_action: new.suggested_action,
            status: ProposalStatus::Open,
            created_at: utc_now(),
            resolved_at: None,
            resolved_by: None,
            resolution_note: None,
        };
        self.store.connection().execute(
            "INSERT INTO maintenance_proposals(proposal_id, session_id, scan_id, dedup_key,
                                               kind, subject, evidence, suggested_action,
                                               status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                proposal.proposal_id,
                proposal.session_id,
                scan_id,
                proposal.dedup_key,
                proposal.kind.as_str(),
                proposal.subject,
                proposal.evidence,
                proposal.suggested_action.as_str(),
                proposal.status.as_str(),
                proposal.created_at,
            ],
        )?;
        Ok(Some(proposal))
    }
}

/// A finding to propose, with its stable dedup key.
struct NewProposal {
    kind: ProposalKind,
    dedup_key: String,
    subject: String,
    evidence: String,
    suggested_action: SuggestedAction,
}

fn md5_of(text: &str) -> u64 {
    // A stable fingerprint of the dirty state for dedup purposes only —
    // not a cryptographic guarantee.
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn map_proposal(row: &rusqlite::Row<'_>) -> rusqlite::Result<Proposal> {
    let kind: String = row.get(3)?;
    let action: String = row.get(6)?;
    let status: String = row.get(7)?;
    Ok(Proposal {
        proposal_id: row.get(0)?,
        session_id: row.get(1)?,
        dedup_key: row.get(2)?,
        kind: match kind.as_str() {
            "stale_knowledge" => ProposalKind::StaleKnowledge,
            "idle_skill" => ProposalKind::IdleSkill,
            "worktree_review" => ProposalKind::WorktreeReview,
            "repo_cleanliness" => ProposalKind::RepoCleanliness,
            other => panic!("maintenance_proposals.kind holds `{other}`"),
        },
        subject: row.get(4)?,
        evidence: row.get(5)?,
        suggested_action: match action.as_str() {
            "archive_knowledge" => SuggestedAction::ArchiveKnowledge,
            "disable_skill" => SuggestedAction::DisableSkill,
            "human_review" => SuggestedAction::HumanReview,
            other => panic!("maintenance_proposals.suggested_action holds `{other}`"),
        },
        status: match status.as_str() {
            "open" => ProposalStatus::Open,
            "executed" => ProposalStatus::Executed,
            "dismissed" => ProposalStatus::Dismissed,
            other => panic!("maintenance_proposals.status holds `{other}`"),
        },
        created_at: row.get(8)?,
        resolved_at: row.get(9)?,
        resolved_by: row.get(10)?,
        resolution_note: row.get(11)?,
    })
}

/// Snapshot the registry's items (the maintenance view over knowledge).
fn registry_export_items(
    registry: &KnowledgeRegistry<'_>,
) -> OfficeResult<Vec<crate::knowledge::KnowledgeItem>> {
    let export = registry.export_json()?;
    let mut items = Vec::new();
    if let Some(entries) = export.get("entries").and_then(|e| e.as_array()) {
        for entry in entries {
            if let Some(item) = entry.get("item") {
                items.push(serde_json::from_value(item.clone())?);
            }
        }
    }
    Ok(items)
}
