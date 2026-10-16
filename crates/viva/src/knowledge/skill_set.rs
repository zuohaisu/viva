//! Member skill sets: claim, restore, sync, overlap hints and weekly
//! summaries (issue #139, Resident-Container §7.1 / §8).
//!
//! Skills remain team-owned; each member carries an independent set of
//! team skill references. Weight decay and automated forgetting live in
//! issue #119 — this module records membership, append-only set events,
//! and `used_in` rows (including the one-time initial 1.0 on join).

use std::collections::HashSet;
use std::path::PathBuf;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::foundation::error::{Error, Result};
use crate::foundation::ids::{ExecutionId, MemberId, utc_now};
use crate::foundation::store::Store;

use super::{KnowledgeRegistry, SkillRegistration};

/// Default Jaccard similarity above which an overlap is reported (not merged).
pub const DEFAULT_OVERLAP_THRESHOLD: f64 = 0.55;

pub const KNOWLEDGE_V2_SKILL_SET_SQL: &str = r#"
ALTER TABLE skills ADD COLUMN derived_from TEXT REFERENCES skills(skill_id);

CREATE TABLE member_skill_memberships (
    member_id          TEXT NOT NULL,
    skill_id           TEXT NOT NULL REFERENCES skills(skill_id),
    membership_state   TEXT NOT NULL CHECK (membership_state IN ('active', 'forgotten')),
    joined_at          TEXT NOT NULL,
    forgotten_at       TEXT,
    PRIMARY KEY (member_id, skill_id)
);

CREATE TABLE member_skill_set_events (
    seq            INTEGER PRIMARY KEY AUTOINCREMENT,
    member_id      TEXT NOT NULL,
    skill_id       TEXT NOT NULL,
    event_kind     TEXT NOT NULL CHECK (event_kind IN
                       ('claimed', 'restored', 'synced', 'overlap_suggested')),
    peer_skill_id  TEXT,
    detail_json    TEXT NOT NULL DEFAULT '{}',
    recorded_at    TEXT NOT NULL
);

CREATE TABLE skill_used_in (
    seq           INTEGER PRIMARY KEY AUTOINCREMENT,
    member_id     TEXT NOT NULL,
    skill_id      TEXT NOT NULL,
    execution_id  TEXT NOT NULL,
    score         REAL NOT NULL,
    note          TEXT NOT NULL DEFAULT '',
    used_at       TEXT NOT NULL,
    UNIQUE (member_id, skill_id, execution_id)
);

CREATE TABLE skill_weekly_reports (
    report_key     TEXT PRIMARY KEY,
    last_run_at    TEXT NOT NULL,
    last_event_seq INTEGER NOT NULL DEFAULT 0
);

