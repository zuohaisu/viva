"""Office control: dispatch, delegation, and the boundary around both."""

from __future__ import annotations

import json
import sys

import pytest

from viva.core.errors import VivaError
from viva.permissions import GrantError, require_invocation_authority

from .conftest import (
    dispatch,
    make_member,
    make_task,
    wait_for,
    wait_until,
    write_config,
)


def _setup(office, git_repo, *, coordinator_role="coordinator"):
    coordinator = make_member(office, "Samuel", role=coordinator_role)
    worker = make_member(office, "Deven", role="developer")
    task = make_task(office, "Deliver the thing", kind="delivery", repository=git_repo)
    return coordinator, worker, task


def test_user_grant_is_the_source_a_worker_spends(office, git_repo):
    coordinator, worker, task = _setup(office, git_repo)
    grant = office.office.grant(
        member=coordinator["id"],
        task=task["id"],
        actions=["dispatch", "status", "result", "stop"],
        mode_max="write",
        reason="Haisu asked Samuel to coordinate this task",
    )
    assert grant["source"] == {"kind": "user", "id": "haisu"}
    assert grant["delegated_from"] is None

    record = dispatch(
        office,
        task["id"],
        worker["id"],
        grant_id=grant["id"],
        origin_execution="exec-coordinator-1",
        message="do the work",
    )

    assert record["request"] == {
        "kind": "worker",
        "id": "exec-coordinator-1",
        "grant_id": grant["id"],
    }
    wait_for(office, record["id"])


def test_worker_dispatch_without_a_grant_is_refused_and_recorded(office, git_repo):
    _, worker, task = _setup(office, git_repo)

    with pytest.raises(GrantError, match="no grant"):
        dispatch(
            office,
            task["id"],
            worker["id"],
            grant_id="grant-does-not-exist",
            origin_execution="exec-coordinator-1",
        )

    refusals = office.grants.refusals()
    assert refusals, "a refused delegation must leave evidence"
    assert "no grant" in refusals[-1]["reason"]
    assert refusals[-1]["attempted"]["task_id"] == task["id"]
    assert any(
        event["event_type"] == "authority.refused" for event in office.journal.read()
    )
    # Nothing was started.
    assert office.executions.list(task_id=task["id"]) == []


def test_a_worker_cannot_dispatch_by_pretending_to_be_the_user(office, git_repo):
    _, worker, task = _setup(office, git_repo)
    # A granted dispatch must name the execution it came from...
    with pytest.raises(VivaError, match="must name the execution it came from"):
        dispatch(office, task["id"], worker["id"], grant_id="whatever")
    # ...and a request that spends a grant may never claim user origin.
    grant = office.office.grant(
        member="deven", task=task["id"], actions=["dispatch"], mode_max="write", reason="scope"
    )
    with pytest.raises(VivaError, match="cannot claim user origin"):
        require_invocation_authority(
            {"kind": "user", "id": "haisu"}, action="dispatch_delegated", grant=grant
        )
    # A worker request with no grant is forbidden outright.
    with pytest.raises(VivaError, match="FORBIDDEN without a live grant"):
        require_invocation_authority(
            {"kind": "worker", "id": "exec-1"}, action="dispatch_delegated", grant=None
        )


def test_grant_is_bounded_to_its_task(office, git_repo):
    coordinator, worker, task = _setup(office, git_repo)
    other = make_task(office, "A different task", kind="delivery", repository=git_repo)
    grant = office.office.grant(
        member=coordinator["id"],
        task=task["id"],
        actions=["dispatch"],
        mode_max="write",
        reason="scope: one task",
    )
    with pytest.raises(GrantError, match="covers task"):
        dispatch(
            office,
            other["id"],
            worker["id"],
            grant_id=grant["id"],
            origin_execution="exec-coordinator-1",
        )


def test_grant_mode_ceiling_is_enforced(office, git_repo):
    coordinator, worker, task = _setup(office, git_repo)
    grant = office.office.grant(
        member=coordinator["id"],
        task=task["id"],
        actions=["dispatch"],
        mode_max="read_only",
        reason="read-only scope",
    )
    with pytest.raises(GrantError, match="allows 'read_only'"):
        office.grants.check(grant["id"], action="dispatch", task_id=task["id"], mode="write")


