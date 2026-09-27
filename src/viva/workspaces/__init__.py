"""Workspace — a long-term working context.

A workspace is not necessarily a git repository; it may associate repos,
files, terminals, tasks, and history. Phase 1 registers local directories and
detects whether they are git repositories.
"""

from viva.workspaces.registry import WorkspaceError, WorkspaceRegistry

__all__ = ["WorkspaceError", "WorkspaceRegistry"]
