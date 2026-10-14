//! Disk layout rename: `office.db` → `viva.db`, `office.sock` → `server.sock` (#98).

use std::fs;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use tempfile::TempDir;

use viva::foundation::legacy::{OLD_DATABASE_FILE, OLD_SOCKET_NAME};
use viva::foundation::paths::{DATABASE_FILE, database_path};
use viva::foundation::store::Store;
use viva::server::{self, SOCKET_NAME};

const FIXTURE: &str = include_str!("fixtures/legacy-office-db.sql");

fn seed_legacy_db_file(home: &Path) -> PathBuf {
    let legacy = home.join(OLD_DATABASE_FILE);
    let conn = Connection::open(&legacy).expect("legacy db");
    conn.execute_batch(FIXTURE).expect("fixture");
    drop(conn);
    legacy
}

#[test]
fn cold_start_renames_office_db_and_removes_office_sock() {
    let dir = TempDir::new().expect("tempdir");
    let home = dir.path();
    seed_legacy_db_file(home);
    fs::write(home.join(OLD_SOCKET_NAME), b"").expect("stale socket file");

    let store = Store::open(&database_path(home), server::migrations()).expect("open");
    assert!(store.legacy_migration().is_some());

    assert!(!home.join(OLD_DATABASE_FILE).exists());
    assert!(home.join(DATABASE_FILE).exists());
    assert!(!home.join(OLD_SOCKET_NAME).exists());
    assert!(!home.join(SOCKET_NAME).exists());
}

#[test]
fn both_database_files_refuse_without_touching_either() {
    let dir = TempDir::new().expect("tempdir");
    let home = dir.path();
    seed_legacy_db_file(home);
    fs::copy(home.join(OLD_DATABASE_FILE), database_path(home)).expect("copy");

    let err = match Store::open(&database_path(home), server::migrations()) {
        Err(err) => err,
        Ok(_) => panic!("expected refusal when both database files exist"),
    };
    assert!(err.to_string().contains("both"), "{err}");
    assert!(home.join(OLD_DATABASE_FILE).exists());
    assert!(home.join(DATABASE_FILE).exists());
}

#[test]
fn a_listening_old_socket_blocks_rename_and_leaves_office_db() {
    let dir = TempDir::new().expect("tempdir");
    let home = dir.path();
    seed_legacy_db_file(home);
    let _listener = UnixListener::bind(home.join(OLD_SOCKET_NAME)).expect("bind");

    let err = match Store::open(&database_path(home), server::migrations()) {
        Err(err) => err,
        Ok(_) => panic!("expected refusal while the old socket is live"),
    };
    assert!(err.to_string().contains("viva shutdown"), "{err}");
    assert!(home.join(OLD_DATABASE_FILE).exists());
    assert!(!home.join(DATABASE_FILE).exists());
}
