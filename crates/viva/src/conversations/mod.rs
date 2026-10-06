//! Conversation metadata: display names, fork trees and cross-harness
//! handoffs (V10, issue #19).
//!
//! What this domain owns — and deliberately does not:
//! - The office stores the conversation **tree** (parent/child), display
//!   names, optional task associations, and *pointers* to harness-native
//!   session/node records. The harness (Pi) owns the transcript and its
//!   internal context; there is no second copy of chat facts here and no
//!   chat engine.
//! - A native fork is registered only AFTER the harness confirmed it (the
//!   extension forks natively first, then records). A native id is written
//!   once per node: the office never edits a harness-native fact, and the
//!   harness never edits office display names — one fact, one owner.
//! - Forking never creates a task. Task association is an explicit,
//!   reversible attach. Archiving a node hides nothing's history and never
//!   removes its children; closing a task never silently deletes sessions.
//! - Cross-harness handoff is recorded as a handoff with an explicit
//!   capability note. Harnesses without a native fork support handoff only
//!   — and no harness is promised a lossless transcript conversion.

use serde::{Deserialize, Serialize};

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::{MemberId, SessionId, TaskId, utc_now};
use crate::foundation::store::Store;

pub const CONVERSATIONS_V1_SQL: &str = r#"
-- The office conversation tree. Points at harness-native records; the
-- harness owns the transcript, this table owns only office facts.
CREATE TABLE conversation_nodes (
    node_id            TEXT PRIMARY KEY,
    session_id         TEXT NOT NULL REFERENCES office_sessions(session_id),
    parent_node_id     TEXT REFERENCES conversation_nodes(node_id),
    display_name       TEXT NOT NULL CHECK (length(trim(display_name)) > 0),
    harness            TEXT NOT NULL,
    native_session_id  TEXT,
    native_node_id     TEXT,
    task_id            TEXT,
    created_at         TEXT NOT NULL,
    archived_at        TEXT
);

-- Cross-harness handoffs: explicit records of identity/brief/history
-- handed to another harness. `history_ref` references the source record —
-- transcripts are never copied or promised lossless.
CREATE TABLE conversation_handoffs (
    handoff_id         TEXT PRIMARY KEY,
    from_node_id       TEXT NOT NULL REFERENCES conversation_nodes(node_id),
    to_harness         TEXT NOT NULL,
    to_capability      TEXT NOT NULL CHECK (to_capability IN ('native_fork', 'handoff_only')),
    native_session_id  TEXT,
    identity_member_id TEXT,
    task_id            TEXT,
    brief_snapshot     TEXT,
    history_ref        TEXT,
    created_at         TEXT NOT NULL
);
"#;

pub fn register_migrations(
    registry: crate::foundation::store::MigrationRegistry,
) -> crate::foundation::store::MigrationRegistry {
    registry.register(
        crate::foundation::store::DOMAIN_CONVERSATIONS,
        1,
        "conversations v1",
        CONVERSATIONS_V1_SQL,
    )
}

/// A harness's fork capability, declared by configuration — never assumed
/// to be uniform across harnesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkCapability {
    /// The harness can fork a conversation natively (Pi can).
    NativeFork,
    /// The harness supports handoff only: identity/brief/history reference
    /// travel, but there is no native fork.
    HandoffOnly,
}

impl ForkCapability {
    pub fn as_str(&self) -> &'static str {
        match self {
            ForkCapability::NativeFork => "native_fork",
            ForkCapability::HandoffOnly => "handoff_only",
        }
    }

    pub fn from_str_value(text: &str) -> OfficeResult<Self> {
        match text {
            "native_fork" => Ok(ForkCapability::NativeFork),
            "handoff_only" => Ok(ForkCapability::HandoffOnly),
            other => Err(OfficeError::Validation(format!(
                "unknown fork capability `{other}`"
            ))),
        }
    }
}

/// One node of the office conversation tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationNode {
    pub node_id: String,
    pub session_id: SessionId,
    pub parent_node_id: Option<String>,
    /// The office display name. Owned by the office; the harness's own
    /// title stays the harness's.
    pub display_name: String,
    pub harness: String,
    /// Pointer to the harness-native session record, written once.
    pub native_session_id: Option<String>,
    /// Pointer to the harness-native node (fork point / branch head).
    pub native_node_id: Option<String>,
    /// Explicit, selective task association. A fork never creates one.
    pub task_id: Option<TaskId>,
    pub created_at: String,
    pub archived_at: Option<String>,
}

