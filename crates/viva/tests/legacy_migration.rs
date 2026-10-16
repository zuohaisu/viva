//! The one-time rename of the pre-removal database names (issue #97).
//!
//! `fixtures/legacy-office-db.sql` was written by the real viva 0.3.6 binary
//! (see its header). Every test starts from a copy of it and opens it with
//! the current code.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags};
use tempfile::TempDir;

use viva::foundation::Error;
use viva::foundation::events::{self, NewEvent};
use viva::foundation::paths::database_path;
use viva::foundation::store::{Domain, Store};
use viva::server;

const FIXTURE: &str = include_str!("fixtures/legacy-office-db.sql");

const RENAMES: [(&str, &str); 6] = [
    ("office_events", "events"),
    ("office_sessions", "sessions"),
    ("office_executions", "executions"),
    ("office_hosts", "hosts"),
    ("office_settings", "settings"),
    ("office_recovery_events", "recovery_events"),
];

fn new_name(table: &str) -> &str {
    RENAMES
        .iter()
        .find(|(old, _)| *old == table)
        .map_or(table, |(_, new)| *new)
}

/// A home directory holding a database exactly as the old binary left it.
fn legacy_home() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let db = database_path(dir.path());
    let conn = Connection::open(&db).expect("create legacy db");
    conn.execute_batch(FIXTURE).expect("replay the fixture");
    drop(conn);
    (dir, db)
}

fn open(db: &Path) -> viva::foundation::Result<Store> {
    Store::open(db, server::migrations())
}

fn read_only(db: &Path) -> Connection {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open read-only")
}

fn user_tables(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .expect("prepare");
    stmt.query_map([], |row| row.get(0))
        .expect("query")
        .map(|row| row.expect("row"))
        .collect()
}

fn dump_table(conn: &Connection, table: &str) -> Vec<String> {
    let sql_rowid = format!("SELECT * FROM \"{table}\" ORDER BY rowid");
    let sql_plain = format!("SELECT * FROM \"{table}\"");
    let mut stmt = conn
        .prepare(&sql_rowid)
        .or_else(|_| conn.prepare(&sql_plain))
        .unwrap_or_else(|err| panic!("prepare dump for `{table}`: {err}"));
    let columns = stmt.column_count();
    stmt.query_map([], |row| {
        let mut cells = Vec::new();
        for index in 0..columns {
            let value: Value = row.get(index)?;
            cells.push(format!("{value:?}"));
        }
        Ok(cells.join(" | "))
    })
    .expect("query dump")
    .map(|row| row.expect("row"))
    .collect()
}

fn parse_migration_domain_version(row: &str) -> (String, u32) {
    let parts: Vec<&str> = row.split(" | ").collect();
    assert!(parts.len() >= 2, "migration row: {row}");
    let domain = parts[0]
        .trim()
        .strip_prefix("Text(\"")
        .and_then(|s| s.strip_suffix("\")"))
        .unwrap_or_else(|| panic!("domain cell in `{row}`"));
    let version = parts[1]
        .trim()
        .strip_prefix("Integer(")
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or_else(|| panic!("version cell in `{row}`"))
        .parse()
        .expect("version number");
    (domain.to_string(), version)
}

fn normalize_migration_domain(domain: &str) -> &str {
    if domain == "office_host" {
        "host"
    } else {
        domain
    }
}

fn migration_keys(rows: &[String]) -> BTreeSet<(String, u32)> {
    rows.iter()
        .map(|row| {
            let (domain, version) = parse_migration_domain_version(row);
            (normalize_migration_domain(&domain).to_string(), version)
        })
        .collect()
}

/// Everything logical in a database: the schema rows and every user table.
fn logical_dump(db: &Path) -> BTreeMap<String, Vec<String>> {
    let conn = read_only(db);
    let mut dump = BTreeMap::new();
    dump.insert("<schema>".to_string(), {
        let mut stmt = conn
            .prepare("SELECT type, name, sql FROM sqlite_master ORDER BY type, name")
            .expect("prepare");
        stmt.query_map([], |row| {
            Ok(format!(
                "{} {} {:?}",
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?
            ))
        })
        .expect("query")
        .map(|row| row.expect("row"))
        .collect()
    });
    for table in user_tables(&conn) {
        dump.insert(table.clone(), dump_table(&conn, &table));
    }
    dump
}