def test_grant_action_set_is_enforced(office, git_repo):
    coordinator, worker, task = _setup(office, git_repo)
    grant = office.office.grant(
        member=coordinator["id"],
        task=task["id"],
        actions=["status"],
        mode_max="read_only",
        reason="observe only",
    )
    with pytest.raises(GrantError, match="does not cover action 'dispatch'"):
        office.grants.check(grant["id"], action="dispatch", task_id=task["id"])


def test_child_grant_cannot_widen_its_parent(office, git_repo):
    coordinator, worker, task = _setup(office, git_repo)
    parent = office.office.grant(
        member=coordinator["id"],
        task=task["id"],
        actions=["dispatch", "status"],
        mode_max="read_only",
        reason="Haisu's original scope",
    )

    with pytest.raises(GrantError, match="outside parent grant"):
        office.grants.create(
            source={"kind": "worker", "id": "exec-coordinator-1"},
            grantee="deven",
            task_id=task["id"],
            actions=["dispatch", "stop"],
            mode_max="read_only",
            reason="worker tries to widen",
            delegated_from=parent["id"],
        )
    with pytest.raises(GrantError, match="exceeds parent grant"):
        office.grants.create(
            source={"kind": "worker", "id": "exec-coordinator-1"},
            grantee="deven",
            task_id=task["id"],
            actions=["dispatch"],
            mode_max="write",
            reason="worker tries to escalate the mode",
            delegated_from=parent["id"],
        )
    with pytest.raises(GrantError, match="outside parent grant"):
        office.grants.create(
            source={"kind": "worker", "id": "exec-coordinator-1"},
            grantee="deven",
            task_id="some-other-task",
            actions=["dispatch"],
            mode_max="read_only",
            reason="worker tries to change the task",
            delegated_from=parent["id"],
        )

    # A genuinely narrower child is allowed and records its lineage.
    child = office.grants.create(
        source={"kind": "worker", "id": "exec-coordinator-1"},
        grantee="deven",
        task_id=task["id"],
        actions=["status"],
        mode_max="read_only",
        reason="worker passes on a narrower scope",
        delegated_from=parent["id"],
    )
    assert child["delegated_from"] == parent["id"]
    assert child["actions"] == ["status"]

    reasons = [record["reason"] for record in office.grants.refusals()]
    assert any("outside parent grant" in reason for reason in reasons)
    assert any("exceeds parent grant" in reason for reason in reasons)


def test_worker_originated_grant_must_name_its_parent(office, git_repo):
    _, _, task = _setup(office, git_repo)
    with pytest.raises(GrantError, match="must name the parent grant"):
        office.grants.create(
            source={"kind": "worker", "id": "exec-coordinator-1"},
            grantee="deven",
            task_id=task["id"],
            actions=["dispatch"],
            mode_max="read_only",
            reason="no parent",
        )


def test_revoked_grant_stops_working(office, git_repo):
    coordinator, worker, task = _setup(office, git_repo)
    grant = office.office.grant(
        member=coordinator["id"],
        task=task["id"],
        actions=["dispatch"],
        mode_max="write",
        reason="temporary",
    )
    office.grants.revoke(grant["id"], reason="Haisu withdrew the scope")
    with pytest.raises(GrantError, match="was revoked"):
        dispatch(
            office,
            task["id"],
            worker["id"],
            grant_id=grant["id"],
            origin_execution="exec-coordinator-1",
        )


def test_any_member_can_be_dispatched_to_any_allowed_task(office, git_repo):
    """The coordination graph is data — there is no fixed dev→QA pipeline."""
    operator = make_member(office, "Oliver", role="operator")
    reviewer = make_member(office, "Alice", role="reviewer")
    task = make_task(office, "Operational task", kind="ops", repository=git_repo)

    oliver = dispatch(office, task["id"], operator["id"], message="operate")
    alice = dispatch(office, task["id"], reviewer["id"], mode="read_only", message="review")
    wait_for(office, oliver["id"])
    wait_for(office, alice["id"])

    records = office.executions.list(task_id=task["id"])
    assert {record["member_id"] for record in records} == {"oliver", "alice"}
    assert {record["role"] for record in records} == {"operator", "reviewer"}
    # No implicit second stage was created for the first execution.
    assert len(records) == 2


# -- real coordinator → real executor ----------------------------------------

COORDINATOR_BODY = """#!/bin/sh
# A real coordinator worker: a process that calls Viva's office API itself.
set -e
task="$(echo "$*" | awk '{print $(NF-1)}')"
member="$(echo "$*" | awk '{print $NF}')"
PYTHON -m viva office dispatch --task "$task" --to "$member" \\
    --message "executor: do the work assigned by the coordinator" --json
"""


