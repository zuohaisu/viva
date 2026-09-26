"""Worktrees: read-only discovery plus the policy that decides who gets one."""

from __future__ import annotations

import os

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


def test_discovery_sees_worktrees_created_by_the_service(git_repo):
    """Creation and discovery are two views of the same single implementation."""
    from viva.worktrees import GitWorktreeService

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


# -- work-location policy -----------------------------------------------------


def test_writable_kinds_need_a_worktree_and_read_only_kinds_do_not(home, git_repo):
    from viva.worktrees import WorkLocationError, requires_worktree, resolve_work_location

    for kind in ("delivery", "implementation", "fix", "chore"):
        assert requires_worktree(kind) is True
    for kind in ("research", "planning", "review", "ops"):
        assert requires_worktree(kind) is False
    with pytest.raises(WorkLocationError, match="unknown task kind"):
        requires_worktree("sideways")

    research = resolve_work_location(
        {"id": "task-r", "kind": "research"}, repository=git_repo, home=home
    )
    assert research["kind"] == "repository" and research["mode"] == "read_only"

    delivery = resolve_work_location(
        {"id": "task-d", "kind": "delivery"}, repository=git_repo, home=home
    )
    assert delivery["kind"] == "worktree" and delivery["mode"] == "write"
    assert delivery["branch"] == "task/task-d"
    assert os.path.isdir(delivery["path"])


def test_each_task_gets_its_own_worktree_and_reuses_its_own(home, git_repo):
    from viva.worktrees import allocate_worktree

    first = allocate_worktree("task-one", repository=git_repo, home=home)
    second = allocate_worktree("task-two", repository=git_repo, home=home)
    assert first["path"] != second["path"]
    assert os.path.isdir(first["path"]) and os.path.isdir(second["path"])

    again = allocate_worktree("task-one", repository=git_repo, home=home)
    assert again["path"] == first["path"] and again.get("reused") is True


def test_an_existing_location_wins_over_reallocation(home, git_repo):
    from viva.worktrees import resolve_work_location

    stored = {"kind": "repository", "path": str(git_repo), "mode": "read_only"}
    resolved = resolve_work_location(
        {"id": "task-x", "kind": "delivery", "work_location": stored},
        repository=git_repo,
        home=home,
    )
    assert resolved == stored  # in-flight work is never silently moved
