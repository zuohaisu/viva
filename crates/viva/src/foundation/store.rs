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
//!   launch specs, terminal events, control requests, events). Other
//!   domains create their own tables and reference them; they never `ALTER`
//!   foundation tables.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::io::AsRawFd as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use rusqlite::Connection;

use crate::foundation::error::{Error, Result};
use crate::foundation::ids::utc_now;
use crate::foundation::legacy;

/// The known migration domains. Each domain owner (see
/// Wiki page `G0-Interface-Contracts`) sequences its own versions.
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
pub const DOMAIN_MEMORY: Domain = Domain::new("memory");
pub const DOMAIN_SKILLS: Domain = Domain::new("skills");
pub const DOMAIN_ENGAGEMENT: Domain = Domain::new("engagement");
pub const DOMAIN_EXPERIENCE: Domain = Domain::new("experience");

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
    DOMAIN_MEMORY,
    DOMAIN_SKILLS,
    DOMAIN_ENGAGEMENT,
    DOMAIN_EXPERIENCE,
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
    /// Schema-dependent upgrades run inside the same versioned transaction.
    /// SQL alone cannot conditionally ADD a column to an already existing table.
    pub apply: Option<fn(&rusqlite::Transaction<'_>) -> Result<()>>,
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
            apply: None,
        });
        self
    }

    /// A versioned, transactional migration needing schema inspection.
    #[must_use]
    pub fn register_fn(
        mut self,
        domain: Domain,
        version: u32,
        name: &'static str,
        apply: fn(&rusqlite::Transaction<'_>) -> Result<()>,
    ) -> Self {
        self.migrations.push(Migration {
            domain,
            version,
            name,
            sql: "",
            apply: Some(apply),
        });
        self
    }

    /// Validate the registry: no duplicate `(domain, version)`, no gaps, and
    /// version 0 does not exist. Returns the frozen set the store applies.
    pub fn freeze(self) -> Result<FrozenMigrations> {
        let mut per_domain: BTreeMap<Domain, Vec<Migration>> = BTreeMap::new();
        for migration in self.migrations {
            if migration.version == 0 {
                return Err(Error::Migration(format!(
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
                return Err(Error::Migration(format!(
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
                    return Err(Error::Migration(format!(
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
/// runs); foundation v1 owns the tables below. See the module docs for
/// the ownership rules of these tables.
pub const FOUNDATION_V1_SQL: &str = r#"
-- Append-only event log. UPDATE/DELETE are rejected by trigger; the
-- CHECK bounds payload growth in bytes (bounded logging, ADR 0011).
CREATE TABLE events (
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
CREATE TRIGGER events_no_update
BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'events is append-only'); END;
CREATE TRIGGER events_no_delete
BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'events is append-only'); END;

-- Viva-side session references. A plain conversation session needs no task;
-- a task execution session must carry its task. Harness-native session ids
-- are pointers to foreign records, never a second copy of the chat facts.
CREATE TABLE sessions (
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
CREATE TABLE executions (
    execution_id        TEXT PRIMARY KEY,
    task_id             TEXT NOT NULL,
    session_id          TEXT REFERENCES sessions(session_id),
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

/// An open store. Single-writer per process; WAL keeps readers cheap.
pub struct Store {
    conn: Connection,
    schema_versions: BTreeMap<Domain, u32>,
    legacy_migration: Option<legacy::Migrated>,
}

/// Sidecar lock released on drop. Held for the whole of one `Store::open`.
struct DatabaseFileLock {
    file: File,
}

impl Drop for DatabaseFileLock {
    fn drop(&mut self) {
        // SAFETY: `file` is the descriptor this guard locked with LOCK_EX.
        unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl DatabaseFileLock {
    fn acquire(db_path: &Path) -> Result<Self> {
        let path = open_lock_path(db_path);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&path)?;
        lock_exclusive(&file)?;
        Ok(Self { file })
    }
}

fn in_process_open_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let mut locks = LOCKS.lock().unwrap_or_else(|err| err.into_inner());
    locks
        .entry(open_lock_key(path))
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn open_lock_key(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("viva.db"));
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf())
            .join(name),
        _ => name,
    }
}

fn open_lock_path(db_path: &Path) -> PathBuf {
    let mut name = db_path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("viva.db"));
    name.push(".open.lock");
    match db_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

fn lock_exclusive(file: &File) -> std::io::Result<()> {
    let fd = file.as_raw_fd();
    loop {
        // SAFETY: `fd` is an open file descriptor owned by `file`. LOCK_EX
        // waits until this process holds the advisory lock.
        let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
        if rc == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(err);
    }
}

impl Store {
    /// Open (creating if needed) the database at `path` and apply all pending
    /// migrations from `migrations`.
    pub fn open(path: &Path, migrations: &FrozenMigrations) -> Result<Store> {
        // Switching a rollback journal to WAL takes an exclusive lock, and
        // SQLite returns SQLITE_BUSY immediately — without calling the busy
        // handler — when two connections race for it. Wait here, before any
        // connection is opened. The mutex covers threads in this process
        // (on macOS `flock` does not block another thread of the same
        // process). The file lock covers other processes. Both guards stay
        // alive until this function returns, so the open finishes before
        // either lock is released.
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let thread_lock = in_process_open_lock(path);
        let _thread_guard = thread_lock.lock().unwrap_or_else(|err| err.into_inner());
        let _file_lock = DatabaseFileLock::acquire(path)?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                legacy::ensure_current_database_file(parent, path)?;
            }
        }
        let mut conn = Connection::open(path)?;
        Self::configure(&conn)?;
        // Old table/value names are migrated first, so the domain migrations
        // below see a database that already speaks the current names.
        let legacy_migration = legacy::migrate_if_needed(&mut conn, path)?;
        if let Some(migrated) = &legacy_migration {
            eprintln!("{}", migrated.notice());
        }
        let mut store = Store {
            conn,
            schema_versions: BTreeMap::new(),
            legacy_migration,
        };
        store.apply_migrations(migrations)?;
        Ok(store)
    }

    /// What the one-time rename of the old table/value names did while this
    /// store was being opened, if it ran.
    pub fn legacy_migration(&self) -> Option<&legacy::Migrated> {
        self.legacy_migration.as_ref()
    }

    /// In-memory store for tests.
    pub fn open_in_memory(migrations: &FrozenMigrations) -> Result<Store> {
        let conn = Connection::open_in_memory()?;
        Self::configure(&conn)?;
        let mut store = Store {
            conn,
            schema_versions: BTreeMap::new(),
            legacy_migration: None,
        };
        store.apply_migrations(migrations)?;
        Ok(store)
    }

    fn configure(conn: &Connection) -> Result<()> {
        // Before the journal-mode change, so a switch that does call the
        // busy handler waits. Some exclusive-lock races never call it;
        // `Store::open` keeps those off this connection by admitting one
        // opener at a time.
        conn.busy_timeout(Duration::from_millis(5_000))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    }

    fn apply_migrations(&mut self, migrations: &FrozenMigrations) -> Result<()> {
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
            // Read-only fast path for a database that is already current.
            // A concurrent first open can still pass this check on a stale
            // snapshot, so the decision is repeated after the write lock.
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
            let already = tx.query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE domain = ?1 AND version = ?2",
                rusqlite::params![migration.domain.as_str(), migration.version as i64],
                |row| row.get::<_, i64>(0),
            )? > 0;
            if already {
                continue;
            }
            tx.execute_batch(migration.sql).map_err(|err| {
                if migration.domain == DOMAIN_MAINTENANCE
                    && migration.version == 2
                    && matches!(&err, rusqlite::Error::SqliteFailure(inner, _)
                        if inner.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE)
                {
                    Error::Migration(format!(
                        "maintenance v2 could not backfill conflicting proposal keys ({err}); \
                         no proposal was changed. Stop the server, preserve the database with \
                         SQLite backup, then inspect both proposals and resolve the key conflict \
                         as described in the Wiki (F01-F04-Acceptance). A private raw \
                         inventory is available via `viva data export --out <dir>`"
                    ))
                } else {
                    err.into()
                }
            })?;
            if let Some(apply) = migration.apply {
                apply(&tx)?;
            }
            tx.execute(
                "INSERT INTO schema_migrations(domain, version, name, applied_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![migration.domain.as_str(), migration.version as i64, migration.name, utc_now()],
            )?;
            tx.commit()?;
        }
        self.reload_schema_versions()?;
        Ok(())
    }

    fn reload_schema_versions(&mut self) -> Result<()> {
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
    pub fn schema_meta(&self) -> Result<BTreeMap<String, String>> {
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
    pub fn transaction(&self) -> Result<rusqlite::Transaction<'_>> {
        Ok(self.conn.unchecked_transaction()?)
    }

    /// Applied migration rows, for diagnostics and drift checks.
    pub fn applied_migrations(&self) -> Result<Vec<(String, u32, String, String)>> {
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
    pub fn row_count(&self, table: &str) -> Result<i64> {
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
    pub fn is_applied(&self, domain: Domain, version: u32) -> Result<bool> {
        let count = self.conn.query_row(
            "SELECT COUNT(*) FROM schema_migrations WHERE domain = ?1 AND version = ?2",
            rusqlite::params![domain.as_str(), version as i64],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count > 0)
    }

    /// Applied domain set as a HashSet (helper for migration tests).
    pub fn applied_versions(&self, domain: Domain) -> Result<HashSet<u32>> {
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
        let db = dir.path().join("viva.db");
        let frozen = foundation_only();

        {
            let store = Store::open(&db, &frozen).expect("open");
            assert_eq!(store.schema_versions().get(&DOMAIN_FOUNDATION), Some(&1));
            assert_eq!(store.row_count("events").expect("count"), 0);
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
        let db = dir.path().join("viva.db");

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
        let db = dir.path().join("viva.db");
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
        let db = dir.path().join("viva.db");
        let store = Store::open(&db, &foundation_only()).expect("open");

        let tx = store.transaction().expect("tx");
        tx.execute(
            "INSERT INTO events(event_id, occurred_at, domain, kind, subject_type, subject_id, origin, payload)
             VALUES ('evt-1', '2026-01-01T00:00:00Z', 'foundation', 'demo', 'user', 'u1', 'cli', '{}')",
            [],
        )
        .expect("insert inside tx");
        let broken = tx.execute_batch("SELECT * FROM definitely_missing_table;");
        assert!(broken.is_err(), "failure inside tx");
        drop(tx); // rollback

        assert_eq!(
            store.row_count("events").expect("count"),
            0,
            "no half event may survive"
        );
    }

    #[test]
    fn event_log_is_append_only_at_the_database_level() {
        let store = Store::open_in_memory(&foundation_only()).expect("open");
        store
            .connection()
            .execute(
                "INSERT INTO events(event_id, occurred_at, domain, kind, subject_type, subject_id, origin, payload)
                 VALUES ('evt-1', '2026-01-01T00:00:00Z', 'foundation', 'demo', 'user', 'u1', 'cli', '{}')",
                [],
            )
            .expect("insert");
        let update = store
            .connection()
            .execute("UPDATE events SET kind = 'tampered'", []);
        assert!(update.is_err(), "UPDATE must be rejected");
        let delete = store.connection().execute("DELETE FROM events", []);
        assert!(delete.is_err(), "DELETE must be rejected");
        assert_eq!(store.row_count("events").expect("count"), 1);
    }

    #[test]
    fn schema_meta_records_layout_version() {
        let store = Store::open_in_memory(&foundation_only()).expect("open");
        let meta = store.schema_meta().expect("meta");
        assert_eq!(meta.get("layout_version").map(String::as_str), Some("1"));
    }

    /// A rollback-journal file switched to WAL takes an exclusive lock.
    /// Openers that arrive together must wait that lock out, not fail it.
    #[test]
    fn concurrent_first_opens_of_a_rollback_journal_database_all_succeed() {
        let dir = TempDir::new().expect("tempdir");
        for trial in 0..12 {
            let db = dir.path().join(format!("trial-{trial}.db"));
            {
                let conn = rusqlite::Connection::open(&db).expect("seed");
                conn.execute_batch("CREATE TABLE seeded (id INTEGER PRIMARY KEY);")
                    .expect("seed table");
            }
            let start = std::sync::Arc::new(std::sync::Barrier::new(8));
            let mut handles = Vec::new();
            for _ in 0..8 {
                let db = db.clone();
                let start = std::sync::Arc::clone(&start);
                handles.push(std::thread::spawn(move || {
                    start.wait();
                    let store =
                        Store::open(&db, &foundation_only()).expect("every opener succeeds");
                    let mode: String = store
                        .connection()
                        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                        .expect("journal mode");
                    assert_eq!(mode, "wal");
                }));
            }
            for handle in handles {
                handle.join().expect("thread");
            }
        }
    }
}
