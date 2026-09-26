"""VivaContext — the composition root shared by the CLI and the TUI.

Wires the domain registries together so both surfaces run the same code path:
every mutation flows through here and lands in the experience journal.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from viva.core.paths import viva_home
from viva.experience import ExperienceJournal
from viva.residents import ResidentRegistry
from viva.runtime import RuntimeState
from viva.workers import WorkerRegistry
from viva.workspaces import WorkspaceRegistry
from viva.worktrees import worktree_summary

import viva


class VivaContext:
    """One handle on persistent Viva state for a home directory."""

    def __init__(self, home: str | Path | None = None):
        self.home = viva_home(home)
        self.residents = ResidentRegistry(self.home)
        self.runtime = RuntimeState(self.home)
        self.journal = ExperienceJournal(self.home)
        self.workers = WorkerRegistry(self.home)
        # Registries that record experience events share the journal context.
        self.workspaces = WorkspaceRegistry(
            self.home,
            runtime=self.runtime,
            journal=self._record_workspace_event,
            resident_id=self.current_resident_id,
            session_id=self.current_session_id,
        )

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

    def _record_workspace_event(self, **kwargs: Any) -> None:
        self.journal.append(resident_id=self.current_resident_id(), **kwargs)

    # -- composed surfaces ----------------------------------------------------

    def status(self) -> dict[str, Any]:
        """Assemble the honest status snapshot used by `viva status` and the TUI."""
        resident = self.current_resident()
        workspace = self.current_workspace()
        git = (
            worktree_summary(workspace["path"])
            if workspace
            else None
        )
        return {
            "version": viva.__version__,
            "home": str(self.home),
            "resident": resident,
            "workspace": workspace,
            "git": git,
            "workers": self.workers.list(),
            "residents": self.residents.list(),
            "workspaces": self.workspaces.list(),
            "experience_count": len(self.journal.read()),
            "last_session_id": self.runtime.load().get("last_session_id"),
        }
