"""Executions: real processes, real concurrency, honest states, no fake restarts."""

from __future__ import annotations

import os

import pytest

from viva.executions import ExecutionError, ExecutionRegistry, process_alive

from .conftest import dispatch, make_member, make_task, wait_for, wait_until


def _task_with_worktree(office, repository, title="Deliver something"):
    task = make_task(office, title, kind="delivery", repository=repository)
    member = make_member(office, "Deven", role="developer")
    return task, member


def test_two_executions_of_one_member_run_concurrently_in_separate_worktrees(
    office, git_repo
):
    """Scenario: the same member works on two tasks at once, isolated by worktree."""
    first, member = _task_with_worktree(office, git_repo, "Issue 150")
    second = make_task(office, "Issue 151", kind="delivery", repository=git_repo)

    slow = dispatch(office, first["id"], member["id"], message="sleep please")
    quick = dispatch(office, second["id"], member["id"], message="quick work")

    paths = {
        office.executions.require(slow["id"])["work_location"]["path"],
        office.executions.require(quick["id"])["work_location"]["path"],
    }
    assert len(paths) == 2  # different tasks never share a writable checkout
    assert all(os.path.isdir(path) for path in paths)
    assert len(office.executions.running(member_id=member["id"])) == 2

    finished_quick = wait_for(office, quick["id"])
    assert finished_quick["status"] == "completed"
    assert office.executions.require(slow["id"])["status"] == "running"

    finished_slow = wait_for(office, slow["id"])
    assert finished_slow["status"] == "completed"
    assert finished_slow["member_id"] == member["id"]
    assert finished_slow["task_id"] == first["id"]


def test_stopping_one_execution_leaves_the_other_running(office, git_repo):
    first, member = _task_with_worktree(office, git_repo, "Stop me")
    second = make_task(office, "Keep me", kind="delivery", repository=git_repo)

    doomed = dispatch(office, first["id"], member["id"], message="sleep long")
    keeper = dispatch(office, second["id"], member["id"], message="sleep long")
    assert office.executions.require(doomed["id"])["status"] == "running"

    stopped = office.office.stop(doomed["id"], reason="user stopped this one task")

    assert stopped["status"] == "stopped"
    assert stopped["failure_reason"] == "user stopped this one task"
    assert office.executions.require(keeper["id"])["status"] == "running"
    assert process_alive(office.executions.require(keeper["id"])["pid"]) is True

    final = wait_for(office, keeper["id"])
    assert final["status"] == "completed"


def test_stopping_an_execution_that_already_finished_is_refused(office, git_repo):
    task, member = _task_with_worktree(office, git_repo)
    record = dispatch(office, task["id"], member["id"], message="quick")
    wait_for(office, record["id"])
    with pytest.raises(ExecutionError, match="not running"):
        office.office.stop(record["id"], reason="too late")


def test_every_execution_records_its_own_attribution_at_launch(office, git_repo):
    task, deven = _task_with_worktree(office, git_repo)
    alice = make_member(office, "Alice", role="reviewer")

    record = dispatch(office, task["id"], deven["id"], message="attribution check")
    assert record["member_id"] == "deven"
    assert record["role"] == "developer"
    assert record["engine"] == {"id": "fake-engine", "model": "fake-1"}
    assert record["tool"] == "fakeworker"
    assert record["request"] == {"kind": "user", "id": "haisu"}
    assert record["work_location"]["mode"] == "write"
    assert record["workspace_id"] == task["workspace_id"]

    # Changing what the UI/CLI currently points at must not rewrite that.
    office.runtime.set_current_resident(alice["id"])
    finished = wait_for(office, record["id"])

    assert finished["member_id"] == "deven"
    assert finished["task_id"] == task["id"]
    event = [
        event
        for event in office.journal.read()
        if event["event_type"] in {"execution.completed", "execution.failed"}
    ][-1]
    assert event["resident_id"] == "deven"
    assert event["payload"]["member_id"] == "deven"
    assert event["payload"]["task_id"] == task["id"]


def test_worker_stdout_is_captured_and_redacted(office, git_repo):
    task, member = _task_with_worktree(office, git_repo)
    record = dispatch(office, task["id"], member["id"], message="print a token")
    wait_for(office, record["id"])

    result = office.office.result(record["id"])
    text = "\n".join(result["output_tail"])
    assert "fakeworker start" in text
    assert "ghp_" not in text
    assert "********" in text


def test_failing_execution_is_reported_as_failed(office, git_repo):
    task, member = _task_with_worktree(office, git_repo)
    record = dispatch(office, task["id"], member["id"], message="fail now")
    finished = wait_for(office, record["id"])
    assert finished["status"] == "failed"
    assert finished["exit_code"] == 3


