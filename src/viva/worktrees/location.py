"""Where a task is allowed to run.

One policy, stated once:

* a task that will modify files (delivery / implementation / fix) runs in its
  own git worktree, so two tasks never write to the same checkout;
* research, planning and review tasks are read-only and run directly in the
  repository — no worktree is created for them;
* tasks with no repository at all (pure conversation, status work) have no
  work location.

Worktrees are Viva-owned: ``~/.viva/worktrees/<repository-name>/<task-id>``.
They are deliberately *not* created inside the repository, so a repository the
owner did not choose to ignore cannot be polluted by a stray directory.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from viva.core.errors import VivaError
from viva.core.paths import ensure_private_dir, viva_home
from viva.worktrees.discovery import GitWorktreeUnavailable, repository_service
from viva.worktrees.service import GitWorktreeError

# Task kinds that mutate files and therefore need an isolated worktree.
WRITABLE_KINDS = frozenset({"delivery", "implementation", "fix", "chore"})
READ_ONLY_KINDS = frozenset({"research", "planning", "review", "ops"})


class WorkLocationError(VivaError):
    """A work location could not be decided or allocated."""


def requires_worktree(task_kind: str) -> bool:
    kind = str(task_kind).strip().casefold()
    if kind in WRITABLE_KINDS:
        return True
    if kind in READ_ONLY_KINDS:
        return False
    raise WorkLocationError(
        f"unknown task kind {task_kind!r}: expected one of "
        f"{sorted(WRITABLE_KINDS | READ_ONLY_KINDS)}"
    )


def worktree_root(home: str | Path | None = None, repository: str | Path = "") -> Path:
    name = Path(repository).expanduser().resolve().name or "repository"
    return ensure_private_dir(viva_home(home) / "worktrees" / name)


def allocate_worktree(
    task_id: str,
    *,
    repository: str | Path,
    home: str | Path | None = None,
    branch: str | None = None,
    base: str | None = None,
) -> dict[str, Any]:
    """Create (or reuse) the isolated worktree for one task."""
    try:
        service = repository_service(repository)
    except GitWorktreeUnavailable as exc:
        raise WorkLocationError(str(exc)) from exc
    target = worktree_root(home, service.repository) / task_id
    branch_name = branch or f"task/{task_id}"
    if target.is_dir():
        current = service.worktree_branch(target)
        if current != branch_name:
            raise WorkLocationError(
                f"existing worktree {target} is on branch {current!r}, expected {branch_name!r}"
            )
        return {
            "kind": "worktree",
            "path": str(target),
            "branch": branch_name,
            "repository": str(service.repository),
            "reused": True,
        }
    base_sha = base
    if base_sha is None:
        base_sha, _ = service.default_base().split(" ", 1)
    try:
        service.create_worktree(target, branch_name, base_sha)
    except GitWorktreeError as exc:
        raise WorkLocationError(str(exc)) from exc
    return {
        "kind": "worktree",
        "path": str(target),
        "branch": branch_name,
        "repository": str(service.repository),
        "base": base_sha,
        "reused": False,
    }


def resolve_work_location(
    task: dict[str, Any],
    *,
    repository: str | Path | None,
    home: str | Path | None = None,
) -> dict[str, Any]:
    """Decide (and, for writable tasks, allocate) where *task* runs.

    An explicit ``work_location`` already stored on the task always wins: a task
    keeps the location it was created with, so a later dispatch cannot silently
    move in-flight work.
    """
    existing = task.get("work_location") or {}
    if existing.get("path"):
        return dict(existing)
    if repository is None:
        return {"kind": "none", "path": None, "mode": "read_only"}
    if requires_worktree(str(task.get("kind", ""))):
        location = allocate_worktree(str(task["id"]), repository=repository, home=home)
        location["mode"] = "write"
        return location
    try:
        service = repository_service(repository)
    except GitWorktreeUnavailable as exc:
        raise WorkLocationError(str(exc)) from exc
    return {
        "kind": "repository",
        "path": str(service.repository),
        "branch": service.current_branch(),
        "repository": str(service.repository),
        "mode": "read_only",
    }
