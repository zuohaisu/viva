"""Worktree discovery: thin, read-only, built on the legacy worktree service."""

from __future__ import annotations

import pytest

from viva.worktrees import (
    GitWorktreeUnavailable,
    current_worktree,
    list_worktrees,
    repository_service,
    worktree_summary,
)


def test_non_repository_is_unavailable(tmp_path):
    with pytest.raises(GitWorktreeUnavailable):
        repository_service(tmp_path)
    summary = worktree_summary(tmp_path)
    assert summary["available"] is False


def test_discovery_lists_main_worktree(git_repo):
    summary = worktree_summary(git_repo)
    assert summary["available"] is True
    assert summary["branch"] == "main"
    assert len(summary["head_sha"]) == 12
    assert summary["worktree_count"] == 1
    only = summary["worktrees"][0]
    assert only["path"] == str(git_repo.resolve())
    assert only["branch"] == "main"


def test_discovery_reuses_legacy_service_for_creation_and_sees_it(git_repo):
    """The create path is the *legacy* audited service; discovery just watches."""
    from ticket_autopilot.services.git_worktree import GitWorktreeService

    service = GitWorktreeService(git_repo)
    target = git_repo.parent / "wt-feature"
    service.create_worktree(target, "feature/one", service.head_sha())

    worktrees = list_worktrees(git_repo)
    assert [entry["branch"] for entry in worktrees] == ["main", "feature/one"]
    assert worktrees[1]["path"] == str(target.resolve())


def test_current_worktree_matches_workspace_root(git_repo):
    entry = current_worktree(git_repo)
    assert entry is not None and entry["branch"] == "main"
    with pytest.raises(GitWorktreeUnavailable):
        current_worktree(git_repo.parent)  # outside any repository: loud failure
