//! Per-member skill weight, decay and forgetting (issue #119).
//!
//! Skills stay in the team library. Each member keeps one weight record per
//! skill. Use events are append-only. Reading the current weight decays from
//! the stored point and does not scan history.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr as _;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::foundation::error::{Error, Result};
use crate::foundation::ids::{ExecutionId, MemberId};
use crate::foundation::records::{self, ExecutionStatus};
use crate::foundation::store::Store;

use super::KnowledgeRegistry;
use super::skill_set::SkillSetService;

pub const KNOWLEDGE_V3_SKILL_WEIGHT_SQL: &str = r#"
CREATE TABLE member_skill_weights (
    member_id              TEXT NOT NULL,
    skill_id               TEXT NOT NULL,
    weight                 REAL NOT NULL,
    track                  TEXT NOT NULL CHECK (track IN ('session', 'time')),
    track_source           TEXT NOT NULL CHECK (track_source IN ('default', 'haisu')),
    updated_at             TEXT NOT NULL,
    updated_session_seq    INTEGER NOT NULL,
    last_use_at            TEXT,
    last_use_session_seq   INTEGER,
    protection_anchor_seq  INTEGER NOT NULL,
    review                 INTEGER NOT NULL DEFAULT 0 CHECK (review IN (0, 1)),
    PRIMARY KEY (member_id, skill_id)
);

CREATE TABLE skill_weight_events (
    seq           INTEGER PRIMARY KEY AUTOINCREMENT,
    member_id     TEXT NOT NULL,
    skill_id      TEXT NOT NULL,
    event_kind    TEXT NOT NULL CHECK (event_kind IN (
                      'state_opened', 'use_applied', 'corrected', 'settled',
                      'track_set', 'projected', 'forgotten', 'would_forget',
                      'capacity_overflow')),
    execution_id  TEXT,
    score         REAL,
    session_seq   INTEGER,
    detail_json   TEXT NOT NULL DEFAULT '{}',
    recorded_at   TEXT NOT NULL
);

CREATE INDEX skill_weight_events_member_skill
    ON skill_weight_events(member_id, skill_id, seq);

CREATE TABLE member_session_index (
    member_id   TEXT NOT NULL,
    session_id  TEXT NOT NULL,
    seq         INTEGER NOT NULL,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (member_id, session_id)
);

CREATE INDEX member_session_index_member
    ON member_session_index(member_id, seq);

CREATE TABLE skill_weight_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// How long a skill keeps half its weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecayTrack {
    Session,
    Time,
}

impl DecayTrack {
    fn as_str(self) -> &'static str {
        match self {
            DecayTrack::Session => "session",
            DecayTrack::Time => "time",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "session" => Ok(DecayTrack::Session),
            "time" => Ok(DecayTrack::Time),
            other => Err(Error::Validation(format!(
                "skill track must be session or time, got `{other}`"
            ))),
        }
    }
}

/// `default` is the session track. `haisu` was set explicitly and is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackSource {
    Default,
    Haisu,
}

impl TrackSource {
    fn as_str(self) -> &'static str {
        match self {
            TrackSource::Default => "default",
            TrackSource::Haisu => "haisu",
        }
    }
}

/// Trial reports would-be forgetting and does not exit the skill set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrialMode {
    /// On for `trial_days` after weight tracking first runs.
    Auto,
    On,
    Off,
}

/// Initial values from Resident-Container §7.1. Override them in `[skill_weight]`.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillWeightConfig {
    pub session_half_life: f64,
    pub time_half_life_days: f64,
    pub forget_weight: f64,
    pub session_unused: i64,
    pub time_unused_days: f64,
    pub capacity: usize,
    pub protection_sessions: i64,
    pub success_score: f64,
    pub failure_score: f64,
    pub trial: TrialMode,
    pub trial_days: i64,
}

impl Default for SkillWeightConfig {
    fn default() -> Self {
        Self {
            session_half_life: 30.0,
            time_half_life_days: 60.0,
            forget_weight: 0.1,
            session_unused: 20,
            time_unused_days: 60.0,
            capacity: 30,
            protection_sessions: 10,
            success_score: 1.0,
            failure_score: 0.25,
            trial: TrialMode::Auto,
            trial_days: 30,
        }
    }
}

