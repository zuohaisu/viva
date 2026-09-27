"""Persistent runtime pointers under ``~/.viva/runtime/state.json``."""

from __future__ import annotations

from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from viva.core.errors import VivaError
from viva.core.ids import new_session_id, utc_now
from viva.core.store import atomic_json_write, read_json_object

SCHEMA_VERSION = "1.0"
STATE_FILENAME = "state.json"


def _empty() -> dict[str, Any]:
    return {
        "schema_version": SCHEMA_VERSION,
        "current_resident": None,
        "current_workspace": None,
        "operator": None,
        "last_session_id": None,
        "last_session_started_at": None,
        "updated_at": None,
    }


class RuntimeState:
    """Load/save the small pointer set that must survive every session."""

    def __init__(self, home: str | Path | None = None):
        from viva.core.paths import ensure_private_dir, viva_home

        self.home = viva_home(home)
        self.path = ensure_private_dir(self.home / "runtime") / STATE_FILENAME

    def load(self) -> dict[str, Any]:
        stored = read_json_object(self.path) or {}
        state = _empty()
        for key in state:
            if key in stored:
                state[key] = stored[key]
        if state.get("schema_version") != SCHEMA_VERSION:
            raise VivaError(
                f"runtime state schema_version {state.get('schema_version')!r} "
                f"is not supported (expected {SCHEMA_VERSION})"
            )
        return state

    def save(self, state: dict[str, Any]) -> dict[str, Any]:
        updated = _empty()
        for key in updated:
            if key in state:
                updated[key] = state[key]
        updated["schema_version"] = SCHEMA_VERSION
        updated["updated_at"] = utc_now()
        atomic_json_write(self.path, updated)
        return updated

    def begin_session(self) -> str:
        """Record a new session id and return it."""
        state = self.load()
        state["last_session_id"] = new_session_id()
        state["last_session_started_at"] = utc_now()
        self.save(state)
        return str(state["last_session_id"])

    def set_current_resident(self, resident_id: str | None) -> None:
        state = self.load()
        state["current_resident"] = resident_id
        self.save(state)

    def set_current_workspace(self, workspace_id: str | None) -> None:
        state = self.load()
        state["current_workspace"] = workspace_id
        self.save(state)

    def set_operator(self, name: str | None) -> None:
        state = self.load()
        state["operator"] = name
        self.save(state)