/// A recorded cross-harness handoff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationHandoff {
    pub handoff_id: String,
    pub from_node_id: String,
    pub to_harness: String,
    pub to_capability: ForkCapability,
    pub native_session_id: Option<String>,
    pub identity_member_id: Option<MemberId>,
    pub task_id: Option<TaskId>,
    /// The brief text carried over: a copy kept as a record, traceable to
    /// its source, not live office facts.
    pub brief_snapshot: Option<String>,
    /// A reference to the source history (record id), not a transcript.
    pub history_ref: Option<String>,
    pub created_at: String,
}

pub struct ConversationRegistry<'a> {
    store: &'a Store,
}

impl<'a> ConversationRegistry<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// Create the root node of a conversation.
    pub fn create_root(
        &self,
        session_id: &SessionId,
        display_name: impl Into<String>,
        harness: impl Into<String>,
    ) -> OfficeResult<ConversationNode> {
        self.insert_node(ConversationNode {
            node_id: new_node_id(),
            session_id: session_id.clone(),
            parent_node_id: None,
            display_name: validate_name(display_name)?,
            harness: harness.into(),
            native_session_id: None,
            native_node_id: None,
            task_id: None,
            created_at: utc_now(),
            archived_at: None,
        })
    }

    /// Record a fork: a child node under `parent`. Called only after the
    /// harness's native fork succeeded — the office never records a fork
    /// the harness did not confirm. A fork never creates a task.
    pub fn record_fork(
        &self,
        parent_node_id: &str,
        display_name: impl Into<String>,
        native_session_id: Option<String>,
        native_node_id: Option<String>,
    ) -> OfficeResult<ConversationNode> {
        let parent = self.require_node(parent_node_id)?;
        if parent.archived_at.is_some() {
            return Err(OfficeError::Validation(format!(
                "cannot fork an archived branch `{}`",
                parent.display_name
            )));
        }
        let child = ConversationNode {
            node_id: new_node_id(),
            session_id: parent.session_id.clone(),
            parent_node_id: Some(parent.node_id.clone()),
            display_name: validate_name(display_name)?,
            harness: parent.harness.clone(),
            native_session_id,
            native_node_id,
            task_id: None,
            created_at: utc_now(),
            archived_at: None,
        };
        self.insert_node(child)
    }

    /// Rename in the office. This never touches the harness's own title —
    /// the display name is office-owned metadata.
    pub fn rename(&self, node_id: &str, display_name: impl Into<String>) -> OfficeResult<()> {
        let display_name = validate_name(display_name)?;
        let n = self.store.connection().execute(
            "UPDATE conversation_nodes SET display_name = ?2 WHERE node_id = ?1",
            rusqlite::params![node_id, display_name],
        )?;
        if n == 0 {
            return Err(OfficeError::NotFound {
                entity: "conversation node",
                id: node_id.to_string(),
            });
        }
        Ok(())
    }

    /// Set the harness-native reference. Write-once: a different value
    /// later is a conflict, because the native record belongs to the
    /// harness and the pointer must stay stable.
    pub fn set_native_ref(
        &self,
        node_id: &str,
        native_session_id: impl Into<String>,
        native_node_id: Option<String>,
    ) -> OfficeResult<()> {
        let node = self.require_node(node_id)?;
        let native_session_id = native_session_id.into();
        if let Some(existing) = &node.native_session_id
            && existing != &native_session_id
        {
            return Err(OfficeError::Validation(format!(
                "node `{}` already points at native session `{existing}`; \
                     the office never rewrites a harness-native reference",
                node.display_name
            )));
        }
        self.store.connection().execute(
            "UPDATE conversation_nodes SET native_session_id = ?2, native_node_id = ?3
             WHERE node_id = ?1",
            rusqlite::params![node_id, native_session_id, native_node_id],
        )?;
        Ok(())
    }

    /// Attach a task to a branch (explicit, selective). Forks and plain
    /// chat never get one automatically.
    pub fn attach_task(&self, node_id: &str, task_id: &TaskId) -> OfficeResult<()> {
        let n = self.store.connection().execute(
            "UPDATE conversation_nodes SET task_id = ?2 WHERE node_id = ?1",
            rusqlite::params![node_id, task_id.as_str()],
        )?;
        if n == 0 {
            return Err(OfficeError::NotFound {
                entity: "conversation node",
                id: node_id.to_string(),
            });
        }
        Ok(())
    }

    pub fn detach_task(&self, node_id: &str) -> OfficeResult<()> {
        let n = self.store.connection().execute(
            "UPDATE conversation_nodes SET task_id = NULL WHERE node_id = ?1",
            rusqlite::params![node_id],
        )?;
        if n == 0 {
            return Err(OfficeError::NotFound {
                entity: "conversation node",
                id: node_id.to_string(),
            });
        }
        Ok(())
    }

    /// Archive one branch. Its history stays, its children stay untouched,
    /// and completing a task never archives anything silently.
    pub fn archive(&self, node_id: &str) -> OfficeResult<()> {
        let n = self.store.connection().execute(
            "UPDATE conversation_nodes SET archived_at = ?2 WHERE node_id = ?1 AND archived_at IS NULL",
            rusqlite::params![node_id, utc_now()],
        )?;
        if n == 0 {
            return Err(OfficeError::NotFound {
                entity: "conversation node",
                id: node_id.to_string(),
            });
        }
        Ok(())
    }

    /// The full tree of a session, parents before children.
    pub fn tree(&self, session_id: &SessionId) -> OfficeResult<Vec<ConversationNode>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT node_id, session_id, parent_node_id, display_name, harness,
                    native_session_id, native_node_id, task_id, created_at, archived_at
             FROM conversation_nodes WHERE session_id = ?1 ORDER BY created_at, node_id",
        )?;
        let rows = stmt.query_map([session_id.as_str()], map_node)?;
        let mut nodes = Vec::new();
        for row in rows {
            nodes.push(row?);
        }
        Ok(nodes)
    }

    pub fn get_node(&self, node_id: &str) -> OfficeResult<Option<ConversationNode>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT node_id, session_id, parent_node_id, display_name, harness,
                    native_session_id, native_node_id, task_id, created_at, archived_at
             FROM conversation_nodes WHERE node_id = ?1",
        )?;
        let row = stmt
            .query_row([node_id], map_node)
            .optional()
            .map_err(OfficeError::from)?;
        Ok(row)
    }

    fn require_node(&self, node_id: &str) -> OfficeResult<ConversationNode> {
        self.get_node(node_id)?
            .ok_or_else(|| OfficeError::NotFound {
                entity: "conversation node",
                id: node_id.to_string(),
            })
    }

    fn insert_node(&self, node: ConversationNode) -> OfficeResult<ConversationNode> {
        // The session must exist: nodes hang off real office sessions.
        let exists: i64 = self.store.connection().query_row(
            "SELECT COUNT(*) FROM office_sessions WHERE session_id = ?1",
            [node.session_id.as_str()],
            |row| row.get(0),
        )?;
        if exists == 0 {
            return Err(OfficeError::NotFound {
                entity: "session",
                id: node.session_id.to_string(),
            });
        }
        if let Some(parent) = &node.parent_node_id {
            self.require_node(parent)?;
        }
        self.store.connection().execute(
            "INSERT INTO conversation_nodes(
                 node_id, session_id, parent_node_id, display_name, harness,
                 native_session_id, native_node_id, task_id, created_at, archived_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                node.node_id,
                node.session_id.as_str(),
                node.parent_node_id,
                node.display_name,
                node.harness,
                node.native_session_id,
                node.native_node_id,
                node.task_id.as_ref().map(|t| t.as_str()),
                node.created_at,
                node.archived_at,
            ],
        )?;
        Ok(node)
    }

    /// Record a handoff to another harness. The record is the traceable
    /// fact: who handed what (brief copy, history reference) to which
    /// harness, with that harness's declared capability.
    /// Record a handoff; identical repeats return the first record.
    #[allow(clippy::too_many_arguments)]
    pub fn record_handoff(
        &self,
        from_node_id: &str,
        to_harness: impl Into<String>,
        to_capability: ForkCapability,
        native_session_id: Option<String>,
        identity_member_id: Option<MemberId>,
        task_id: Option<TaskId>,
        brief_snapshot: Option<String>,
        history_ref: Option<String>,
    ) -> OfficeResult<ConversationHandoff> {
        self.record_handoff_checked(
            from_node_id,
            to_harness,
            to_capability,
            native_session_id,
            identity_member_id,
            task_id,
            brief_snapshot,
            history_ref,
        )
        .map(|(handoff, _duplicate)| handoff)
    }

    /// Like [`record_handoff`], but tells the caller whether the handoff
    /// was newly created (false) or an identical earlier record was reused
    /// (true) — clients can surface the difference honestly.
    #[allow(clippy::too_many_arguments)]
    pub fn record_handoff_checked(
        &self,
        from_node_id: &str,
        to_harness: impl Into<String>,
        to_capability: ForkCapability,
        native_session_id: Option<String>,
        identity_member_id: Option<MemberId>,
        task_id: Option<TaskId>,
        brief_snapshot: Option<String>,
        history_ref: Option<String>,
    ) -> OfficeResult<(ConversationHandoff, bool)> {
        self.require_node(from_node_id)?;
        let to_harness = to_harness.into();
        // Idempotent: the SAME handoff (same source node, target harness,
        // native reference, identity, task, brief copy and history
        // reference) recorded twice is ONE handoff — the second call
        // returns the first record unchanged. A genuinely different
        // handoff (different brief/target/refs) still gets its own row.
        let existing: Option<ConversationHandoff> = {
            let mut stmt = self.store.connection().prepare(
                "SELECT handoff_id, from_node_id, to_harness, to_capability,
                        native_session_id, identity_member_id, task_id,
                        brief_snapshot, history_ref, created_at
                 FROM conversation_handoffs
                 WHERE from_node_id = ?1 AND to_harness = ?2
                   AND to_capability = ?3
                   AND native_session_id IS ?4 AND identity_member_id IS ?5
                   AND task_id IS ?6 AND brief_snapshot IS ?7 AND history_ref IS ?8
                 ORDER BY created_at LIMIT 1",
            )?;
            stmt.query_row(
                rusqlite::params![
                    from_node_id,
                    to_harness,
                    to_capability.as_str(),
                    native_session_id,
                    identity_member_id.as_ref().map(|m| m.as_str()),
                    task_id.as_ref().map(|t| t.as_str()),
                    brief_snapshot,
                    history_ref,
                ],
                map_handoff,
            )
            .optional()
            .map_err(OfficeError::from)?
        };
        if let Some(handoff) = existing {
            return Ok((handoff, true));
        }
        let handoff = ConversationHandoff {
            handoff_id: format!("handoff-{}", uuid::Uuid::new_v4().simple()),
            from_node_id: from_node_id.to_string(),
            to_harness,
            to_capability,
            native_session_id,
            identity_member_id,
            task_id,
            brief_snapshot,
            history_ref,
            created_at: utc_now(),
        };
        self.store.connection().execute(
            "INSERT INTO conversation_handoffs(
                 handoff_id, from_node_id, to_harness, to_capability,
                 native_session_id, identity_member_id, task_id,
                 brief_snapshot, history_ref, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                handoff.handoff_id,
                handoff.from_node_id,
                handoff.to_harness,
                handoff.to_capability.as_str(),
                handoff.native_session_id,
                handoff.identity_member_id.as_ref().map(|m| m.as_str()),
                handoff.task_id.as_ref().map(|t| t.as_str()),
                handoff.brief_snapshot,
                handoff.history_ref,
                handoff.created_at,
            ],
        )?;
        Ok((handoff, false))
    }

    pub fn handoffs_for_node(&self, node_id: &str) -> OfficeResult<Vec<ConversationHandoff>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT handoff_id, from_node_id, to_harness, to_capability,
                    native_session_id, identity_member_id, task_id,
                    brief_snapshot, history_ref, created_at
             FROM conversation_handoffs WHERE from_node_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map([node_id], map_handoff)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

