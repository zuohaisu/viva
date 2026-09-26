"""Persistent Resident records under ``~/.viva/residents/``.

One JSON file per resident keeps the schema evolvable: adding a field never
rewrites unrelated records. Residents are created by the user
(``viva resident add <name>``); Viva core never seeds a persona.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from viva.core.errors import VivaError
from viva.core.ids import slugify, utc_now
from viva.core.paths import ensure_private_dir
from viva.core.store import atomic_json_write, read_json_object

SCHEMA_VERSION = "1.0"
RESIDENT_FIELDS = ("id", "name", "created_at", "notes")


class ResidentError(VivaError):
    """A Resident record could not be created, read, or selected."""


class ResidentRegistry:
    """Create and load persistent Resident records."""

    def __init__(self, home: str | Path | None = None):
        from viva.core.paths import viva_home

        self.home = ensure_private_dir(viva_home(home) / "residents")

    def _path(self, resident_id: str) -> Path:
        return self.home / f"{resident_id}.json"

    def create(self, name: str, *, notes: str = "", resident_id: str | None = None) -> dict[str, Any]:
        if not isinstance(name, str) or not name.strip():
            raise ResidentError("resident name must be a non-empty string")
        if not isinstance(notes, str):
            raise ResidentError("resident notes must be a string")
        try:
            rid = resident_id or slugify(name, what="resident name")
        except ValueError as exc:
            raise ResidentError(str(exc)) from exc
        if not rid == slugify(rid, what="resident id"):
            raise ResidentError(
                f"resident id may contain only a-z, 0-9 and '-', got {resident_id!r}"
            )
        path = self._path(rid)
        if path.exists():
            raise ResidentError(f"resident {rid!r} already exists")
        record = {
            "schema_version": SCHEMA_VERSION,
            "id": rid,
            "name": name.strip(),
            "created_at": utc_now(),
            "notes": notes,
        }
        atomic_json_write(path, record)
        return record

    def get(self, resident_id_or_name: str) -> dict[str, Any] | None:
        """Look up a resident by exact id, then by case-insensitive name."""
        if not isinstance(resident_id_or_name, str) or not resident_id_or_name.strip():
            return None
        needle = resident_id_or_name.strip()
        direct = read_json_object(self._path(needle))
        if direct is not None and direct.get("id"):
            return direct
        for record in self.list():
            if str(record.get("name", "")).casefold() == needle.casefold():
                return record
        return None

    def list(self) -> list[dict[str, Any]]:
        residents = []
        for path in sorted(self.home.glob("*.json")):
            record = read_json_object(path)
            if record is None:
                raise ResidentError(f"resident record is unreadable: {path.name}")
            residents.append(record)
        residents.sort(key=lambda record: (str(record.get("created_at", "")), str(record.get("id", ""))))
        return residents

    def export(self, record: dict[str, Any]) -> str:
        return json.dumps(record, ensure_ascii=False, sort_keys=True)
