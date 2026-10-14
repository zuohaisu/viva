//! One-time migration of the database names Viva used before "office" was
//! removed from the product (issue #97, block 4 of 6 of the removal tracked
//! in #89).
//!
//! This is the only module that still spells the old names. It runs inside
//! [`Store::open`](crate::foundation::store::Store::open) **before** the
//! domain migrations, and only when it finds old names:
//!
//! 1. the pre-migration state is saved with `VACUUM INTO` to
//!    `<state root>/backups/<db file name>.pre-office-removal.<UTC stamp>`
//!    (directory 0700, file 0600); if that fails nothing is migrated;
//! 2. one `BEGIN IMMEDIATE` transaction then renames the six tables, swaps
//!    the append-only triggers (they must be dropped before the event rows
//!    can be rewritten), renames the migration-registry domain and names,
//!    and rewrites the values Viva itself wrote into event rows;
//! 3. the result is verified inside the same transaction; any leftover old
//!    name rolls the whole migration back.
//!
//! Re-running it is a no-op: a migrated database has no old names left, so
//! detection finds nothing. The migration only moves forward; going back is
//! "stop the server, restore the backup, use the old binary".
//!
//! An older server that is still running blocks the migration (its tables
//! would vanish under it), and so does a live handoff from one: the new
//! binary cannot adopt terminals from a server that still speaks the old
//! names. Values the *user* typed (workspace names, task goals, events
//! appended through the CLI) are never touched.
//!
//! Issue #99 deletes this module once every installation has been migrated.

use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};

use crate::foundation::error::{Error, Result};
use crate::foundation::paths::{DATABASE_FILE, ensure_private_dir};

/// Directory (inside the state root) that holds the pre-migration backups.
const BACKUP_DIR: &str = "backups";
/// Marks a backup as the one taken before the old names were removed.
const BACKUP_INFIX: &str = "pre-office-removal";
/// The control socket the old server listens on (renamed by issue #98).
pub const OLD_SOCKET_NAME: &str = "office.sock";
/// The database file the old server kept inside the state root (issue #98).
pub const OLD_DATABASE_FILE: &str = "office.db";

/// Tables renamed by this migration: `(old, new)`.
const TABLE_RENAMES: [(&str, &str); 6] = [
    ("office_events", "events"),
    ("office_sessions", "sessions"),
    ("office_executions", "executions"),
    ("office_hosts", "hosts"),
    ("office_settings", "settings"),
    ("office_recovery_events", "recovery_events"),
];
/// The old append-only triggers on the event log.
const OLD_TRIGGERS: [&str; 2] = ["office_events_no_update", "office_events_no_delete"];
/// The migration-registry domain (and event domain/origin) value of the
/// host registry, before and after.
const OLD_HOST_DOMAIN: &str = "office_host";
const NEW_HOST_DOMAIN: &str = "host";

/// What a successful migration did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migrated {
    /// The pre-migration backup (opens with the old names and old rows).
    pub backup: PathBuf,
    /// Tables that were renamed, `(old, new)`.
    pub tables_renamed: Vec<(&'static str, &'static str)>,
    /// Event column values rewritten (domain, origin, kind, subject type).
    pub event_values_rewritten: usize,
    /// Empty tables dropped because an older binary re-created them next to
    /// the migrated ones (see [`reconcile_downgrade_residue`]).
    pub leftovers_dropped: Vec<&'static str>,
}

impl Migrated {
    /// One line for the operator (CLI stderr, `server.log`).
    pub fn notice(&self) -> String {
        if self.tables_renamed.is_empty() {
            return format!(
                "viva: removed empty leftover tables ({}) that an older Viva version created \
                 when it was run against the upgraded database; the pre-cleanup copy is {}",
                self.leftovers_dropped.join(", "),
                self.backup.display()
            );
        }
        format!(
            "viva: upgraded the database to the current table and value names \
             ({} tables renamed, {} event values rewritten); the pre-upgrade copy is {}",
            self.tables_renamed.len(),
            self.event_values_rewritten,
            self.backup.display()
        )
    }
}