use rusqlite::OptionalExtension as _;

fn map_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConversationNode> {
    let task_id: Option<String> = row.get(7)?;
    Ok(ConversationNode {
        node_id: row.get(0)?,
        session_id: SessionId::from_str(&row.get::<_, String>(1)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?,
        parent_node_id: row.get(2)?,
        display_name: row.get(3)?,
        harness: row.get(4)?,
        native_session_id: row.get(5)?,
        native_node_id: row.get(6)?,
        task_id: task_id.as_deref().and_then(|t| TaskId::from_str(t).ok()),
        created_at: row.get(8)?,
        archived_at: row.get(9)?,
    })
}

fn map_handoff(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConversationHandoff> {
    let capability: String = row.get(3)?;
    let task_id: Option<String> = row.get(6)?;
    let member_id: Option<String> = row.get(5)?;
    Ok(ConversationHandoff {
        handoff_id: row.get(0)?,
        from_node_id: row.get(1)?,
        to_harness: row.get(2)?,
        to_capability: ForkCapability::from_str_value(&capability).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(e))
        })?,
        native_session_id: row.get(4)?,
        identity_member_id: member_id
            .as_deref()
            .and_then(|m| MemberId::from_str(m).ok()),
        task_id: task_id.as_deref().and_then(|t| TaskId::from_str(t).ok()),
        brief_snapshot: row.get(7)?,
        history_ref: row.get(8)?,
        created_at: row.get(9)?,
    })
}

