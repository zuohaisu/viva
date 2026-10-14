"""Issue #97: the two one-off tools that accompany the database rename.

* `tools/legacy/hermes_session_id.py` moves the memory adapter's old
  provenance id in an existing Hermes memory database (dry run by default,
  explicit `--db`, backup before `--apply`).
* `tools/legacy/audit_office_values.py` is the read-only audit that shows what
  the Rust migration rewrites and what it must leave alone.

The Hermes schema below is the provenance part of the community plugin's
`fact_versions` table and its full-text index, which is the one place the
adapter's session id is persisted.
"""

from __future__ import annotations

import hashlib
import os
import sqlite3
import stat
import subprocess
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
HERMES_TOOL = REPO_ROOT / "tools" / "legacy" / "hermes_session_id.py"
AUDIT_TOOL = REPO_ROOT / "tools" / "legacy" / "audit_office_values.py"
FIXTURE = REPO_ROOT / "crates" / "viva" / "tests" / "fixtures" / "legacy-office-db.sql"

OLD = "viva-office-adapter"
NEW = "viva-adapter"

HERMES_SCHEMA = """
CREATE TABLE facts (
    fact_id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL,
    session_id TEXT
);
CREATE TABLE fact_versions (
    version_id     INTEGER PRIMARY KEY AUTOINCREMENT,
    fact_id        INTEGER NOT NULL,
    seq            INTEGER NOT NULL,
    content        TEXT NOT NULL,
    tags           TEXT DEFAULT '',
    src_via        TEXT,
    src_session_id TEXT,
    UNIQUE (fact_id, seq)
);
CREATE VIRTUAL TABLE fact_versions_fts
    USING fts5(content, tags, content=fact_versions, content_rowid=version_id);
CREATE TRIGGER fact_versions_ai AFTER INSERT ON fact_versions BEGIN
    INSERT INTO fact_versions_fts(rowid, content, tags) VALUES (new.version_id, new.content, new.tags);
END;
CREATE TRIGGER fact_versions_au AFTER UPDATE OF content, tags ON fact_versions BEGIN
    INSERT INTO fact_versions_fts(fact_versions_fts, rowid, content, tags)
        VALUES ('delete', old.version_id, old.content, old.tags);
    INSERT INTO fact_versions_fts(rowid, content, tags) VALUES (new.version_id, new.content, new.tags);
END;
"""


def make_hermes_db(path: Path, adapter_rows: int = 3) -> None:
    con = sqlite3.connect(path)
    con.executescript(HERMES_SCHEMA)
    for n in range(adapter_rows):
        con.execute("INSERT INTO facts(content, session_id) VALUES (?, ?)", (f"fact {n}", OLD))
        con.execute(
            "INSERT INTO fact_versions(fact_id, seq, content, tags, src_via, src_session_id) VALUES (?, 1, ?, 'a', 'tool', ?)",
            (n + 1, f"adapter fact {n}", OLD),
        )
    # A fact from another source, and a fact whose TEXT talks about the id.
    con.execute("INSERT INTO fact_versions(fact_id, seq, content, src_session_id) VALUES (100, 1, 'from chat', 'sess-chat')")
    con.execute(
        "INSERT INTO fact_versions(fact_id, seq, content, src_session_id) VALUES (101, 1, ?, 'sess-chat')",
        (f"the adapter used {OLD} before",),
    )
    con.commit()
    con.close()


def logical(path: Path) -> list[tuple]:
    con = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        rows: list[tuple] = []
        for (name,) in con.execute(
            "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name"
        ).fetchall():
            if name.startswith("sqlite_"):
                continue
            rows.append((name, con.execute(f'SELECT * FROM "{name}" ORDER BY 1').fetchall()))
        return rows
    except sqlite3.OperationalError:
        return []
    finally:
        con.close()


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(tool: Path, *args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(tool), *args],
        capture_output=True,
        text=True,
        env={**os.environ, **(env or {})},
        check=False,
    )


def backups(db: Path) -> list[Path]:
    return sorted(db.parent.glob(f"{db.name}.pre-office-removal.*"))


# ---------------------------------------------------------------------------
# Hermes memory session id
# ---------------------------------------------------------------------------


