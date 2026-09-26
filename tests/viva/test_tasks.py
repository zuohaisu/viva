"""Tasks: intent that outlives every worker session."""

from __future__ import annotations

import pytest

from viva.tasks import TaskError

from .conftest import dispatch, make_member, make_task, wait_for


def test_task_lifecycle_statuses_are_validated(office, git_repo):
    task = make_task(office, "Do the thing", repository=git_repo)
    assert task["status"] == "todo"
    assert office.tasks.set_status(task["id"], "in_review")["status"] == "in_review"
    with pytest.raises(TaskError, match="unknown task status"):
        office.tasks.set_status(task["id"], "nearly-done")


def test_task_kind_decides_whether_a_worktree_is_needed(office, git_repo):
    research = make_task(office, "Investigate", kind="research", repository=git_repo)
    delivery = make_task(office, "Implement", kind="delivery", repository=git_repo)
    assert research["kind"] == "research" and delivery["kind"] == "delivery"
    with pytest.raises(TaskError, match="unknown task kind"):
        make_task(office, "Vague", kind="whatever", repository=git_repo)


def test_outputs_and_unfinished_items_are_persisted(office, git_repo):
    task = make_task(office, "Do the thing", repository=git_repo)
    member = make_member(office, "Deven")
    record = dispatch(office, task["id"], member["id"], message="quick")
    wait_for(office, record["id"])

    office.tasks.record_output(
        task["id"],
        execution_id=record["id"],
        summary="produced a patch",
        artifacts=["patch.diff"],
    )
    office.tasks.set_unfinished(task["id"], ["run the migration", "ask Alice to review"])

    stored = office.tasks.require(task["id"])
    assert stored["outputs"][0]["summary"] == "produced a patch"
    assert stored["outputs"][0]["artifacts"] == ["patch.diff"]
    assert stored["unfinished"] == ["run the migration", "ask Alice to review"]
    assert [entry["execution_id"] for entry in stored["executions"]] == [record["id"]]
    assert office.tasks.unfinished()[0]["id"] == task["id"]


def test_handoff_brief_carries_goal_constraints_attempts_and_failures(office, git_repo):
    task = make_task(
        office,
        "Do the thing",
        repository=git_repo,
        intent="produce a reviewed patch",
        constraints=["never touch main", "keep diffs small"],
    )
    member = make_member(office, "Deven")
    first = dispatch(office, task["id"], member["id"], message="fail now")
    wait_for(office, first["id"])

    brief = office.office.brief(task["id"])
    assert brief["goal"] == "produce a reviewed patch"
    assert brief["constraints"] == ["never touch main", "keep diffs small"]
    assert brief["work_location"]["mode"] == "write"
    assert brief["attempts"][0]["execution_id"] == first["id"]
    assert brief["attempts"][0]["status"] == "failed"
    assert brief["attempts"][0]["failure_reason"] == "fakeworker failed" or brief["attempts"][0][
        "status"
    ] == "failed"

    text = office.office.brief_text(task["id"], coordinator=True)
    assert "## Previous attempts" in text
    assert "produce a reviewed patch" in text
    assert "Office protocol" in text  # a coordinator is told how to dispatch
    assert "viva office dispatch" in text


def test_a_non_coordinator_brief_has_no_dispatch_protocol(office, git_repo):
    task = make_task(office, "Do the thing", repository=git_repo)
    text = office.office.brief_text(task["id"])
    assert "Office protocol" not in text


def test_task_list_filters(office, git_repo):
    first = make_task(office, "First", repository=git_repo)
    make_task(office, "Second", kind="research", repository=git_repo)
    office.tasks.set_status(first["id"], "done")

    assert len(office.tasks.list()) == 2
    assert [task["id"] for task in office.tasks.list(open_only=True)] == [
        task["id"] for task in office.tasks.list() if task["id"] != first["id"]
    ]
    assert [task["id"] for task in office.tasks.list(status="done")] == [first["id"]]