INSERT INTO skill_weekly_reports(report_key, last_run_at, last_event_seq)
VALUES ('default', '1970-01-01T00:00:00Z', 0);
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipState {
    Active,
    Forgotten,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemberSkillMembership {
    pub member_id: MemberId,
    pub skill_id: String,
    pub state: MembershipState,
    pub joined_at: String,
    pub forgotten_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillSetEvent {
    pub seq: i64,
    pub member_id: MemberId,
    pub skill_id: String,
    pub event_kind: String,
    pub peer_skill_id: Option<String>,
    pub detail: serde_json::Value,
    pub recorded_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillUsedIn {
    pub seq: i64,
    pub member_id: MemberId,
    pub skill_id: String,
    pub execution_id: ExecutionId,
    pub score: f64,
    pub note: String,
    pub used_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillBodyRead {
    pub skill_id: String,
    pub entry_path: PathBuf,
    pub body: String,
    pub used_in: Option<SkillUsedIn>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeeklyReport {
    pub since: String,
    pub generated_at: String,
    pub claimed: Vec<SkillSetEvent>,
    pub restored: Vec<SkillSetEvent>,
    pub synced: Vec<SkillSetEvent>,
    pub overlap_suggested: Vec<SkillSetEvent>,
    pub forgetting_section: String,
}

pub struct SkillSetService<'a> {
    store: &'a Store,
    knowledge: KnowledgeRegistry<'a>,
    overlap_threshold: f64,
}

impl<'a> SkillSetService<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self {
            store,
            knowledge: KnowledgeRegistry::new(store),
            overlap_threshold: DEFAULT_OVERLAP_THRESHOLD,
        }
    }

    pub fn with_overlap_threshold(mut self, threshold: f64) -> Self {
        self.overlap_threshold = threshold;
        self
    }

    pub fn list_member_skills(&self, member: &MemberId) -> Result<Vec<MemberSkillMembership>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT member_id, skill_id, membership_state, joined_at, forgotten_at
             FROM member_skill_memberships
             WHERE member_id = ?1 AND membership_state = 'active'
             ORDER BY joined_at",
        )?;
        let rows = stmt.query_map([member.as_str()], map_membership)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn membership(
        &self,
        member: &MemberId,
        skill_id: &str,
    ) -> Result<Option<MemberSkillMembership>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT member_id, skill_id, membership_state, joined_at, forgotten_at
             FROM member_skill_memberships WHERE member_id = ?1 AND skill_id = ?2",
        )?;
        let row = stmt
            .query_row(rusqlite::params![member.as_str(), skill_id], map_membership)
            .optional()?;
        Ok(row)
    }

    /// Claim a team skill into the member's set (callback path). Idempotent
    /// when already active; re-activates forgotten skills without a second
    /// initial 1.0.
    pub fn claim(
        &self,
        member: &MemberId,
        skill_id: &str,
        execution_id: Option<&ExecutionId>,
    ) -> Result<MemberSkillMembership> {
        self.require_team_skill(skill_id)?;
        if let Some(existing) = self.membership(member, skill_id)? {
            return match existing.state {
                MembershipState::Active => Ok(existing),
                MembershipState::Forgotten => {
                    self.restore_membership(member, skill_id, "claimed_after_use")?;
                    self.membership(member, skill_id)?
                        .ok_or_else(|| Error::Validation("restore left no membership row".into()))
                }
            };
        }
        let now = utc_now();
        self.store.connection().execute(
            "INSERT INTO member_skill_memberships(member_id, skill_id, membership_state, joined_at)
             VALUES (?1, ?2, 'active', ?3)",
            rusqlite::params![member.as_str(), skill_id, now],
        )?;
        self.append_event(member, skill_id, "claimed", None, json!({}))?;
        self.record_initial_use(member, skill_id, execution_id)?;
        self.check_overlap_and_record(member, skill_id)?;
        self.membership(member, skill_id)?
            .ok_or_else(|| Error::Validation("claim left no membership row".into()))
    }

    /// Owner/manual restore — does not add initial 1.0.
    pub fn restore_manual(
        &self,
        member: &MemberId,
        skill_id: &str,
    ) -> Result<MemberSkillMembership> {
        self.require_team_skill(skill_id)?;
        if self.membership(member, skill_id)?.is_none() {
            let now = utc_now();
            self.store.connection().execute(
                "INSERT INTO member_skill_memberships(member_id, skill_id, membership_state, joined_at)
                 VALUES (?1, ?2, 'active', ?3)",
                rusqlite::params![member.as_str(), skill_id, now],
            )?;
            self.append_event(member, skill_id, "restored", None, json!({"manual": true}))?;
        } else {
            self.restore_membership(member, skill_id, "manual_restore")?;
        }
        self.membership(member, skill_id)?
            .ok_or_else(|| Error::Validation("restore left no membership row".into()))
    }

    /// Sync an existing team skill into another member's set (Neo-style download).
    pub fn sync_existing(
        &self,
        target: &MemberId,
        skill_id: &str,
        execution_id: Option<&ExecutionId>,
    ) -> Result<MemberSkillMembership> {
        self.require_team_skill(skill_id)?;
        if let Some(existing) = self.membership(target, skill_id)? {
            if existing.state == MembershipState::Active {
                return Ok(existing);
            }
            self.restore_membership(target, skill_id, "sync_reactivate")?;
        } else {
            let now = utc_now();
            self.store.connection().execute(
                "INSERT INTO member_skill_memberships(member_id, skill_id, membership_state, joined_at)
                 VALUES (?1, ?2, 'active', ?3)",
                rusqlite::params![target.as_str(), skill_id, now],
            )?;
            self.record_initial_use(target, skill_id, execution_id)?;
            self.check_overlap_and_record(target, skill_id)?;
        }
        self.append_event(target, skill_id, "synced", None, json!({}))?;
        self.membership(target, skill_id)?
            .ok_or_else(|| Error::Validation("sync left no membership row".into()))
    }

    /// Register a rewritten skill as a new team skill with `derived_from`, then
    /// add it to the target member's set as a new skill.
    pub fn sync_derived(
        &self,
        target: &MemberId,
        source_skill_id: &str,
        title: impl Into<String>,
        entry_path: PathBuf,
        provenance: impl Into<String>,
        execution_id: Option<&ExecutionId>,
    ) -> Result<SkillRegistration> {
        self.require_team_skill(source_skill_id)?;
        let registration =
            self.knowledge
                .register_skill(title, entry_path, provenance, None, vec![])?;
        self.store.connection().execute(
            "UPDATE skills SET derived_from = ?2 WHERE skill_id = ?1",
            rusqlite::params![registration.skill_id, source_skill_id],
        )?;
        let now = utc_now();
        self.store.connection().execute(
            "INSERT INTO member_skill_memberships(member_id, skill_id, membership_state, joined_at)
             VALUES (?1, ?2, 'active', ?3)",
            rusqlite::params![target.as_str(), registration.skill_id, now],
        )?;
        self.append_event(
            target,
            &registration.skill_id,
            "synced",
            Some(source_skill_id),
            json!({"derived": true}),
        )?;
        self.record_initial_use(target, &registration.skill_id, execution_id)?;
        self.check_overlap_and_record(target, &registration.skill_id)?;
        Ok(registration)
    }

    pub fn skill_derived_from(&self, skill_id: &str) -> Result<Option<String>> {
        let mut stmt = self
            .store
            .connection()
            .prepare("SELECT derived_from FROM skills WHERE skill_id = ?1")?;
        let row = stmt
            .query_row([skill_id], |row| row.get::<_, Option<String>>(0))
            .optional()?;
        Ok(row.flatten())
    }

    /// Read skill body for a member. Records at most one `used_in` row per
    /// execution; restores forgotten skills on read without a second initial 1.0.
    pub fn get_body(
        &self,
        member: &MemberId,
        skill_id: &str,
        execution_id: &ExecutionId,
        score_on_read: f64,
    ) -> Result<SkillBodyRead> {
        let skill = self
            .knowledge
            .skill(skill_id)?
            .ok_or_else(|| Error::NotFound {
                entity: "skill",
                id: skill_id.to_string(),
            })?;
        if let Some(m) = self.membership(member, skill_id)? {
            if m.state == MembershipState::Forgotten {
                self.restore_membership(member, skill_id, "used_after_forgotten")?;
            }
        } else {
            return Err(Error::Validation(format!(
                "skill `{skill_id}` is not in member {} skill set; claim or sync first",
                member.as_str()
            )));
        }
        let body = std::fs::read_to_string(&skill.entry_path).map_err(|err| {
            Error::Validation(format!(
                "skill body unreadable at {}: {err}",
                skill.entry_path.display()
            ))
        })?;
        let used_in = self.record_read_use(member, skill_id, execution_id, score_on_read)?;
        Ok(SkillBodyRead {
            skill_id: skill_id.to_string(),
            entry_path: skill.entry_path,
            body,
            used_in,
        })
    }

    pub fn search(
        &self,
        member: &MemberId,
        query: &str,
        include_team_library: bool,
    ) -> Result<Vec<SkillRegistration>> {
        let needle = normalize_tokens(query);
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let mut hits = Vec::new();
        if include_team_library {
            for skill in self.knowledge.list_skills(true)? {
                if skill_matches(&needle, &self.knowledge, &skill)? {
                    hits.push(skill);
                }
            }
        } else {
            for m in self.list_member_skills(member)? {
                if let Some(skill) = self.knowledge.skill(&m.skill_id)? {
                    if skill_matches(&needle, &self.knowledge, &skill)? {
                        hits.push(skill);
                    }
                }
            }
        }
        Ok(hits)
    }

    /// Mark a skill forgotten in one member's set (#119 will call this).
    pub fn mark_forgotten(&self, member: &MemberId, skill_id: &str) -> Result<()> {
        let n = self.store.connection().execute(
            "UPDATE member_skill_memberships
             SET membership_state = 'forgotten', forgotten_at = ?3
             WHERE member_id = ?1 AND skill_id = ?2 AND membership_state = 'active'",
            rusqlite::params![member.as_str(), skill_id, utc_now()],
        )?;
        if n == 0 {
            return Err(Error::NotFound {
                entity: "member skill membership",
                id: format!("{}:{skill_id}", member.as_str()),
            });
        }
        Ok(())
    }

    pub fn used_in_for_member_skill(
        &self,
        member: &MemberId,
        skill_id: &str,
    ) -> Result<Vec<SkillUsedIn>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT seq, member_id, skill_id, execution_id, score, note, used_at
             FROM skill_used_in WHERE member_id = ?1 AND skill_id = ?2 ORDER BY seq",
        )?;
        let rows = stmt.query_map(rusqlite::params![member.as_str(), skill_id], map_used_in)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn weekly_report(&self) -> Result<WeeklyReport> {
        let conn = self.store.connection();
        let (since, last_seq): (String, i64) = conn.query_row(
            "SELECT last_run_at, last_event_seq FROM skill_weekly_reports WHERE report_key = 'default'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut stmt = conn.prepare(
            "SELECT seq, member_id, skill_id, event_kind, peer_skill_id, detail_json, recorded_at
             FROM member_skill_set_events
             WHERE seq > ?1 OR recorded_at > ?2
             ORDER BY seq",
        )?;
        let rows = stmt.query_map(rusqlite::params![last_seq, since], map_event)?;
        let mut claimed = Vec::new();
        let mut restored = Vec::new();
        let mut synced = Vec::new();
        let mut overlap_suggested = Vec::new();
        let mut max_seq = last_seq;
        for row in rows {
            let event = row?;
            max_seq = max_seq.max(event.seq);
            match event.event_kind.as_str() {
                "claimed" => claimed.push(event),
                "restored" => restored.push(event),
                "synced" => synced.push(event),
                "overlap_suggested" => overlap_suggested.push(event),
                _ => {}
            }
        }
        let generated_at = utc_now();
        conn.execute(
            "UPDATE skill_weekly_reports SET last_run_at = ?1, last_event_seq = ?2
             WHERE report_key = 'default'",
            rusqlite::params![generated_at, max_seq],
        )?;
        Ok(WeeklyReport {
            since,
            generated_at,
            claimed,
            restored,
            synced,
            overlap_suggested,
            forgetting_section: "尚未启用（技能权重与遗忘见 issue #119）".into(),
        })
    }

    fn require_team_skill(&self, skill_id: &str) -> Result<()> {
        if self.knowledge.skill(skill_id)?.is_some() {
            return Ok(());
        }
        Err(Error::NotFound {
            entity: "skill",
            id: skill_id.to_string(),
        })
    }

    fn restore_membership(&self, member: &MemberId, skill_id: &str, reason: &str) -> Result<()> {
        let Some(existing) = self.membership(member, skill_id)? else {
            return Err(Error::NotFound {
                entity: "member skill membership",
                id: format!("{}:{skill_id}", member.as_str()),
            });
        };
        if existing.state == MembershipState::Active {
            return Ok(());
        }
        let n = self.store.connection().execute(
            "UPDATE member_skill_memberships
             SET membership_state = 'active', forgotten_at = NULL
             WHERE member_id = ?1 AND skill_id = ?2 AND membership_state = 'forgotten'",
            rusqlite::params![member.as_str(), skill_id],
        )?;
        if n == 0 {
            return Err(Error::NotFound {
                entity: "member skill membership",
                id: format!("{}:{skill_id}", member.as_str()),
            });
        }
        self.append_event(
            member,
            skill_id,
            "restored",
            None,
            json!({"reason": reason}),
        )?;
        Ok(())
    }

    fn record_initial_use(
        &self,
        member: &MemberId,
        skill_id: &str,
        execution_id: Option<&ExecutionId>,
    ) -> Result<()> {
        let execution_id = execution_id.cloned().unwrap_or_else(ExecutionId::new);
        self.insert_used_in(member, skill_id, &execution_id, 1.0, "initial_on_join")?;
        Ok(())
    }

    fn record_read_use(
        &self,
        member: &MemberId,
        skill_id: &str,
        execution_id: &ExecutionId,
        score: f64,
    ) -> Result<Option<SkillUsedIn>> {
        if self.has_used_in(member, skill_id, execution_id)? {
            return Ok(None);
        }
        Ok(Some(self.insert_used_in(
            member,
            skill_id,
            execution_id,
            score,
            "read_body",
        )?))
    }

    fn has_used_in(
        &self,
        member: &MemberId,
        skill_id: &str,
        execution_id: &ExecutionId,
    ) -> Result<bool> {
        let count: i64 = self.store.connection().query_row(
            "SELECT COUNT(1) FROM skill_used_in
             WHERE member_id = ?1 AND skill_id = ?2 AND execution_id = ?3",
            rusqlite::params![member.as_str(), skill_id, execution_id.as_str()],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    fn insert_used_in(
        &self,
        member: &MemberId,
        skill_id: &str,
        execution_id: &ExecutionId,
        score: f64,
        note: &str,
    ) -> Result<SkillUsedIn> {
        let now = utc_now();
        self.store.connection().execute(
            "INSERT INTO skill_used_in(member_id, skill_id, execution_id, score, note, used_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                member.as_str(),
                skill_id,
                execution_id.as_str(),
                score,
                note,
                now
            ],
        )?;
        Ok(SkillUsedIn {
            seq: self.store.connection().last_insert_rowid(),
            member_id: member.clone(),
            skill_id: skill_id.to_string(),
            execution_id: execution_id.clone(),
            score,
            note: note.to_string(),
            used_at: now,
        })
    }

    fn append_event(
        &self,
        member: &MemberId,
        skill_id: &str,
        kind: &str,
        peer_skill_id: Option<&str>,
        detail: serde_json::Value,
    ) -> Result<()> {
        self.store.connection().execute(
            "INSERT INTO member_skill_set_events(member_id, skill_id, event_kind, peer_skill_id, detail_json, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                member.as_str(),
                skill_id,
                kind,
                peer_skill_id,
                detail.to_string(),
                utc_now()
            ],
        )?;
        Ok(())
    }

    fn check_overlap_and_record(&self, member: &MemberId, skill_id: &str) -> Result<()> {
        let new_skill = self
            .knowledge
            .skill(skill_id)?
            .ok_or_else(|| Error::NotFound {
                entity: "skill",
                id: skill_id.to_string(),
            })?;
        let new_text = skill_text_blob(&self.knowledge, &new_skill)?;
        let new_tokens = token_set(&new_text);
        for m in self.list_member_skills(member)? {
            if m.skill_id == skill_id {
                continue;
            }
            let Some(other) = self.knowledge.skill(&m.skill_id)? else {
                continue;
            };
            let other_text = skill_text_blob(&self.knowledge, &other)?;
            let sim = jaccard(&new_tokens, &token_set(&other_text));
            if sim >= self.overlap_threshold {
                self.append_event(
                    member,
                    skill_id,
                    "overlap_suggested",
                    Some(&m.skill_id),
                    json!({
                        "similarity": sim,
                        "peer_title": other.entry_path.display().to_string(),
                    }),
                )?;
            }
        }
        Ok(())
    }
}