def test_dry_run_reports_and_changes_nothing(tmp_path: Path) -> None:
    db = tmp_path / "memory_store.db"
    make_hermes_db(db)
    before = digest(db)

    result = run(HERMES_TOOL, "--db", str(db))

    assert result.returncode == 0, result.stderr
    assert "dry run" in result.stdout
    assert "fact_versions.src_session_id: 3 row(s)" in result.stdout
    assert "facts.session_id: 3 row(s)" in result.stdout
    assert "left alone" in result.stdout, "the fact that only mentions the id is reported, not rewritten"
    assert digest(db) == before, "a dry run must not touch the file"
    assert backups(db) == []


def test_apply_backs_up_then_rewrites_only_the_provenance_column(tmp_path: Path) -> None:
    db = tmp_path / "memory_store.db"
    make_hermes_db(db)
    before = logical(db)

    result = run(HERMES_TOOL, "--db", str(db), "--apply")

    assert result.returncode == 0, result.stderr
    assert "rewrote 6 row(s)" in result.stdout

    con = sqlite3.connect(db)
    try:
        versions = con.execute("SELECT src_session_id, content FROM fact_versions ORDER BY version_id").fetchall()
        assert [v for v, _ in versions[:3]] == [NEW] * 3
        assert [v for v, _ in versions[3:]] == ["sess-chat", "sess-chat"]
        # Text that merely talks about the old id is content, not provenance.
        assert (f"the adapter used {OLD} before") in [c for _, c in versions]
        assert con.execute("SELECT DISTINCT session_id FROM facts").fetchall() == [(NEW,)]
        # The full-text index still agrees with the table.
        con.execute("INSERT INTO fact_versions_fts(fact_versions_fts) VALUES ('integrity-check')")
        assert con.execute("SELECT COUNT(*) FROM fact_versions_fts WHERE fact_versions_fts MATCH 'adapter'").fetchone()[0] >= 3
        assert con.execute("PRAGMA quick_check").fetchone() == ("ok",)
    finally:
        con.close()

    (backup,) = backups(db)
    assert stat.S_IMODE(backup.stat().st_mode) == 0o600
    assert logical(backup) == before, "the backup is the exact pre-change state"


def test_second_apply_finds_nothing_and_makes_no_second_backup(tmp_path: Path) -> None:
    db = tmp_path / "memory_store.db"
    make_hermes_db(db)
    assert run(HERMES_TOOL, "--db", str(db), "--apply").returncode == 0
    after_first = logical(db)

    again = run(HERMES_TOOL, "--db", str(db), "--apply")

    assert again.returncode == 0, again.stderr
    assert "nothing to change" in again.stdout
    assert logical(db) == after_first
    assert len(backups(db)) == 1


def test_a_database_that_never_held_the_id_needs_no_change(tmp_path: Path) -> None:
    db = tmp_path / "memory_store.db"
    make_hermes_db(db, adapter_rows=0)
    con = sqlite3.connect(db)
    con.execute("DELETE FROM fact_versions WHERE content LIKE '%used%'")
    con.commit()
    con.close()
    before = digest(db)

    result = run(HERMES_TOOL, "--db", str(db), "--apply")

    assert result.returncode == 0, result.stderr
    assert "nothing to change" in result.stdout
    assert digest(db) == before
    assert backups(db) == []


def test_db_is_mandatory_and_the_hermes_home_is_never_consulted(tmp_path: Path) -> None:
    home = tmp_path / "hermes-home"
    home.mkdir()
    real = home / "memory_store.db"
    make_hermes_db(real)
    before = digest(real)

    result = run(HERMES_TOOL, "--apply", env={"HERMES_HOME": str(home), "HOME": str(tmp_path)})

    assert result.returncode == 2, "argparse rejects the missing --db"
    assert "--db" in result.stderr
    assert digest(real) == before
    assert backups(real) == []


def test_a_missing_database_is_refused_not_created(tmp_path: Path) -> None:
    missing = tmp_path / "nope.db"

    result = run(HERMES_TOOL, "--db", str(missing), "--apply")

    assert result.returncode == 1
    assert "never creates a database" in result.stderr
    assert not missing.exists()


def test_apply_aborts_when_the_backup_cannot_be_written(tmp_path: Path) -> None:
    if os.geteuid() == 0:
        pytest.skip("root ignores directory permissions")
    folder = tmp_path / "ro"
    folder.mkdir()
    db = folder / "memory_store.db"
    make_hermes_db(db)
    before = logical(db)
    folder.chmod(0o500)
    try:
        result = run(HERMES_TOOL, "--db", str(db), "--apply")
    finally:
        folder.chmod(0o700)

    assert result.returncode == 1
    assert "nothing was changed" in result.stderr
    assert logical(db) == before