/// Migrate `conn` (the database at `db_path`) if it still uses the old
/// names. Returns `None` when there was nothing to do.
///
/// Idempotent and safe under concurrent openers: the work happens in one
/// `BEGIN IMMEDIATE` transaction, and detection is repeated inside it, so a
/// second opener that waited on the lock finds the database already done.
pub fn migrate_if_needed(conn: &mut Connection, db_path: &Path) -> Result<Option<Migrated>> {
    if !needs_migration(conn)? {
        return Ok(None);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !needs_migration(&tx)? {
        return Ok(None);
    }
    refuse_if_old_server_alive(&tx, db_path)?;
    // Nothing is written yet and the write lock is ours, so this copy is the
    // exact state the migration starts from. A failure here aborts before
    // any change (the transaction rolls back on drop).
    let backup = backup_database(db_path)?;
    let migrated = apply(&tx, backup)?;
    verify_clean(&tx)?;
    tx.commit()?;
    Ok(Some(migrated))
}

/// True when the database file at `db_path` exists and still uses the old
/// names. Read-only: never creates or changes the file.
pub fn is_legacy_database(db_path: &Path) -> Result<bool> {
    if !db_path.exists() {
        return Ok(false);
    }
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    needs_migration(&conn)
}

/// A live handoff cannot cross this upgrade: the resumed server would have
/// to migrate the database while the old server still serves from it. Called
/// before anything is asked of the running server, so it keeps serving.
pub fn refuse_live_handoff(db_path: &Path) -> Result<()> {
    if let Some(home) = db_path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        let legacy_db = home.join(OLD_DATABASE_FILE);
        if legacy_db.exists() {
            return Err(cross_version_upgrade_blocked(&format!(
                "the database file {} is still present",
                legacy_db.display()
            )));
        }
        if old_control_socket_alive(home)? {
            return Err(cross_version_upgrade_blocked(&format!(
                "the previous control socket {} is still accepting connections",
                home.join(OLD_SOCKET_NAME).display()
            )));
        }
    }
    if is_legacy_database(db_path)? {
        return Err(cross_version_upgrade_blocked(&format!(
            "the database {} still uses the previous table names",
            db_path.display()
        )));
    }
    Ok(())
}

fn cross_version_upgrade_blocked(detail: &str) -> Error {
    Error::Validation(format!(
        "{detail}. This upgrade cannot use the previous on-disk layout while the old server may \
         still be active; any running server was left untouched. Stop it with the previous \
         version's `viva shutdown`, then start Viva again to finish the migration"
    ))
}

/// Read-only CLI paths (`viva status`, …) must not report an empty store when
/// the previous `office.db` / `office.sock` layout is still present.
pub fn refuse_unmigrated_home_layout(home: &Path) -> Result<()> {
    refuse_live_handoff(&home.join(DATABASE_FILE))
}

/// True when the old control socket exists and a server is listening on it.
pub fn old_control_socket_alive(home: &Path) -> Result<bool> {
    let socket = home.join(OLD_SOCKET_NAME);
    if !socket.exists() {
        return Ok(false);
    }
    Ok(UnixStream::connect(&socket).is_ok())
}

/// Rename `office.db` → `viva.db` (and `-wal` / `-shm`) once on cold start.
/// Schema migration on the old file runs here before the rename; at runtime
/// the product never probes both names.
pub fn ensure_current_database_file(state_root: &Path, database_file: &Path) -> Result<()> {
    let Some(name) = database_file.file_name().and_then(|n| n.to_str()) else {
        return Ok(());
    };
    if name != DATABASE_FILE {
        return Ok(());
    }
    let legacy_db = state_root.join(OLD_DATABASE_FILE);
    if database_file.exists() {
        if legacy_db.exists() {
            return Err(Error::Migration(format!(
                "both {} and {} exist under {}; refusing to pick one. Inspect both files by \
                 hand or restore from a backup in `{BACKUP_DIR}/`",
                legacy_db.display(),
                database_file.display(),
                state_root.display()
            )));
        }
        return Ok(());
    }
    if !legacy_db.exists() {
        return Ok(());
    }
    if old_control_socket_alive(state_root)? {
        return Err(Error::Validation(format!(
            "the previous control socket {} is still accepting connections; refusing to rename \
             {} under a running server. Stop it with the previous version's `viva shutdown`, then \
             start Viva again",
            state_root.join(OLD_SOCKET_NAME).display(),
            legacy_db.display()
        )));
    }

    let conn = Connection::open(&legacy_db)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    refuse_if_old_server_alive(&conn, &legacy_db)?;
    conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
    drop(conn);

    rename_database_sidecars(state_root, OLD_DATABASE_FILE, DATABASE_FILE)?;
    let stale_socket = state_root.join(OLD_SOCKET_NAME);
    if stale_socket.exists() {
        let _ = std::fs::remove_file(stale_socket);
    }
    Ok(())
}

