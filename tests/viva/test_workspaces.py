"""Workspace registry: persistence, repo detection, current selection."""

from __future__ import annotations

import pytest

from viva.workspaces import WorkspaceError, WorkspaceRegistry
from viva.runtime import RuntimeState


def _registry(home):
    runtime = RuntimeState(home)
    return WorkspaceRegistry(home, runtime=runtime), runtime


def test_register_git_repo_detects_repository(home, git_repo):
    registry, _ = _registry(home)
    workspace = registry.register(git_repo, "Demo")
    assert workspace["is_git"] is True
    assert workspace["path"] == str(git_repo.resolve())
    assert workspace["last_opened_at"] is None


def test_register_plain_directory_marks_not_git(home, tmp_path):
    plain = tmp_path / "plain"
    plain.mkdir()
    registry, _ = _registry(home)
    workspace = registry.register(plain)
    assert workspace["is_git"] is False
    assert workspace["name"] == "plain"  # name defaults to directory name


def test_missing_path_rejected(home, tmp_path):
    registry, _ = _registry(home)
    with pytest.raises(WorkspaceError, match="does not exist"):
        registry.register(tmp_path / "ghost")


def test_duplicate_name_or_path_rejected(home, git_repo, tmp_path):
    registry, _ = _registry(home)
    registry.register(git_repo, "Demo")
    with pytest.raises(WorkspaceError, match="already registered"):
        registry.register(git_repo, "Demo")
    other = tmp_path / "other"
    other.mkdir()
    with pytest.raises(WorkspaceError, match="already registered"):
        registry.register(other, "demo")  # slug collision with Demo


def test_registration_and_selection_persist_across_instances(home, git_repo):
    registry, runtime = _registry(home)
    registry.register(git_repo, "Demo")
    registry.use("Demo")

    registry2, _ = _registry(home)
    assert [item["id"] for item in registry2.list()] == ["demo"]
    current = registry2.current()
    assert current is not None and current["id"] == "demo"
    assert runtime.load()["current_workspace"] == "demo"
    opened = registry2.list()[0]["last_opened_at"]
    assert opened is not None  # selection time is recorded on the entry itself


def test_current_is_none_when_unselected_or_gone(home, git_repo):
    registry, _ = _registry(home)
    assert registry.current() is None
    registry.register(git_repo, "Demo")
    assert registry.current() is None  # registered but not selected
    registry.use("Demo")
    import shutil

    shutil.rmtree(git_repo)
    assert registry.current() is None  # path vanished: honest None, not a crash