# ---------------------------------------------------------------------------
# The audit
# ---------------------------------------------------------------------------


def legacy_db(path: Path) -> Path:
    con = sqlite3.connect(path)
    con.executescript(FIXTURE.read_text())
    con.close()
    return path


def test_audit_lists_what_the_migration_owns_and_what_it_leaves(tmp_path: Path) -> None:
    db = legacy_db(tmp_path / "office.db")
    before = digest(db)

    result = run(AUDIT_TOOL, "--db", str(db))

    assert result.returncode == 1, "old names present: the audit fails"
    must, _, content = result.stdout.partition("== USER CONTENT")
    assert "`office_events`" in must and "`office_events_no_update`" in must
    assert "schema_migrations `office_host` v1" in must
    assert "office_events.kind = 'office_paused' (origin `office_host`)" in must
    assert "control_requests.rejection" in must and "github_evidence.state_json" in must
    # What a person typed is reported separately and never counted as a failure.
    assert "tasks.goal = 'Migrate the office host'" in content
    assert "projects.display_name = \"Haisu's office\"" in content
    assert "(origin `cli`)" in content
    assert digest(db) == before, "the audit is read-only"


def test_audit_passes_a_database_with_only_user_content_left(tmp_path: Path) -> None:
    db = tmp_path / "viva.db"
    con = sqlite3.connect(db)
    con.executescript(
        """
        CREATE TABLE schema_migrations (domain TEXT, version INTEGER, name TEXT);
        INSERT INTO schema_migrations VALUES ('host', 1, 'host v1');
        CREATE TABLE events (
            seq INTEGER PRIMARY KEY AUTOINCREMENT, -- one row per office event (stored comments are not schema names)
            domain TEXT, kind TEXT, subject_type TEXT, subject_id TEXT, origin TEXT, payload TEXT);
        INSERT INTO events(domain, kind, subject_type, subject_id, origin, payload)
            VALUES ('host', 'paused', 'host', 'h1', 'host', '{}'),
                   ('tasks', 'office_paused', 'office', 'note', 'cli', '{"note":"office"}');
        CREATE TABLE tasks (goal TEXT);
        INSERT INTO tasks VALUES ('Tidy the office');
        """
    )
    con.commit()
    con.close()

    result = run(AUDIT_TOOL, "--db", str(db))

    assert result.returncode == 0, result.stdout
    assert "MUST BE CLEAN (0)" in result.stdout
    assert "tasks.goal = 'Tidy the office'" in result.stdout


def test_audit_requires_an_explicit_database(tmp_path: Path) -> None:
    assert run(AUDIT_TOOL).returncode == 2
    assert run(AUDIT_TOOL, "--db", str(tmp_path / "missing.db")).returncode == 2


def _strip_wal_sidecars(db: Path) -> None:
    for suffix in ("-wal", "-shm"):
        side = Path(f"{db}{suffix}")
        if side.exists():
            side.unlink()


def _wal_quiet(db: Path) -> None:
    con = sqlite3.connect(db)
    con.execute("PRAGMA journal_mode=WAL")
    con.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    con.commit()
    con.close()
    _strip_wal_sidecars(db)


def test_tools_open_a_wal_database_with_no_sidecar_files(tmp_path: Path) -> None:
    """After a clean shutdown only the main file remains; URI mode=ro cannot open it."""
    audit_db = legacy_db(tmp_path / "office.db")
    _wal_quiet(audit_db)
    before = digest(audit_db)
    assert not any(p.name.endswith(("-wal", "-shm")) for p in tmp_path.iterdir())

    audit = run(AUDIT_TOOL, "--db", str(audit_db))
    assert audit.returncode == 1, audit.stderr or audit.stdout
    assert "MUST BE CLEAN" in audit.stdout
    assert digest(audit_db) == before

    hermes_db = tmp_path / "memory.db"
    make_hermes_db(hermes_db)
    _wal_quiet(hermes_db)
    hermes = run(HERMES_TOOL, "--db", str(hermes_db))
    assert hermes.returncode == 0, hermes.stderr or hermes.stdout
    assert "dry run" in hermes.stdout
    assert "not a readable SQLite database" not in hermes.stderr
