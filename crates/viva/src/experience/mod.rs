//! Session capture into experience (issue #115).
//!
//! Checked before adding this module: execution records hold attribution and
//! status, not transcripts; `agent_native_sessions` stores a resume pointer;
//! [`crate::redaction::Redactor`] already scrubs configured secrets. None of
//! those produce a member-scoped turn sequence. This module is that gap.
//!
//! Captured turns stay in experience. Nothing here writes knowledge.
//! A session Viva started is attributed from its execution record. A session
//! Viva did not start stays unassigned until `assign` names a member.
//! The dedup key is harness + session id + inclusive turn range. The same
//! key again is a no-op. A resumed or forked file with a new session id keeps
//! the parent id and stores only turns that were not already collected.

mod parse;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use rusqlite::OptionalExtension;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::foundation::error::{Error, Result};
use crate::foundation::events::{self, NewEvent};
use crate::foundation::ids::{ExecutionId, MemberId, TaskId, utc_now};
use crate::foundation::records::{AttributionSnapshot, get_execution};
use crate::foundation::store::{DOMAIN_EXPERIENCE, MigrationRegistry, Store};
use crate::redaction::Redactor;
use crate::tasks::{ExecutionStart, TaskRegistry};

pub use parse::{HARNESS_CLAUDE, HARNESS_CODEX, HARNESS_PI, canonical_harness};

pub fn register_migrations(registry: MigrationRegistry) -> MigrationRegistry {
    registry.register(
        DOMAIN_EXPERIENCE,
        1,
        "experience v1 session capture",
        EXPERIENCE_V1_SQL,
    )
}

pub const EXPERIENCE_V1_SQL: &str = r#"
CREATE TABLE experience_launches (
    harness      TEXT NOT NULL,
    session_id   TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    PRIMARY KEY (harness, session_id)
);

CREATE TABLE experience_terminal_launches (
    terminal_id  TEXT PRIMARY KEY,
    execution_id TEXT NOT NULL,
    harness      TEXT NOT NULL,
    created_at   TEXT NOT NULL
);

CREATE TABLE experience_sessions (
    harness         TEXT NOT NULL,
    session_id      TEXT NOT NULL,
    parent_session  TEXT,
    source_path     TEXT NOT NULL,
    member_id       TEXT,
    execution_id    TEXT,
    archive_relpath TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    PRIMARY KEY (harness, session_id)
);

CREATE TABLE experience_imports (
    import_id   TEXT PRIMARY KEY,
    harness     TEXT NOT NULL,
    session_id  TEXT NOT NULL,
    turn_start  INTEGER NOT NULL,
    turn_end    INTEGER NOT NULL,
    created_at  TEXT NOT NULL,
    UNIQUE (harness, session_id, turn_start, turn_end)
);

CREATE TABLE experience_turns (
    harness      TEXT NOT NULL,
    session_id   TEXT NOT NULL,
    turn_index   INTEGER NOT NULL,
    role         TEXT NOT NULL,
    content      TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    import_id    TEXT NOT NULL,
    PRIMARY KEY (harness, session_id, turn_index)
);
"#;

