"""Project — a body of work inside a workspace that owns repositories.

The ontology the previous round collapsed is restored here:

    Workspace  != Project  != Repository  != Worktree  != Task

* a **workspace** is a long-term working context ("my office", "the VicTrader
  era") and holds projects;
* a **project** is a named body of work inside one workspace (for example
  "Viva" or "Hiring automation");
* a **repository** is a git repository a project works in — one project may
  span several repositories, and a repository may be referenced by more than
  one project (that is explicit, not accidental);
* **worktrees** and **tasks** hang off the project's repositories.

Nothing here replaces the workspace registry: a project always names the
workspace it belongs to.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any, Callable

from viva.core.errors import VivaError
from viva.core.ids import slugify, utc_now
from viva.core.paths import ensure_private_dir, viva_home
from viva.core.store import atomic_json_write, read_json_object

SCHEMA_VERSION = "1.0"
REGISTRY_FILENAME = "registry.json"


class ProjectError(VivaError):
    """A project could not be registered, found, or bound to a repository."""


class ProjectRegistry:
    """Register projects and the repositories they work in."""

    def __init__(self, home: str | Path | None = None, *, journal: Callable[..., Any] | None = None):
        self.home = ensure_private_dir(viva_home(home) / "projects")
        self.path = self.home / REGISTRY_FILENAME
        self._journal = journal

    # -- persistence ---------------------------------------------------------

    def _load(self) -> dict[str, Any]:
        stored = read_json_object(self.path) or {}
        if not stored:
            return {"schema_version": SCHEMA_VERSION, "projects": []}
        if stored.get("schema_version") != SCHEMA_VERSION or not isinstance(
            stored.get("projects"), list
        ):
            raise ProjectError(f"project registry is unreadable or unsupported: {self.path}")
        return stored

    def _save(self, registry: dict[str, Any]) -> None:
        atomic_json_write(self.path, registry)

    # -- operations ----------------------------------------------------------

    def register(
        self,
        *,
        workspace_id: str,
        name: str,
        repositories: list[str] | None = None,
    ) -> dict[str, Any]:
        workspace_id = str(workspace_id).strip()
        if not workspace_id:
            raise ProjectError("a project must name the workspace it belongs to")
        display = str(name).strip()
        if not display:
            raise ProjectError("project name must be a non-empty string")
        try:
            project_id = slugify(display, what="project name")
        except ValueError as exc:
            raise ProjectError(str(exc)) from exc
        resolved = self._resolve_repositories(repositories or [])
        registry = self._load()
        for existing in registry["projects"]:
            if existing.get("id") == project_id:
                raise ProjectError(f"project {display!r} is already registered")
        project = {
            "id": project_id,
            "name": display,
            "workspace_id": workspace_id,
            "repositories": resolved,
            "created_at": utc_now(),
        }
        registry["projects"].append(project)
        self._save(registry)
        self._event("project.registered", payload={"project_id": project_id, **project})
        return project

    def bind_repository(self, project_id_or_name: str, repository: str | Path) -> dict[str, Any]:
        project = self.require(project_id_or_name)
        resolved = self._resolve_repositories([str(repository)])[0]
        registry = self._load()
        for entry in registry["projects"]:
            if entry.get("id") == project["id"]:
                repositories = list(entry.get("repositories") or [])
                if resolved not in repositories:
                    repositories.append(resolved)
                    entry["repositories"] = repositories
                project = dict(entry)
        self._save(registry)
        self._event(
            "project.repository_bound",
            payload={"project_id": project["id"], "repository": resolved},
        )
        return project

    def list(self, *, workspace_id: str | None = None) -> list[dict[str, Any]]:
        projects = list(self._load()["projects"])
        if workspace_id is not None:
            needle = str(workspace_id).casefold()
            projects = [
                project
                for project in projects
                if str(project.get("workspace_id", "")).casefold() == needle
            ]
        return projects

    def get(self, project_id_or_name: str) -> dict[str, Any] | None:
        needle = str(project_id_or_name).strip().casefold()
        for project in self.list():
            if project.get("id") == needle or str(project.get("name", "")).casefold() == needle:
                return project
        return None

    def require(self, project_id_or_name: str) -> dict[str, Any]:
        project = self.get(project_id_or_name)
        if project is None:
            raise ProjectError(
                f"no registered project matches {project_id_or_name!r} — run: viva project list"
            )
        return project

    def primary_repository(self, project_id_or_name: str) -> str | None:
        project = self.get(project_id_or_name)
        if project is None:
            return None
        repositories = list(project.get("repositories") or [])
        return repositories[0] if repositories else None

    def for_repository(self, repository: str | Path) -> list[dict[str, Any]]:
        resolved = str(Path(repository).expanduser().resolve())
        return [
            project
            for project in self.list()
            if resolved in (project.get("repositories") or [])
        ]

    # -- internals -----------------------------------------------------------

    def _resolve_repositories(self, repositories: list[str]) -> list[str]:
        resolved: list[str] = []
        for entry in repositories:
            path = Path(str(entry)).expanduser().resolve()
            if not path.is_dir():
                raise ProjectError(f"repository path does not exist: {path}")
            if str(path) not in resolved:
                resolved.append(str(path))
        return resolved

    def _event(self, event_type: str, *, payload: dict[str, Any]) -> None:
        if self._journal is None:
            return
        self._journal(event_type=event_type, source="viva", payload=payload)