fn rename_database_sidecars(dir: &Path, old_base: &str, new_base: &str) -> Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let old = dir.join(format!("{old_base}{suffix}"));
        if !old.exists() {
            continue;
        }
        let new = dir.join(format!("{new_base}{suffix}"));
        if new.exists() {
            return Err(Error::Migration(format!(
                "cannot rename {} to {} because the destination already exists",
                old.display(),
                new.display()
            )));
        }
        std::fs::rename(&old, &new).map_err(|err| {
            Error::Migration(format!(
                "could not rename {} to {} ({err})",
                old.display(),
                new.display()
            ))
        })?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |row| row.get(0),
    )?)
}

fn row_count(conn: &Connection, table: &str) -> Result<i64> {
    Ok(
        conn.query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |row| {
            row.get(0)
        })?,
    )
}

/// True when any old table, old trigger, old registry domain or old
/// registry name is present.
fn needs_migration(conn: &Connection) -> Result<bool> {
    let names: Vec<String> = TABLE_RENAMES
        .iter()
        .map(|(old, _)| *old)
        .chain(OLD_TRIGGERS)
        .map(|name| format!("'{name}'"))
        .collect();
    let old_object: bool = conn.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name IN ({}))",
            names.join(", ")
        ),
        [],
        |row| row.get(0),
    )?;
    if old_object {
        return Ok(true);
    }
    if table_exists(conn, "schema_migrations")? {
        return Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE domain = ?1 OR name LIKE '%office%')",
            [OLD_HOST_DOMAIN],
            |row| row.get(0),
        )?);
    }
    Ok(false)
}

// ---------------------------------------------------------------------------
// The running-server guard
// ---------------------------------------------------------------------------

/// Refuse while a host recorded in the old `office_hosts` table is still
/// alive: an old server keeps using the old names and would break the moment
/// they change. A recorded pid that is alive but whose control socket exists
/// and refuses connections is a crash leftover with a recycled pid, not a
/// server, and does not block.
fn refuse_if_old_server_alive(conn: &Connection, db_path: &Path) -> Result<()> {
    let hosts_table = if table_exists(conn, "office_hosts")? {
        "office_hosts"
    } else if table_exists(conn, "hosts")? {
        "hosts"
    } else {
        return Ok(());
    };
    let hosts: Vec<(String, i64)> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT host_id, pid FROM {hosts_table} WHERE exited_at IS NULL"
        ))?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let socket = db_path
        .parent()
        .map(|dir| dir.join(OLD_SOCKET_NAME))
        .filter(|path| path.exists());
    for (host_id, pid) in hosts {
        if process_alive(pid) && !socket_refuses_connections(socket.as_deref()) {
            return Err(Error::Validation(format!(
                "an older Viva server (host `{host_id}`, pid {pid}) is still running against {}; \
                 refusing to migrate the database under it. Stop it with the older version's \
                 `viva shutdown`, then start Viva again. Nothing was changed",
                db_path.display()
            )));
        }
    }
    Ok(())
}

/// `kill(pid, 0)`: the process exists (or exists but belongs to someone
/// else). Pids that cannot name one process (0, negative, init) are dead.
fn process_alive(pid: i64) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 1 {
        return false;
    }
    // SAFETY: signal 0 performs only the existence/permission check.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// True only when the socket file exists and nothing listens on it.