fn validate_name(name: impl Into<String>) -> OfficeResult<String> {
    let name = name.into();
    if name.trim().is_empty() {
        return Err(OfficeError::Validation(
            "conversation display name must not be empty".into(),
        ));
    }
    Ok(name)
}

fn new_node_id() -> String {
    format!("node-{}", uuid::Uuid::new_v4().simple())
}

use std::str::FromStr as _;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::records::{SessionKind, SessionRecord, insert_session};
    use crate::foundation::store::{
        DOMAIN_CONVERSATIONS, DOMAIN_FOUNDATION, DOMAIN_TASKS_EXECUTIONS, FOUNDATION_V1_SQL,
        MigrationRegistry,
    };

    fn store() -> Store {
        let frozen = MigrationRegistry::new()
            .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
            .register(
                DOMAIN_TASKS_EXECUTIONS,
                1,
                "tasks and executions v1",
                crate::tasks::TASKS_EXECUTIONS_V1_SQL,
            )
            .register(
                DOMAIN_CONVERSATIONS,
                1,
                "conversations v1",
                CONVERSATIONS_V1_SQL,
            )
            .freeze()
            .expect("registry");
        Store::open_in_memory(&frozen).expect("store")
    }

    fn session(store: &Store) -> SessionRecord {
        let session = SessionRecord::new(SessionKind::Conversation, "Morning chat");
        insert_session(store, &session).expect("session");
        session
    }

    #[test]
    fn fork_builds_a_tree_without_creating_tasks() {
        let store = store();
        let session = session(&store);
        let registry = ConversationRegistry::new(&store);
        let root = registry
            .create_root(&session.session_id, "Main line", "pi")
            .expect("root");
        let branch_a = registry
            .record_fork(&root.node_id, "Theme A", Some("pi-native-a".into()), None)
            .expect("fork a");
        let branch_b = registry
            .record_fork(&root.node_id, "Theme B", Some("pi-native-b".into()), None)
            .expect("fork b");

        assert_eq!(
            branch_a.parent_node_id.as_deref(),
            Some(root.node_id.as_str())
        );
        assert_eq!(
            branch_b.parent_node_id.as_deref(),
            Some(root.node_id.as_str())
        );
        assert!(
            branch_a.task_id.is_none() && branch_b.task_id.is_none() && root.task_id.is_none(),
            "forking never creates a task"
        );

        let tree = registry.tree(&session.session_id).expect("tree");
        assert_eq!(tree.len(), 3, "root + two branches");
    }

    #[test]
    fn rename_and_tree_survive_reopen() {
        let dir = tempfile::TempDir::new().expect("dir");
        let db = dir.path().join("office.db");
        let frozen = MigrationRegistry::new()
            .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
            .register(
                DOMAIN_TASKS_EXECUTIONS,
                1,
                "tasks and executions v1",
                crate::tasks::TASKS_EXECUTIONS_V1_SQL,
            )
            .register(
                DOMAIN_CONVERSATIONS,
                1,
                "conversations v1",
                CONVERSATIONS_V1_SQL,
            )
            .freeze()
            .expect("registry");
        let (session_id, root_id) = {
            let store = Store::open(&db, &frozen).expect("open");
            let session = session(&store);
            let registry = ConversationRegistry::new(&store);
            let root = registry
                .create_root(&session.session_id, "Main line", "pi")
                .expect("root");
            registry
                .rename(&root.node_id, "Renamed main")
                .expect("rename");
            (session.session_id, root.node_id)
        };
        {
            let store = Store::open(&db, &frozen).expect("reopen");
            let registry = ConversationRegistry::new(&store);
            let node = registry.get_node(&root_id).expect("get").expect("exists");
            assert_eq!(node.display_name, "Renamed main");
            assert_eq!(node.session_id, session_id);
        }
    }

    #[test]
    fn native_ref_is_write_once() {
        let store = store();
        let session = session(&store);
        let registry = ConversationRegistry::new(&store);
        let root = registry
            .create_root(&session.session_id, "Main", "pi")
            .expect("root");
        registry
            .set_native_ref(&root.node_id, "pi-native-1", Some("node-1".into()))
            .expect("first write");
        let err = registry
            .set_native_ref(&root.node_id, "pi-native-2", None)
            .expect_err("a different native id must be refused");
        assert!(err.to_string().contains("never rewrites"), "got: {err}");
        // Idempotent same-value write is fine.
        registry
            .set_native_ref(&root.node_id, "pi-native-1", Some("node-1".into()))
            .expect("same value is accepted");
    }

    #[test]
    fn archiving_one_branch_leaves_the_other_intact() {
        let store = store();
        let session = session(&store);
        let registry = ConversationRegistry::new(&store);
        let root = registry
            .create_root(&session.session_id, "Main", "pi")
            .expect("root");
        let a = registry
            .record_fork(&root.node_id, "A", None, None)
            .expect("a");
        let b = registry
            .record_fork(&root.node_id, "B", None, None)
            .expect("b");
        registry.archive(&a.node_id).expect("archive a");
        assert!(
            registry
                .get_node(&a.node_id)
                .expect("get")
                .expect("exists")
                .archived_at
                .is_some()
        );
        assert!(
            registry
                .get_node(&b.node_id)
                .expect("get")
                .expect("exists")
                .archived_at
                .is_none()
        );
        assert!(
            registry
                .get_node(&root.node_id)
                .expect("get")
                .expect("exists")
                .archived_at
                .is_none(),
            "completing/archiving one branch never deletes another"
        );
    }

    #[test]
    fn an_identical_handoff_recorded_twice_is_one_row() {
        let store = store();
        let session = session(&store);
        let registry = ConversationRegistry::new(&store);
        let root = registry
            .create_root(&session.session_id, "Main", "pi")
            .expect("root");
        let identity = MemberId::new();
        let record = |registry: &ConversationRegistry| {
            registry.record_handoff(
                &root.node_id,
                "codex",
                ForkCapability::HandoffOnly,
                Some("codex-native-1".into()),
                Some(identity.clone()),
                None,
                Some("brief".into()),
                Some("hist-ref".into()),
            )
        };
        let first = record(&registry).expect("first");
        let second = record(&registry).expect("second");
        assert_eq!(
            first.handoff_id, second.handoff_id,
            "the same handoff returns the first record"
        );
        assert_eq!(
            registry
                .handoffs_for_node(&root.node_id)
                .expect("list")
                .len(),
            1
        );
        // A different brief is a genuinely different handoff and gets a row.
        registry
            .record_handoff(
                &root.node_id,
                "codex",
                ForkCapability::HandoffOnly,
                None,
                None,
                None,
                Some("different brief".into()),
                None,
            )
            .expect("different");
        assert_eq!(
            registry
                .handoffs_for_node(&root.node_id)
                .expect("list")
                .len(),
            2
        );
    }

    #[test]
    fn handoff_records_capability_and_stays_traceable() {
        let store = store();
        let session = session(&store);
        let registry = ConversationRegistry::new(&store);
        let root = registry
            .create_root(&session.session_id, "Main", "pi")
            .expect("root");
        let handoff = registry
            .record_handoff(
                &root.node_id,
                "codex",
                ForkCapability::HandoffOnly,
                Some("codex-native-1".into()),
                Some(MemberId::new()),
                None,
                Some("brief text snapshot".into()),
                Some("node-xxxx history reference".into()),
            )
            .expect("handoff");
        assert_eq!(handoff.to_capability, ForkCapability::HandoffOnly);
        let recorded = registry.handoffs_for_node(&root.node_id).expect("list");
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            recorded[0].brief_snapshot.as_deref(),
            Some("brief text snapshot")
        );
        assert!(
            recorded[0].history_ref.is_some(),
            "the original record stays traceable"
        );
    }
}