const REDACTION_SECRETS_FILE: &str = "redaction.secrets";
const UNASSIGNED_DIR: &str = "_unassigned";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkedLaunch {
    pub harness: String,
    pub session_id: String,
    pub execution_id: String,
    pub member_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionSummary {
    pub harness: String,
    pub session_id: String,
    pub parent_session: Option<String>,
    pub execution_id: Option<String>,
    pub source_path: String,
    pub member_id: Option<String>,
    pub archive_path: String,
    pub turn_count: i64,
    pub turn_start: Option<i64>,
    pub turn_end: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExperienceTurn {
    pub index: i64,
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionDetail {
    pub session: SessionSummary,
    pub turns: Vec<ExperienceTurn>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScanReport {
    pub sessions_seen: usize,
    pub imported_ranges: usize,
    pub noop_ranges: usize,
    pub assigned: usize,
    pub unassigned: usize,
    pub skipped_files: usize,
}

#[derive(Debug, Clone)]
pub struct HarnessRoots {
    pub claude_projects: PathBuf,
    pub codex_sessions: PathBuf,
    pub pi_sessions: PathBuf,
}

impl HarnessRoots {
    pub fn default_roots() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let pi_agent = std::env::var_os("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".pi").join("agent"));
        Self {
            claude_projects: home.join(".claude").join("projects"),
            codex_sessions: home.join(".codex").join("sessions"),
            pi_sessions: pi_agent.join("sessions"),
        }
    }
}

pub struct ExperienceService<'a> {
    store: &'a Store,
    home: &'a Path,
}

impl<'a> ExperienceService<'a> {
    pub fn new(store: &'a Store, home: &'a Path) -> Self {
        Self { store, home }
    }

    pub fn link_execution(
        &self,
        execution_id: &ExecutionId,
        harness: &str,
        session_id: &str,
    ) -> Result<LinkedLaunch> {
        link_execution(self.store, execution_id, harness, session_id)
    }

    pub fn scan(&self, roots: &HarnessRoots, secrets: &[String]) -> Result<ScanReport> {
        let mut report = ScanReport {
            sessions_seen: 0,
            imported_ranges: 0,
            noop_ranges: 0,
            assigned: 0,
            unassigned: 0,
            skipped_files: 0,
        };
        let mut index = HashIndex::load(self.store)?;
        for (harness, root) in [
            (HARNESS_CLAUDE, &roots.claude_projects),
            (HARNESS_CODEX, &roots.codex_sessions),
            (HARNESS_PI, &roots.pi_sessions),
        ] {
            let files = match jsonl_files(root) {
                Ok(files) => files,
                Err(_) => {
                    report.skipped_files += 1;
                    continue;
                }
            };
            let mut parsed = Vec::new();
            for path in files {
                match parse::parse_transcript(harness, &path) {
                    Ok(Some(session)) => parsed.push((path, session)),
                    Ok(None) => {}
                    Err(_) => report.skipped_files += 1,
                }
            }
            import_batch(self, &mut parsed, secrets, &mut index, &mut report)?;
        }
        Ok(report)
    }

    pub fn list(&self, member: &MemberId) -> Result<Vec<SessionSummary>> {
        load_summaries(self.store, self.home, MemberScope::Member(member))
    }

    pub fn unassigned(&self) -> Result<Vec<SessionSummary>> {
        load_summaries(self.store, self.home, MemberScope::Unassigned)
    }

    pub fn show(
        &self,
        member: &MemberId,
        harness: &str,
        session_id: &str,
    ) -> Result<SessionDetail> {
        let harness = require_harness(harness)?;
        require_session_id(session_id)?;
        let Some(session) = load_one(self.store, self.home, harness, session_id)? else {
            return Err(not_found(session_id));
        };
        if session.member_id.as_deref() != Some(member.as_str()) {
            return Err(not_found(session_id));
        }
        let mut stmt = self.store.connection().prepare(
            "SELECT turn_index, role, content FROM experience_turns
             WHERE harness = ?1 AND session_id = ?2 ORDER BY turn_index",
        )?;
        let turns = stmt
            .query_map(rusqlite::params![harness, session_id], |row| {
                Ok(ExperienceTurn {
                    index: row.get(0)?,
                    role: row.get(1)?,
                    content: row.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(SessionDetail { session, turns })
    }

    pub fn assign(
        &self,
        member: &MemberId,
        harness: &str,
        session_id: &str,
        secrets: &[String],
    ) -> Result<SessionSummary> {
        let harness = require_harness(harness)?;
        require_session_id(session_id)?;
        let Some(existing) = load_one(self.store, self.home, harness, session_id)? else {
            return Err(Error::Validation(format!(
                "session `{session_id}` is not in experience yet; run `viva experience scan` first"
            )));
        };
        if let Some(current) = existing.member_id.as_deref() {
            if current == member.as_str() {
                return Ok(existing);
            }
            return Err(Error::Validation(format!(
                "session `{session_id}` already belongs to another member"
            )));
        }
        let new_rel = archive_rel(Some(member), harness, session_id);
        let new_path = archive_abs(self.home, &new_rel)?;
        let old_path = PathBuf::from(&existing.archive_path);
        if old_path != new_path {
            if old_path.is_file() {
                if let Some(parent) = new_path.parent() {
                    fs::create_dir_all(parent)?;
                    chmod_private_dir(parent)?;
                }
                fs::rename(&old_path, &new_path)?;
            } else if Path::new(&existing.source_path).is_file() {
                let body = redact_file(Path::new(&existing.source_path), secrets)?;
                write_private(&new_path, &body)?;
            }
        }
        let now = utc_now();
        let changed = self.store.connection().execute(
            "UPDATE experience_sessions
             SET member_id = ?1, archive_relpath = ?2, updated_at = ?3
             WHERE harness = ?4 AND session_id = ?5 AND member_id IS NULL",
            rusqlite::params![member.as_str(), new_rel, now, harness, session_id],
        )?;
        if changed == 0 {
            let current = load_one(self.store, self.home, harness, session_id)?
                .ok_or_else(|| not_found(session_id))?;
            if current.member_id.as_deref() == Some(member.as_str()) {
                return Ok(current);
            }
            return Err(Error::Validation(format!(
                "session `{session_id}` already belongs to another member"
            )));
        }
        events::append(
            self.store,
            NewEvent {
                domain: DOMAIN_EXPERIENCE,
                kind: "experience_assigned".into(),
                subject_type: "harness_session".into(),
                subject_id: format!("{harness}:{session_id}"),
                origin: "experience".into(),
                payload: json!({
                    "harness": harness,
                    "session_id": session_id,
                    "member_id": member.as_str(),
                }),
            },
        )?;
        load_one(self.store, self.home, harness, session_id)?.ok_or_else(|| not_found(session_id))
    }
}

pub fn link_execution(
    store: &Store,
    execution_id: &ExecutionId,
    harness: &str,
    session_id: &str,
) -> Result<LinkedLaunch> {
    let harness = require_harness(harness)?;
    require_session_id(session_id)?;
    let execution = get_execution(store, execution_id)?.ok_or_else(|| Error::NotFound {
        entity: "execution",
        id: execution_id.to_string(),
    })?;
    if let Some(existing) = launch_execution(store, harness, session_id)? {
        if existing.as_str() != execution_id.as_str() {
            return Err(Error::Validation(format!(
                "session `{session_id}` is already linked to execution `{existing}`"
            )));
        }
    } else {
        store.connection().execute(
            "INSERT INTO experience_launches(harness, session_id, execution_id, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![harness, session_id, execution_id.as_str(), utc_now()],
        )?;
    }
    Ok(LinkedLaunch {
        harness: harness.to_string(),
        session_id: session_id.to_string(),
        execution_id: execution_id.to_string(),
        member_id: execution.attribution.member_id.to_string(),
    })
}

pub fn begin_harness_execution(
    store: &Store,
    task_id: &TaskId,
    member_id: &MemberId,
    role: &str,
    model_binding: &str,
    harness: &str,
) -> Result<ExecutionId> {
    let harness = require_harness(harness)?;
    let attribution = AttributionSnapshot::capture(member_id.clone(), role, model_binding, harness);
    let request_key = format!("harness-{harness}-{}", uuid::Uuid::new_v4().simple());
    let started = TaskRegistry::new(store).begin_execution(task_id, &attribution, request_key)?;
    let intent = match started {
        ExecutionStart::Created(intent) | ExecutionStart::Replayed(intent) => intent,
    };
    intent
        .execution_id
        .ok_or_else(|| Error::Validation("harness launch did not record an execution id".into()))
}

pub fn remember_terminal_launch(
    store: &Store,
    terminal_id: &str,
    execution_id: &ExecutionId,
    harness: &str,
) -> Result<()> {
    let harness = require_harness(harness)?;
    store.connection().execute(
        "INSERT INTO experience_terminal_launches(terminal_id, execution_id, harness, created_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(terminal_id) DO UPDATE SET
            execution_id = excluded.execution_id,
            harness = excluded.harness,
            created_at = excluded.created_at",
        rusqlite::params![terminal_id, execution_id.as_str(), harness, utc_now()],
    )?;
    Ok(())
}

/// When a Viva-spawned pane reports its native session id, bind that id to
/// the execution created at launch. The member still comes from the execution.
pub fn link_reported_session(
    store: &Store,
    terminal_id: &str,
    agent: &str,
    session_id: &str,
) -> Result<Option<LinkedLaunch>> {
    let Some(harness) = canonical_harness(agent) else {
        return Ok(None);
    };
    let Some(execution_id) = terminal_execution(store, terminal_id, harness)? else {
        return Ok(None);
    };
    if !parse::valid_session_id(session_id) {
        return Ok(None);
    }
    Ok(Some(link_execution(
        store,
        &execution_id,
        harness,
        session_id,
    )?))
}

pub fn load_redaction_secrets(home: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(home.join(REDACTION_SECRETS_FILE)) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

struct StoredSession {
    member_id: Option<MemberId>,
    execution_id: Option<String>,
    archive_relpath: String,
}

struct HashIndex {
    sessions: HashMap<(String, String), BTreeMap<i64, String>>,
}

impl HashIndex {
    fn load(store: &Store) -> Result<Self> {
        let mut stmt = store.connection().prepare(
            "SELECT harness, session_id, turn_index, content_hash FROM experience_turns",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut sessions = HashMap::new();
        for row in rows {
            let (harness, session_id, index, hash) = row?;
            sessions
                .entry((harness, session_id))
                .or_insert_with(BTreeMap::new)
                .insert(index, hash);
        }
        Ok(Self { sessions })
    }

    fn ordered(&self, harness: &str, session_id: &str) -> Vec<String> {
        let Some(map) = self
            .sessions
            .get(&(harness.to_string(), session_id.to_string()))
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (index, hash) in map {
            if *index != out.len() as i64 {
                break;
            }
            out.push(hash.clone());
        }
        out
    }

    fn note(&mut self, harness: &str, session_id: &str, index: i64, hash: String) {
        self.sessions
            .entry((harness.to_string(), session_id.to_string()))
            .or_default()
            .insert(index, hash);
    }
}

fn import_batch(
    service: &ExperienceService<'_>,
    pending: &mut Vec<(PathBuf, parse::ParsedSession)>,
    secrets: &[String],
    index: &mut HashIndex,
    report: &mut ScanReport,
) -> Result<()> {
    let mut guard = 0;
    while !pending.is_empty() {
        guard += 1;
        let ids: BTreeSet<String> = pending
            .iter()
            .map(|(_, session)| session.session_id.clone())
            .collect();
        let mut deferred = Vec::new();
        let mut ready = Vec::new();
        for item in pending.drain(..) {
            let blocked = item
                .1
                .parent_session
                .as_ref()
                .is_some_and(|parent| parent != &item.1.session_id && ids.contains(parent));
            if blocked {
                deferred.push(item);
            } else {
                ready.push(item);
            }
        }
        if ready.is_empty() {
            ready.append(&mut deferred);
        }
        for (path, session) in ready {
            import_one(service, &path, session, secrets, index, report)?;
        }
        if deferred.is_empty() || guard > 10_000 {
            for (path, session) in deferred {
                import_one(service, &path, session, secrets, index, report)?;
            }
            break;
        }
        *pending = deferred;
    }
    Ok(())
}

fn import_one(
    service: &ExperienceService<'_>,
    path: &Path,
    session: parse::ParsedSession,
    secrets: &[String],
    index: &mut HashIndex,
    report: &mut ScanReport,
) -> Result<()> {
    let harness = session.harness;
    let session_id = session.session_id;
    let child_hashes: Vec<String> = session
        .turns
        .iter()
        .map(|turn| turn_hash(&turn.role, &turn.content))
        .collect();
    let explicit_parent = session.parent_session.clone();
    let (parent_session, prefix) = resolve_parent(
        harness,
        &session_id,
        session.parent_session,
        &child_hashes,
        index,
    );
    let existing: BTreeSet<i64> = index
        .sessions
        .get(&(harness.to_string(), session_id.clone()))
        .map(|map| map.keys().copied().collect())
        .unwrap_or_default();
    let mut new_indices = Vec::new();
    for idx in prefix..session.turns.len() {
        let idx = idx as i64;
        if !existing.contains(&idx) {
            new_indices.push(idx);
        }
    }
    let launch = launch_for(service.store, harness, &session_id)?;
    let stored = stored_session(service.store, harness, &session_id)?;
    let (mut member_id, mut execution_id) = merge_attribution(stored.as_ref(), launch.as_ref());
    // A sidechain names its parent. It belongs to that parent's member.
    // An inferred hash prefix is not this kind of parent and does not assign anyone.
    if member_id.is_none() {
        if let Some(parent) = explicit_parent.as_deref() {
            if let Some(parent_row) = stored_session(service.store, harness, parent)? {
                if parent_row.member_id.is_some() {
                    member_id = parent_row.member_id;
                    if execution_id.is_none() {
                        execution_id = parent_row.execution_id;
                    }
                }
            }
        }
    }
    let rel = archive_rel(member_id.as_ref(), harness, &session_id);
    let archive_path = archive_abs(service.home, &rel)?;
    if let Some(previous) = stored.as_ref() {
        let previous_path = service.home.join(&previous.archive_relpath);
        if previous_path != archive_path && previous_path.is_file() {
            let _ = fs::remove_file(&previous_path);
        }
    }
    let redacted = redact_file(path, secrets)?;
    write_private(&archive_path, &redacted)?;

    let source_path = path.to_string_lossy().into_owned();
    let now = utc_now();
    let tx = service.store.transaction()?;
    if stored.is_some() {
        tx.execute(
            "UPDATE experience_sessions
             SET parent_session = ?1, source_path = ?2, member_id = ?3, execution_id = ?4,
                 archive_relpath = ?5, updated_at = ?6
             WHERE harness = ?7 AND session_id = ?8",
            rusqlite::params![
                parent_session,
                source_path,
                member_id.as_ref().map(|id| id.as_str()),
                execution_id.as_deref(),
                rel,
                now,
                harness,
                session_id,
            ],
        )?;
    } else {
        tx.execute(
            "INSERT INTO experience_sessions(
                harness, session_id, parent_session, source_path, member_id, execution_id,
                archive_relpath, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            rusqlite::params![
                harness,
                session_id,
                parent_session,
                source_path,
                member_id.as_ref().map(|id| id.as_str()),
                execution_id.as_deref(),
                rel,
                now,
            ],
        )?;
    }
    if new_indices.is_empty() {
        report.noop_ranges += 1;
    }
    for (start, end) in contiguous(&new_indices) {
        let import_id = format!("import-{}", uuid::Uuid::new_v4().simple());
        let inserted = tx.execute(
            "INSERT INTO experience_imports(import_id, harness, session_id, turn_start, turn_end, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![import_id, harness, session_id, start, end, now],
        );
        if let Err(rusqlite::Error::SqliteFailure(err, _)) = &inserted
            && err.code == rusqlite::ErrorCode::ConstraintViolation
        {
            report.noop_ranges += 1;
            continue;
        }
        inserted?;
        for idx in start..=end {
            let turn = &session.turns[idx as usize];
            let hash = turn_hash(&turn.role, &turn.content);
            let content = redact_text(secrets, &turn.content);
            tx.execute(
                "INSERT INTO experience_turns(
                    harness, session_id, turn_index, role, content, content_hash, import_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    harness, session_id, idx, turn.role, content, hash, import_id
                ],
            )?;
            index.note(harness, &session_id, idx, hash);
        }
        events::append_in(
            &tx,
            NewEvent {
                domain: DOMAIN_EXPERIENCE,
                kind: "experience_imported".into(),
                subject_type: "harness_session".into(),
                subject_id: format!("{harness}:{session_id}"),
                origin: "experience".into(),
                payload: json!({
                    "harness": harness,
                    "session_id": session_id,
                    "turn_start": start,
                    "turn_end": end,
                    "parent_session": parent_session,
                    "member_id": member_id.as_ref().map(|id| id.as_str()),
                    "execution_id": execution_id,
                }),
            },
        )?;
        report.imported_ranges += 1;
    }
    tx.commit()?;
    report.sessions_seen += 1;
    if member_id.is_some() {
        report.assigned += 1;
    } else {
        report.unassigned += 1;
    }
    Ok(())
}

fn resolve_parent(
    harness: &str,
    session_id: &str,
    explicit: Option<String>,
    child_hashes: &[String],
    index: &HashIndex,
) -> (Option<String>, usize) {
    if let Some(parent) = explicit.as_ref() {
        let prefix = shared_prefix(child_hashes, &index.ordered(harness, parent));
        return (Some(parent.clone()), prefix);
    }
    let mut best: Option<(String, usize)> = None;
    for (other_harness, other_id) in index.sessions.keys() {
        if other_harness != harness || other_id == session_id {
            continue;
        }
        let parent_hashes = index.ordered(harness, other_id);
        let prefix = shared_prefix(child_hashes, &parent_hashes);
        let full_copy =
            prefix == child_hashes.len() && prefix == parent_hashes.len() && prefix >= 1;
        let continued = prefix >= 2 && prefix < child_hashes.len() && prefix == parent_hashes.len();
        if (full_copy || continued) && best.as_ref().is_none_or(|(_, len)| prefix > *len) {
            best = Some((other_id.clone(), prefix));
        }
    }
    match best {
        Some((parent, prefix)) => (Some(parent), prefix),
        None => (None, 0),
    }
}

fn shared_prefix(child: &[String], parent: &[String]) -> usize {
    child
        .iter()
        .zip(parent.iter())
        .take_while(|(left, right)| left == right)
        .count()
}

fn merge_attribution(
    stored: Option<&StoredSession>,
    launch: Option<&(MemberId, ExecutionId)>,
) -> (Option<MemberId>, Option<String>) {
    let mut member_id = stored.and_then(|row| row.member_id.clone());
    let mut execution_id = stored.and_then(|row| row.execution_id.clone());
    if let Some((launch_member, launch_execution)) = launch {
        if member_id.is_none() {
            member_id = Some(launch_member.clone());
        }
        if execution_id.is_none() && member_id.as_ref() == Some(launch_member) {
            execution_id = Some(launch_execution.to_string());
        }
    }
    (member_id, execution_id)
}

fn contiguous(indices: &[i64]) -> Vec<(i64, i64)> {
    let mut ranges = Vec::new();
    let Some(&first) = indices.first() else {
        return ranges;
    };
    let mut start = first;
    let mut prev = first;
    for &idx in &indices[1..] {
        if idx == prev + 1 {
            prev = idx;
        } else {
            ranges.push((start, prev));
            start = idx;
            prev = idx;
        }
    }
    ranges.push((start, prev));
    ranges
}

fn launch_for(
    store: &Store,
    harness: &str,
    session_id: &str,
) -> Result<Option<(MemberId, ExecutionId)>> {
    let Some(execution_id) = launch_execution(store, harness, session_id)? else {
        return Ok(None);
    };
    let Some(execution) = get_execution(store, &execution_id)? else {
        return Ok(None);
    };
    Ok(Some((execution.attribution.member_id, execution_id)))
}

fn launch_execution(store: &Store, harness: &str, session_id: &str) -> Result<Option<ExecutionId>> {
    let text: Option<String> = store
        .connection()
        .query_row(
            "SELECT execution_id FROM experience_launches WHERE harness = ?1 AND session_id = ?2",
            rusqlite::params![harness, session_id],
            |row| row.get(0),
        )
        .optional()?;
    match text {
        Some(text) => Ok(Some(ExecutionId::from_str_pub(&text)?)),
        None => Ok(None),
    }
}

fn terminal_execution(
    store: &Store,
    terminal_id: &str,
    harness: &str,
) -> Result<Option<ExecutionId>> {
    let text: Option<String> = store
        .connection()
        .query_row(
            "SELECT execution_id FROM experience_terminal_launches
             WHERE terminal_id = ?1 AND harness = ?2",
            rusqlite::params![terminal_id, harness],
            |row| row.get(0),
        )
        .optional()?;
    match text {
        Some(text) => Ok(Some(ExecutionId::from_str_pub(&text)?)),
        None => Ok(None),
    }
}

fn stored_session(store: &Store, harness: &str, session_id: &str) -> Result<Option<StoredSession>> {
    let row = store
        .connection()
        .query_row(
            "SELECT parent_session, member_id, execution_id, archive_relpath
             FROM experience_sessions WHERE harness = ?1 AND session_id = ?2",
            rusqlite::params![harness, session_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((_parent_session, member_id, execution_id, archive_relpath)) = row else {
        return Ok(None);
    };
    let member_id = match member_id {
        Some(text) => Some(MemberId::from_str_pub(&text)?),
        None => None,
    };
    Ok(Some(StoredSession {
        member_id,
        execution_id,
        archive_relpath,
    }))
}

enum MemberScope<'a> {
    Member(&'a MemberId),
    Unassigned,
}

fn load_summaries(
    store: &Store,
    home: &Path,
    scope: MemberScope<'_>,
) -> Result<Vec<SessionSummary>> {
    let (sql_filter, param): (&str, Option<&str>) = match scope {
        MemberScope::Member(member) => ("s.member_id = ?1", Some(member.as_str())),
        MemberScope::Unassigned => ("s.member_id IS NULL", None),
    };
    let sql = format!(
        "SELECT s.harness, s.session_id, s.parent_session, s.execution_id, s.source_path,
                s.member_id, s.archive_relpath,
                COUNT(t.turn_index), MIN(t.turn_index), MAX(t.turn_index)
         FROM experience_sessions s
         LEFT JOIN experience_turns t
           ON t.harness = s.harness AND t.session_id = s.session_id
         WHERE {sql_filter}
         GROUP BY s.harness, s.session_id
         ORDER BY s.harness, s.session_id"
    );
    let mut stmt = store.connection().prepare(&sql)?;
    let map = |row: &rusqlite::Row<'_>| row_summary(home, row);
    let rows = if let Some(member) = param {
        stmt.query_map(rusqlite::params![member], map)?
            .collect::<std::result::Result<Vec<_>, _>>()?
    } else {
        stmt.query_map([], map)?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    rows.into_iter().collect()
}

fn load_one(
    store: &Store,
    home: &Path,
    harness: &str,
    session_id: &str,
) -> Result<Option<SessionSummary>> {
    let sql = "SELECT s.harness, s.session_id, s.parent_session, s.execution_id, s.source_path,
                s.member_id, s.archive_relpath,
                COUNT(t.turn_index), MIN(t.turn_index), MAX(t.turn_index)
         FROM experience_sessions s
         LEFT JOIN experience_turns t
           ON t.harness = s.harness AND t.session_id = s.session_id
         WHERE s.harness = ?1 AND s.session_id = ?2
         GROUP BY s.harness, s.session_id";
    let mut stmt = store.connection().prepare(sql)?;
    stmt.query_row(rusqlite::params![harness, session_id], |row| {
        row_summary(home, row)
    })
    .optional()?
    .transpose()
}

fn row_summary(home: &Path, row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<SessionSummary>> {
    let rel: String = row.get(6)?;
    Ok(Ok(SessionSummary {
        harness: row.get(0)?,
        session_id: row.get(1)?,
        parent_session: row.get(2)?,
        execution_id: row.get(3)?,
        source_path: row.get(4)?,
        member_id: row.get(5)?,
        archive_path: home.join(rel).display().to_string(),
        turn_count: row.get(7)?,
        turn_start: row.get(8)?,
        turn_end: row.get(9)?,
    }))
}

fn archive_rel(member: Option<&MemberId>, harness: &str, session_id: &str) -> String {
    let owner = member
        .map(|id| id.as_str().to_string())
        .unwrap_or_else(|| UNASSIGNED_DIR.to_string());
    format!("experience/{owner}/{harness}/{session_id}/session.jsonl")
}

fn archive_abs(home: &Path, rel: &str) -> Result<PathBuf> {
    let full = home.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent)?;
        chmod_tree(home, parent)?;
    }
    Ok(full)
}

fn chmod_tree(home: &Path, dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let experience = home.join("experience");
        if experience.exists() {
            fs::set_permissions(&experience, fs::Permissions::from_mode(0o700))?;
        }
        if let Ok(rest) = dir.strip_prefix(&experience) {
            let mut cursor = experience;
            for component in rest.components() {
                cursor.push(component);
                if cursor.exists() {
                    fs::set_permissions(&cursor, fs::Permissions::from_mode(0o700))?;
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (home, dir);
    }
    Ok(())
}

fn chmod_private_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
    Ok(())
}

fn write_private(path: &Path, body: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        chmod_private_dir(parent)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(body.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        fs::write(path, body)?;
    }
    Ok(())
}

fn redact_file(path: &Path, secrets: &[String]) -> Result<String> {
    let text = fs::read_to_string(path)?;
    let mut redactor = Redactor::new(secrets.iter().cloned());
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        out.push_str(&redactor.push(line));
    }
    out.push_str(&redactor.flush());
    Ok(out)
}

fn redact_text(secrets: &[String], text: &str) -> String {
    Redactor::new(secrets.iter().cloned()).redact_str(text)
}

fn turn_hash(role: &str, content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(role.as_bytes());
    hasher.update(b"\n");
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn jsonl_files(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn require_harness(name: &str) -> Result<&'static str> {
    canonical_harness(name).ok_or_else(|| {
        Error::Validation(format!(
            "harness `{name}` is not collected; expected claude-code, codex, or pi"
        ))
    })
}

fn require_session_id(session_id: &str) -> Result<()> {
    if parse::valid_session_id(session_id) {
        Ok(())
    } else {
        Err(Error::Validation(format!(
            "session id `{session_id}` is empty or contains unsupported characters"
        )))
    }
}

fn not_found(session_id: &str) -> Error {
    Error::NotFound {
        entity: "experience session",
        id: session_id.to_string(),
    }
}

trait IdParse {
    fn from_str_pub(text: &str) -> Result<Self>
    where
        Self: Sized;
}

impl IdParse for ExecutionId {
    fn from_str_pub(text: &str) -> Result<Self> {
        use std::str::FromStr;
        Ok(ExecutionId::from_str(text)?)
    }
}

impl IdParse for MemberId {
    fn from_str_pub(text: &str) -> Result<Self> {
        use std::str::FromStr;
        Ok(MemberId::from_str(text)?)
    }
}
