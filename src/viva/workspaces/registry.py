"""Persistent workspace registry and current-workspace selection."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Callable

from viva.core.errors import VivaError
from viva.core.ids import slugify, utc_now
from viva.core.store import atomic_json_write, read_json_object
from viva.core.paths import ensure_private_dir, viva_home
from viva.worktrees.discovery import GitWorktreeUnavailable

SCHEMA_VERSION = "1.0"
REGISTRY_FILENAME = "registry.json"


class WorkspaceError(VivaError):
    """A workspace could not be registered, found, or selected."""


class WorkspaceRegistry:
    """Register local directories as workspaces and remember the current one."""

    def __init__(
        self,
        home: str | Path | None = None,
        *,
        runtime=None,
        journal: Callable[..., Any] | None = None,
        resident_id: Callable[[], str | None] | None = None,
        session_id: Callable[[], str | None] | None = None,
    ):
        self.home = ensure_private_dir(viva_home(home) / "workspaces")
        self.path = self.home / REGISTRY_FILENAME
        self._runtime = runtime
        self._journal = journal
        self._resident_id = resident_id
        self._session_id = session_id

    # -- persistence ---------------------------------------------------------

    def _load(self) -> dict[str, Any]:
        stored = read_json_object(self.path) or {}
        if not stored:
            return {"schema_version": SCHEMA_VERSION, "workspaces": []}
        if stored.get("schema_version") != SCHEMA_VERSION or not isinstance(
            stored.get("workspaces"), list
        ):
            raise WorkspaceError(
                f"workspace registry is unreadable or unsupported: {self.path}"
            )
        return stored

    def _save(self, registry: dict[str, Any]) -> None:
        atomic_json_write(self.path, registry)

    # -- operations ----------------------------------------------------------

    def register(self, path: str | Path, name: str | None = None) -> dict[str, Any]:
        raw = Path(path).expanduser()
        if not raw.is_dir():
            raise WorkspaceError(f"workspace path does not exist or is not a directory: {raw}")
        resolved = str(raw.resolve())
        display = str(name or raw.resolve().name or "").strip()
        if not display:
            raise WorkspaceError("workspace name must be a non-empty string")
        try:
            wid = slugify(display, what="workspace name")
        except ValueError as exc:
            raise WorkspaceError(str(exc)) from exc
        registry = self._load()
        for existing in registry["workspaces"]:
            if existing.get("id") == wid:
                raise WorkspaceError(f"workspace name {display!r} is already registered")
            if existing.get("path") == resolved:
                raise WorkspaceError(f"path is already registered as workspace {existing.get('id')!r}: {resolved}")
        is_git = True
        try:
            from viva.worktrees.discovery import GitWorktreeUnavailable, repository_service

            repository_service(resolved)
        except GitWorktreeUnavailable:
            is_git = False
        workspace = {
            "id": wid,
            "name": display,
            "path": resolved,
            "is_git": is_git,
            "created_at": utc_now(),
            "last_opened_at": None,
        }
        registry["workspaces"].append(workspace)
        self._save(registry)
        self._record("workspace.registered", workspace)
        return workspace

    def list(self) -> list[dict[str, Any]]:
        return list(self._load()["workspaces"])

    def get(self, workspace_id_or_name: str) -> dict[str, Any] | None:
        needle = str(workspace_id_or_name).strip().casefold()
        for workspace in self.list():
            if workspace.get("id") == needle or str(workspace.get("name", "")).casefold() == needle:
                return workspace
        return None

    def use(self, workspace_id_or_name: str) -> dict[str, Any]:
        workspace = self.get(workspace_id_or_name)
        if workspace is None:
            raise WorkspaceError(f"no registered workspace matches {workspace_id_or_name!r}")
        if not Path(str(workspace["path"])).is_dir():
            raise WorkspaceError(
                f"workspace {workspace['id']!r} path is missing on disk: {workspace['path']}"
            )
        registry = self._load()
        for entry in registry["workspaces"]:
            if entry.get("id") == workspace["id"]:
                entry["last_opened_at"] = utc_now()
                workspace = dict(entry)
        self._save(registry)
        if self._runtime is not None:
            self._runtime.set_current_workspace(workspace["id"])
        self._record("workspace.selected", workspace)
        return workspace

    def current(self) -> dict[str, Any] | None:
        """Return the persisted current workspace, or None when unset/invalid."""
        if self._runtime is None:
            return None
        state = self._runtime.load()
        wid = state.get("current_workspace")
        if not wid:
            return None
        workspace = self.get(str(wid))
        if workspace is None or not Path(str(workspace["path"])).is_dir():
            return None
        return workspace

    # -- experience ----------------------------------------------------------

    def _record(self, event_type: str, workspace: dict[str, Any]) -> None:
        if self._journal is None:
            return
        self._journal(
            event_type=event_type,
            workspace=workspace.get("id"),
            source="viva",
            payload={"workspace_id": workspace.get("id"), "path": workspace.get("path")},
        )