/// `-- line` comments inside stored CREATE statements are text, not schema.
fn without_comments(sql: &str) -> String {
    sql.lines()
        .map(|line| line.split("--").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Schema normalised so a migrated database can be compared with a fresh one:
/// RENAME leaves the new table name double-quoted; whitespace is layout.
fn normalized_schema(db: &Path) -> BTreeMap<(String, String), String> {
    let conn = read_only(db);
    let mut stmt = conn
        .prepare("SELECT type, name, sql FROM sqlite_master WHERE sql IS NOT NULL")
        .expect("prepare");
    stmt.query_map([], |row| {
        let sql: String = row.get(2)?;
        let flat = without_comments(&sql).replace('"', "");
        Ok((
            (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
            flat.split_whitespace().collect::<Vec<_>>().join(" "),
        ))
    })
    .expect("query")
    .map(|row| row.expect("row"))
    .collect()
}

/// Every place the old word can live in a database, other than the content
/// people typed: schema object names and statements (comments excluded), the
/// migration registry, sequence names, and the event rows Viva wrote itself.
fn old_names_left(db: &Path) -> Vec<String> {
    let conn = read_only(db);
    let mut left = Vec::new();
    let has = |text: &str| text.to_ascii_lowercase().contains("office");
    for ((kind, name), sql) in normalized_schema(db) {
        if has(&name) || has(&sql) {
            left.push(format!("schema: {kind} {name}"));
        }
    }
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE name LIKE '%office%'")
        .expect("prepare");
    for name in stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query")
    {
        left.push(format!("sqlite_master name: {}", name.expect("row")));
    }
    for row in dump_table(&conn, "schema_migrations") {
        if has(&row) {
            left.push(format!("schema_migrations: {row}"));
        }
    }
    for row in dump_table(&conn, "sqlite_sequence") {
        if has(&row) {
            left.push(format!("sqlite_sequence: {row}"));
        }
    }
    let table = if user_tables(&conn).contains(&"events".to_string()) {
        "events"
    } else {
        "office_events"
    };
    for row in dump_table(&conn, table) {
        // `cli` rows are what a person appended; Viva-written rows must be clean.
        if has(&row) && !row.contains("\"cli\"") {
            left.push(format!("{table}: {row}"));
        }
    }
    left
}

fn backups(home: &Path) -> Vec<PathBuf> {
    let dir = home.join("backups");
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            let mut files: Vec<PathBuf> = entries.map(|e| e.expect("entry").path()).collect();
            files.sort();
            files
        }
        Err(_) => Vec::new(),
    }
}

type EventRow = (
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
);

fn event_rows(db: &Path, table: &str) -> Vec<EventRow> {
    let conn = read_only(db);
    let mut stmt = conn
        .prepare(&format!(
            "SELECT seq, event_id, occurred_at, domain, kind, subject_type, subject_id, origin, payload \
             FROM {table} ORDER BY seq"
        ))
        .expect("prepare");
    stmt.query_map([], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
        ))
    })
    .expect("query")
    .map(|row| row.expect("row"))
    .collect()
}

// ---------------------------------------------------------------------------
// Upgrade
// ---------------------------------------------------------------------------

