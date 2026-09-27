"""Roles are configuration data, never code.

A role says what an AI member is *for* and how far it may go on its own:

    coordinator  owns the plan; dispatches and observes other members
    developer    implements and fixes code in an isolated worktree
    reviewer     independent read-only review; may never write
    operator     operational / environment work
    researcher   investigation and reporting; no file mutation

Nothing in Viva branches on a member's name. ``Samuel``, ``Deven`` and
``Alice`` are records a user creates; the role catalogue ships as editable
seeds in ``~/.viva/config/roles.json``.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from viva.core.errors import VivaError
from viva.core.paths import ensure_private_dir, viva_home
from viva.core.store import atomic_json_write, read_json_object

SCHEMA_VERSION = "1.0"
ROLES_FILENAME = "roles.json"

DEFAULT_ROLES: list[dict[str, Any]] = [
    {
        "id": "coordinator",
        "title": "PM / coordinator",
        "purpose": "Owns the plan, dispatches work to other members, and reports back.",
        "allowed_modes": ["read_only", "write"],
        "default_mode": "read_only",
    },
    {
        "id": "developer",
        "title": "Developer",
        "purpose": "Implements and fixes code inside an isolated worktree.",
        "allowed_modes": ["read_only", "write"],
        "default_mode": "write",
    },
    {
        "id": "reviewer",
        "title": "QA / reviewer",
        "purpose": "Independent read-only review of another member's work.",
        "allowed_modes": ["read_only"],
        "default_mode": "read_only",
    },
    {
        "id": "operator",
        "title": "Ops engineer",
        "purpose": "Operational and environment work.",
        "allowed_modes": ["read_only", "write"],
        "default_mode": "read_only",
    },
    {
        "id": "researcher",
        "title": "Researcher",
        "purpose": "Investigation and reporting; never mutates files.",
        "allowed_modes": ["read_only"],
        "default_mode": "read_only",
    },
]


class RoleError(VivaError):
    """A role is unknown or its definition is unusable."""


def _normalize(entry: Any, index: int) -> dict[str, Any]:
    if not isinstance(entry, dict):
        raise RoleError(f"roles[{index}] must be an object")
    role_id = str(entry.get("id", "")).strip()
    if not role_id or any(ch.isspace() for ch in role_id):
        raise RoleError(f"roles[{index}].id must be a non-empty token")
    modes = entry.get("allowed_modes", ["read_only"])
    if not isinstance(modes, list) or not modes or not all(isinstance(mode, str) for mode in modes):
        raise RoleError(f"roles[{index}].allowed_modes must be a non-empty list of strings")
    default_mode = str(entry.get("default_mode", modes[0]))
    if default_mode not in modes:
        raise RoleError(
            f"roles[{index}].default_mode {default_mode!r} is not in allowed_modes {modes}"
        )
    return {
        "id": role_id,
        "title": str(entry.get("title") or role_id),
        "purpose": str(entry.get("purpose") or ""),
        "allowed_modes": list(modes),
        "default_mode": default_mode,
    }


class RoleCatalog:
    """Load ``~/.viva/config/roles.json`` (seeding defaults on first use)."""

    def __init__(self, home: str | Path | None = None):
        self.home = viva_home(home)
        self.path = ensure_private_dir(self.home / "config") / ROLES_FILENAME

    def _load(self) -> list[dict[str, Any]]:
        stored = read_json_object(self.path)
        if stored is None:
            payload = {
                "schema_version": SCHEMA_VERSION,
                "note": "Roles are configuration data, not code: edit to fit your office.",
                "roles": DEFAULT_ROLES,
            }
            atomic_json_write(self.path, payload)
            stored = payload
        if stored.get("schema_version") != SCHEMA_VERSION or not isinstance(
            stored.get("roles"), list
        ):
            raise RoleError(f"role catalogue is unreadable or unsupported: {self.path}")
        roles = [_normalize(entry, index) for index, entry in enumerate(stored["roles"])]
        ids = [role["id"] for role in roles]
        if len(ids) != len(set(ids)):
            raise RoleError("role ids must be unique")
        return roles

    def list(self) -> list[dict[str, Any]]:
        return self._load()

    def get(self, role_id: str) -> dict[str, Any]:
        needle = str(role_id).strip().casefold()
        for role in self._load():
            if role["id"].casefold() == needle:
                return role
        raise RoleError(
            f"unknown role {role_id!r}; configured roles: "
            f"{[role['id'] for role in self._load()]}"
        )

    def export_config_path(self) -> str:
        return str(self.path)