fn socket_refuses_connections(socket: Option<&Path>) -> bool {
    let Some(path) = socket else {
        return false;
    };
    matches!(
        UnixStream::connect(path),
        Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused
    )
}

// ---------------------------------------------------------------------------
// Backup
// ---------------------------------------------------------------------------

fn backup_error(what: &Path, detail: impl std::fmt::Display) -> Error {
    Error::Migration(format!(
        "could not back up the database to {} before migrating it ({detail}); \
         nothing was migrated and the previous version can keep using the database",
        what.display()
    ))
}

/// `YYYYMMDDTHHMMSSZ`, UTC.
fn file_stamp() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

/// `VACUUM INTO` a new 0600 file in `<state root>/backups` (0700), then
/// check the copy opens and passes `quick_check`.
fn backup_database(db_path: &Path) -> Result<PathBuf> {
    let state_root = db_path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let dir = state_root.join(BACKUP_DIR);
    ensure_private_dir(&dir).map_err(|err| backup_error(&dir, err))?;
    let file_name = db_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| backup_error(&dir, "the database path has no UTF-8 file name"))?;
    let stamp = file_stamp();
    for attempt in 0..100 {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!("-{attempt}")
        };
        let target = dir.join(format!("{file_name}.{BACKUP_INFIX}.{stamp}{suffix}"));
        // The file is created 0600 *before* any data lands in it, so the
        // copy is never readable by others, not even briefly.
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&target)
        {
            Ok(_) => return copy_database(db_path, &target),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(backup_error(&target, err)),
        }
    }
    Err(backup_error(&dir, "no free backup file name"))
}

fn copy_database(db_path: &Path, target: &Path) -> Result<PathBuf> {
    let attempt = (|| -> Result<()> {
        let target_text = target
            .to_str()
            .ok_or_else(|| Error::Validation("backup path is not UTF-8".into()))?;
        let source = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        source.busy_timeout(Duration::from_secs(5))?;
        source.execute("VACUUM INTO ?1", [target_text])?;
        let copy = Connection::open_with_flags(target, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let verdict: String = copy.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if verdict != "ok" {
            return Err(Error::Validation(format!("quick_check said `{verdict}`")));
        }
        Ok(())
    })();
    match attempt {
        Ok(()) => Ok(target.to_path_buf()),
        Err(err) => {
            // A backup that was not verified must not be left behind looking
            // like one.
            let _ = std::fs::remove_file(target);
            Err(backup_error(target, err))
        }
    }
}

// ---------------------------------------------------------------------------
// The migration
// ---------------------------------------------------------------------------

/// Owned by the foundation v1 migration; recreated here under the new names
/// after the event rows were rewritten (text kept identical on purpose, a
/// test compares it with a freshly created database).
const NEW_TRIGGERS_SQL: &str = r#"
CREATE TRIGGER events_no_update
BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'events is append-only'); END;
CREATE TRIGGER events_no_delete
BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'events is append-only'); END;
"#;