#[test]
fn upgrade_renames_everything_and_keeps_every_row() {
    let (dir, db) = legacy_home();
    let before = logical_dump(&db);
    let events_before = event_rows(&db, "office_events");
    assert!(
        !old_names_left(&db).is_empty(),
        "the fixture must start with old names"
    );
    for (old, _) in RENAMES {
        assert!(
            !before[old].is_empty(),
            "the fixture needs rows in `{old}` for this test to mean anything"
        );
    }

    let store = open(&db).expect("the new binary opens an old database");
    let migrated = store.legacy_migration().expect("the migration ran").clone();
    assert_eq!(migrated.tables_renamed.len(), 6);
    drop(store);

    // No old name anywhere Viva owns.
    assert_eq!(old_names_left(&db), Vec::<String>::new());

    // Every row survived under the new table name, byte for byte, except the
    // values the migration is meant to rewrite.
    let after = logical_dump(&db);
    for (table, rows) in &before {
        if table == "<schema>" || table == "office_events" {
            continue;
        }
        if matches!(
            table.as_str(),
            "schema_migrations" | "control_requests" | "github_evidence"
        ) {
            if table == "schema_migrations" {
                assert!(
                    after[table].len() >= rows.len(),
                    "schema_migrations must not shrink"
                );
                let before_keys = migration_keys(rows);
                let after_keys = migration_keys(&after[table]);
                let appended: BTreeSet<_> = after_keys.difference(&before_keys).cloned().collect();
                let expected: BTreeSet<(String, u32)> = BTreeSet::from([
                    ("host".into(), 4),
                    ("memory".into(), 3),
                    ("memory".into(), 4),
                    ("skills".into(), 1),
                ]);
                assert_eq!(
                    appended, expected,
                    "only expected pending domain migrations may append rows"
                );
            } else {
                assert_eq!(after[table].len(), rows.len(), "row count of {table}");
            }
            continue;
        }
        assert_eq!(&after[new_name(table)], rows, "rows of {table}");
    }
    assert_eq!(after["events"].len(), before["office_events"].len());

    // Event rows: ids, order, time, subject id and payload never change; the
    // five Viva-written spellings do, the person-typed row does not.
    let events_after = event_rows(&db, "events");
    assert_eq!(events_after.len(), events_before.len());
    for (old, new) in events_before.iter().zip(&events_after) {
        assert_eq!(
            (&old.0, &old.1, &old.2, &old.6, &old.8),
            (&new.0, &new.1, &new.2, &new.6, &new.8)
        );
        if old.7 == "cli" {
            assert_eq!(
                old, new,
                "an event someone appended is theirs and unchanged"
            );
            continue;
        }
        assert_eq!(old.7, "office_host", "fixture sanity: Viva-written origin");
        assert_eq!(new.7, "host");
        assert_eq!(
            new.3,
            if old.3 == "office_host" {
                "host"
            } else {
                old.3.as_str()
            }
        );
        let kind = match old.4.as_str() {
            "office_paused" => "paused",
            "office_resumed" => "resumed",
            other => other,
        };
        assert_eq!(new.4, kind);
        assert_eq!(
            new.5,
            if old.5 == "office" {
                "host"
            } else {
                old.5.as_str()
            }
        );
    }
    assert!(
        events_after
            .iter()
            .any(|e| e.4 == "paused" && e.5 == "host")
    );
    assert!(events_after.iter().any(|e| e.4 == "resumed"));
    // The person-typed row keeps the words, so a blanket `office` check on
    // event rows would be wrong, and the migration does not make it.
    assert!(
        events_after
            .iter()
            .any(|e| e.7 == "cli" && e.4 == "office_paused" && e.5 == "office")
    );

    // User content is untouched.
    let conn = read_only(&db);
    let name: String = conn
        .query_row("SELECT display_name FROM projects", [], |r| r.get(0))
        .expect("project");
    assert_eq!(name, "Haisu's office");
    let goal: String = conn
        .query_row("SELECT goal FROM tasks", [], |r| r.get(0))
        .expect("task");
    assert_eq!(goal, "Migrate the office host");
    let title: String = conn
        .query_row(
            "SELECT title FROM sessions WHERE session_id = 'sess-fixture-chat'",
            [],
            |r| r.get(0),
        )
        .expect("session");
    assert_eq!(title, "Haisu's office chat");

    // The two free-text messages the old code could persist.
    let rejection: String = conn
        .query_row("SELECT rejection FROM control_requests", [], |r| r.get(0))
        .expect("rejection");
    assert!(
        rejection.contains("never issued by this server"),
        "{rejection}"
    );
    let evidence: String = conn
        .query_row("SELECT state_json FROM github_evidence", [], |r| r.get(0))
        .expect("evidence");
    assert!(evidence.contains("Viva's read-only surface"), "{evidence}");

    // The registry now reads exactly like a fresh database's.
    let registry = |db: &Path| {
        let mut rows: Vec<String> = dump_table(&read_only(db), "schema_migrations")
            .iter()
            .map(|row| row.rsplit_once(" | ").expect("applied_at").0.to_string())
            .collect();
        rows.sort();
        rows
    };
    let fresh = TempDir::new().expect("fresh dir");
    let fresh_db = database_path(fresh.path());
    drop(open(&fresh_db).expect("fresh open"));
    assert_eq!(
        registry(&db),
        registry(&fresh_db),
        "migrated and fresh databases register the same migrations"
    );
    // ... and the schema is the fresh schema (comments inside stored
    // statements are the SQL-comment cleanup of #95, not schema).
    assert_eq!(normalized_schema(&db), normalized_schema(&fresh_db));
    assert!(dir.path().join("backups").is_dir());
}

