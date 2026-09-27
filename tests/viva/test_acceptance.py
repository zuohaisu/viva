"""The nine acceptance scenarios, end to end, with real worker processes.

1. Samuel dispatches two independent tasks to Deven.
2. Both executions really run concurrently, in separate worktrees.
3. Alice reviews one of them read-only, independently.
4. Switching member/workspace mid-run leaves attribution intact.
5. Stopping one task leaves the other alone.
6. After a restart, tasks, outputs and unfinished items are recoverable.
7. Changing a member's model keeps its state and history.
8. An out-of-scope delegation is refused, with the reason recorded.
9. GitHub linkage traces back to the right task and output.
"""

from __future__ import annotations

import json
import os

from viva.context import VivaContext
from viva.executions import process_alive

from .conftest import (
    dispatch,
    make_member,
    make_task,
    make_fake_worker,
    wait_until,
    write_config,
)

PARALLEL_WORKER = """#!/bin/sh
# A real worker that leaves a per-process trace inside its work location.
echo "worker pid=$$ cwd=$(pwd)"
echo "$$" > worker-marker.txt
case "$*" in
  *sleep*) sleep 3 ;;
esac
echo "worker done pid=$$"
"""


def _parallel_tool(office, home, fake_bin):
    make_fake_worker(fake_bin, "parallelworker", body=PARALLEL_WORKER)
    current = json.loads((home / "config" / "workers.json").read_text(encoding="utf-8"))
    current["workers"].append(
        {
            "name": "parallelworker",
            "command": "parallelworker",
            "args": [],
            "probe_args": [],
            "capabilities": ["test"],
        }
    )
    write_config(home, "workers.json", current)
    engines = json.loads((home / "config" / "engines.json").read_text(encoding="utf-8"))
    engines["engines"].append(
        {
            "id": "parallel-engine",
            "label": "Parallel engine (test)",
            "tool": "parallelworker",
            "model": "fake-parallel",
            "model_flag": "--model",
            "args": [],
        }
    )
    write_config(home, "engines.json", engines)
    return "parallel-engine"


def _office_with_team(office, home, fake_bin, git_repo):
    """Samuel (coordinator), Deven (developer), Alice (reviewer) + 2 tasks."""
    engine = _parallel_tool(office, home, fake_bin)
    samuel = make_member(office, "Samuel", role="coordinator", engine=engine)
    deven = make_member(office, "Deven", role="developer", engine=engine)
    alice = make_member(office, "Alice", role="reviewer", engine=engine)
    first = make_task(
        office, "Issue 150 — delivery", kind="delivery", repository=git_repo,
        intent="Fix issue 150", constraints=["never touch main"],
    )
    second = make_task(
        office, "Issue 151 — delivery", kind="delivery", repository=git_repo,
        intent="Fix issue 151",
    )
    return samuel, deven, alice, first, second


def test_scenario_1_and_2_samuel_dispatches_two_tasks_that_really_run_in_parallel(
    office, home, fake_bin, git_repo
):
    samuel, deven, alice, first, second = _office_with_team(office, home, fake_bin, git_repo)
    grant = office.office.grant(
        member=samuel["id"],
        task=first["id"],
        actions=["dispatch", "status", "result", "stop"],
        mode_max="write",
        reason="Haisu asked Samuel to coordinate issue 150",
    )

    # Samuel dispatches both tasks to Deven (scenario 1: one member, two tasks).
    one = dispatch(office, first["id"], deven["id"], message="sleep on 150")
    two = dispatch(office, second["id"], deven["id"], message="sleep on 151")

    assert one["member_id"] == two["member_id"] == "deven"
    assert one["task_id"] != two["task_id"]

    worktrees = {
        office.executions.require(one["id"])["work_location"]["path"],
        office.executions.require(two["id"])["work_location"]["path"],
    }
    assert len(worktrees) == 2, "concurrent writable work must be isolated"

    # Scenario 2: both processes are alive at the same time, in their own worktree.
    assert wait_until(
        lambda: all(
            os.path.exists(os.path.join(path, "worker-marker.txt")) for path in worktrees
        ),
        timeout=30,
    )
    pids = {
        int(open(os.path.join(path, "worker-marker.txt")).read().strip()) for path in worktrees
    }
    assert len(pids) == 2
    assert all(process_alive(pid) is True for pid in pids), "both executions run concurrently"

    first_done = office.executions_runner.wait(office.executions_handles[one["id"]])
    second_done = office.executions_runner.wait(office.executions_handles[two["id"]])
    assert first_done["status"] == second_done["status"] == "completed"

    # The grant travelled with Samuel's authority scope only; the workers' own
    # executions recorded their attribution independently.
    assert office.tasks.require(first["id"])["work_location"]["path"] in worktrees
    assert grant["task_id"] == first["id"]


