#!/usr/bin/env python3
"""Read-only audit: where does the old word still live in a Viva database?

Used for issue #97 (rename of the database-internal names): run it on a
database BEFORE the upgrade to see what the migration will rewrite, and AFTER
to confirm nothing it owns is left. It never writes: the file is opened with
`mode=ro` and no WAL checkpoint is requested.

    python3 tools/legacy/audit_office_values.py --db /path/to/office.db

The report has two parts.

* MUST BE CLEAN - names and values Viva itself writes: schema objects (table,
  index and trigger names and statements, comments excluded), the migration
  registry, sqlite_sequence names, and event rows that were not appended by a
  person (origin other than `cli`). Exit status 1 when any remain.
* USER CONTENT - everything else that contains the word (workspace names,
  task goals, titles, notes a person typed). Listed so you can see it was
  left alone; never counted as a failure.

Deleted together with the legacy migration by issue #99.
"""

from __future__ import annotations

import argparse
import collections
import sqlite3
import sys
from pathlib import Path

WORD = "office"
# Event rows appended by a person through the CLI keep their words.
PERSON_ORIGINS = {"cli"}
# Columns of the event log that carry Viva-written values.
EVENT_COLUMNS = ("domain", "kind", "subject_type", "subject_id", "origin", "payload")
# Free-text messages the old code wrote itself. Only these exact phrases are
# Viva's; the same column can also hold text from elsewhere (a PR title that
# gh returned), which stays user content.
VIVA_PHRASES = {
    ("control_requests", "rejection"): "was never issued by this office",
    ("github_evidence", "state_json"): "the office's read-only surface",
}


def strip_sql_comments(sql: str) -> str:
    """Drop `-- line` and `/* block */` comments, keeping quoted text."""
    out: list[str] = []
    i, n = 0, len(sql)
    quote = ""
    while i < n:
        c = sql[i]
        if quote:
            out.append(c)
            if c == quote:
                quote = ""
            i += 1
        elif c in "'\"`":
            quote = c
            out.append(c)
            i += 1
        elif sql.startswith("--", i):
            j = sql.find("\n", i)
            i = n if j < 0 else j
        elif sql.startswith("/*", i):
            j = sql.find("*/", i + 2)
            out.append(" ")
            i = n if j < 0 else j + 2
        else:
            out.append(c)
            i += 1
    return "".join(out)


def has_word(text: object) -> bool:
    return WORD in str(text).lower()


def user_tables(con: sqlite3.Connection) -> list[str]:
    rows = con.execute(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' "
        "AND sql NOT LIKE 'CREATE VIRTUAL%' ORDER BY name"
    )
    return [r[0] for r in rows]


def audit(con: sqlite3.Connection) -> tuple[list[str], list[str]]:
    """Return (must_be_clean, user_content) report lines."""
    must: list[str] = []
    content: list[str] = []

    for kind, name, tbl, sql in con.execute(
        "SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name"
    ):
        via = []
        if has_word(name) or has_word(tbl):
            via.append("name")
        if sql and has_word(strip_sql_comments(sql)):
            via.append("statement")
        if via:
            must.append(f"schema {kind} `{name}` (table `{tbl}`) via {', '.join(via)}")

    tables = user_tables(con)
    if "schema_migrations" in tables:
        for domain, version, name in con.execute(
            "SELECT domain, version, name FROM schema_migrations ORDER BY domain, version"
        ):
            if has_word(domain) or has_word(name):
                must.append(f"schema_migrations `{domain}` v{version} `{name}`")
    if con.execute("SELECT 1 FROM sqlite_master WHERE name = 'sqlite_sequence'").fetchone():
        for (name,) in con.execute("SELECT name FROM sqlite_sequence"):
            if has_word(name):
                must.append(f"sqlite_sequence `{name}`")

    event_table = next((t for t in ("events", "office_events") if t in tables), None)
    for table in tables:
        columns = [r[1] for r in con.execute(f'PRAGMA table_info("{table}")')]
        for column in columns:
            if table == event_table and column in EVENT_COLUMNS:
                query = (
                    f'SELECT substr(CAST("{column}" AS TEXT), 1, 160), origin FROM "{table}" '
                    f'WHERE CAST("{column}" AS TEXT) LIKE ?'
                )
                found = collections.Counter(
                    (value, origin)
                    for value, origin in con.execute(query, (f"%{WORD}%",))
                )
                for (value, origin), count in found.most_common():
                    line = f"{table}.{column} = {value!r} (origin `{origin}`) x{count}"
                    (content if origin in PERSON_ORIGINS else must).append(line)
                continue
            if table == "schema_migrations":
                continue
            query = f'SELECT CAST("{column}" AS TEXT) FROM "{table}" WHERE CAST("{column}" AS TEXT) LIKE ?'
            phrase = VIVA_PHRASES.get((table, column))
            found = collections.Counter(v for (v,) in con.execute(query, (f"%{WORD}%",)))
            for value, count in found.most_common():
                line = f"{table}.{column} = {value[:160]!r} x{count}"
                (must if phrase and phrase in value else content).append(line)
    return must, content


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--db", required=True, type=Path, help="path to the Viva database (no default on purpose)")
    args = parser.parse_args(argv)
    if not args.db.is_file():
        print(f"audit: {args.db} is not a file", file=sys.stderr)
        return 2
    con = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
    try:
        must, content = audit(con)
    finally:
        con.close()
    print(f"== MUST BE CLEAN ({len(must)})")
    for line in must:
        print(f"  {line}")
    print(f"== USER CONTENT, left alone ({len(content)})")
    for line in content:
        print(f"  {line}")
    return 1 if must else 0


if __name__ == "__main__":
    raise SystemExit(main())
