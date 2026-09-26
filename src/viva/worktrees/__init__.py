"""Worktree — a git execution environment inside a workspace.

Viva does not implement a second worktree system. Creation, removal, and
branch safety stay in the delivery subsystem's audited
``ticket_autopilot.services.git_worktree.GitWorktreeService``; this module is
a thin adapter that reuses it and adds read-only discovery
(``git worktree list --porcelain``).
"""

from viva.worktrees.discovery import (
    GitWorktreeUnavailable,
    current_worktree,
    list_worktrees,
    repository_service,
    worktree_summary,
)

__all__ = [
    "GitWorktreeUnavailable",
    "current_worktree",
    "list_worktrees",
    "repository_service",
    "worktree_summary",
]
