//! The append-only office event log.
//!
//! G0 semantics: office events are audit facts. Rows are never updated or
//! deleted (enforced by trigger), payloads are bounded, and the log is the
//! raw-history substrate that experiences and memories must cite — writing an
//! event never claims a memory was formed.

use std::str::FromStr as _;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::{EventId, utc_now};
use crate::foundation::store::{Domain, Store};

/// Hard payload bound in bytes, matching the `office_events` CHECK (which is
/// enforced on the cast BLOB, i.e. bytes, not characters).
pub const MAX_EVENT_PAYLOAD_BYTES: usize = 65_536;

/// A new office event to record.
#[derive(Debug, Clone)]
pub struct NewEvent {
    pub domain: Domain,
    pub kind: String,
    pub subject_type: String,
    pub subject_id: String,
    /// Where the event came from (`cli`, `channel:<id>`, a domain module, …).
    pub origin: String,
    pub payload: Value,
}

/// A recorded event as read back from the store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedEvent {
    pub seq: i64,
    pub event_id: EventId,
    pub occurred_at: String,
    pub domain: String,
    pub kind: String,
    pub subject_type: String,
    pub subject_id: String,
    pub origin: String,
    pub payload: Value,
}

/// Append one event with autocommit semantics.
pub fn append(store: &Store, event: NewEvent) -> OfficeResult<RecordedEvent> {
    append_within(store.connection(), event)
}

/// Append one event inside a caller-owned transaction so related fact changes
/// commit atomically (ADR 0011 §8). `tx` derefs to `Connection`.
pub fn append_in(tx: &rusqlite::Transaction<'_>, event: NewEvent) -> OfficeResult<RecordedEvent> {
    append_within(tx, event)
}

fn append_within(conn: &rusqlite::Connection, event: NewEvent) -> OfficeResult<RecordedEvent> {
    if event.kind.trim().is_empty() {
        return Err(OfficeError::Validation(
            "event kind must not be empty".into(),
        ));
    }
    if event.subject_type.trim().is_empty() || event.subject_id.trim().is_empty() {
        return Err(OfficeError::Validation(
            "event subject_type and subject_id must not be empty".into(),
        ));
    }
    if event.origin.trim().is_empty() || event.origin.len() > 128 {
        return Err(OfficeError::Validation(
            "event origin must be 1..=128 characters".into(),
        ));
    }
    let payload = serde_json::to_string(&event.payload)?;
    if payload.len() > MAX_EVENT_PAYLOAD_BYTES {
        return Err(OfficeError::Validation(format!(
            "event payload is {} bytes; the bound is {MAX_EVENT_PAYLOAD_BYTES}",
            payload.len()
        )));
    }
    let event_id = EventId::new();
    let occurred_at = utc_now();
    conn.execute(
        "INSERT INTO office_events(event_id, occurred_at, domain, kind, subject_type, subject_id, origin, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            event_id.as_str(),
            occurred_at,
            event.domain.as_str(),
            event.kind,
            event.subject_type,
            event.subject_id,
            event.origin,
            payload,
        ],
    )?;
    Ok(RecordedEvent {
        seq: conn.last_insert_rowid(),
        event_id,
        occurred_at,
        domain: event.domain.as_str().to_string(),
        kind: event.kind,
        subject_type: event.subject_type,
        subject_id: event.subject_id,
        origin: event.origin,
        payload: event.payload,
    })
}

/// Read the most recent events, newest first.
pub fn list(store: &Store, limit: u32) -> OfficeResult<Vec<RecordedEvent>> {
    let conn = store.connection();
    let mut stmt = conn.prepare(
        "SELECT seq, event_id, occurred_at, domain, kind, subject_type, subject_id, origin, payload
         FROM office_events ORDER BY seq DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit], |row| {
        Ok(RecordedEvent {
            seq: row.get(0)?,
            event_id: EventId::from_str(row.get::<_, String>(1)?.as_str()).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            occurred_at: row.get(2)?,
            domain: row.get(3)?,
            kind: row.get(4)?,
            subject_type: row.get(5)?,
            subject_id: row.get(6)?,
            origin: row.get(7)?,
            payload: serde_json::from_str(&row.get::<_, String>(8)?).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    8,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
        })
    })?;
    let mut events = Vec::new();
    for row in rows {
        events.push(row?);
    }
    Ok(events)
}