def test_read_only_task_needs_no_worktree(office, git_repo):
    task = make_task(office, "Research the calendar semantics", kind="research", repository=git_repo)
    member = make_member(office, "Richard", role="researcher")
    record = dispatch(office, task["id"], member["id"], message="read only please")

    location = office.executions.require(record["id"])["work_location"]
    assert location["kind"] == "repository"
    assert location["mode"] == "read_only"
    assert location["path"] == str(git_repo.resolve())
    assert office.tasks.require(task["id"])["work_location"]["kind"] == "repository"


def test_recovery_reports_states_honestly_and_launches_nothing(office, git_repo, monkeypatch):
    task, member = _task_with_worktree(office, git_repo)
    # A record claiming to run while its process is long gone: exactly what a
    # reboot leaves behind.
    orphan = office.executions.create(
        task_id=task["id"],
        member_id=member["id"],
        role=member["role"],
        engine={"id": "fake-engine", "model": "fake-1"},
        tool="fakeworker",
        work_location={"kind": "worktree", "path": str(git_repo), "mode": "write"},
        request={"kind": "user", "id": "haisu"},
    )
    office.executions.save({**orphan, "pid": 999_999_999, "process_started": "Mon Jan  1 00:00:00 2024"})

    from viva.executions import runner as runner_module

    def explode(*args, **kwargs):  # pragma: no cover - only fires on a bug
        raise AssertionError("recovery must never start a process")

    monkeypatch.setattr(runner_module.ExecutionRunner, "start", explode)

    report = office.office.recover()

    assert orphan["id"] in report["exited"]
    assert orphan["id"] in report["recoverable"]
    recorded = office.executions.require(orphan["id"])
    assert recorded["status"] == "exited"
    assert "without a recorded result" in recorded["failure_reason"]
    assert recorded["recoverable"] is True


def test_recovery_does_not_touch_completed_or_running_executions(office, git_repo):
    task, member = _task_with_worktree(office, git_repo)
    done = dispatch(office, task["id"], member["id"], message="quick")
    wait_for(office, done["id"])
    live = dispatch(office, task["id"], member["id"], message="sleep more")

    report = office.office.recover()

    assert done["id"] not in report["recoverable"]
    assert office.executions.require(done["id"])["status"] == "completed"
    assert live["id"] in report["running"]
    assert office.executions.require(live["id"])["status"] == "running"
    office.office.stop(live["id"], reason="test cleanup")


def test_reconcile_is_idempotent_and_distinguishes_exited_from_unknown(office, git_repo):
    task, member = _task_with_worktree(office, git_repo)
    long_gone = office.executions.create(
        task_id=task["id"],
        member_id=member["id"],
        role=member["role"],
        engine={"id": "fake-engine", "model": "fake-1"},
        tool="fakeworker",
        work_location={"kind": "worktree", "path": str(git_repo), "mode": "write"},
        request={"kind": "user", "id": "haisu"},
    )
    office.executions.save({**long_gone, "pid": 999_999_999})
    no_pid = office.executions.create(
        task_id=task["id"],
        member_id=member["id"],
        role=member["role"],
        engine={"id": "fake-engine", "model": "fake-1"},
        tool="fakeworker",
        work_location={"kind": "worktree", "path": str(git_repo), "mode": "write"},
        request={"kind": "user", "id": "haisu"},
    )

    first = office.office.recover()
    second = office.office.recover()

    assert first["exited"] == [long_gone["id"]]
    assert first["unknown"] == [no_pid["id"]]  # no pid recorded: honest "cannot tell"
    assert second["exited"] == [] and second["unknown"] == []
    assert office.executions.require(long_gone["id"])["status"] == "exited"
    assert office.executions.require(no_pid["id"])["status"] == "unknown"


def test_process_alive_rejects_a_reused_pid(git_repo):
    assert process_alive(os.getpid(), "Mon Jan  1 00:00:00 1990") is False
    assert process_alive(None) is None
    assert process_alive(999_999_999) is False


def test_registry_refuses_an_execution_without_a_real_request_source(office, git_repo):
    with pytest.raises(ExecutionError, match="explicit request source"):
        office.executions.create(
            task_id="task-x",
            member_id="deven",
            role="developer",
            engine={},
            tool="fakeworker",
            work_location={},
            request={"kind": "nobody", "id": "?"},
        )


def test_wait_until_helper_is_used_for_process_state(office, git_repo):
    """A stopped execution's process really is gone (not just marked stopped)."""
    task, member = _task_with_worktree(office, git_repo)
    record = dispatch(office, task["id"], member["id"], message="sleep")
    pid = office.executions.require(record["id"])["pid"]
    office.office.stop(record["id"], reason="cleanup")
    assert wait_until(lambda: process_alive(pid) is False, timeout=10)
