//! SQLite store, schema metadata and the per-domain migration registry.
//!
//! G0 storage semantics (ADR 0011 §8 + rollout §2):
//! - SQLite owns facts and relations with append-only audit/event rows;
//!   ordinary files own bodies, raw material and large logs. One fact, one
//!   authoritative source.
//! - Migrations are registered **per domain** (`foundation`, `members`,
//!   `tasks_executions`, …) with independent version sequences starting at 1.
//!   A domain merging later still applies its pending migrations on the next
//!   open — merge order can never skip a migration, and pre-numbering across
//!   domains can never collide.
//! - Applied migrations are recorded in `schema_migrations(domain, version)`;
//!   each migration runs in its own transaction so a failed migration leaves
//!   no half state.
//! - Foundation tables are the frozen reference slice (sessions, executions,
//!   launch specs, terminal events, control requests, office events). Other
//!   domains create their own tables and reference them; they never `ALTER`
//!   foundation tables.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::utc_now;

/// The known migration domains. Each domain owner (see
/// `docs/implementation/g0-contracts.md`) sequences its own versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Domain(&'static str);

impl Domain {
    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    pub fn as_str(&self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for Domain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

pub const DOMAIN_FOUNDATION: Domain = Domain::new("foundation");
pub const DOMAIN_MEMBERS: Domain = Domain::new("members");
pub const DOMAIN_WORKSPACES_PROJECTS: Domain = Domain::new("workspaces_projects");
pub const DOMAIN_TASKS_EXECUTIONS: Domain = Domain::new("tasks_executions");
pub const DOMAIN_AUTHORITY: Domain = Domain::new("authority");
pub const DOMAIN_GIT: Domain = Domain::new("git");
pub const DOMAIN_CONVERSATIONS: Domain = Domain::new("conversations");
pub const DOMAIN_KNOWLEDGE: Domain = Domain::new("knowledge");
pub const DOMAIN_WORKFLOWS: Domain = Domain::new("workflows");
pub const DOMAIN_MAINTENANCE: Domain = Domain::new("maintenance");
pub const DOMAIN_TOOLS_COMPUTER: Domain = Domain::new("tools_computer");

/// Every domain V01 knows about; used by the CLI to validate event domains.
pub const KNOWN_DOMAINS: &[Domain] = &[
    DOMAIN_FOUNDATION,
    DOMAIN_MEMBERS,
    DOMAIN_WORKSPACES_PROJECTS,
    DOMAIN_TASKS_EXECUTIONS,
    DOMAIN_AUTHORITY,
    DOMAIN_GIT,
    DOMAIN_CONVERSATIONS,
    DOMAIN_KNOWLEDGE,
    DOMAIN_WORKFLOWS,
    DOMAIN_MAINTENANCE,
    DOMAIN_TOOLS_COMPUTER,
];

/// Map a domain name read from the database back to a `Domain`. Known domains
/// reuse their static constant; a foreign (future) domain name interns its
/// own copy — bounded by the number of domains that ever touch this database.
fn domain_from_stored(name: String) -> Domain {
    if let Some(known) = KNOWN_DOMAINS.iter().find(|d| d.as_str() == name) {
        return *known;
    }
    Domain::new(Box::leak(name.into_boxed_str()))
}

/// One pending schema change owned by a domain.
#[derive(Debug, Clone)]
pub struct Migration {
    pub domain: Domain,
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

/// Registry under construction; validated by [`MigrationRegistry::freeze`].
#[derive(Debug, Clone, Default)]
pub struct MigrationRegistry {
    migrations: Vec<Migration>,
}

impl MigrationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one migration; versions for a domain start at 1 and must be
    /// contiguous.
    #[must_use]
    pub fn register(
        mut self,
        domain: Domain,
        version: u32,
        name: &'static str,
        sql: &'static str,
    ) -> Self {
        self.migrations.push(Migration {
            domain,
            version,
            name,
            sql,
        });
        self
    }

    /// Validate the registry: no duplicate `(domain, version)`, no gaps, and
    /// version 0 does not exist. Returns the frozen set the store applies.
    pub fn freeze(self) -> OfficeResult<FrozenMigrations> {
        let mut per_domain: BTreeMap<Domain, Vec<Migration>> = BTreeMap::new();
        for migration in self.migrations {
            if migration.version == 0 {
                return Err(OfficeError::Migration(format!(
                    "domain `{}`: migration versions start at 1, got 0 ({})",
                    migration.domain, migration.name
                )));
            }
            if per_domain
                .entry(migration.domain)
                .or_default()
                .iter()
                .any(|m| m.version == migration.version)
            {
                return Err(OfficeError::Migration(format!(
                    "domain `{}`: duplicate migration version {}",
                    migration.domain, migration.version
                )));
            }
            per_domain
                .get_mut(&migration.domain)
                .expect("entry just created")
                .push(migration);
        }
        for (domain, list) in per_domain.iter_mut() {
            list.sort_by_key(|m| m.version);
            for (expected, migration) in list.iter().enumerate() {
                let want = (expected + 1) as u32;
                if migration.version != want {
                    return Err(OfficeError::Migration(format!(
                        "domain `{}`: migration versions must be contiguous from 1; \
                         expected {want}, found {} ({})",
                        domain, migration.version, migration.name
                    )));
                }
            }
        }
        Ok(FrozenMigrations { per_domain })
    }
}

/// A validated migration set. Safe to reuse across many [`Store::open`] calls.
#[derive(Debug, Clone)]
pub struct FrozenMigrations {
    per_domain: BTreeMap<Domain, Vec<Migration>>,
}

impl FrozenMigrations {
    pub fn domains(&self) -> impl Iterator<Item = Domain> + '_ {
        self.per_domain.keys().copied()
    }

    pub fn migrations_for(&self, domain: Domain) -> &[Migration] {
        self.per_domain
            .get(&domain)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn all(&self) -> impl Iterator<Item = &Migration> {
        self.per_domain.values().flatten()
    }
}

/// The foundation reference schema (version 1). The migration engine itself
/// owns `schema_meta` and `schema_migrations` (created before any migration
/// runs); foundation v1 owns the office tables below. See the module docs for
/// the ownership rules of these tables.
pub const FOUNDATION_V1_SQL: &str = r#"
-- Append-only office event log. UPDATE/DELETE are rejected by trigger; the
-- CHECK bounds payload growth in bytes (bounded logging, ADR 0011).
CREATE TABLE office_events (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id     TEXT NOT NULL UNIQUE,
    occurred_at  TEXT NOT NULL,
    domain       TEXT NOT NULL,
    kind         TEXT NOT NULL,
    subject_type TEXT NOT NULL,
    subject_id   TEXT NOT NULL,
    origin       TEXT NOT NULL,
    payload      TEXT NOT NULL,
    CHECK (length(CAST(payload AS BLOB)) <= 65536)
);
CREATE TRIGGER office_events_no_update
BEFORE UPDATE ON office_events
BEGIN SELECT RAISE(ABORT, 'office_events is append-only'); END;
CREATE TRIGGER office_events_no_delete
BEFORE DELETE ON office_events
BEGIN SELECT RAISE(ABORT, 'office_events is append-only'); END;

-- Office-side session references. A plain conversation session needs no task;
-- a task execution session must carry its task. Harness-native session ids
-- are pointers to foreign records, never a second copy of the chat facts.
CREATE TABLE office_sessions (
    session_id               TEXT PRIMARY KEY,
    kind                     TEXT NOT NULL CHECK (kind IN ('conversation', 'task_execution')),
    title                    TEXT NOT NULL,
    task_id                  TEXT,
    member_id                TEXT,
    harness                  TEXT,
    harness_native_session_id TEXT,
    created_at               TEXT NOT NULL,
    CHECK (kind <> 'task_execution' OR task_id IS NOT NULL)
);

-- Execution records. A task execution must attach its task and its full
-- attribution snapshot; completion requires evidence — a process exit code is
-- never accepted as proof of completion.
CREATE TABLE office_executions (
    execution_id        TEXT PRIMARY KEY,
    task_id             TEXT NOT NULL,
    session_id          TEXT REFERENCES office_sessions(session_id),
    member_id           TEXT NOT NULL,
    attribution         TEXT NOT NULL,
    status              TEXT NOT NULL CHECK (status IN ('requested', 'running', 'stopped', 'failed', 'completed')),
    process_exit        INTEGER,
    completion_evidence TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    CHECK (status <> 'completed' OR completion_evidence IS NOT NULL)
);

-- Launch specifications: explicit argv/cwd, bindings, request origin and
-- resource budget. argv is a JSON array (program + arguments); there is no
-- field that accepts a joined shell string as a generic entry point.
CREATE TABLE launch_specs (
    spec_id        TEXT PRIMARY KEY,
    execution_id   TEXT,
    terminal_id    TEXT,
    argv           TEXT NOT NULL,
    cwd            TEXT NOT NULL,
    member_id      TEXT,
    model_binding  TEXT,
    tool_binding   TEXT,
    worktree_id    TEXT,
    request_origin TEXT NOT NULL,
    budget_json    TEXT,
    created_at     TEXT NOT NULL
);

-- Terminal events. Terminal state and process events are separate records;
-- `owner` distinguishes member executions from user shells, and only a member
-- execution may carry an execution id (a user shell can never fake one).
CREATE TABLE terminal_events (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    terminal_id  TEXT NOT NULL,
    owner        TEXT NOT NULL CHECK (owner IN ('member_execution', 'user_shell', 'agent_cli', 'test_run')),
    execution_id TEXT,
    event_kind   TEXT NOT NULL CHECK (event_kind IN ('spawned', 'input', 'resize', 'snapshot', 'stopped', 'exited')),
    payload      TEXT NOT NULL,
    occurred_at  TEXT NOT NULL,
    CHECK ((owner = 'member_execution') = (execution_id IS NOT NULL))
);

-- Control-request ledger: source validation, request id, rejection reasons
-- and idempotency are all persisted facts.
CREATE TABLE control_requests (
    request_id      TEXT PRIMARY KEY,
    idempotency_key TEXT UNIQUE,
    caller_channel  TEXT NOT NULL,
    caller_role     TEXT NOT NULL,
    kind            TEXT NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('accepted', 'rejected', 'replayed')),
    rejection       TEXT,
    result_json     TEXT,
    received_at     TEXT NOT NULL
);

INSERT INTO schema_meta(key, value) VALUES ('layout_version', '1');
"#;

/// An open office store. Single-writer per process; WAL keeps readers cheap.
pub struct Store {
    conn: Connection,
    schema_versions: BTreeMap<Domain, u32>,
}

impl Store {
    /// Open (creating if needed) the database at `path` and apply all pending
    /// migrations from `migrations`.
    pub fn open(path: &Path, migrations: &FrozenMigrations) -> OfficeResult<Store> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        Self::configure(&conn)?;
        let mut store = Store {
            conn,
            schema_versions: BTreeMap::new(),
        };
        store.apply_migrations(migrations)?;
        Ok(store)
    }