fn map_membership(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemberSkillMembership> {
    let state: String = row.get(2)?;
    Ok(MemberSkillMembership {
        member_id: MemberId::from_str(row.get::<_, String>(0)?.as_str())
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
        skill_id: row.get(1)?,
        state: if state == "active" {
            MembershipState::Active
        } else {
            MembershipState::Forgotten
        },
        joined_at: row.get(3)?,
        forgotten_at: row.get(4)?,
    })
}

fn map_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<SkillSetEvent> {
    let detail: String = row.get(5)?;
    Ok(SkillSetEvent {
        seq: row.get(0)?,
        member_id: MemberId::from_str(row.get::<_, String>(1)?.as_str())
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
        skill_id: row.get(2)?,
        event_kind: row.get(3)?,
        peer_skill_id: row.get(4)?,
        detail: serde_json::from_str(&detail).unwrap_or(json!({})),
        recorded_at: row.get(6)?,
    })
}

fn map_used_in(row: &rusqlite::Row<'_>) -> rusqlite::Result<SkillUsedIn> {
    Ok(SkillUsedIn {
        seq: row.get(0)?,
        member_id: MemberId::from_str(row.get::<_, String>(1)?.as_str())
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
        skill_id: row.get(2)?,
        execution_id: ExecutionId::from_str(row.get::<_, String>(3)?.as_str())
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
        score: row.get(4)?,
        note: row.get(5)?,
        used_at: row.get(6)?,
    })
}

fn skill_text_blob(knowledge: &KnowledgeRegistry<'_>, skill: &SkillRegistration) -> Result<String> {
    let item = knowledge
        .get_item(&skill.item_id)?
        .ok_or_else(|| Error::Validation("skill knowledge item missing".into()))?;
    let body = std::fs::read_to_string(&skill.entry_path).unwrap_or_default();
    Ok(format!("{} {}", item.title, body))
}

fn skill_matches(
    needle: &HashSet<String>,
    knowledge: &KnowledgeRegistry<'_>,
    skill: &SkillRegistration,
) -> Result<bool> {
    let hay = token_set(&skill_text_blob(knowledge, skill)?);
    Ok(needle.iter().any(|t| hay.contains(t)))
}

fn normalize_tokens(query: &str) -> HashSet<String> {
    token_set(query)
}

fn token_set(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2)
        .map(str::to_string)
        .collect()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 { 0.0 } else { inter / union }
}