#[test]
fn append_only_triggers_survive_with_their_new_names() {
    let (_dir, db) = legacy_home();
    drop(open(&db).expect("migrate"));
    let conn = Connection::open(&db).expect("writer");

    let update = conn
        .execute("UPDATE events SET kind = 'tampered'", [])
        .expect_err("UPDATE must be rejected");
    assert!(
        update.to_string().contains("events is append-only"),
        "{update}"
    );
    let delete = conn
        .execute("DELETE FROM events", [])
        .expect_err("DELETE must be rejected");
    assert!(
        delete.to_string().contains("events is append-only"),
        "{delete}"
    );

    let triggers: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'trigger' ORDER BY name")
            .expect("prepare");
        stmt.query_map([], |row| row.get(0))
            .expect("query")
            .map(|row| row.expect("row"))
            .collect()
    };
    assert_eq!(
        triggers,
        [
            "events_no_delete",
            "events_no_update",
            "memory_facts_ad",
            "memory_facts_ai",
            "memory_facts_au",
        ]
    );
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .expect("count");
    assert_eq!(count, 6, "the rejected statements changed nothing");
}

#[test]
fn foreign_keys_and_sequences_follow_the_renamed_tables() {
    let (_dir, db) = legacy_home();
    let store = open(&db).expect("migrate");
    let conn = store.connection();

    let references = |table: &str| -> Vec<String> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA foreign_key_list(\"{table}\")"))
            .expect("prepare");
        stmt.query_map([], |row| row.get::<_, String>(2))
            .expect("query")
            .map(|row| row.expect("row"))
            .collect()
    };
    assert_eq!(references("executions"), ["sessions"]);
    assert!(references("conversation_nodes").contains(&"sessions".to_string()));
    assert!(references("launch_intents").contains(&"executions".to_string()));
    for table in ["executions", "conversation_nodes", "launch_intents"] {
        assert!(
            references(table)
                .iter()
                .all(|target| !target.contains("office")),
            "{table}"
        );
    }
    let violations: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .expect("fk check");
    assert_eq!(violations, 0);
    let dangling = conn.execute(
        "INSERT INTO executions(execution_id, task_id, session_id, member_id, attribution, status, started_at, updated_at) \
         VALUES ('exec-dangling', 't', 'sess-nope', 'm', '{}', 'requested', 'now', 'now')",
        [],
    );
    assert!(
        dangling.is_err(),
        "the renamed foreign key is still enforced"
    );

    // New writes continue the old sequence: the event log never restarts.
    let appended = events::append(
        &store,
        NewEvent {
            domain: Domain::new("host"),
            kind: "paused".into(),
            subject_type: "host".into(),
            subject_id: "host-x".into(),
            origin: "host".into(),
            payload: serde_json::json!({}),
        },
    )
    .expect("append after migration");
    assert_eq!(appended.seq, 7);
}

#[test]
fn backup_is_a_private_openable_copy_of_the_pre_migration_state() {
    let (dir, db) = legacy_home();
    let before = logical_dump(&db);
    drop(open(&db).expect("migrate"));

    let files = backups(dir.path());
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_str().unwrap().to_string();
    assert!(name.starts_with("viva.db.pre-office-removal."), "{name}");
    let stamp = name.trim_start_matches("viva.db.pre-office-removal.");
    assert!(
        stamp.len() == 16 && stamp.ends_with('Z') && stamp.as_bytes()[8] == b'T',
        "UTC stamp, got `{stamp}`"
    );
    let file_mode = std::fs::metadata(&files[0]).unwrap().permissions().mode() & 0o777;
    let dir_mode = std::fs::metadata(dir.path().join("backups"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(file_mode, 0o600);
    assert_eq!(dir_mode, 0o700);

    // It opens, is healthy, and still has the old names and all old rows.
    let backup = read_only(&files[0]);
    let verdict: String = backup
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .expect("quick_check");
    assert_eq!(verdict, "ok");
    drop(backup);
    let restored = logical_dump(&files[0]);
    for (table, rows) in &before {
        if table == "<schema>" {
            continue;
        }
        assert_eq!(&restored[table], rows, "backup rows of {table}");
    }
    assert!(
        restored["<schema>"]
            .iter()
            .any(|l| l.contains("office_events_no_update"))
    );
}

#[test]
fn reopening_a_migrated_database_changes_nothing() {
    let (dir, db) = legacy_home();
    drop(open(&db).expect("first open migrates"));
    let after_first = logical_dump(&db);
    let backups_first = backups(dir.path());

    for _ in 0..3 {
        let store = open(&db).expect("reopen");
        assert!(
            store.legacy_migration().is_none(),
            "second open must not migrate"
        );
    }
    assert_eq!(
        logical_dump(&db),
        after_first,
        "reopen must not touch any row or schema text"
    );
    assert_eq!(backups(dir.path()), backups_first, "no second backup");
}

#[test]
fn concurrent_openers_migrate_exactly_once() {
    let (dir, db) = legacy_home();
    let start = std::sync::Arc::new(std::sync::Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let db = db.clone();
            let start = std::sync::Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                let store = open(&db).expect("every opener succeeds");
                store.legacy_migration().is_some()
            })
        })
        .collect();
    let migrators = handles
        .into_iter()
        .map(|h| h.join().expect("thread"))
        .filter(|ran| *ran)
        .count();
    assert_eq!(migrators, 1, "one opener migrates, the others find it done");
    assert_eq!(backups(dir.path()).len(), 1);
    assert_eq!(old_names_left(&db), Vec::<String>::new());
}

