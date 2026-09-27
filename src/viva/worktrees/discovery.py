"""Read-only worktree discovery.

Thin reader over :mod:`viva.worktrees.service` — the single worktree
implementation Viva owns after the delivery subsystem was retired.
"""

from __future__ import annotations

import subprocess
from pathlib import Path
from typing import Any

from viva.worktrees.service import GitWorktreeError, GitWorktreeService


class GitWorktreeUnavailable(RuntimeError):
    """The path is not a usable git repository (or git itself failed)."""


def repository_service(path: str | Path) -> GitWorktreeService:
    """Return the worktree service for *path*, or raise a clear error."""
    try:
        return GitWorktreeService(path)
    except GitWorktreeError as exc:
        raise GitWorktreeUnavailable(str(exc)) from exc


def list_worktrees(path: str | Path) -> list[dict[str, Any]]:
    """Return every worktree of the repository containing *path*.

    Each entry: ``{"path", "head", "branch"}`` where ``branch`` is None for
    detached worktrees. Ordered with the main worktree first, as git reports.
    """
    root = repository_service(path).repository
    proc = subprocess.run(
        ["git", "worktree", "list", "--porcelain"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    )
    worktrees: list[dict[str, Any]] = []
    current: dict[str, Any] = {}
    for line in proc.stdout.splitlines():
        if line.startswith("worktree "):
            current = {"path": line.removeprefix("worktree ").strip(), "head": None, "branch": None}
            worktrees.append(current)
        elif line.startswith("HEAD "):
            if current is not None:
                current["head"] = line.removeprefix("HEAD ").strip()
        elif line.startswith("branch "):
            if current is not None:
                current["branch"] = line.removeprefix("branch ").strip().removeprefix("refs/heads/")
        elif line == "detached":
            if current is not None:
                current["branch"] = None
        # blank lines separate entries; nothing to do
    return worktrees


def current_worktree(path: str | Path) -> dict[str, Any] | None:
    """Return the worktree entry that *path* itself resolves into, if any."""
    service = repository_service(path)
    resolved = str(Path(path).resolve())
    for entry in list_worktrees(service.repository):
        if Path(entry["path"]).resolve() == resolved or str(service.repository) == entry["path"]:
            return entry
    return None


def worktree_summary(path: str | Path) -> dict[str, Any]:
    """A compact, UI-ready summary for status surfaces."""
    try:
        service = repository_service(path)
    except GitWorktreeUnavailable as exc:
        return {"available": False, "reason": str(exc)}
    summary: dict[str, Any] = {"available": True}
    try:
        worktrees = list_worktrees(service.repository)
        summary["branch"] = service.current_branch()
        summary["head_sha"] = service.head_sha()[:12]
        summary["worktrees"] = worktrees
        summary["worktree_count"] = len(worktrees)
        summary["current"] = current_worktree(service.repository)
    except (GitWorktreeError, GitWorktreeUnavailable, subprocess.SubprocessError) as exc:
        summary["available"] = False
        summary["reason"] = str(exc)
    return summary