use std::str::FromStr as _;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::store::{
        DOMAIN_FOUNDATION, FOUNDATION_V1_SQL, MigrationRegistry, Store,
    };
    use crate::knowledge::register_migrations;

    fn store() -> Store {
        let dir = tempfile::TempDir::new().expect("dir");
        let db = dir.path().join("viva.db");
        let frozen = register_migrations(MigrationRegistry::new().register(
            DOMAIN_FOUNDATION,
            1,
            "foundation v1",
            FOUNDATION_V1_SQL,
        ))
        .freeze()
        .expect("registry");
        let store = Store::open(&db, &frozen).expect("open");
        std::mem::forget(dir);
        store
    }

    fn write_skill(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, body).expect("write");
        path.canonicalize().expect("canonical")
    }

    #[test]
    fn claim_records_single_initial_use_and_dedupes_reads() {
        let store = store();
        let dir = tempfile::TempDir::new().expect("dir");
        let kr = KnowledgeRegistry::new(&store);
        let skill = kr
            .register_skill(
                "Alpha",
                write_skill(&dir, "SKILL.md", "alpha alpha alpha unique"),
                "test",
                None,
                vec![],
            )
            .expect("skill");
        let member = MemberId::new();
        let exec = ExecutionId::new();
        let svc = SkillSetService::new(&store);
        svc.claim(&member, &skill.skill_id, Some(&exec))
            .expect("claim");
        let uses = svc
            .used_in_for_member_skill(&member, &skill.skill_id)
            .expect("uses");
        assert_eq!(uses.len(), 1);
        assert!((uses[0].score - 1.0).abs() < f64::EPSILON);
        svc.get_body(&member, &skill.skill_id, &exec, 1.0)
            .expect("read");
        let uses = svc
            .used_in_for_member_skill(&member, &skill.skill_id)
            .expect("uses");
        assert_eq!(uses.len(), 1, "same execution must not double-count");
    }
}
