"""The worktree service Viva kept: isolation, protected branches, owner-gated push."""

from __future__ import annotations

import subprocess

import pytest

from viva.permissions.authority import DeliveryAuthorizationError
from viva.worktrees import GitWorktreeError, GitWorktreeService, is_protected_branch


def test_protected_branch_detection():
    assert is_protected_branch("main")
    assert is_protected_branch("refs/heads/master")
    assert not is_protected_branch("feature/x")


def test_create_use_and_remove_a_worktree(git_repo, tmp_path):
    service = GitWorktreeService(git_repo)
    target = tmp_path / "wt-one"
    service.create_worktree(target, "feature/one", service.head_sha())

    assert service.worktree_branch(target) == "feature/one"
    assert service.branch_exists("feature/one")
    assert target.is_dir()

    service.remove_worktree(target)
    assert not target.exists()


def test_create_refuses_protected_branches_and_existing_targets(git_repo, tmp_path):
    service = GitWorktreeService(git_repo)
    with pytest.raises(GitWorktreeError, match="protected branch"):
        service.create_worktree(tmp_path / "wt-main", "main", service.head_sha())

    existing = tmp_path / "wt-taken"
    existing.mkdir()
    with pytest.raises(GitWorktreeError, match="refusing to reuse"):
        service.create_worktree(existing, "feature/two", service.head_sha())


def test_delete_refuses_a_protected_branch(git_repo):
    service = GitWorktreeService(git_repo)
    with pytest.raises(GitWorktreeError, match="refusing to delete a protected branch"):
        service.delete_unmerged_branch("main")


def test_non_repository_is_refused(tmp_path):
    with pytest.raises(GitWorktreeError, match="repository does not exist"):
        GitWorktreeService(tmp_path / "missing")


def test_push_requires_owner_authorization(git_repo, tmp_path):
    service = GitWorktreeService(git_repo)
    service.create_worktree(tmp_path / "wt-push", "feature/push", service.head_sha())

    # Authorization is checked first, so an unauthenticated push never reaches git.
    with pytest.raises(DeliveryAuthorizationError):
        service.push_feature_branch("feature/push", authorization=None)

    owner_ok = {
        "approved": True,
        "actor_type": "repository_owner",
        "actor": "hzuo",
        "action": "push_feature_branch",
        "approved_at": "2026-09-27T10:00:00Z",
        "reason": "ship it",
    }
    with pytest.raises(GitWorktreeError, match="protected or empty branch"):
        service.push_feature_branch("main", authorization=owner_ok)
    with pytest.raises(GitWorktreeError, match="local feature branch does not exist"):
        service.push_feature_branch("feature/ghost", authorization=owner_ok)


def test_default_base_prefers_the_remote_tracking_default(git_repo, tmp_path):
    remote = tmp_path / "origin.git"
    subprocess.run(["git", "init", "--bare", str(remote)], check=True, capture_output=True)
    subprocess.run(
        ["git", "remote", "add", "origin", str(remote)], cwd=git_repo, check=True, capture_output=True
    )
    subprocess.run(
        ["git", "push", "-u", "origin", "main"], cwd=git_repo, check=True, capture_output=True
    )
    subprocess.run(
        ["git", "remote", "set-head", "origin", "main"], cwd=git_repo, check=True, capture_output=True
    )
    # A local commit that was never pushed must not become the base of new work.
    (git_repo / "local.txt").write_text("local only\n", encoding="utf-8")
    subprocess.run(["git", "add", "local.txt"], cwd=git_repo, check=True, capture_output=True)
    subprocess.run(
        ["git", "-c", "user.email=t@e", "-c", "user.name=t", "commit", "-m", "local"],
        cwd=git_repo,
        check=True,
        capture_output=True,
    )

    sha, branch = GitWorktreeService(git_repo).default_base().split(" ", 1)

    assert branch == "main"
    origin_sha = subprocess.run(
        ["git", "rev-parse", "origin/main"], cwd=git_repo, check=True, capture_output=True, text=True
    ).stdout.strip()
    assert sha == origin_sha