/// Fetch one event by id, if present.
pub fn get(store: &Store, event_id: &EventId) -> OfficeResult<Option<RecordedEvent>> {
    let conn = store.connection();
    let mut stmt = conn.prepare(
        "SELECT seq, event_id, occurred_at, domain, kind, subject_type, subject_id, origin, payload
         FROM office_events WHERE event_id = ?1",
    )?;
    let row = stmt
        .query_row([event_id.as_str()], |row| {
            Ok(RecordedEvent {
                seq: row.get(0)?,
                event_id: EventId::from_str(row.get::<_, String>(1)?.as_str()).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                occurred_at: row.get(2)?,
                domain: row.get(3)?,
                kind: row.get(4)?,
                subject_type: row.get(5)?,
                subject_id: row.get(6)?,
                origin: row.get(7)?,
                payload: serde_json::from_str(&row.get::<_, String>(8)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        8,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
            })
        })
        .optional()?;
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::store::{
        DOMAIN_FOUNDATION, FOUNDATION_V1_SQL, MigrationRegistry, Store,
    };

    fn store() -> Store {
        let frozen = MigrationRegistry::new()
            .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
            .freeze()
            .expect("registry");
        Store::open_in_memory(&frozen).expect("store")
    }

    fn demo(kind: &str) -> NewEvent {
        NewEvent {
            domain: DOMAIN_FOUNDATION,
            kind: kind.to_string(),
            subject_type: "user".into(),
            subject_id: "user-haisu".into(),
            origin: "cli".into(),
            payload: serde_json::json!({"note": "hello"}),
        }
    }

    #[test]
    fn append_and_read_back_roundtrip() {
        let store = store();
        let recorded = append(&store, demo("demo_note")).expect("append");
        assert!(recorded.seq > 0);
        assert_eq!(recorded.kind, "demo_note");

        let fetched = get(&store, &recorded.event_id)
            .expect("get")
            .expect("event exists");
        assert_eq!(fetched, recorded);

        let listed = list(&store, 10).expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0], recorded);
    }

    #[test]
    fn oversized_payload_is_rejected_before_insert() {
        let store = store();
        let mut event = demo("too_big");
        event.payload = serde_json::json!({ "blob": "x".repeat(MAX_EVENT_PAYLOAD_BYTES + 1) });
        let err = append(&store, event).expect_err("oversized payload must fail");
        assert!(err.to_string().contains("bound"), "got: {err}");
        assert_eq!(store.row_count("office_events").expect("count"), 0);
    }

    #[test]
    fn empty_fields_are_rejected() {
        let store = store();
        let mut event = demo("no_kind");
        event.kind = "  ".into();
        assert!(append(&store, event).is_err());

        let mut event = demo("no_subject");
        event.subject_id = String::new();
        assert!(append(&store, event).is_err());
    }

    #[test]
    fn events_append_atomically_with_related_facts() {
        let store = store();
        let tx = store.transaction().expect("tx");
        let recorded = append_in(&tx, demo("atomic_note")).expect("append in tx");
        // Simulate a related fact write failing after the event was added.
        let broken = tx.execute("INSERT INTO office_sessions(session_id, kind, title, task_id, member_id, harness, harness_native_session_id, created_at) VALUES ('x', 'nonsense', 't', NULL, NULL, NULL, NULL, 'now')", []);
        assert!(broken.is_err(), "invalid session insert must fail");
        drop(tx); // rollback: event and session both gone

        assert!(
            get(&store, &recorded.event_id).expect("get").is_none(),
            "no half event may survive a rolled-back transaction"
        );
    }
}
