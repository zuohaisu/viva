"""Worktree — an isolated git execution environment owned by Viva.

Creation, removal and branch safety live in ``viva.worktrees.service`` (moved
out of the retired delivery subsystem unchanged); discovery reads git state;
``location`` decides *whether* a task gets a worktree at all.
"""

from viva.worktrees.discovery import (
    GitWorktreeUnavailable,
    current_worktree,
    list_worktrees,
    repository_service,
    worktree_summary,
)
from viva.worktrees.location import (
    READ_ONLY_KINDS,
    WRITABLE_KINDS,
    WorkLocationError,
    allocate_worktree,
    requires_worktree,
    resolve_work_location,
    worktree_root,
)
from viva.worktrees.service import GitWorktreeError, GitWorktreeService, is_protected_branch

__all__ = [
    "GitWorktreeError",
    "GitWorktreeService",
    "GitWorktreeUnavailable",
    "READ_ONLY_KINDS",
    "WRITABLE_KINDS",
    "WorkLocationError",
    "allocate_worktree",
    "current_worktree",
    "is_protected_branch",
    "list_worktrees",
    "repository_service",
    "requires_worktree",
    "resolve_work_location",
    "worktree_root",
    "worktree_summary",
]