def test_scenario_3_alice_reviews_read_only_and_independently(office, home, fake_bin, git_repo):
    samuel, deven, alice, first, second = _office_with_team(office, home, fake_bin, git_repo)
    delivered = dispatch(office, first["id"], deven["id"], message="do the work")
    office.executions_runner.wait(office.executions_handles[delivered["id"]])

    review = dispatch(office, first["id"], alice["id"], message="review the work")

    record = office.executions.require(review["id"])
    assert record["member_id"] == "alice"
    assert record["role"] == "reviewer"
    assert record["work_location"]["mode"] == "read_only"
    # She reviews the artefact where it is — the same worktree, read-only — and
    # the task keeps its writable allocation for the developer.
    assert record["work_location"]["path"] == delivered["work_location"]["path"]
    assert office.tasks.require(first["id"])["work_location"]["mode"] == "write"
    assert record["tool"] == "parallelworker"
    finished = office.executions_runner.wait(office.executions_handles[review["id"]])
    assert finished["status"] == "completed"
    # Alice's review is a separate execution on the same task, with the
    # developer's attempt still visible in the handoff brief.
    brief = office.office.brief(first["id"])
    assert [attempt["member_id"] for attempt in brief["attempts"]] == ["deven", "alice"]
    # A reviewer can never be given a write execution.
    from viva.residents import InvocationUnavailable
    import pytest

    with pytest.raises(InvocationUnavailable, match="may only run"):
        office.residents.resolve_invocation(alice, mode="write", workers=office.workers)


def test_scenario_4_switching_selection_mid_run_keeps_attribution(
    office, home, fake_bin, git_repo, tmp_path
):
    samuel, deven, alice, first, second = _office_with_team(office, home, fake_bin, git_repo)
    (tmp_path / "another-context").mkdir()
    other_workspace = office.workspaces.register(tmp_path / "another-context", "Elsewhere")
    office.workspaces.use(str(other_workspace["id"]))
    launched = dispatch(office, first["id"], deven["id"], message="sleep while selection changes")

    # Mid-run: select a different member and a different workspace.
    office.runtime.set_current_resident(alice["id"])
    office.workspaces.use(str(other_workspace["id"]))
    assert office.current_resident()["id"] == "alice"
    assert office.current_workspace()["id"] == "elsewhere"

    finished = office.executions_runner.wait(office.executions_handles[launched["id"]])
    assert finished["member_id"] == "deven"
    assert finished["workspace_id"] == first["workspace_id"]
    assert finished["role"] == "developer"

    completion = [
        event
        for event in office.journal.read()
        if event["event_type"] in {"execution.completed", "execution.failed"}
    ][-1]
    assert completion["resident_id"] == "deven"
    assert completion["payload"]["member_id"] == "deven"
    assert completion["payload"]["task_id"] == first["id"]
    # The task's own record was written from the execution, not from the selection.
    assert office.tasks.require(first["id"])["executions"][-1]["member_id"] == "deven"


def test_scenario_5_stopping_one_task_leaves_the_other_running(office, home, fake_bin, git_repo):
    samuel, deven, alice, first, second = _office_with_team(office, home, fake_bin, git_repo)
    one = dispatch(office, first["id"], deven["id"], message="sleep 150")
    two = dispatch(office, second["id"], deven["id"], message="sleep 151")
    assert wait_until(
        lambda: os.path.exists(
            os.path.join(office.executions.require(one["id"])["work_location"]["path"], "worker-marker.txt")
        )
    )

    stopped = office.office.stop(one["id"], reason="Haisu stopped issue 150 only")
    assert stopped["status"] == "stopped"

    keeper = office.executions.require(two["id"])
    assert keeper["status"] == "running"
    assert process_alive(keeper["pid"]) is True
    assert office.executions_runner.wait(office.executions_handles[two["id"]])["status"] == "completed"