// ---------------------------------------------------------------------------
// Fresh installs
// ---------------------------------------------------------------------------

#[test]
fn a_new_database_never_has_the_old_names() {
    let dir = TempDir::new().expect("dir");
    let db = database_path(dir.path());
    let store = open(&db).expect("create");
    assert!(store.legacy_migration().is_none());
    drop(store);
    assert_eq!(old_names_left(&db), Vec::<String>::new());
    assert!(
        backups(dir.path()).is_empty(),
        "nothing to back up on a fresh database"
    );
    let conn = read_only(&db);
    for (_, new) in RENAMES {
        // `hosts`/`settings`/`recovery_events` belong to the server domain,
        // which `server::migrations()` registers.
        assert!(user_tables(&conn).contains(&new.to_string()), "{new}");
    }
}

fn viva(home: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_viva"))
        .args(args)
        .env("VIVA_HOME", home)
        .output()
        .expect("spawn viva");
    assert!(
        output.status.success(),
        "viva {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The real binary, a real server, the events Viva itself writes (pause,
/// resume, auto-pull) and a registered host: none of it may say "office".
#[test]
fn a_server_run_by_the_current_binary_writes_only_new_names() {
    let dir = TempDir::new().expect("dir");
    let home = dir.path().join("h");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "x",
        ][..],
    ] {
        let status = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .stdout(Stdio::null())
            .status()
            .expect("git");
        assert!(status.success());
    }
    viva(&home, &["init"]);
    viva(
        &home,
        &[
            "create-project",
            "--name",
            "p",
            "--repo",
            repo.to_str().unwrap(),
        ],
    );
    viva(
        &home,
        &["terminal", "create", "--purpose", "t", "--", "sleep", "3"],
    );
    viva(&home, &["pause"]);
    viva(&home, &["resume"]);
    viva(&home, &["sync", "auto-pull", "on"]);
    viva(&home, &["shutdown"]);
    for _ in 0..100 {
        if !home.join("office.sock").exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    let db = database_path(&home);
    assert_eq!(old_names_left(&db), Vec::<String>::new());
    let events = event_rows(&db, "events");
    for (kind, subject_type) in [
        ("paused", "host"),
        ("resumed", "host"),
        ("auto_pull_toggled", "host"),
    ] {
        assert!(
            events
                .iter()
                .any(|e| e.3 == "host" && e.4 == kind && e.5 == subject_type && e.7 == "host"),
            "expected a `{kind}` event written under the new names, got {events:?}"
        );
    }
    assert!(backups(&home).is_empty());
}

// ---------------------------------------------------------------------------
// A server of the previous version is still running
// ---------------------------------------------------------------------------

fn mark_host_running(db: &Path, pid: i64) {
    let conn = Connection::open(db).expect("writer");
    conn.execute(
        "UPDATE office_hosts SET exited_at = NULL, exit_kind = NULL, pid = ?1",
        [pid],
    )
    .expect("mark running");
}

fn is_validation(err: &Error, needle: &str) -> bool {
    matches!(err, Error::Validation(text) if text.contains(needle))
}

#[test]
fn an_old_server_that_is_still_alive_blocks_the_migration_and_changes_nothing() {
    let (dir, db) = legacy_home();
    // A real, live process stands in for the old server's pid.
    let mut sleeper = Command::new("sleep").arg("30").spawn().expect("sleep");
    mark_host_running(&db, i64::from(sleeper.id()));
    let before = logical_dump(&db);

    let err = open(&db).err().expect("must refuse");
    assert!(is_validation(&err, "viva shutdown"), "{err}");
    assert!(is_validation(&err, "still running"), "{err}");
    assert_eq!(
        logical_dump(&db),
        before,
        "the database is exactly as it was"
    );
    assert!(backups(dir.path()).is_empty(), "no backup, no partial work");

    // Retrying while it is alive refuses the same way ...
    assert!(open(&db).is_err());
    // ... and once the old server is gone the same call goes through.
    sleeper.kill().expect("kill");
    sleeper.wait().expect("wait");
    assert!(
        open(&db)
            .expect("migrates after the old server exited")
            .legacy_migration()
            .is_some()
    );
    assert_eq!(old_names_left(&db), Vec::<String>::new());
}

#[test]
fn a_dead_recorded_pid_is_a_crash_not_a_server() {
    let (_dir, db) = legacy_home();
    let mut gone = Command::new("sleep").arg("30").spawn().expect("sleep");
    let pid = i64::from(gone.id());
    gone.kill().expect("kill");
    gone.wait().expect("wait");
    mark_host_running(&db, pid);

    let store = open(&db).expect("a host row left by a crash does not block the upgrade");
    assert!(store.legacy_migration().is_some());
}

#[test]
fn a_live_listener_on_the_old_socket_blocks_and_a_stale_one_does_not() {
    let (dir, db) = legacy_home();
    // The pid is alive (this process), so only the socket can decide.
    mark_host_running(&db, i64::from(std::process::id()));
    let socket = dir.path().join("office.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let err = open(&db).err().expect("a listening server blocks");
    assert!(is_validation(&err, "viva shutdown"), "{err}");

    // The socket file outlives a crashed server; nothing accepts on it. The
    // pid has since been recycled by an unrelated process.
    drop(listener);
    assert!(socket.exists(), "the socket file is left behind");
    let store = open(&db).expect("a stale socket plus a recycled pid is not a server");
    assert!(store.legacy_migration().is_some());
}

#[test]
fn live_handoff_from_an_old_server_is_refused_before_it_is_asked() {
    let (dir, db) = legacy_home();
    mark_host_running(&db, i64::from(std::process::id()));
    let before = logical_dump(&db);

    let err = viva::foundation::legacy::refuse_live_handoff(&db).expect_err("refused");
    assert!(is_validation(&err, "viva shutdown"), "{err}");
    assert!(is_validation(&err, "previous table names"), "{err}");

    // `viva server --resume` takes the same path and must not wait for a
    // handoff socket (30 seconds) or open the database.
    let started = std::time::Instant::now();
    let err = server::resume_server(dir.path()).expect_err("resume refused");
    assert!(is_validation(&err, "viva shutdown"), "{err}");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    // So does the restart that would have asked the old server to hand over.
    let err = server::restart_server_with(dir.path(), Path::new("/nonexistent/viva"))
        .expect_err("restart refused");
    assert!(is_validation(&err, "viva shutdown"), "{err}");
    assert_eq!(
        logical_dump(&db),
        before,
        "the old server's database is untouched"
    );
    assert!(backups(dir.path()).is_empty());
}

#[test]
fn a_migrated_or_new_database_does_not_block_a_live_handoff() {
    let dir = TempDir::new().expect("dir");
    let db = database_path(dir.path());
    viva::foundation::legacy::refuse_live_handoff(&db).expect("no database yet");
    drop(open(&db).expect("create"));
    viva::foundation::legacy::refuse_live_handoff(&db).expect("current names");
}

// ---------------------------------------------------------------------------
// Failure paths: nothing is migrated half-way
// ---------------------------------------------------------------------------

#[test]
fn a_failed_backup_aborts_before_any_change() {
    let (dir, db) = legacy_home();
    // `backups` is a file, so the directory cannot be created.
    std::fs::write(dir.path().join("backups"), b"in the way").unwrap();
    let before = logical_dump(&db);

    let err = open(&db).err().expect("no backup, no migration");
    assert!(
        matches!(&err, Error::Migration(text) if text.contains("could not back up") && text.contains("nothing was migrated")),
        "{err}"
    );
    assert_eq!(logical_dump(&db), before);

    // Removing the obstruction lets the same database migrate.
    std::fs::remove_file(dir.path().join("backups")).unwrap();
    assert!(open(&db).expect("migrates").legacy_migration().is_some());
    assert_eq!(backups(dir.path()).len(), 1);
}

#[test]
fn a_leftover_old_name_rolls_the_whole_migration_back() {
    let (dir, db) = legacy_home();
    // An index the migration does not know about keeps the old word in the
    // schema after the renames: the post-condition must catch it.
    Connection::open(&db)
        .unwrap()
        .execute_batch("CREATE INDEX idx_office_probe ON office_settings(key);")
        .unwrap();
    let before = logical_dump(&db);

    let err = open(&db).err().expect("verification fails");
    assert!(
        matches!(&err, Error::Migration(text) if text.contains("left old names behind") && text.contains("idx_office_probe")),
        "{err}"
    );
    assert_eq!(
        logical_dump(&db),
        before,
        "rolled back to the old state, triggers included"
    );
    assert_eq!(
        backups(dir.path()).len(),
        1,
        "the backup stays for the operator"
    );
}

#[test]
fn a_database_with_old_and_new_tables_both_holding_rows_is_refused_not_merged() {
    let (dir, db) = legacy_home();
    Connection::open(&db)
        .unwrap()
        .execute_batch("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL); INSERT INTO settings VALUES ('a', 'b');")
        .unwrap();
    let before = logical_dump(&db);

    let err = open(&db).err().expect("refuse");
    assert!(
        matches!(&err, Error::Migration(text) if text.contains("both `office_settings`") && text.contains("refusing to merge")),
        "{err}"
    );
    assert_eq!(logical_dump(&db), before);
    let _ = dir;
}

// ---------------------------------------------------------------------------
// Downgrade: the previous binary against an upgraded database
// ---------------------------------------------------------------------------

/// What the real viva 0.3.6 binary does to an already migrated database
/// (recorded in the PR): it does not recognise the `host` registry domain,
/// so it re-applies host v1 and v2 (creating empty `office_hosts`,
/// `office_recovery_events` and `office_settings` and registering
/// `office_host` v1/v2) and then dies on host v3 with `table recovery_plans
/// already exists`. The upgraded data is untouched and the old binary does
/// not start. This reproduces exactly that residue.
fn add_what_the_previous_binary_leaves_behind(home: &Path, db: &Path) {
    let backup = backups(home).pop().expect("a backup holds the old DDL");
    let old_ddl: Vec<String> = {
        let old = read_only(&backup);
        let mut stmt = old
            .prepare(
                "SELECT sql FROM sqlite_master WHERE name IN \
                 ('office_hosts', 'office_recovery_events', 'office_settings') ORDER BY name",
            )
            .expect("prepare");
        stmt.query_map([], |row| row.get(0))
            .expect("query")
            .map(|row| row.expect("row"))
            .collect()
    };
    assert_eq!(old_ddl.len(), 3);
    let conn = Connection::open(db).expect("writer");
    for ddl in old_ddl {
        conn.execute_batch(&ddl).expect("recreate old table");
    }
    conn.execute_batch(
        "INSERT INTO schema_migrations VALUES ('office_host', 1, 'office host v1', '2026-10-13T00:00:00Z'); \
         INSERT INTO schema_migrations VALUES ('office_host', 2, 'office host v2 (settings)', '2026-10-13T00:00:00Z');",
    )
    .expect("register the old domain");
}

#[test]
fn what_the_previous_binary_leaves_in_an_upgraded_database_is_cleaned_up() {
    let (dir, db) = legacy_home();
    drop(open(&db).expect("upgrade"));
    let upgraded = logical_dump(&db);
    add_what_the_previous_binary_leaves_behind(dir.path(), &db);
    assert!(!old_names_left(&db).is_empty(), "the residue has old names");

    let store = open(&db).expect("the current binary recovers");
    let note = store.legacy_migration().expect("cleanup ran").notice();
    assert!(note.contains("empty leftover tables"), "{note}");
    assert!(note.contains("office_hosts"), "{note}");
    drop(store);

    assert_eq!(old_names_left(&db), Vec::<String>::new());
    assert_eq!(
        logical_dump(&db),
        upgraded,
        "after cleanup the database is exactly the upgraded one"
    );
    assert_eq!(backups(dir.path()).len(), 2, "cleanup backs up too");
}
