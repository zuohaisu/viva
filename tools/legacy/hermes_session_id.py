#!/usr/bin/env python3
"""Rewrite the memory-adapter session id stored in an existing Hermes memory DB.

Until issue #96 the Pi memory adapter initialised the Hermes holographic
memory provider with `session_id="viva-office-adapter"`. The bundled provider
never stores that value, but the community plugin (v0.13.0 and later) keeps
it as the provenance of every fact version in `fact_versions.src_session_id`.
New writes use `viva-adapter` (issue #96); this tool moves the old records so
a fact's source reads the same before and after.

Safe by construction:

* `--db` is required and there is no default. The tool never looks at
  `$HERMES_HOME` or `~/.hermes`: pointing it at a real memory store is always
  an explicit act.
* The default run is a dry run. It opens the database without writing rows,
  reports what it found and changes nothing.
* `--apply` first writes a verified, private (0600) backup next to the
  database (`<db>.pre-office-removal.<UTC stamp>`), then rewrites inside one
  `BEGIN IMMEDIATE` transaction and checks the result before committing. If
  the backup cannot be made, nothing is rewritten.
* Only whole-cell matches in columns named `session_id` or `*_session_id` are
  rewritten. Any other cell that merely mentions the old id (a fact's text, a
  note) is reported and left alone.
* Re-running after a successful `--apply` finds nothing and changes nothing.

Restore: stop Hermes, then copy the backup over the database file and delete
the database's `-wal` and `-shm` files.

Exit status: 0 done / nothing to do, 1 refused or failed, 2 usage error.

Deleted together with the legacy migration by issue #99.
"""

from __future__ import annotations

import argparse
import datetime as dt
import os
import sqlite3
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from sqlite_open import open_existing  # noqa: E402

OLD_ID = "viva-office-adapter"
NEW_ID = "viva-adapter"


class Refused(Exception):
    """The tool declined to change anything; the message says why."""


def utc_stamp() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def is_session_column(name: str) -> bool:
    lowered = name.lower()
    return lowered == "session_id" or lowered.endswith("_session_id")


def real_tables(con: sqlite3.Connection) -> list[str]:
    """Tables with ordinary storage. FTS shadow tables are internal copies that
    the full-text triggers maintain; they are never rewritten directly."""
    rows = con.execute(
        "SELECT name, sql FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'"
    ).fetchall()
    virtual = {name for name, sql in rows if sql and sql.upper().startswith("CREATE VIRTUAL")}
    shadows = {
        name
        for name, _ in rows
        if any(name.startswith(f"{v}_") for v in virtual)
    }
    return sorted(name for name, _ in rows if name not in virtual and name not in shadows)


def scan(con: sqlite3.Connection, old: str) -> dict[str, list[tuple[str, int, bool]]]:
    """Find every cell equal to or mentioning `old`.

    Returns {table: [(column, rows, rewritable)]}; rows counts exact matches
    for rewritable columns and mentions elsewhere.
    """
    found: dict[str, list[tuple[str, int, bool]]] = {}
    for table in real_tables(con):
        for column in (r[1] for r in con.execute(f'PRAGMA table_info("{table}")')):
            rewritable = is_session_column(column)
            if rewritable:
                (count,) = con.execute(
                    f'SELECT COUNT(*) FROM "{table}" WHERE "{column}" = ?', (old,)
                ).fetchone()
            else:
                (count,) = con.execute(
                    f'SELECT COUNT(*) FROM "{table}" WHERE CAST("{column}" AS TEXT) LIKE ?',
                    (f"%{old}%",),
                ).fetchone()
            if count:
                found.setdefault(table, []).append((column, count, rewritable))
    return found


def report(found: dict[str, list[tuple[str, int, bool]]], old: str, new: str) -> int:
    """Print the findings; return how many cells are rewritable."""
    rewritable = 0
    for table, columns in found.items():
        for column, count, can_rewrite in columns:
            if can_rewrite:
                rewritable += count
                print(f"  {table}.{column}: {count} row(s) = {old!r}  -> {new!r}")
            else:
                print(f"  {table}.{column}: {count} row(s) mention {old!r} (left alone)")
    return rewritable