@pytest.fixture
def coordinator_tool(office, home, fake_bin):
    """A second real worker CLI that dispatches through the office API."""
    script = fake_bin / "fakecoordinator"
    script.write_text(
        COORDINATOR_BODY.replace("PYTHON", sys.executable), encoding="utf-8"
    )
    script.chmod(0o755)
    current = json.loads((home / "config" / "workers.json").read_text(encoding="utf-8"))
    current["workers"].append(
        {
            "name": "fakecoordinator",
            "command": "fakecoordinator",
            "args": [],
            "probe_args": [],
            "capabilities": ["test"],
        }
    )
    write_config(home, "workers.json", current)
    engines = json.loads((home / "config" / "engines.json").read_text(encoding="utf-8"))
    engines["engines"].append(
        {
            "id": "coordinator-engine",
            "label": "Coordinator engine (test)",
            "tool": "fakecoordinator",
            "model": "fake-coordinator",
            "model_flag": "--model",
            "args": [],
        }
    )
    write_config(home, "engines.json", engines)
    return "coordinator-engine"


def test_real_coordinator_worker_dispatches_a_real_execution_worker(
    office, git_repo, coordinator_tool
):
    """A real worker process, spending a real grant, starts a real worker process."""
    samuel = make_member(office, "Samuel", role="coordinator", engine=coordinator_tool)
    deven = make_member(office, "Deven", role="developer")
    task = make_task(office, "Coordinated delivery", kind="delivery", repository=git_repo)
    grant = office.office.grant(
        member=samuel["id"],
        task=task["id"],
        actions=["dispatch", "status", "result", "stop"],
        mode_max="write",
        reason="Haisu: coordinate this task and use Deven for the work",
    )

    coordinator_execution = dispatch(
        office,
        task["id"],
        samuel["id"],
        message=f"TASK {task['id']} {deven['id']}",
    )
    finished = wait_for(office, coordinator_execution["id"])
    assert finished["status"] == "completed", finished.get("failure_reason")

    child = [
        record
        for record in office.executions.list(task_id=task["id"])
        if record["id"] != coordinator_execution["id"]
    ]
    assert len(child) == 1, "the coordinator's dispatch really started an execution"
    child_record = child[0]

    assert child_record["member_id"] == deven["id"]
    assert child_record["role"] == "developer"
    assert child_record["request"] == {
        "kind": "worker",
        "id": coordinator_execution["id"],
        "grant_id": grant["id"],
    }
    # The executor really ran: its own output log proves it.
    assert wait_until(
        lambda: "fakeworker done"
        in "\n".join(office.office.result(child_record["id"])["output_tail"]),
        timeout=30,
    )
    # Its supervising process (the coordinator) has already exited, so Viva
    # reports the outcome honestly instead of inventing one.
    office.office.recover()
    child_finished = office.executions.require(child_record["id"])
    assert child_finished["status"] == "exited"
    assert "without a recorded result" in child_finished["failure_reason"]
    assert child_finished["recoverable"] is True
    brief = office.office.brief(task["id"])
    assert [attempt["execution_id"] for attempt in brief["attempts"]] == [
        coordinator_execution["id"],
        child_record["id"],
    ]

    dispatches = [
        event for event in office.journal.read() if event["event_type"] == "office.dispatched"
    ]
    assert len(dispatches) == 2
    assert dispatches[-1]["payload"]["requested_by"]["kind"] == "worker"
    assert dispatches[-1]["payload"]["requested_by"]["id"] == coordinator_execution["id"]


def test_coordinator_dispatch_is_refused_once_the_grant_is_gone(
    office, git_repo, coordinator_tool
):
    samuel = make_member(office, "Samuel", role="coordinator", engine=coordinator_tool)
    deven = make_member(office, "Deven", role="developer")
    task = make_task(office, "Ungranted coordination", kind="delivery", repository=git_repo)

    coordinator_execution = dispatch(
        office,
        task["id"],
        samuel["id"],
        message=f"TASK {task['id']} {deven['id']}",
    )
    finished = wait_for(office, coordinator_execution["id"])

    # The coordinator ran, tried to dispatch, and really failed — visibly.
    assert finished["status"] == "failed"
    result = office.office.result(coordinator_execution["id"])
    assert "FORBIDDEN without a live grant" in "\n".join(result["output_tail"])
    assert len(office.executions.list(task_id=task["id"])) == 1