/// Current weight after lazy decay.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SkillWeightView {
    pub member_id: MemberId,
    pub skill_id: String,
    pub weight: f64,
    pub track: DecayTrack,
    pub track_source: TrackSource,
    pub review: bool,
    pub updated_at: String,
    pub updated_session_seq: i64,
    pub last_use_at: Option<String>,
    pub last_use_session_seq: Option<i64>,
    pub protection_anchor_seq: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillWeightEvent {
    pub seq: i64,
    pub member_id: MemberId,
    pub skill_id: String,
    pub event_kind: String,
    pub execution_id: Option<String>,
    pub score: Option<f64>,
    pub session_seq: Option<i64>,
    pub detail: serde_json::Value,
    pub recorded_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ForgettingDigest {
    pub forgotten: Vec<SkillWeightEvent>,
    pub would_forget: Vec<SkillWeightEvent>,
    pub capacity_overflow: Vec<SkillWeightEvent>,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionEndReport {
    pub member_id: MemberId,
    pub session_id: String,
    pub session_seq: i64,
    pub forgotten: Vec<String>,
    pub would_forget: Vec<String>,
    pub capacity_excess: i64,
}

#[derive(Debug, Clone)]
struct WeightRow {
    weight: f64,
    track: DecayTrack,
    track_source: TrackSource,
    updated_at: String,
    updated_session_seq: i64,
    last_use_at: Option<String>,
    last_use_session_seq: Option<i64>,
    protection_anchor_seq: i64,
    review: bool,
}

#[derive(Debug, Clone)]
struct UseRow {
    execution_id: String,
    score: f64,
    note: String,
}

struct ExecUse {
    member_id: String,
    skill_id: String,
    use_row: UseRow,
}

pub struct SkillWeightService<'a> {
    store: &'a Store,
    home: Option<PathBuf>,
    config: SkillWeightConfig,
    now: OffsetDateTime,
}

impl<'a> SkillWeightService<'a> {
    /// Config and `soul.md` come from `home`. The clock is the real current time.
    pub fn open(store: &'a Store, home: &Path) -> Result<Self> {
        Ok(Self {
            store,
            home: Some(home.to_path_buf()),
            config: load_config(home)?,
            now: OffsetDateTime::now_utc(),
        })
    }

    /// Built-in parameters and no `soul.md`.
    pub fn with_defaults(store: &'a Store) -> Self {
        Self {
            store,
            home: None,
            config: SkillWeightConfig::default(),
            now: OffsetDateTime::now_utc(),
        }
    }

    /// Tests inject the clock and parameters. `home` is where `soul.md` lives.
    pub fn with_parts(
        store: &'a Store,
        home: Option<PathBuf>,
        config: SkillWeightConfig,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            store,
            home,
            config,
            now,
        }
    }

    pub fn config(&self) -> &SkillWeightConfig {
        &self.config
    }

    /// Fold `used_in` rows that have no weight event yet. Does not add a second
    /// initial 1.0. The protection window starts when this record first opens.
    pub fn absorb(&self, member: &MemberId, skill_id: &str) -> Result<()> {
        if !self.membership_exists(member, skill_id)? {
            return Ok(());
        }
        self.ensure_opened(member, skill_id)?;
        let pending = self.pending_uses(member, skill_id)?;
        if pending.is_empty() {
            if self.load_row(member, skill_id)?.is_none() {
                self.recompute(member, skill_id)?;
            }
            return Ok(());
        }
        let seq = self.current_session_seq(member)?;
        for use_row in pending {
            self.append_event(
                member,
                skill_id,
                "use_applied",
                Some(&use_row.execution_id),
                Some(use_row.score),
                Some(seq),
                json!({"note": use_row.note}),
            )?;
        }
        self.recompute(member, skill_id)?;
        Ok(())
    }

    /// Current weight. Decays from the stored point and does not replay events.
    pub fn read(&self, member: &MemberId, skill_id: &str) -> Result<SkillWeightView> {
        self.absorb(member, skill_id)?;
        self.materialize(member, skill_id)
    }

    pub fn read_active(&self, member: &MemberId) -> Result<Vec<SkillWeightView>> {
        let mut views = Vec::new();
        for membership in self.sets().list_member_skills(member)? {
            views.push(self.read(member, &membership.skill_id)?);
        }
        Ok(views)
    }

    /// Replay append-only events into the state row.
    pub fn rebuild(&self, member: &MemberId, skill_id: &str) -> Result<SkillWeightView> {
        self.ensure_opened(member, skill_id)?;
        self.recompute(member, skill_id)?;
        self.load_view(member, skill_id)
    }

    /// Pin the track. A later default does not put it back on the session track.
    pub fn assign_track(
        &self,
        member: &MemberId,
        skill_id: &str,
        track: DecayTrack,
    ) -> Result<SkillWeightView> {
        self.absorb(member, skill_id)?;
        if self.load_row(member, skill_id)?.is_none() {
            self.recompute(member, skill_id)?;
        }
        let _ = self.materialize(member, skill_id)?;
        let seq = self.current_session_seq(member)?;
        self.append_event(
            member,
            skill_id,
            "track_set",
            None,
            None,
            Some(seq),
            json!({"track": track.as_str()}),
        )?;
        self.recompute(member, skill_id)?;
        self.load_view(member, skill_id)
    }

    /// Set that Execution's use to 0, flag the skill for review, and rebuild.
    pub fn correct(
        &self,
        execution_id: &ExecutionId,
        skill_id: &str,
        member: Option<&MemberId>,
    ) -> Result<SkillWeightView> {
        let member = self.resolve_use_member(execution_id, skill_id, member)?;
        if !self.has_event_exec(&member, skill_id, "corrected", execution_id.as_str())? {
            let seq = self.current_session_seq(&member)?;
            self.append_event(
                &member,
                skill_id,
                "corrected",
                Some(execution_id.as_str()),
                Some(0.0),
                Some(seq),
                json!({"review": true}),
            )?;
        }
        self.recompute(&member, skill_id)?;
        self.load_view(&member, skill_id)
    }

    /// Move body-read scores onto the Execution's end status. Join scores stay.
    pub fn settle_execution(&self, execution_id: &ExecutionId) -> Result<()> {
        let Some(record) = records::get_execution(self.store, execution_id)? else {
            return Ok(());
        };
        let target = match record.status {
            ExecutionStatus::Failed | ExecutionStatus::Stopped => self.config.failure_score,
            ExecutionStatus::Completed => self.config.success_score,
            ExecutionStatus::Requested | ExecutionStatus::Running => return Ok(()),
        };
        for use_row in self.uses_for_execution(execution_id.as_str())? {
            if use_row.use_row.note != "read_body" {
                continue;
            }
            let member = MemberId::from_str(&use_row.member_id)?;
            if self.has_event_exec(
                &member,
                &use_row.skill_id,
                "corrected",
                execution_id.as_str(),
            )? {
                continue;
            }
            let effective = self.effective_score(&member, &use_row.skill_id, &use_row.use_row)?;
            if (effective - target).abs() < 1e-12 {
                continue;
            }
            let seq = self.current_session_seq(&member)?;
            self.append_event(
                &member,
                &use_row.skill_id,
                "settled",
                Some(execution_id.as_str()),
                Some(target),
                Some(seq),
                json!({"status": record.status.as_str()}),
            )?;
            self.recompute(&member, &use_row.skill_id)?;
        }
        Ok(())
    }

    /// Failure and interruption score 0.25. Anything not ended yet scores 1.0
    /// until [`Self::settle_execution`] runs.
    pub fn score_for_execution(&self, execution_id: &ExecutionId) -> Result<f64> {
        let Some(record) = records::get_execution(self.store, execution_id)? else {
            return Ok(self.config.success_score);
        };
        Ok(match record.status {
            ExecutionStatus::Failed | ExecutionStatus::Stopped => self.config.failure_score,
            _ => self.config.success_score,
        })
    }

    /// Projection is an observation only. Weight and `used_in` stay put.
    pub fn observe_projection(
        &self,
        member: &MemberId,
        skill_id: &str,
        execution_id: &ExecutionId,
    ) -> Result<()> {
        let seq = self.current_session_seq(member)?;
        self.append_event(
            member,
            skill_id,
            "projected",
            Some(execution_id.as_str()),
            Some(0.0),
            Some(seq),
            json!({"counts_as_use": false}),
        )?;
        Ok(())
    }

    /// Record harness body-reads from a recovered session. Same dedup as a CLI read.
    pub fn ingest_transcript(
        &self,
        member: &MemberId,
        execution_id: &ExecutionId,
        transcript: &str,
    ) -> Result<Vec<String>> {
        let score = self.score_for_execution(execution_id)?;
        let knowledge = KnowledgeRegistry::new(self.store);
        let sets = self.sets();
        let mut recorded = Vec::new();
        for skill_id in self.member_skill_ids(member)? {
            let Some(skill) = knowledge.skill(&skill_id)? else {
                continue;
            };
            let path = skill.entry_path.display().to_string();
            if !transcript_reads_skill_body(transcript, &skill_id, &path) {
                continue;
            }
            sets.get_body(member, &skill_id, execution_id, score)?;
            recorded.push(skill_id);
        }
        Ok(recorded)
    }

    /// Count one member session and run forgetting. The same session id does not
    /// advance the index twice.
    pub fn on_session_ended(
        &self,
        member: &MemberId,
        session_id: &str,
    ) -> Result<SessionEndReport> {
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return Err(Error::Validation(
                "session end needs a non-empty session id".into(),
            ));
        }
        self.settle_session(member, session_id)?;
        let session_seq = self.observe_session(member, session_id)?;
        for membership in self.sets().list_member_skills(member)? {
            self.absorb(member, &membership.skill_id)?;
            let _ = self.materialize(member, &membership.skill_id)?;
        }
        let (forgotten, would_forget, capacity_excess) = self.forget_pass(member)?;
        Ok(SessionEndReport {
            member_id: member.clone(),
            session_id: session_id.to_string(),
            session_seq,
            forgotten,
            would_forget,
            capacity_excess,
        })
    }

    pub fn forgetting_since(&self, since: &str) -> Result<ForgettingDigest> {
        let mut stmt = self.store.connection().prepare(
            "SELECT seq, member_id, skill_id, event_kind, execution_id, score,
                    session_seq, detail_json, recorded_at
             FROM skill_weight_events
             WHERE recorded_at > ?1
               AND event_kind IN ('forgotten', 'would_forget', 'capacity_overflow')
             ORDER BY seq",
        )?;
        let rows = stmt.query_map([since], map_event)?;
        let mut forgotten = Vec::new();
        let mut would_forget = Vec::new();
        let mut capacity_overflow = Vec::new();
        for row in rows {
            let event = row?;
            match event.event_kind.as_str() {
                "forgotten" => forgotten.push(event),
                "would_forget" => would_forget.push(event),
                "capacity_overflow" => capacity_overflow.push(event),
                _ => {}
            }
        }
        let summary =
            if forgotten.is_empty() && would_forget.is_empty() && capacity_overflow.is_empty() {
                "本周没有遗忘。".to_string()
            } else {
                format!(
                    "遗忘 {} 个；试运行将遗忘 {} 个；容量超额 {} 条。",
                    forgotten.len(),
                    would_forget.len(),
                    capacity_overflow.len()
                )
            };
        Ok(ForgettingDigest {
            forgotten,
            would_forget,
            capacity_overflow,
            summary,
        })
    }

    pub fn trial_active(&self) -> Result<bool> {
        match self.config.trial {
            TrialMode::On => Ok(true),
            TrialMode::Off => Ok(false),
            TrialMode::Auto => {
                let enabled = self.ensure_enabled_at()?;
                Ok(days_between(enabled, self.now) < self.config.trial_days as f64)
            }
        }
    }

    fn sets(&self) -> SkillSetService<'a> {
        match &self.home {
            Some(home) => SkillSetService::new(self.store).with_home(home.clone()),
            None => SkillSetService::new(self.store),
        }
    }

    fn timestamp(&self) -> String {
        format_time(self.now)
    }

    fn ensure_enabled_at(&self) -> Result<OffsetDateTime> {
        let existing: Option<String> = self
            .store
            .connection()
            .query_row(
                "SELECT value FROM skill_weight_meta WHERE key = 'enabled_at'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(value) = existing {
            return parse_time(&value);
        }
        let stamp = self.timestamp();
        self.store.connection().execute(
            "INSERT INTO skill_weight_meta(key, value) VALUES ('enabled_at', ?1)",
            [&stamp],
        )?;
        Ok(self.now)
    }

    fn ensure_opened(&self, member: &MemberId, skill_id: &str) -> Result<()> {
        if self.has_kind(member, skill_id, "state_opened")? {
            return Ok(());
        }
        let seq = self.current_session_seq(member)?;
        self.append_event(
            member,
            skill_id,
            "state_opened",
            None,
            None,
            Some(seq),
            json!({"protection_anchor_seq": seq}),
        )?;
        Ok(())
    }

    fn recompute(&self, member: &MemberId, skill_id: &str) -> Result<()> {
        let events = self.events_for(member, skill_id)?;
        let uses = self.uses_for(member, skill_id)?;
        let mut corrected = HashSet::new();
        let mut settled: HashMap<String, f64> = HashMap::new();
        for event in &events {
            let Some(exec_id) = event.execution_id.as_deref() else {
                continue;
            };
            match event.event_kind.as_str() {
                "corrected" => {
                    corrected.insert(exec_id.to_string());
                }
                "settled" => {
                    if let Some(score) = event.score {
                        settled.insert(exec_id.to_string(), score);
                    }
                }
                _ => {}
            }
        }

        let mut weight = 0.0;
        let mut track = DecayTrack::Session;
        let mut track_source = TrackSource::Default;
        let mut anchor = 0_i64;
        let mut cursor_at = self.now;
        let mut cursor_seq = 0_i64;
        let mut started = false;
        let mut review = false;
        let mut last_use_at: Option<OffsetDateTime> = None;
        let mut last_use_seq: Option<i64> = None;

        for event in &events {
            if event.event_kind == "corrected" {
                review = true;
            }
            let at = parse_time(&event.recorded_at)?;
            let at_seq = event.session_seq.unwrap_or(cursor_seq);
            match event.event_kind.as_str() {
                "state_opened" if !started => {
                    cursor_at = at;
                    cursor_seq = at_seq;
                    anchor = at_seq;
                    started = true;
                }
                "track_set" => {
                    if !started {
                        cursor_at = at;
                        cursor_seq = at_seq;
                        anchor = at_seq;
                        started = true;
                    }
                    weight = decay_between(
                        weight,
                        track,
                        cursor_at,
                        cursor_seq,
                        at,
                        at_seq,
                        &self.config,
                    );
                    cursor_at = at;
                    cursor_seq = at_seq;
                    if let Some(name) = event.detail.get("track").and_then(|value| value.as_str()) {
                        track = DecayTrack::parse(name)?;
                    }
                    track_source = TrackSource::Haisu;
                }
                "use_applied" => {
                    if !started {
                        cursor_at = at;
                        cursor_seq = at_seq;
                        anchor = at_seq;
                        started = true;
                    }
                    weight = decay_between(
                        weight,
                        track,
                        cursor_at,
                        cursor_seq,
                        at,
                        at_seq,
                        &self.config,
                    );
                    cursor_at = at;
                    cursor_seq = at_seq;
                    let exec_id = event.execution_id.clone().unwrap_or_default();
                    let score = score_of(&exec_id, &uses, &corrected, &settled);
                    weight += score;
                    if score > 0.0 {
                        last_use_at = Some(at);
                        last_use_seq = Some(at_seq);
                    }
                }
                _ => {}
            }
        }

        let cur_seq = self.current_session_seq(member)?;
        if started {
            weight = decay_between(
                weight,
                track,
                cursor_at,
                cursor_seq,
                self.now,
                cur_seq,
                &self.config,
            );
        }
        self.save_row(
            member,
            skill_id,
            &WeightRow {
                weight,
                track,
                track_source,
                updated_at: self.timestamp(),
                updated_session_seq: cur_seq,
                last_use_at: last_use_at.map(format_time),
                last_use_session_seq: last_use_seq,
                protection_anchor_seq: anchor,
                review,
            },
        )
    }

    fn materialize(&self, member: &MemberId, skill_id: &str) -> Result<SkillWeightView> {
        let Some(row) = self.load_row(member, skill_id)? else {
            return Err(Error::NotFound {
                entity: "skill weight",
                id: format!("{}:{skill_id}", member.as_str()),
            });
        };
        let cur_seq = self.current_session_seq(member)?;
        let from_at = parse_time(&row.updated_at)?;
        let weight = decay_between(
            row.weight,
            row.track,
            from_at,
            row.updated_session_seq,
            self.now,
            cur_seq,
            &self.config,
        );
        let moved = match row.track {
            DecayTrack::Session => cur_seq > row.updated_session_seq,
            DecayTrack::Time => days_between(from_at, self.now) > 0.0,
        };
        if moved {
            self.store.connection().execute(
                "UPDATE member_skill_weights
                 SET weight = ?1, updated_at = ?2, updated_session_seq = ?3
                 WHERE member_id = ?4 AND skill_id = ?5",
                rusqlite::params![weight, self.timestamp(), cur_seq, member.as_str(), skill_id],
            )?;
        }
        self.load_view(member, skill_id)
    }

    fn forget_pass(&self, member: &MemberId) -> Result<(Vec<String>, Vec<String>, i64)> {
        let innate = self.innate_ids(member)?;
        let seq = self.current_session_seq(member)?;
        let trial = self.trial_active()?;
        let mut rows = Vec::new();
        for membership in self.sets().list_member_skills(member)? {
            if let Some(row) = self.load_row(member, &membership.skill_id)? {
                rows.push((membership.skill_id, row));
            }
        }
        let non_innate: Vec<_> = rows.iter().filter(|(id, _)| !innate.contains(id)).collect();
        let mut planned: Vec<(String, &'static str)> = Vec::new();
        for (id, row) in &non_innate {
            if !self.in_protection(seq, row) && self.threshold_hit(seq, row) {
                planned.push(((*id).clone(), "threshold"));
            }
        }
        let mut remaining = non_innate.len() - planned.len();
        while remaining > self.config.capacity {
            let next = non_innate
                .iter()
                .filter(|(id, row)| {
                    !self.in_protection(seq, row)
                        && !planned.iter().any(|(planned_id, _)| *planned_id == **id)
                })
                .min_by(|left, right| {
                    left.1
                        .weight
                        .total_cmp(&right.1.weight)
                        .then_with(|| left.0.cmp(&right.0))
                });
            match next {
                Some((id, _)) => {
                    planned.push(((*id).clone(), "capacity"));
                    remaining -= 1;
                }
                None => break,
            }
        }
        let excess = remaining.saturating_sub(self.config.capacity) as i64;
        let mut forgotten = Vec::new();
        let mut would_forget = Vec::new();
        for (skill_id, reason) in planned {
            if trial {
                if !self.last_kind_is(member, &skill_id, "would_forget")? {
                    self.append_event(
                        member,
                        &skill_id,
                        "would_forget",
                        None,
                        None,
                        Some(seq),
                        json!({"reason": reason}),
                    )?;
                }
                would_forget.push(skill_id);
            } else {
                self.forget_now(member, &skill_id, reason, seq)?;
                forgotten.push(skill_id);
            }
        }
        if excess > 0 {
            self.append_event(
                member,
                "*",
                "capacity_overflow",
                None,
                None,
                Some(seq),
                json!({"excess": excess}),
            )?;
        }
        Ok((forgotten, would_forget, excess))
    }

    fn in_protection(&self, seq: i64, row: &WeightRow) -> bool {
        seq.saturating_sub(row.protection_anchor_seq) <= self.config.protection_sessions
    }

    fn threshold_hit(&self, seq: i64, row: &WeightRow) -> bool {
        if row.weight >= self.config.forget_weight {
            return false;
        }
        match row.track {
            DecayTrack::Session => match row.last_use_session_seq {
                Some(last) => seq.saturating_sub(last) >= self.config.session_unused,
                None => true,
            },
            DecayTrack::Time => match row.last_use_at.as_deref() {
                Some(stamp) => match parse_time(stamp) {
                    Ok(then) => days_between(then, self.now) >= self.config.time_unused_days,
                    Err(_) => false,
                },
                None => true,
            },
        }
    }

    fn forget_now(&self, member: &MemberId, skill_id: &str, reason: &str, seq: i64) -> Result<()> {
        let tx = self.store.transaction()?;
        let updated = tx.execute(
            "UPDATE member_skill_memberships
             SET membership_state = 'forgotten', forgotten_at = ?3
             WHERE member_id = ?1 AND skill_id = ?2 AND membership_state = 'active'",
            rusqlite::params![member.as_str(), skill_id, self.timestamp()],
        )?;
        if updated == 0 {
            tx.commit()?;
            return Ok(());
        }
        tx.execute(
            "INSERT INTO skill_weight_events(
                member_id, skill_id, event_kind, execution_id, score, session_seq,
                detail_json, recorded_at
             ) VALUES (?1, ?2, 'forgotten', NULL, NULL, ?3, ?4, ?5)",
            rusqlite::params![
                member.as_str(),
                skill_id,
                seq,
                json!({"reason": reason}).to_string(),
                self.timestamp()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn settle_session(&self, member: &MemberId, session_id: &str) -> Result<()> {
        let mut stmt = self.store.connection().prepare(
            "SELECT execution_id FROM executions WHERE session_id = ?1 AND member_id = ?2",
        )?;
        let ids = stmt.query_map(rusqlite::params![session_id, member.as_str()], |row| {
            row.get::<_, String>(0)
        })?;
        for id in ids {
            if let Ok(execution_id) = ExecutionId::from_str(&id?) {
                self.settle_execution(&execution_id)?;
            }
        }
        Ok(())
    }

    fn observe_session(&self, member: &MemberId, session_id: &str) -> Result<i64> {
        self.sync_sessions_table(member)?;
        if let Some(seq) = self.session_seq_of(member, session_id)? {
            return Ok(seq);
        }
        let next = self.max_session_seq(member)? + 1;
        self.store.connection().execute(
            "INSERT INTO member_session_index(member_id, session_id, seq, recorded_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![member.as_str(), session_id, next, self.timestamp()],
        )?;
        Ok(next)
    }

    fn current_session_seq(&self, member: &MemberId) -> Result<i64> {
        self.sync_sessions_table(member)?;
        self.max_session_seq(member)
    }

    fn sync_sessions_table(&self, member: &MemberId) -> Result<()> {
        let mut stmt = self.store.connection().prepare(
            "SELECT session_id FROM sessions WHERE member_id = ?1 ORDER BY created_at, session_id",
        )?;
        let ids = stmt.query_map([member.as_str()], |row| row.get::<_, String>(0))?;
        for id in ids {
            let id = id?;
            if self.session_seq_of(member, &id)?.is_none() {
                let next = self.max_session_seq(member)? + 1;
                self.store.connection().execute(
                    "INSERT INTO member_session_index(member_id, session_id, seq, recorded_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![member.as_str(), id, next, self.timestamp()],
                )?;
            }
        }
        Ok(())
    }

    fn session_seq_of(&self, member: &MemberId, session_id: &str) -> Result<Option<i64>> {
        Ok(self
            .store
            .connection()
            .query_row(
                "SELECT seq FROM member_session_index WHERE member_id = ?1 AND session_id = ?2",
                rusqlite::params![member.as_str(), session_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    fn max_session_seq(&self, member: &MemberId) -> Result<i64> {
        let seq: Option<i64> = self
            .store
            .connection()
            .query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM member_session_index WHERE member_id = ?1",
                [member.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(seq.unwrap_or(0))
    }

    fn innate_ids(&self, member: &MemberId) -> Result<BTreeSet<String>> {
        let Some(home) = &self.home else {
            return Ok(BTreeSet::new());
        };
        match std::fs::read_to_string(member_soul_path(home, member)) {
            Ok(text) => Ok(parse_innate_skills(&text)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
            Err(err) => Err(err.into()),
        }
    }

    fn membership_exists(&self, member: &MemberId, skill_id: &str) -> Result<bool> {
        let count: i64 = self.store.connection().query_row(
            "SELECT COUNT(1) FROM member_skill_memberships WHERE member_id = ?1 AND skill_id = ?2",
            rusqlite::params![member.as_str(), skill_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    fn member_skill_ids(&self, member: &MemberId) -> Result<Vec<String>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT skill_id FROM member_skill_memberships WHERE member_id = ?1 ORDER BY skill_id",
        )?;
        let rows = stmt.query_map([member.as_str()], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn pending_uses(&self, member: &MemberId, skill_id: &str) -> Result<Vec<UseRow>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT execution_id, score, note FROM skill_used_in u
             WHERE member_id = ?1 AND skill_id = ?2
               AND NOT EXISTS (
                   SELECT 1 FROM skill_weight_events e
                   WHERE e.member_id = u.member_id AND e.skill_id = u.skill_id
                     AND e.execution_id = u.execution_id AND e.event_kind = 'use_applied'
               )
             ORDER BY seq",
        )?;
        let rows = stmt.query_map(rusqlite::params![member.as_str(), skill_id], map_use)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn uses_for(&self, member: &MemberId, skill_id: &str) -> Result<Vec<UseRow>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT execution_id, score, note FROM skill_used_in
             WHERE member_id = ?1 AND skill_id = ?2 ORDER BY seq",
        )?;
        let rows = stmt.query_map(rusqlite::params![member.as_str(), skill_id], map_use)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn uses_for_execution(&self, execution_id: &str) -> Result<Vec<ExecUse>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT member_id, skill_id, execution_id, score, note FROM skill_used_in
             WHERE execution_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([execution_id], |row| {
            Ok(ExecUse {
                member_id: row.get(0)?,
                skill_id: row.get(1)?,
                use_row: UseRow {
                    execution_id: row.get(2)?,
                    score: row.get(3)?,
                    note: row.get(4)?,
                },
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn effective_score(&self, member: &MemberId, skill_id: &str, use_row: &UseRow) -> Result<f64> {
        if self.has_event_exec(member, skill_id, "corrected", &use_row.execution_id)? {
            return Ok(0.0);
        }
        if use_row.note == "read_body" {
            if let Some(score) = self.latest_settled(member, skill_id, &use_row.execution_id)? {
                return Ok(score);
            }
        }
        Ok(use_row.score)
    }

    fn latest_settled(
        &self,
        member: &MemberId,
        skill_id: &str,
        execution_id: &str,
    ) -> Result<Option<f64>> {
        let score = self
            .store
            .connection()
            .query_row(
                "SELECT score FROM skill_weight_events
                 WHERE member_id = ?1 AND skill_id = ?2 AND execution_id = ?3
                   AND event_kind = 'settled'
                 ORDER BY seq DESC LIMIT 1",
                rusqlite::params![member.as_str(), skill_id, execution_id],
                |row| row.get::<_, Option<f64>>(0),
            )
            .optional()?;
        Ok(score.flatten())
    }

    fn resolve_use_member(
        &self,
        execution_id: &ExecutionId,
        skill_id: &str,
        member: Option<&MemberId>,
    ) -> Result<MemberId> {
        let mut stmt = self.store.connection().prepare(
            "SELECT member_id FROM skill_used_in
             WHERE execution_id = ?1 AND skill_id = ?2 ORDER BY seq",
        )?;
        let rows = stmt.query_map(rusqlite::params![execution_id.as_str(), skill_id], |row| {
            row.get::<_, String>(0)
        })?;
        let mut found = Vec::new();
        for row in rows {
            found.push(MemberId::from_str(&row?)?);
        }
        if let Some(member) = member {
            if found.iter().any(|id| id == member) {
                return Ok(member.clone());
            }
            return Err(Error::NotFound {
                entity: "skill use",
                id: format!("{}:{skill_id}", execution_id.as_str()),
            });
        }
        match found.len() {
            0 => Err(Error::NotFound {
                entity: "skill use",
                id: format!("{}:{skill_id}", execution_id.as_str()),
            }),
            1 => Ok(found.remove(0)),
            _ => Err(Error::Validation(
                "more than one member used this skill in the execution; pass --member".into(),
            )),
        }
    }

    fn events_for(&self, member: &MemberId, skill_id: &str) -> Result<Vec<SkillWeightEvent>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT seq, member_id, skill_id, event_kind, execution_id, score,
                    session_seq, detail_json, recorded_at
             FROM skill_weight_events
             WHERE member_id = ?1 AND skill_id = ?2
             ORDER BY seq",
        )?;
        let rows = stmt.query_map(rusqlite::params![member.as_str(), skill_id], map_event)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn has_kind(&self, member: &MemberId, skill_id: &str, kind: &str) -> Result<bool> {
        let count: i64 = self.store.connection().query_row(
            "SELECT COUNT(1) FROM skill_weight_events
             WHERE member_id = ?1 AND skill_id = ?2 AND event_kind = ?3",
            rusqlite::params![member.as_str(), skill_id, kind],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    fn has_event_exec(
        &self,
        member: &MemberId,
        skill_id: &str,
        kind: &str,
        execution_id: &str,
    ) -> Result<bool> {
        let count: i64 = self.store.connection().query_row(
            "SELECT COUNT(1) FROM skill_weight_events
             WHERE member_id = ?1 AND skill_id = ?2 AND event_kind = ?3 AND execution_id = ?4",
            rusqlite::params![member.as_str(), skill_id, kind, execution_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    fn last_kind_is(&self, member: &MemberId, skill_id: &str, kind: &str) -> Result<bool> {
        let last: Option<String> = self
            .store
            .connection()
            .query_row(
                "SELECT event_kind FROM skill_weight_events
                 WHERE member_id = ?1 AND skill_id = ?2
                 ORDER BY seq DESC LIMIT 1",
                rusqlite::params![member.as_str(), skill_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(last.as_deref() == Some(kind))
    }

    #[allow(clippy::too_many_arguments)]
    fn append_event(
        &self,
        member: &MemberId,
        skill_id: &str,
        kind: &str,
        execution_id: Option<&str>,
        score: Option<f64>,
        session_seq: Option<i64>,
        detail: serde_json::Value,
    ) -> Result<()> {
        self.store.connection().execute(
            "INSERT INTO skill_weight_events(
                member_id, skill_id, event_kind, execution_id, score, session_seq,
                detail_json, recorded_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                member.as_str(),
                skill_id,
                kind,
                execution_id,
                score,
                session_seq,
                detail.to_string(),
                self.timestamp()
            ],
        )?;
        Ok(())
    }

    fn save_row(&self, member: &MemberId, skill_id: &str, row: &WeightRow) -> Result<()> {
        self.store.connection().execute(
            "INSERT INTO member_skill_weights(
                member_id, skill_id, weight, track, track_source, updated_at,
                updated_session_seq, last_use_at, last_use_session_seq,
                protection_anchor_seq, review
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(member_id, skill_id) DO UPDATE SET
                weight = excluded.weight,
                track = excluded.track,
                track_source = excluded.track_source,
                updated_at = excluded.updated_at,
                updated_session_seq = excluded.updated_session_seq,
                last_use_at = excluded.last_use_at,
                last_use_session_seq = excluded.last_use_session_seq,
                protection_anchor_seq = excluded.protection_anchor_seq,
                review = excluded.review",
            rusqlite::params![
                member.as_str(),
                skill_id,
                row.weight,
                row.track.as_str(),
                row.track_source.as_str(),
                row.updated_at,
                row.updated_session_seq,
                row.last_use_at,
                row.last_use_session_seq,
                row.protection_anchor_seq,
                i64::from(row.review)
            ],
        )?;
        Ok(())
    }

    fn load_row(&self, member: &MemberId, skill_id: &str) -> Result<Option<WeightRow>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT weight, track, track_source, updated_at, updated_session_seq,
                    last_use_at, last_use_session_seq, protection_anchor_seq, review
             FROM member_skill_weights WHERE member_id = ?1 AND skill_id = ?2",
        )?;
        let row = stmt
            .query_row(rusqlite::params![member.as_str(), skill_id], |row| {
                Ok(WeightRow {
                    weight: row.get(0)?,
                    track: DecayTrack::parse(&row.get::<_, String>(1)?)
                        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
                    track_source: match row.get::<_, String>(2)?.as_str() {
                        "haisu" => TrackSource::Haisu,
                        _ => TrackSource::Default,
                    },
                    updated_at: row.get(3)?,
                    updated_session_seq: row.get(4)?,
                    last_use_at: row.get(5)?,
                    last_use_session_seq: row.get(6)?,
                    protection_anchor_seq: row.get(7)?,
                    review: row.get::<_, i64>(8)? != 0,
                })
            })
            .optional()?;
        Ok(row)
    }

    fn load_view(&self, member: &MemberId, skill_id: &str) -> Result<SkillWeightView> {
        let row = self
            .load_row(member, skill_id)?
            .ok_or_else(|| Error::NotFound {
                entity: "skill weight",
                id: format!("{}:{skill_id}", member.as_str()),
            })?;
        Ok(SkillWeightView {
            member_id: member.clone(),
            skill_id: skill_id.to_string(),
            weight: row.weight,
            track: row.track,
            track_source: row.track_source,
            review: row.review,
            updated_at: row.updated_at,
            updated_session_seq: row.updated_session_seq,
            last_use_at: row.last_use_at,
            last_use_session_seq: row.last_use_session_seq,
            protection_anchor_seq: row.protection_anchor_seq,
        })
    }
}

/// `VIVA_HOME/members/<member>/soul.md`. Weight tracking only reads the innate list.
pub fn member_soul_path(home: &Path, member: &MemberId) -> PathBuf {
    home.join("members").join(member.as_str()).join("soul.md")
}

/// Innate skill ids from YAML `innate_skills` or a `## 本命技能` list.
pub fn parse_innate_skills(text: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    if let Some(rest) = text.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            let mut in_list = false;
            for line in rest[..end].lines() {
                let line = line.trim();
                if let Some(inline) = line.strip_prefix("innate_skills:") {
                    in_list = true;
                    let inline = inline.trim();
                    if inline.starts_with('[') {
                        for part in inline.trim_matches(['[', ']']).split(',') {
                            if let Some(id) = normalize_skill_id(part) {
                                ids.insert(id);
                            }
                        }
                        in_list = false;
                    }
                    continue;
                }
                if in_list {
                    if let Some(item) = line.strip_prefix('-') {
                        if let Some(id) = normalize_skill_id(item) {
                            ids.insert(id);
                        }
                    } else if !line.is_empty() && !line.starts_with('#') {
                        in_list = false;
                    }
                }
            }
        }
    }
    let mut in_section = false;
    for line in text.lines() {
        let trim = line.trim();
        if let Some(title) = trim.strip_prefix('#') {
            let title = title.trim_start_matches('#').trim();
            in_section = title == "本命技能"
                || title.eq_ignore_ascii_case("innate skills")
                || title.eq_ignore_ascii_case("innate");
            continue;
        }
        if in_section {
            if let Some(item) = trim.strip_prefix('-') {
                if let Some(id) = normalize_skill_id(item) {
                    ids.insert(id);
                }
            }
        }
    }
    ids
}

/// A recovered transcript counts as a body read on a Read of the skill file,
/// a `skill_body_read` marker, or `viva skill get --skill`. A projected list does not.
pub fn transcript_reads_skill_body(transcript: &str, skill_id: &str, entry_path: &str) -> bool {
    if skill_id.is_empty() {
        return false;
    }
    if transcript.contains(&format!("\"skill_body_read\":\"{skill_id}\""))
        || transcript.contains(&format!("\"skill_body_read\": \"{skill_id}\""))
    {
        return true;
    }
    let cli = transcript.lines().any(|line| {
        line.contains("skill")
            && line.contains("get")
            && (line.contains(&format!("--skill {skill_id}"))
                || line.contains(&format!("--skill={skill_id}")))
    });
    if cli {
        return true;
    }
    if entry_path.is_empty() || !transcript.contains(entry_path) {
        return false;
    }
    let mut start = 0;
    while let Some(rel) = transcript[start..].find(entry_path) {
        let abs = start + rel;
        let from = abs.saturating_sub(240);
        let window = &transcript[from..abs];
        if window.contains("Read") || window.contains("read_file") {
            return true;
        }
        start = abs + entry_path.len();
    }
    false
}

pub fn load_config(home: &Path) -> Result<SkillWeightConfig> {
    match std::fs::read_to_string(home.join("config.toml")) {
        Ok(text) => parse_skill_weight_config(&text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(SkillWeightConfig::default()),
        Err(err) => Err(err.into()),
    }
}

pub fn parse_skill_weight_config(text: &str) -> Result<SkillWeightConfig> {
    let mut config = SkillWeightConfig::default();
    let mut in_section = false;
    for raw in text.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_section = line == "[skill_weight]";
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(Error::Validation(format!(
                "skill_weight line `{line}` needs key = value"
            )));
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"');
        apply_config_key(&mut config, key, value)?;
    }
    Ok(config)
}

fn apply_config_key(config: &mut SkillWeightConfig, key: &str, value: &str) -> Result<()> {
    match key {
        "session_half_life" => config.session_half_life = parse_positive(key, value)?,
        "time_half_life_days" => config.time_half_life_days = parse_positive(key, value)?,
        "forget_weight" => config.forget_weight = parse_positive(key, value)?,
        "session_unused" => config.session_unused = parse_nonneg_int(key, value)?,
        "time_unused_days" => config.time_unused_days = parse_positive(key, value)?,
        "capacity" => {
            config.capacity = usize::try_from(parse_nonneg_int(key, value)?).map_err(|_| {
                Error::Validation(format!("skill_weight capacity `{value}` is too large"))
            })?;
        }
        "protection_sessions" => config.protection_sessions = parse_nonneg_int(key, value)?,
        "success_score" => config.success_score = parse_nonneg_f64(key, value)?,
        "failure_score" => config.failure_score = parse_nonneg_f64(key, value)?,
        "trial_days" => {
            let days = parse_nonneg_int(key, value)?;
            if days == 0 {
                return Err(Error::Validation(
                    "skill_weight trial_days must be at least 1".into(),
                ));
            }
            config.trial_days = days;
        }
        "trial" => {
            config.trial = match value {
                "auto" => TrialMode::Auto,
                "on" | "true" => TrialMode::On,
                "off" | "false" => TrialMode::Off,
                other => {
                    return Err(Error::Validation(format!(
                        "skill_weight trial `{other}` must be auto, on, or off"
                    )));
                }
            };
        }
        other => {
            return Err(Error::Validation(format!(
                "unknown skill_weight key `{other}`"
            )));
        }
    }
    Ok(())
}

fn normalize_skill_id(raw: &str) -> Option<String> {
    let raw = raw
        .trim()
        .trim_matches(|ch: char| ch == '`' || ch == '"' || ch == '\'' || ch == ',');
    if raw.is_empty() {
        return None;
    }
    if let Some(start) = raw.find("skill-") {
        let id: String = raw[start..]
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
            .collect();
        if id.len() > "skill-".len() {
            return Some(id);
        }
    }
    if !raw.contains(' ') {
        Some(raw.to_string())
    } else {
        None
    }
}

fn score_of(
    execution_id: &str,
    uses: &[UseRow],
    corrected: &HashSet<String>,
    settled: &HashMap<String, f64>,
) -> f64 {
    if corrected.contains(execution_id) {
        return 0.0;
    }
    let Some(use_row) = uses.iter().find(|row| row.execution_id == execution_id) else {
        return 0.0;
    };
    if use_row.note == "read_body" {
        if let Some(score) = settled.get(execution_id) {
            return *score;
        }
    }
    use_row.score
}

fn decay_between(
    weight: f64,
    track: DecayTrack,
    from_at: OffsetDateTime,
    from_seq: i64,
    to_at: OffsetDateTime,
    to_seq: i64,
    config: &SkillWeightConfig,
) -> f64 {
    let (elapsed, half_life) = match track {
        DecayTrack::Session => ((to_seq - from_seq).max(0) as f64, config.session_half_life),
        DecayTrack::Time => (days_between(from_at, to_at), config.time_half_life_days),
    };
    if elapsed <= 0.0 || half_life <= 0.0 || weight == 0.0 {
        return weight;
    }
    weight * 0.5_f64.powf(elapsed / half_life)
}

fn days_between(from: OffsetDateTime, to: OffsetDateTime) -> f64 {
    let secs = (to - from).as_seconds_f64();
    if secs <= 0.0 { 0.0 } else { secs / 86_400.0 }
}

fn format_time(stamp: OffsetDateTime) -> String {
    stamp
        .format(&Rfc3339)
        .expect("OffsetDateTime is representable as RFC 3339")
}

fn parse_time(stamp: &str) -> Result<OffsetDateTime> {
    OffsetDateTime::parse(stamp, &Rfc3339)
        .map_err(|err| Error::Validation(format!("bad timestamp `{stamp}`: {err}")))
}

fn map_use(row: &rusqlite::Row<'_>) -> rusqlite::Result<UseRow> {
    Ok(UseRow {
        execution_id: row.get(0)?,
        score: row.get(1)?,
        note: row.get(2)?,
    })
}

fn map_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<SkillWeightEvent> {
    let detail: String = row.get(7)?;
    Ok(SkillWeightEvent {
        seq: row.get(0)?,
        member_id: MemberId::from_str(&row.get::<_, String>(1)?)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
        skill_id: row.get(2)?,
        event_kind: row.get(3)?,
        execution_id: row.get(4)?,
        score: row.get(5)?,
        session_seq: row.get(6)?,
        detail: serde_json::from_str(&detail).unwrap_or(json!({})),
        recorded_at: row.get(8)?,
    })
}

fn parse_positive(key: &str, value: &str) -> Result<f64> {
    let number = parse_nonneg_f64(key, value)?;
    if number <= 0.0 {
        return Err(Error::Validation(format!(
            "skill_weight {key} must be greater than 0"
        )));
    }
    Ok(number)
}

fn parse_nonneg_f64(key: &str, value: &str) -> Result<f64> {
    let number: f64 = value.parse().map_err(|_| {
        Error::Validation(format!(
            "skill_weight {key} value `{value}` is not a number"
        ))
    })?;
    if !number.is_finite() || number < 0.0 {
        return Err(Error::Validation(format!(
            "skill_weight {key} value `{value}` must be a non-negative number"
        )));
    }
    Ok(number)
}

fn parse_nonneg_int(key: &str, value: &str) -> Result<i64> {
    let number: i64 = value.parse().map_err(|_| {
        Error::Validation(format!(
            "skill_weight {key} value `{value}` is not an integer"
        ))
    })?;
    if number < 0 {
        return Err(Error::Validation(format!(
            "skill_weight {key} value `{value}` must be >= 0"
        )));
    }
    Ok(number)
}

fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    for (index, ch) in line.char_indices() {
        match ch {
            '"' => in_string = !in_string,
            '#' if !in_string => return &line[..index],
            _ => {}
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::foundation::ids::{ExecutionId, MemberId};
    use crate::foundation::store::{
        DOMAIN_FOUNDATION, FOUNDATION_V1_SQL, MigrationRegistry, Store,
    };
    use crate::knowledge::register_migrations;
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;

    fn open_store(dir: &Path) -> Store {
        let frozen = register_migrations(MigrationRegistry::new().register(
            DOMAIN_FOUNDATION,
            1,
            "foundation v1",
            FOUNDATION_V1_SQL,
        ))
        .freeze()
        .expect("registry");
        Store::open(&dir.join("viva.db"), &frozen).expect("open")
    }

    fn at(stamp: &str) -> OffsetDateTime {
        OffsetDateTime::parse(stamp, &Rfc3339).expect("stamp")
    }

    fn join(store: &Store, member: &MemberId, skill_id: &str, execution: &ExecutionId) {
        let item_id = format!("item-{skill_id}");
        store
            .connection()
            .execute(
                "INSERT INTO knowledge_items(
                    item_id, title, scope, tags_json, body_path, provenance,
                    status, external_state, created_at, updated_at
                 ) VALUES (?1, ?2, 'skill', '[]', '/tmp/SKILL.md', 'test',
                    'active', 'none', '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z')",
                rusqlite::params![item_id, skill_id],
            )
            .expect("item");
        store
            .connection()
            .execute(
                "INSERT INTO skills(skill_id, item_id, entry_path, enabled, registered_at)
                 VALUES (?1, ?2, '/tmp/SKILL.md', 1, '2020-01-01T00:00:00Z')",
                rusqlite::params![skill_id, item_id],
            )
            .expect("skill");
        store
            .connection()
            .execute(
                "INSERT INTO member_skill_memberships(
                    member_id, skill_id, membership_state, joined_at
                 ) VALUES (?1, ?2, 'active', '2020-01-01T00:00:00Z')",
                rusqlite::params![member.as_str(), skill_id],
            )
            .expect("membership");
        store
            .connection()
            .execute(
                "INSERT INTO skill_used_in(
                    member_id, skill_id, execution_id, score, note, used_at
                 ) VALUES (?1, ?2, ?3, 1.0, 'initial_on_join', '2020-01-01T00:00:00Z')",
                rusqlite::params![member.as_str(), skill_id, execution.as_str()],
            )
            .expect("use");
    }

    #[test]
    fn soul_and_transcript_parsers() {
        let text = "---\ninnate_skills:\n  - skill-aaa\n---\n\n## 本命技能\n\n- skill-bbb\n";
        let ids = parse_innate_skills(text);
        assert!(ids.contains("skill-aaa"));
        assert!(ids.contains("skill-bbb"));
        let projected = r#"{"projected_skills":["skill-aaa","/tmp/SKILL.md"]}"#;
        assert!(!transcript_reads_skill_body(
            projected,
            "skill-aaa",
            "/tmp/SKILL.md"
        ));
        let read = r#"{"name":"Read","input":{"file_path":"/tmp/SKILL.md"}}"#;
        assert!(transcript_reads_skill_body(
            read,
            "skill-aaa",
            "/tmp/SKILL.md"
        ));
    }

    #[test]
    fn rebuild_drops_a_corrected_score_and_keeps_review() {
        let dir = tempfile::TempDir::new().expect("dir");
        let store = open_store(dir.path());
        let member = MemberId::new();
        let skill_id = "skill-one";
        let initial = ExecutionId::new();
        let read = ExecutionId::new();
        join(&store, &member, skill_id, &initial);
        store
            .connection()
            .execute(
                "INSERT INTO skill_used_in(
                    member_id, skill_id, execution_id, score, note, used_at
                 ) VALUES (?1, ?2, ?3, 1.0, 'read_body', '2026-01-02T00:00:00Z')",
                rusqlite::params![member.as_str(), skill_id, read.as_str()],
            )
            .expect("read");
        let svc = SkillWeightService::with_parts(
            &store,
            None,
            SkillWeightConfig::default(),
            at("2026-01-02T00:00:00Z"),
        );
        svc.absorb(&member, skill_id).expect("absorb");
        let view = svc.read(&member, skill_id).expect("read");
        assert!((view.weight - 2.0).abs() < 1e-9, "weight {}", view.weight);
        let corrected = svc.correct(&read, skill_id, None).expect("correct");
        assert!(corrected.review);
        assert!(
            (corrected.weight - 1.0).abs() < 1e-9,
            "revoked weight {}",
            corrected.weight
        );
        store
            .connection()
            .execute(
                "UPDATE member_skill_weights SET weight = 999 WHERE skill_id = ?1",
                [skill_id],
            )
            .expect("corrupt");
        let rebuilt = svc.rebuild(&member, skill_id).expect("rebuild");
        assert!((rebuilt.weight - 1.0).abs() < 1e-9);
        assert!(rebuilt.review);
    }

    #[test]
    fn session_track_ignores_elapsed_days_and_time_track_does_not() {
        let dir = tempfile::TempDir::new().expect("dir");
        let store = open_store(dir.path());
        let member = MemberId::new();
        let session_skill = "skill-session";
        let time_skill = "skill-time";
        join(&store, &member, session_skill, &ExecutionId::new());
        join(&store, &member, time_skill, &ExecutionId::new());
        let start = at("2026-01-01T00:00:00Z");
        let svc = SkillWeightService::with_parts(&store, None, SkillWeightConfig::default(), start);
        svc.absorb(&member, session_skill).expect("session");
        svc.assign_track(&member, time_skill, DecayTrack::Time)
            .expect("pin");
        let later = SkillWeightService::with_parts(
            &store,
            None,
            SkillWeightConfig::default(),
            at("2026-03-02T00:00:00Z"),
        );
        let session_view = later.read(&member, session_skill).expect("session read");
        let time_view = later.read(&member, time_skill).expect("time read");
        assert_eq!(session_view.track, DecayTrack::Session);
        assert!(
            (session_view.weight - 1.0).abs() < 1e-9,
            "session track decayed across days: {}",
            session_view.weight
        );
        assert_eq!(time_view.track, DecayTrack::Time);
        assert_eq!(time_view.track_source, TrackSource::Haisu);
        assert!(
            (time_view.weight - 0.5).abs() < 1e-6,
            "time weight {}",
            time_view.weight
        );
        later
            .assign_track(&member, time_skill, DecayTrack::Time)
            .expect("pin again");
        let again = later.read(&member, time_skill).expect("again");
        assert_eq!(again.track, DecayTrack::Time);
        assert_eq!(again.track_source, TrackSource::Haisu);
    }

    #[test]
    fn existing_skill_protection_starts_when_the_record_opens() {
        let dir = tempfile::TempDir::new().expect("dir");
        let store = open_store(dir.path());
        let member = MemberId::new();
        let now = at("2026-06-01T00:00:00Z");
        let config = SkillWeightConfig {
            trial: TrialMode::Off,
            ..SkillWeightConfig::default()
        };
        let svc = SkillWeightService::with_parts(&store, None, config, now);
        for n in 1..=15 {
            svc.on_session_ended(&member, &format!("pre-{n}"))
                .expect("session");
        }
        let skill_id = "skill-old";
        join(&store, &member, skill_id, &ExecutionId::new());
        svc.absorb(&member, skill_id).expect("open");
        let view = svc.read(&member, skill_id).expect("view");
        assert_eq!(view.protection_anchor_seq, 15);
    }

    #[test]
    fn trial_auto_expires_after_the_configured_month() {
        let dir = tempfile::TempDir::new().expect("dir");
        let store = open_store(dir.path());
        let start = SkillWeightService::with_parts(
            &store,
            None,
            SkillWeightConfig::default(),
            at("2026-01-01T00:00:00Z"),
        );
        assert!(start.trial_active().expect("on"));
        let later = SkillWeightService::with_parts(
            &store,
            None,
            SkillWeightConfig::default(),
            at("2026-01-31T00:00:00Z"),
        );
        assert!(!later.trial_active().expect("off"));
    }
}