def test_scenario_6_a_restart_recovers_tasks_outputs_and_unfinished_items(
    office, home, fake_bin, git_repo
):
    samuel, deven, alice, first, second = _office_with_team(office, home, fake_bin, git_repo)
    ran = dispatch(office, first["id"], deven["id"], message="do the work")
    office.executions_runner.wait(office.executions_handles[ran["id"]])
    office.tasks.record_output(
        first["id"], execution_id=ran["id"], summary="patched issue 150", artifacts=["a.diff"]
    )
    office.tasks.set_unfinished(first["id"], ["run the migration", "ask Alice to review"])

    # An execution that was running when the process died: it has a pid that is
    # long gone and no recorded result.
    orphan = office.executions.create(
        task_id=first["id"],
        member_id=deven["id"],
        role="developer",
        engine={"id": "parallel-engine", "model": "fake-parallel"},
        tool="parallelworker",
        work_location=office.tasks.require(first["id"])["work_location"],
        request={"kind": "user", "id": "haisu"},
    )
    office.executions.save({**orphan, "pid": 999_999_999, "process_started": "Mon Jan  1 00:00:00 2024"})

    # --- restart: a brand new process reading the same state ---
    restarted = VivaContext(home)
    report = restarted.office.recover()

    assert ran["id"] not in report["recoverable"]  # a completed run is never re-offered
    assert restarted.executions.require(ran["id"])["status"] == "completed"
    assert orphan["id"] in report["exited"]
    assert restarted.executions.require(orphan["id"])["status"] == "exited"

    task = restarted.tasks.require(first["id"])
    assert task["outputs"][0]["summary"] == "patched issue 150"
    assert task["unfinished"] == ["run the migration", "ask Alice to review"]
    assert restarted.tasks.unfinished()[0]["id"] == first["id"]

    # The next worker can be briefed with everything it needs.
    brief = restarted.office.brief_text(first["id"])
    assert "Fix issue 150" in brief
    assert "never touch main" in brief
    assert "patched issue 150" in brief
    assert "run the migration" in brief
    assert orphan["id"] in brief
    assert "outcome unknown" in brief


def test_scenario_7_changing_the_model_keeps_member_state_and_history(
    office, home, fake_bin, git_repo
):
    engine = _parallel_tool(office, home, fake_bin)
    deven = make_member(office, "Deven", role="developer", engine=engine)
    task = make_task(office, "Issue 152", kind="delivery", repository=git_repo)
    ran = dispatch(office, task["id"], deven["id"], message="do the work")
    office.executions_runner.wait(office.executions_handles[ran["id"]])
    office.knowledge.add(
        kind="personal_memory",
        title="Deven keeps diffs small",
        body="Break work into reviewable slices.",
        provenance={"task_id": task["id"], "execution_id": ran["id"]},
        member_id=deven["id"],
    )
    history_before = [
        event for event in office.journal.read() if event.get("resident_id") == "deven"
    ]

    office.residents.set_engine("deven", engine="fake-engine", model="fake-1")

    record = office.residents.require("deven")
    assert record["engine"]["id"] == "fake-engine"
    assert record["engine"]["model"] == "fake-1"
    assert record["created_at"] == deven["created_at"]
    history_after = [
        event for event in office.journal.read() if event.get("resident_id") == "deven"
    ]
    assert len(history_after) > len(history_before)
    assert all(
        event["id"] in {item["id"] for item in history_after} for event in history_before
    )  # every earlier event is still there
    assert office.knowledge.list(member_id="deven")[0]["title"] == "Deven keeps diffs small"
    assert office.executions.require(ran["id"])["engine"]["id"] == engine  # recorded as used