def make_backup(db: Path) -> Path:
    """A verified private copy via the SQLite backup API (consistent even when
    the database is in WAL mode)."""
    stamp = utc_stamp()
    for attempt in range(100):
        suffix = "" if attempt == 0 else f"-{attempt}"
        target = db.with_name(f"{db.name}.pre-office-removal.{stamp}{suffix}")
        try:
            fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        except FileExistsError:
            continue
        except OSError as exc:
            raise Refused(f"cannot create the backup {target}: {exc}; nothing was changed") from exc
        os.close(fd)
        try:
            source = open_existing(db)
            dest = sqlite3.connect(target)
            try:
                source.backup(dest)
            finally:
                dest.close()
                source.close()
            check = open_existing(target)
            try:
                verdict = check.execute("PRAGMA quick_check").fetchone()[0]
            finally:
                check.close()
            if verdict != "ok":
                raise Refused(f"the backup failed its integrity check ({verdict})")
        except (sqlite3.Error, Refused) as exc:
            target.unlink(missing_ok=True)
            raise Refused(f"cannot back up {db}: {exc}; nothing was changed") from exc
        return target
    raise Refused(f"no free backup file name next to {db}; nothing was changed")


def apply(db: Path, old: str, new: str) -> tuple[Path | None, int]:
    """Back up, rewrite, verify, commit. Returns (backup, rows rewritten)."""
    probe = open_existing(db)
    try:
        if report_count(scan(probe, old)) == 0:
            return None, 0
    finally:
        probe.close()

    backup = make_backup(db)
    con = sqlite3.connect(db, timeout=10, isolation_level=None)
    try:
        con.execute("BEGIN IMMEDIATE")
        found = scan(con, old)
        rewritten = 0
        for table, columns in found.items():
            for column, count, rewritable in columns:
                if not rewritable:
                    continue
                cursor = con.execute(
                    f'UPDATE "{table}" SET "{column}" = ? WHERE "{column}" = ?', (new, old)
                )
                if cursor.rowcount != count:
                    raise Refused(
                        f"{table}.{column}: expected to rewrite {count} row(s), "
                        f"rewrote {cursor.rowcount}; rolled back"
                    )
                rewritten += cursor.rowcount
        left = [
            f"{table}.{column}"
            for table, columns in scan(con, old).items()
            for column, _, rewritable in columns
            if rewritable
        ]
        if left:
            raise Refused(f"the old id is still present in {', '.join(left)}; rolled back")
        con.execute("COMMIT")
        return backup, rewritten
    except BaseException:
        if con.in_transaction:
            con.execute("ROLLBACK")
        raise
    finally:
        con.close()


def report_count(found: dict[str, list[tuple[str, int, bool]]]) -> int:
    return sum(count for columns in found.values() for _, count, rewritable in columns if rewritable)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--db", required=True, type=Path, help="the Hermes memory database to migrate (no default on purpose)")
    parser.add_argument("--apply", action="store_true", help="make the change (default: dry run)")
    parser.add_argument("--old", default=OLD_ID, help=f"id to replace (default {OLD_ID})")
    parser.add_argument("--new", default=NEW_ID, help=f"replacement id (default {NEW_ID})")
    args = parser.parse_args(argv)
    db: Path = args.db.expanduser()
    if args.old == args.new or not args.old or not args.new:
        parser.error("--old and --new must be different, non-empty ids")
    try:
        if not db.is_file():
            raise Refused(f"{db} is not an existing file; the tool never creates a database")
        try:
            probe = open_existing(db)
        except sqlite3.OperationalError as exc:
            raise Refused(f"cannot open {db} ({exc}); check permissions and that the path is a SQLite file") from exc
        try:
            found = scan(probe, args.old)
        except sqlite3.DatabaseError as exc:
            raise Refused(f"{db} opened but could not be read as SQLite: {exc}") from exc
        finally:
            probe.close()

        mode = "apply" if args.apply else "dry run"
        print(f"hermes session id migration ({mode}): {db}")
        if not found:
            print(f"  no occurrence of {args.old!r}: nothing to change")
            return 0
        pending = report(found, args.old, args.new)
        if not args.apply:
            print(f"dry run: {pending} row(s) would be rewritten; re-run with --apply to change them")
            return 0
        if pending == 0:
            print("nothing rewritable (only mentions elsewhere): nothing to change")
            return 0
        backup, rewritten = apply(db, args.old, args.new)
        print(f"rewrote {rewritten} row(s); backup: {backup}")
        return 0
    except Refused as exc:
        print(f"refused: {exc}", file=sys.stderr)
        return 1
    except sqlite3.Error as exc:
        print(f"failed: {exc}; the database was not changed", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
