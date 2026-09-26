"""Workspace != Project != Repository != Worktree != Task — each held separately."""

from __future__ import annotations

import pytest

from viva.projects import ProjectError

from .conftest import make_task


def test_a_workspace_holds_projects_which_hold_repositories(office, git_repo, tmp_path):
    workspace = office.workspaces.register(git_repo, "Office")
    other_repo = tmp_path / "second-repo"
    other_repo.mkdir()

    project = office.projects.register(
        workspace_id=workspace["id"], name="Viva", repositories=[str(git_repo)]
    )
    assert project["workspace_id"] == workspace["id"]
    assert project["repositories"] == [str(git_repo.resolve())]

    # A project may span several repositories; the same repository may serve two
    # projects — that is explicit, never implied by directory nesting.
    office.projects.bind_repository(project["id"], other_repo)
    another = office.projects.register(
        workspace_id=workspace["id"], name="Twin", repositories=[str(git_repo)]
    )

    assert len(office.projects.get("viva")["repositories"]) == 2
    assert office.projects.primary_repository("viva") == str(git_repo.resolve())
    assert {entry["id"] for entry in office.projects.for_repository(git_repo)} == {"viva", "twin"}
    assert another["id"] == "twin"


def test_projects_are_scoped_to_their_workspace(office, git_repo):
    first = office.workspaces.register(git_repo, "First")
    office.projects.register(workspace_id=first["id"], name="Alpha")
    assert [entry["id"] for entry in office.projects.list(workspace_id="first")] == ["alpha"]
    assert office.projects.list(workspace_id="second") == []


def test_unknown_project_and_bad_repository_are_refused(office, git_repo, tmp_path):
    with pytest.raises(ProjectError, match="no registered project"):
        office.projects.require("ghost")
    with pytest.raises(ProjectError, match="does not exist"):
        office.projects.register(
            workspace_id="office", name="Broken", repositories=[str(tmp_path / "nope")]
        )


def test_task_records_its_project_and_workspace_separately(office, git_repo):
    workspace = office.workspaces.register(git_repo, "Office")
    project = office.projects.register(
        workspace_id=workspace["id"], name="Viva", repositories=[str(git_repo)]
    )
    task = make_task(
        office,
        "Ship the thing",
        kind="delivery",
        repository=git_repo,
        workspace_id=workspace["id"],
        project_id=project["id"],
    )
    assert task["workspace_id"] == "office"
    assert task["project_id"] == "viva"
    assert task["repositories"] == [str(git_repo.resolve())]
    # A task is not a worktree attribute: it exists before any work location.
    assert task["work_location"] == {}
    assert task["status"] == "todo"