fn apply(tx: &Connection, backup: PathBuf) -> Result<Migrated> {
    let leftovers_dropped = reconcile_downgrade_residue(tx)?;

    // The triggers go first: the event rows cannot be rewritten while they
    // exist. DROP is neither UPDATE nor DELETE, so append-only is not
    // violated; the new triggers are in place before the transaction ends.
    for trigger in OLD_TRIGGERS {
        tx.execute_batch(&format!("DROP TRIGGER IF EXISTS {trigger}"))?;
    }

    let mut tables_renamed = Vec::new();
    for (old, new) in TABLE_RENAMES {
        if !table_exists(tx, old)? {
            continue;
        }
        if table_exists(tx, new)? {
            return Err(both_exist(old, new, row_count(tx, old)?));
        }
        // Rewrites the REFERENCES in other tables (`executions`,
        // `conversation_nodes`, `launch_intents`) and sqlite_sequence.
        tx.execute_batch(&format!("ALTER TABLE {old} RENAME TO {new}"))?;
        tables_renamed.push((old, new));
    }

    if table_exists(tx, "schema_migrations")? {
        // A downgrade attempt can leave `office_host` rows next to the
        // `host` ones; the migrated rows win.
        tx.execute(
            "DELETE FROM schema_migrations WHERE domain = ?1 \
             AND version IN (SELECT version FROM schema_migrations WHERE domain = ?2)",
            params![OLD_HOST_DOMAIN, NEW_HOST_DOMAIN],
        )?;
        tx.execute(
            "UPDATE schema_migrations SET domain = ?2 WHERE domain = ?1",
            params![OLD_HOST_DOMAIN, NEW_HOST_DOMAIN],
        )?;
        tx.execute(
            "UPDATE schema_migrations \
             SET name = replace(replace(name, 'office host', 'host'), 'office ', '') \
             WHERE name LIKE '%office%'",
            [],
        )?;
    }

    let mut event_values_rewritten = 0;
    if table_exists(tx, "events")? {
        // Only rows Viva wrote itself (origin `office_host`) get their kind
        // and subject type rewritten; the same words in an event someone
        // appended through the CLI are that person's content.
        for (column, old, new) in [
            ("kind", "office_paused", "paused"),
            ("kind", "office_resumed", "resumed"),
            ("subject_type", "office", NEW_HOST_DOMAIN),
        ] {
            event_values_rewritten += tx.execute(
                &format!("UPDATE events SET {column} = ?2 WHERE {column} = ?1 AND origin = ?3"),
                params![old, new, OLD_HOST_DOMAIN],
            )?;
        }
        event_values_rewritten += tx.execute(
            "UPDATE events SET domain = ?2 WHERE domain = ?1",
            params![OLD_HOST_DOMAIN, NEW_HOST_DOMAIN],
        )?;
        event_values_rewritten += tx.execute(
            "UPDATE events SET origin = ?2 WHERE origin = ?1",
            params![OLD_HOST_DOMAIN, NEW_HOST_DOMAIN],
        )?;

        for trigger in ["events_no_update", "events_no_delete"] {
            tx.execute_batch(&format!("DROP TRIGGER IF EXISTS {trigger}"))?;
        }
        tx.execute_batch(NEW_TRIGGERS_SQL)?;
    }

    // Free-text messages the old code could persist. No production path
    // writes them today (the control ledger and the gh evidence rejection
    // are reachable only from tests and misuse), but a row that exists must
    // not keep the old word.
    if table_exists(tx, "control_requests")? {
        tx.execute(
            "UPDATE control_requests \
             SET rejection = replace(rejection, 'was never issued by this office', \
                                                'was never issued by this server') \
             WHERE rejection LIKE '%was never issued by this office%'",
            [],
        )?;
    }
    if table_exists(tx, "github_evidence")? {
        tx.execute(
            "UPDATE github_evidence \
             SET state_json = replace(state_json, 'the office''s read-only surface', \
                                                  'Viva''s read-only surface') \
             WHERE state_json LIKE '%the office''s read-only surface%'",
            [],
        )?;
    }

    Ok(Migrated {
        backup,
        tables_renamed,
        event_values_rewritten,
        leftovers_dropped,
    })
}

fn both_exist(old: &str, new: &str, old_rows: i64) -> Error {
    Error::Migration(format!(
        "the database has both `{old}` ({old_rows} rows) and `{new}`; refusing to merge them. \
         Nothing was changed. Restore the pre-upgrade copy from the `backups` directory next to \
         the database, or inspect both tables by hand"
    ))
}

/// A previous version that was run against an already migrated database
/// re-creates the three empty host tables (and their registry rows) before
/// failing on `recovery_plans`. Empty leftovers are dropped; anything with
/// rows is refused rather than guessed at.
fn reconcile_downgrade_residue(tx: &Connection) -> Result<Vec<&'static str>> {
    let mut dropped = Vec::new();
    for (old, new) in TABLE_RENAMES {
        if table_exists(tx, old)? && table_exists(tx, new)? {
            let rows = row_count(tx, old)?;
            if rows != 0 {
                return Err(both_exist(old, new, rows));
            }
            tx.execute_batch(&format!("DROP TABLE {old}"))?;
            dropped.push(old);
        }
    }
    Ok(dropped)
}

