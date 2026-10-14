"""Open an existing SQLite file for these one-off legacy tools."""

from __future__ import annotations

import sqlite3
from pathlib import Path


def open_existing(path: Path | str, *, timeout: float = 10.0) -> sqlite3.Connection:
    """Open a database for read-only queries (audit, dry-run, backup source).

    Viva and Hermes memory stores use WAL. After a clean shutdown the main file
    may be the only path left (no ``-wal`` / ``-shm`` sidecars). URI
    ``mode=ro`` then fails on some platforms because SQLite cannot attach the
    WAL index read-only. Try ``mode=ro`` first; on failure use ``mode=rw`` with
    ``PRAGMA query_only=ON`` so callers that only ``SELECT`` do not change
    application rows.
    """
    resolved = Path(path).expanduser().resolve()
    last: sqlite3.OperationalError | None = None
    for mode in ("ro", "rw"):
        try:
            con = sqlite3.connect(f"file:{resolved}?mode={mode}", uri=True, timeout=timeout)
        except sqlite3.OperationalError as exc:
            last = exc
            continue
        if mode == "rw":
            con.execute("PRAGMA query_only=ON")
        return con
    assert last is not None
    raise last
