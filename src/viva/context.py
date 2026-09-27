"""VivaContext — the composition root shared by the CLI and the TUI.

Wires the office together so both surfaces run the same code path: every
mutation flows through here and lands in the experience journal, and every
execution keeps its own attribution regardless of what the UI is showing.

Object map (see docs/architecture/domain-model.md):

    members (residents) -> roles/engines/tools      who + how
    workspaces -> projects -> repositories          where
    tasks -> executions                             why + what happened
    grants -> requests                              under whose authority
    knowledge                                       what is learned, owned by whom
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import viva
from viva.core.paths import viva_home
from viva.executions import ExecutionHandle, ExecutionRegistry, ExecutionRunner
from viva.experience import ExperienceJournal
from viva.github import GitHubClient
from viva.knowledge import KnowledgeRegistry
from viva.office.control import Office
from viva.permissions import GrantRegistry
from viva.projects import ProjectRegistry
from viva.residents import ResidentRegistry
from viva.runtime import RuntimeState
from viva.tasks import TaskRegistry
from viva.workers import WorkerRegistry
from viva.workspaces import WorkspaceRegistry
from viva.worktrees import worktree_summary


class VivaContext:
    """One handle on persistent Viva state for a home directory."""

    def __init__(self, home: str | Path | None = None):
        self.home = viva_home(home)
        self.runtime = RuntimeState(self.home)
        self.journal = ExperienceJournal(self.home)
        self.residents = ResidentRegistry(self.home, journal=self._record)
        self.workers = WorkerRegistry(self.home)
        self.grants = GrantRegistry(self.home, journal=self._record)
        self.projects = ProjectRegistry(self.home, journal=self._record)
        self.tasks = TaskRegistry(self.home, journal=self._record)
        self.knowledge = KnowledgeRegistry(self.home, journal=self._record)
        self.executions = ExecutionRegistry(self.home, journal=self._record)
        self.executions_runner = ExecutionRunner(self.executions, viva_home_root=self.home)
        self.executions_handles: dict[str, ExecutionHandle] = {}
        self.github = GitHubClient()
        self.workspaces = WorkspaceRegistry(
            self.home,
            runtime=self.runtime,
            journal=self._record,
            resident_id=self.current_resident_id,
            session_id=self.current_session_id,
        )
        self.office = Office(self)

    # -- session / identity context -----------------------------------------

    def current_resident_id(self) -> str | None:
        resident = self.current_resident()
        return str(resident["id"]) if resident else None

    def current_session_id(self) -> str | None:
        value = self.runtime.load().get("last_session_id")
        return str(value) if value else None

    def current_resident(self) -> dict[str, Any] | None:
        state = self.runtime.load()
        wid = state.get("current_resident")
        if not wid:
            return None
        return self.residents.get(str(wid))

    def current_workspace(self) -> dict[str, Any] | None:
        return self.workspaces.current()

    def begin_session(self) -> str:
        session_id = self.runtime.begin_session()
        self.journal.append(
            event_type="session.started",
            resident_id=self.current_resident_id(),
            session_id=session_id,
            workspace=(self.current_workspace() or {}).get("id"),
            source="viva",
            payload={"session_id": session_id},
        )
        return session_id

    def remember_handle(self, handle: ExecutionHandle) -> ExecutionHandle:
        self.executions_handles[str(handle.record["id"])] = handle
        return handle

    def _record(self, **kwargs: Any) -> None:
        """Journal callback: fills in session defaults, never overwrites facts."""
        kwargs.setdefault("resident_id", self.current_resident_id())
        kwargs.setdefault("session_id", self.current_session_id())
        self.journal.append(**kwargs)

    # -- composed surfaces ----------------------------------------------------

    def status(self) -> dict[str, Any]:
        """Assemble the honest status snapshot used by `viva status` and the TUI."""
        resident = self.current_resident()
        workspace = self.current_workspace()
        git = worktree_summary(workspace["path"]) if workspace else None
        executions = self.executions
        running = executions.running()
        unresolved = executions.list(status="exited") + executions.list(status="unknown")
        tasks = self.tasks.list()
        return {
            "version": viva.__version__,
            "home": str(self.home),
            "resident": resident,
            "workspace": workspace,
            "git": git,
            "workers": self.workers.list(),
            "residents": self.residents.list(),
            "workspaces": self.workspaces.list(),
            "projects": self.projects.list(),
            "tasks": tasks,
            "open_tasks": self.tasks.list(open_only=True),
            "running_executions": running,
            "unresolved_executions": unresolved,
            "recoverable_executions": [item for item in executions.list() if item.get("recoverable")],
            "grants": self.grants.list(),
            "knowledge_count": len(self.knowledge.list()),
            "knowledge_reuse_count": len(self.knowledge.reuse_evidence()),
            "experience_count": len(self.journal.read()),
            "last_session_id": self.runtime.load().get("last_session_id"),
        }