// ---------------------------------------------------------------------------
// Post-condition
// ---------------------------------------------------------------------------

/// Drop `-- line` and `/* block */` comments, leaving string literals and
/// quoted identifiers alone.
fn strip_sql_comments(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            out.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => {
                quote = Some(c);
                out.push(c);
            }
            '-' if chars.peek() == Some(&'-') => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for next in chars.by_ref() {
                    if prev == '*' && next == '/' {
                        break;
                    }
                    prev = next;
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out
}

fn mentions_office(text: &str) -> bool {
    text.to_ascii_lowercase().contains("office")
}

/// The migration's own acceptance test, run before commit: no old name may
/// survive in the schema (comments inside stored statements are not schema),
/// the migration registry, or the Viva-written event values.
fn verify_clean(tx: &Connection) -> Result<()> {
    let mut left: Vec<String> = Vec::new();

    let mut stmt = tx.prepare("SELECT type, name, sql FROM sqlite_master")?;
    let objects = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    for object in objects {
        let (kind, name, sql) = object?;
        let in_sql = sql
            .as_deref()
            .is_some_and(|sql| mentions_office(&strip_sql_comments(sql)));
        if mentions_office(&name) || in_sql {
            left.push(format!("{kind} `{name}`"));
        }
    }
    drop(stmt);

    if table_exists(tx, "sqlite_sequence")? {
        let mut stmt = tx.prepare("SELECT name FROM sqlite_sequence")?;
        for name in stmt.query_map([], |row| row.get::<_, String>(0))? {
            let name = name?;
            if mentions_office(&name) {
                left.push(format!("sqlite_sequence entry `{name}`"));
            }
        }
    }

    if table_exists(tx, "schema_migrations")? {
        let mut stmt = tx.prepare("SELECT domain, version, name FROM schema_migrations")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (domain, version, name) = row?;
            if mentions_office(&domain) || mentions_office(&name) {
                left.push(format!("schema_migrations `{domain}` v{version} `{name}`"));
            }
        }
    }

    if table_exists(tx, "events")? {
        let stale: i64 = tx.query_row(
            "SELECT COUNT(*) FROM events WHERE domain = ?1 OR origin = ?1",
            [OLD_HOST_DOMAIN],
            |row| row.get(0),
        )?;
        if stale != 0 {
            left.push(format!("{stale} event row(s) with the old host values"));
        }
    }

    if left.is_empty() {
        return Ok(());
    }
    left.truncate(8);
    Err(Error::Migration(format!(
        "the migration left old names behind ({}); it was rolled back and the database is \
         unchanged. Keep the pre-upgrade copy in the `backups` directory and report this",
        left.join("; ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_are_stripped_but_strings_and_identifiers_are_not() {
        let sql = "CREATE TABLE t ( -- the office\n  a TEXT /* office */ DEFAULT 'a -- office', \
                   \"office\" TEXT\n)";
        let stripped = strip_sql_comments(sql);
        assert!(!stripped.contains("the office"), "{stripped}");
        assert!(stripped.contains("'a -- office'"), "{stripped}");
        assert!(stripped.contains("\"office\""), "{stripped}");
        assert!(!stripped.contains("/*"), "{stripped}");
    }

    #[test]
    fn only_real_processes_count_as_alive() {
        assert!(process_alive(i64::from(std::process::id())));
        for impossible in [0, 1, -1, i64::MAX] {
            assert!(!process_alive(impossible), "pid {impossible}");
        }
    }

    #[test]
    fn a_missing_socket_is_not_proof_of_a_dead_server() {
        assert!(!socket_refuses_connections(None));
        assert!(!socket_refuses_connections(Some(Path::new(
            "/nonexistent/viva.sock"
        ))));
    }

    #[test]
    fn backup_stamp_is_a_sortable_utc_name() {
        let stamp = file_stamp();
        assert_eq!(stamp.len(), 16, "{stamp}");
        assert!(
            stamp.ends_with('Z') && stamp.as_bytes()[8] == b'T',
            "{stamp}"
        );
    }
}