    /// In-memory store for tests.
    pub fn open_in_memory(migrations: &FrozenMigrations) -> OfficeResult<Store> {
        let conn = Connection::open_in_memory()?;
        Self::configure(&conn)?;
        let mut store = Store {
            conn,
            schema_versions: BTreeMap::new(),
        };
        store.apply_migrations(migrations)?;
        Ok(store)
    }

    fn configure(conn: &Connection) -> OfficeResult<()> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(Duration::from_millis(5_000))?;
        Ok(())
    }

    fn apply_migrations(&mut self, migrations: &FrozenMigrations) -> OfficeResult<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS schema_migrations (
                domain     TEXT NOT NULL,
                version    INTEGER NOT NULL,
                name       TEXT NOT NULL,
                applied_at TEXT NOT NULL,
                PRIMARY KEY (domain, version)
            );",
        )?;
        for migration in migrations.all() {
            let already = self.conn.query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE domain = ?1 AND version = ?2",
                rusqlite::params![migration.domain.as_str(), migration.version as i64],
                |row| row.get::<_, i64>(0),
            )? > 0;
            if already {
                continue;
            }
            // Immediate transaction: migration DDL takes the write lock up
            // front so a concurrent opener waits instead of interleaving.
            let tx = self
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            tx.execute_batch(migration.sql)?;
            tx.execute(
                "INSERT INTO schema_migrations(domain, version, name, applied_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![migration.domain.as_str(), migration.version as i64, migration.name, utc_now()],
            )?;
            tx.commit()?;
        }
        self.reload_schema_versions()?;
        Ok(())
    }

    fn reload_schema_versions(&mut self) -> OfficeResult<()> {
        let mut stmt = self
            .conn
            .prepare("SELECT domain, MAX(version) FROM schema_migrations GROUP BY domain")?;
        let rows = stmt.query_map([], |row| {
            let name: String = row.get(0)?;
            let version: i64 = row.get(1)?;
            Ok((domain_from_stored(name), version as u32))
        })?;
        self.schema_versions.clear();
        for row in rows {
            let (domain, version) = row?;
            self.schema_versions.insert(domain, version);
        }
        Ok(())
    }

    /// Highest applied migration version per domain, read at open time.
    pub fn schema_versions(&self) -> &BTreeMap<Domain, u32> {
        &self.schema_versions
    }

    /// Values from the `schema_meta` key/value table.
    pub fn schema_meta(&self) -> OfficeResult<BTreeMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM schema_meta ORDER BY key")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut map = BTreeMap::new();
        for row in rows {
            let (k, v) = row?;
            map.insert(k, v);
        }
        Ok(map)
    }

    /// The underlying connection for domain queries that the foundation does
    /// not wrap. Domain modules own their own statements.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Begin a transaction (deferred) usable through `&self`. Failed
    /// transactions are dropped and rolled back — never half-applied.
    pub fn transaction(&self) -> OfficeResult<rusqlite::Transaction<'_>> {
        Ok(self.conn.unchecked_transaction()?)
    }

    /// Applied migration rows, for diagnostics and drift checks.
    pub fn applied_migrations(&self) -> OfficeResult<Vec<(String, u32, String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT domain, version, name, applied_at FROM schema_migrations ORDER BY domain, version")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? as u32,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Count rows of a table; used by tests and the doctor command.
    pub fn row_count(&self, table: &str) -> OfficeResult<i64> {
        // Table names are code constants, never user input; quote defensively.
        let sql = format!("SELECT COUNT(*) FROM \"{table}\"");
        let count = self.conn.query_row(&sql, [], |row| row.get::<_, i64>(0))?;
        Ok(count)
    }

    /// Applied domains seen at open time (helper for tests/diagnostics).
    pub fn applied_domains(&self) -> Vec<Domain> {
        self.schema_versions.keys().copied().collect()
    }

    /// True if `(domain, version)` has been applied.
    pub fn is_applied(&self, domain: Domain, version: u32) -> OfficeResult<bool> {
        let count = self.conn.query_row(
            "SELECT COUNT(*) FROM schema_migrations WHERE domain = ?1 AND version = ?2",
            rusqlite::params![domain.as_str(), version as i64],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }

    /// Applied domain set as a HashSet (helper for migration tests).
    pub fn applied_versions(&self, domain: Domain) -> OfficeResult<HashSet<u32>> {
        let mut stmt = self
            .conn
            .prepare("SELECT version FROM schema_migrations WHERE domain = ?1")?;
        let rows = stmt.query_map([domain.as_str()], |row| row.get::<_, i64>(0))?;
        let mut set = HashSet::new();
        for row in rows {
            set.insert(row? as u32);
        }
        Ok(set)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const CONVERSATIONS_V1_SQL: &str = "CREATE TABLE conversations_probe (id TEXT PRIMARY KEY);";
    const GIT_V1_SQL: &str = "CREATE TABLE git_probe (id TEXT PRIMARY KEY);";
    // Fails midway: the second CREATE collides with the first, after the
    // first statement already "succeeded" inside the migration transaction.
    const BROKEN_SQL: &str = "CREATE TABLE broken_probe (id TEXT PRIMARY KEY); CREATE TABLE broken_probe (id TEXT PRIMARY KEY);";

    fn foundation_only() -> FrozenMigrations {
        MigrationRegistry::new()
            .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
            .freeze()
            .expect("foundation registry is valid")
    }

    #[test]
    fn migrations_apply_once_and_survive_reopen() {
        let dir = TempDir::new().expect("tempdir");
        let db = dir.path().join("office.db");
        let frozen = foundation_only();

        {
            let store = Store::open(&db, &frozen).expect("open");
            assert_eq!(store.schema_versions().get(&DOMAIN_FOUNDATION), Some(&1));
            assert_eq!(store.row_count("office_events").expect("count"), 0);
        }
        // Reopen: no re-application, no duplicate rows.
        {
            let store = Store::open(&db, &frozen).expect("reopen");
            assert_eq!(store.schema_versions().get(&DOMAIN_FOUNDATION), Some(&1));
            let applied = store.applied_migrations().expect("applied");
            assert_eq!(applied.len(), 1, "exactly one migration row after reopen");
        }
    }

    #[test]
    fn late_merged_domain_still_applies_after_reopen() {
        let dir = TempDir::new().expect("tempdir");
        let db = dir.path().join("office.db");

        // First integration: only foundation + git existed.
        let early = MigrationRegistry::new()
            .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
            .register(DOMAIN_GIT, 1, "git v1", GIT_V1_SQL)
            .freeze()
            .expect("early registry");
        drop(Store::open(&db, &early).expect("early open"));

        // Second integration: conversations merged later — its migration must
        // still run on the next open (merge order never skips a migration).
        let late = MigrationRegistry::new()
            .register(DOMAIN_GIT, 1, "git v1", GIT_V1_SQL)
            .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
            .register(
                DOMAIN_CONVERSATIONS,
                1,
                "conversations v1",
                CONVERSATIONS_V1_SQL,
            )
            .freeze()
            .expect("late registry");
        let store = Store::open(&db, &late).expect("late open");
        assert_eq!(store.schema_versions().get(&DOMAIN_CONVERSATIONS), Some(&1));
        assert!(store.is_applied(DOMAIN_CONVERSATIONS, 1).expect("check"));
        assert!(store.is_applied(DOMAIN_GIT, 1).expect("check"));
    }

    #[test]
    fn registry_rejects_duplicate_versions() {
        let err = MigrationRegistry::new()
            .register(DOMAIN_GIT, 1, "git v1", GIT_V1_SQL)
            .register(DOMAIN_GIT, 1, "git v1 again", GIT_V1_SQL)
            .freeze()
            .expect_err("duplicate version must fail");
        assert!(err.to_string().contains("duplicate"), "got: {err}");
    }

    #[test]
    fn registry_rejects_version_gaps() {
        let err = MigrationRegistry::new()
            .register(DOMAIN_GIT, 1, "git v1", GIT_V1_SQL)
            .register(DOMAIN_GIT, 3, "git v3", GIT_V1_SQL)
            .freeze()
            .expect_err("gap must fail");
        assert!(err.to_string().contains("contiguous"), "got: {err}");
    }

    #[test]
    fn failed_migration_leaves_no_half_state() {
        let dir = TempDir::new().expect("tempdir");
        let db = dir.path().join("office.db");
        let early = MigrationRegistry::new()
            .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
            .freeze()
            .expect("early registry");
        drop(Store::open(&db, &early).expect("early open"));

        let bad = MigrationRegistry::new()
            .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
            .register(DOMAIN_GIT, 1, "git v1", GIT_V1_SQL)
            .register(DOMAIN_GIT, 2, "git v2 broken", BROKEN_SQL)
            .freeze()
            .expect("registry shape is fine");
        let result = Store::open(&db, &bad);
        assert!(result.is_err(), "broken migration must fail open");
        // The failure surfaces; reopen with the good registry to inspect.
        let store = Store::open(&db, &early).expect("reopens with good registry");
        assert!(
            !store.is_applied(DOMAIN_GIT, 2).expect("check"),
            "broken migration must not be recorded"
        );
        // foundation v1 and git v1 applied; the broken git v2 did not.
        assert_eq!(store.row_count("schema_migrations").expect("count"), 2);
        assert!(
            store.row_count("broken_probe").is_err(),
            "no half-created table may survive"
        );
    }

    #[test]
    fn failed_transaction_leaves_no_half_state() {
        let dir = TempDir::new().expect("tempdir");
        let db = dir.path().join("office.db");
        let store = Store::open(&db, &foundation_only()).expect("open");

        let tx = store.transaction().expect("tx");
        tx.execute(
            "INSERT INTO office_events(event_id, occurred_at, domain, kind, subject_type, subject_id, origin, payload)
             VALUES ('evt-1', '2026-01-01T00:00:00Z', 'foundation', 'demo', 'user', 'u1', 'cli', '{}')",
            [],
        )
        .expect("insert inside tx");
        let broken = tx.execute_batch("SELECT * FROM definitely_missing_table;");
        assert!(broken.is_err(), "failure inside tx");
        drop(tx); // rollback

        assert_eq!(
            store.row_count("office_events").expect("count"),
            0,
            "no half event may survive"
        );
    }

    #[test]
    fn office_events_is_append_only_at_the_database_level() {
        let store = Store::open_in_memory(&foundation_only()).expect("open");
        store
            .connection()
            .execute(
                "INSERT INTO office_events(event_id, occurred_at, domain, kind, subject_type, subject_id, origin, payload)
                 VALUES ('evt-1', '2026-01-01T00:00:00Z', 'foundation', 'demo', 'user', 'u1', 'cli', '{}')",
                [],
            )
            .expect("insert");
        let update = store
            .connection()
            .execute("UPDATE office_events SET kind = 'tampered'", []);
        assert!(update.is_err(), "UPDATE must be rejected");
        let delete = store.connection().execute("DELETE FROM office_events", []);
        assert!(delete.is_err(), "DELETE must be rejected");
        assert_eq!(store.row_count("office_events").expect("count"), 1);
    }

    #[test]
    fn schema_meta_records_layout_version() {
        let store = Store::open_in_memory(&foundation_only()).expect("open");
        let meta = store.schema_meta().expect("meta");
        assert_eq!(meta.get("layout_version").map(String::as_str), Some("1"));
    }
}