def test_scenario_8_an_out_of_scope_delegation_is_refused_and_explained(
    office, home, fake_bin, git_repo
):
    samuel, deven, alice, first, second = _office_with_team(office, home, fake_bin, git_repo)
    grant = office.office.grant(
        member=samuel["id"],
        task=first["id"],
        actions=["dispatch", "status"],
        mode_max="read_only",
        reason="Haisu: only observe issue 150",
    )

    # Out of scope: a different task, a wider mode, and a wider action set.
    cases = [
        ({"task": second["id"]}, "covers task"),
        ({"mode": "write"}, "allows 'read_only'"),
    ]
    from viva.permissions import GrantError
    import pytest

    for kwargs, expected in cases:
        with pytest.raises(GrantError, match=expected):
            office.grants.check(
                grant["id"],
                action="dispatch",
                task_id=kwargs.get("task", first["id"]),
                mode=kwargs.get("mode"),
            )

    with pytest.raises(GrantError, match="outside parent grant"):
        office.grants.create(
            source={"kind": "worker", "id": "exec-coordinator"},
            grantee="deven",
            task_id=first["id"],
            actions=["dispatch", "stop"],
            mode_max="read_only",
            reason="escalation attempt",
            delegated_from=grant["id"],
        )

    refusals = office.grants.refusals()
    assert len(refusals) >= 3
    assert all(record.get("reason") for record in refusals)
    assert any(
        event["event_type"] == "authority.refused" for event in office.journal.read()
    )
    assert office.executions.list(task_id=second["id"]) == []


def test_scenario_9_github_linkage_traces_to_the_task_and_its_output(
    office, home, fake_bin, git_repo, tmp_path, monkeypatch
):
    samuel, deven, alice, first, second = _office_with_team(office, home, fake_bin, git_repo)
    ran = dispatch(office, first["id"], deven["id"], message="do the work")
    office.executions_runner.wait(office.executions_handles[ran["id"]])
    office.tasks.record_output(first["id"], execution_id=ran["id"], summary="patched issue 150")

    branch = office.tasks.require(first["id"])["work_location"]["branch"]
    office.tasks.link_github(first["id"], repo="zuohaisu/viva", issue=150, branch=branch)

    # A fake gh (a real subprocess) stands in for the network, not for the logic.
    log = tmp_path / "gh.log"
    log.write_text("", encoding="utf-8")
    script = fake_bin / "gh"
    script.write_text(
        """#!/bin/sh
echo "$@" >> "$GH_LOG"
case "$1 $2" in
  "auth status") echo "logged in" ;;
  "issue view") echo '%s' ;;
  "pr list") echo '%s' ;;
  "pr view") echo '%s' ;;
  *) echo "unexpected: $*" >&2; exit 1 ;;
esac
"""
        % (
            json.dumps(
                {"number": 150, "title": "Issue 150", "state": "OPEN", "url": "u", "labels": [], "assignees": [], "updatedAt": "t"}
            ),
            json.dumps([{"number": 77, "title": "fix issue 150", "state": "OPEN", "url": "u2", "headRefName": branch, "isDraft": False}]),
            json.dumps(
                {
                    "number": 77,
                    "title": "fix issue 150",
                    "state": "OPEN",
                    "url": "u2",
                    "headRefName": branch,
                    "isDraft": False,
                    "mergeable": "MERGEABLE",
                    "reviews": [],
                    "statusCheckRollup": [{"name": "tests", "conclusion": "SUCCESS"}],
                }
            ),
        ),
        encoding="utf-8",
    )
    script.chmod(0o755)
    monkeypatch.setenv("GH_LOG", str(log))

    evidence = office.office.github_evidence(first["id"])

    assert evidence["issue"]["number"] == 150
    assert evidence["pull_request"]["number"] == 77
    assert evidence["branch"] == branch
    # The link is recorded on the task, next to the output it belongs to.
    linked = office.tasks.require(first["id"])
    assert linked["github"]["issue"] == 150
    assert linked["github"]["pr"] == 77
    assert linked["outputs"][0]["execution_id"] == ran["id"]
    assert office.executions.require(ran["id"])["task_id"] == first["id"]
