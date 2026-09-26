"""Knowledge: ownership boundaries, provenance, and what counts as reuse."""

from __future__ import annotations

import pytest

from viva.knowledge import KnowledgeError, KnowledgeRegistry

from .conftest import dispatch, make_member, make_task, wait_for


def test_every_kind_has_one_owner_and_a_boundary_test(office, git_repo):
    workspace = office.workspaces.register(git_repo, "Office")
    project = office.projects.register(
        workspace_id=workspace["id"], name="Viva", repositories=[str(git_repo)]
    )
    deven = make_member(office, "Deven")
    alice = make_member(office, "Alice", role="reviewer")

    personal = office.knowledge.add(
        kind="personal_memory",
        title="Deven breaks work into small diffs",
        body="Smaller diffs get reviewed faster.",
        provenance={"source": "observation"},
        member_id=deven["id"],
    )
    hypothesis = office.knowledge.add(
        kind="self_model_candidate",
        title="Deven asks for verification when uncertain",
        body="Candidate hypothesis — evidence accumulating, never auto-promoted.",
        provenance={"source": "observation"},
        member_id=deven["id"],
    )
    project_note = office.knowledge.add(
        kind="project_knowledge",
        title="Viva protects main",
        body="Never commit directly to main in this repository.",
        provenance={"source": "repository convention"},
        project_id=project["id"],
    )
    team_note = office.knowledge.add(
        kind="team_knowledge",
        title="Haisu wants a read-only pass before edits",
        body="Audit first, then propose a change.",
        provenance={"source": "user preference"},
    )
    skill = office.knowledge.add(
        kind="skill",
        title="Independent review of a patch",
        body="# Steps\n1. Read the diff.\n2. Re-run the checks.\n",
        provenance={"source": "past task"},
    )

    assert [entry["owner"] for entry in (personal, hypothesis, project_note, team_note, skill)] == [
        "member",
        "member",
        "project",
        "team",
        "team",
    ]
    # Skills are written as portable SKILL.md files.
    assert office.knowledge.skill_path(skill["id"]).read_text(encoding="utf-8").startswith("---")

    # The boundary is executable: a task only sees what its ownership allows.
    task = make_task(
        office, "Review #812", kind="review", repository=git_repo, project_id=project["id"]
    )
    office.tasks.assign(task["id"], deven["id"])
    visible = {entry["id"] for entry in office.knowledge.for_task(office.tasks.require(task["id"]))}
    assert visible == {personal["id"], hypothesis["id"], project_note["id"], team_note["id"], skill["id"]}

    alices_task = make_task(office, "Alice review", kind="review", repository=git_repo)
    office.tasks.assign(alices_task["id"], alice["id"])
    alice_visible = {
        entry["id"]
        for entry in office.knowledge.for_task(office.tasks.require(alices_task["id"]))
    }
    assert personal["id"] not in alice_visible  # another member's personal memory
    assert team_note["id"] in alice_visible  # team knowledge is shared
    assert project_note["id"] not in alice_visible  # project knowledge without the project


def test_an_entry_without_provenance_is_refused(office):
    with pytest.raises(KnowledgeError, match="provenance"):
        office.knowledge.add(kind="team_knowledge", title="Unmoored", body="?", provenance={})


def test_personal_knowledge_must_name_its_member(office):
    with pytest.raises(KnowledgeError, match="must name the member"):
        office.knowledge.add(
            kind="personal_memory", title="Whose?", body="?", provenance={"source": "test"}
        )


def test_reuse_evidence_only_counts_after_later_use(office, git_repo):
    task = make_task(office, "Do the thing", repository=git_repo)
    member = make_member(office, "Deven")
    execution = dispatch(office, task["id"], member["id"], message="quick")
    wait_for(office, execution["id"])

    entry = office.knowledge.add(
        kind="team_knowledge",
        title="Check the CI result before reporting done",
        body="Do not report success without evidence.",
        provenance={"task_id": task["id"], "execution_id": execution["id"]},
    )
    assert office.knowledge.reuse_evidence() == []

    office.knowledge.record_usage(entry["id"], execution_id=execution["id"], task_id=task["id"])

    reused = office.knowledge.reuse_evidence()
    assert [item["id"] for item in reused] == [entry["id"]]
    assert reused[0]["used_in"][0]["execution_id"] == execution["id"]
    assert any(event["event_type"] == "knowledge.used" for event in office.journal.read())


def test_retracted_entries_keep_their_record_but_leave_the_active_set(office, git_repo):
    entry = office.knowledge.add(
        kind="team_knowledge",
        title="Temporary assumption",
        body="Might be wrong.",
        provenance={"source": "test"},
    )
    office.knowledge.retract(entry["id"], reason="superseded by a later finding")

    stored = office.knowledge.get(entry["id"])
    assert stored["status"] == "retracted"
    assert stored["retracted_reason"] == "superseded by a later finding"
    task = make_task(office, "Anything", kind="research", repository=git_repo)
    assert office.knowledge.for_task(task) == []
    assert any(event["event_type"] == "knowledge.retracted" for event in office.journal.read())


def test_ledger_is_open_format_and_append_only(home):
    registry = KnowledgeRegistry(home)
    registry.add(
        kind="team_knowledge", title="One", body="Body", provenance={"source": "test"}
    )
    lines = (home / "knowledge" / "entries.jsonl").read_text(encoding="utf-8").splitlines()
    assert len(lines) == 1
    registry.record_usage(registry.list()[0]["id"], execution_id="exec-1")
    lines = (home / "knowledge" / "entries.jsonl").read_text(encoding="utf-8").splitlines()
    assert len(lines) == 2  # usage is appended, the entry is never rewritten
