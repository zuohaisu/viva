"""AI members: many of them, with configurable roles and replaceable engines."""

from __future__ import annotations

import json

import pytest

from viva.residents import InvocationUnavailable, ResidentError, RoleCatalog


def test_many_members_with_different_roles_coexist(office):
    samuel = office.residents.create("Samuel", role="coordinator", engine="fake-engine")
    deven = office.residents.create("Deven", role="developer", engine="fake-engine")
    alice = office.residents.create("Alice", role="reviewer", engine="fake-engine")

    members = office.residents.list()
    assert [member["id"] for member in members] == ["samuel", "deven", "alice"]
    assert {member["id"]: member["role"] for member in members} == {
        "samuel": "coordinator",
        "deven": "developer",
        "alice": "reviewer",
    }
    assert samuel["engine"]["id"] == deven["engine"]["id"] == alice["engine"]["id"]


def test_names_are_data_not_identity(office):
    member = office.residents.create("Richard", role="researcher", engine="fake-engine")
    assert member["id"] == "richard"
    record = json.loads((office.home / "residents" / "richard.json").read_text(encoding="utf-8"))
    assert record["name"] == "Richard"
    assert office.residents.get("richard")["role"] == "researcher"


def test_roles_come_from_an_editable_catalogue(home):
    catalog = RoleCatalog(home)
    seeded = [role["id"] for role in catalog.list()]
    assert {"coordinator", "developer", "reviewer", "operator", "researcher"} <= set(seeded)

    path = home / "config" / "roles.json"
    data = json.loads(path.read_text(encoding="utf-8"))
    data["roles"].append(
        {
            "id": "archivist",
            "title": "Archivist",
            "purpose": "Tends the record.",
            "allowed_modes": ["read_only"],
            "default_mode": "read_only",
        }
    )
    path.write_text(json.dumps(data), encoding="utf-8")

    assert "archivist" in [role["id"] for role in RoleCatalog(home).list()]


def test_unknown_role_is_refused(office):
    with pytest.raises(ResidentError, match="unknown role"):
        office.residents.create("Nobody", role="wizard", engine="fake-engine")


def test_reviewer_role_may_not_run_a_write_execution(office):
    alice = office.residents.create("Alice", role="reviewer", engine="fake-engine")
    with pytest.raises(InvocationUnavailable, match="may only run"):
        office.residents.resolve_invocation(alice, mode="write", workers=office.workers)
    invocation = office.residents.resolve_invocation(
        alice, mode="read_only", workers=office.workers
    )
    assert invocation["mode"] == "read_only"


def test_changing_the_engine_keeps_record_history_and_knowledge(office):
    deven = office.residents.create("Deven", role="developer", engine="fake-engine")
    office.knowledge.add(
        kind="personal_memory",
        title="Deven prefers small diffs",
        body="Break work into reviewable slices.",
        provenance={"source": "test"},
        member_id=deven["id"],
    )
    before_events = len([e for e in office.journal.read() if e.get("resident_id") == "deven"])

    updated = office.residents.set_engine("deven", engine="secondary-engine")

    assert updated["engine"]["id"] == "secondary-engine"
    assert updated["engine"]["model"] == "fake-2"
    assert updated["role"] == deven["role"]
    assert updated["created_at"] == deven["created_at"]
    after_events = [e for e in office.journal.read() if e.get("resident_id") == "deven"]
    assert len(after_events) > before_events
    assert any(event["event_type"] == "member.engine_changed" for event in after_events)
    assert office.knowledge.list(member_id="deven")[0]["title"] == "Deven prefers small diffs"
    assert office.residents.get("deven")["engine"]["id"] == "secondary-engine"


def test_engine_change_records_what_was_used_before(office):
    office.residents.create("Deven", role="developer", engine="fake-engine")
    office.residents.set_engine("deven", engine="secondary-engine")
    event = [
        event for event in office.journal.read() if event["event_type"] == "member.engine_changed"
    ][-1]
    assert event["payload"]["from"] == "fake-engine"
    assert event["payload"]["to"] == "secondary-engine"
    assert event["payload"]["history_retained"] is True


def test_unavailable_tool_fails_loudly_and_is_not_substituted(office, monkeypatch):
    member = office.residents.create("Deven", role="developer", engine="fake-engine")
    monkeypatch.setenv("PATH", "/nonexistent-bin")
    with pytest.raises(InvocationUnavailable) as excinfo:
        office.residents.resolve_invocation(member, workers=office.workers)
    message = str(excinfo.value)
    assert "fakeworker" in message
    assert "will not silently substitute" in message


def test_invocation_records_the_concrete_model_and_tool(office):
    member = office.residents.create("Deven", role="developer", engine="secondary-engine")
    invocation = office.residents.resolve_invocation(member, workers=office.workers)
    assert invocation["engine"] == {"id": "secondary-engine", "model": "fake-2"}
    assert invocation["tool"] == "fakeworker"
    argv = office.residents.build_argv(invocation, "hello")
    assert argv[0] == "fakeworker"
    assert argv[-1] == "hello"
    assert "fake-2" in argv


def test_member_records_do_not_claim_self_continuity(office):
    office.residents.create("Samuel", role="coordinator", engine="fake-engine")
    record = office.residents.get("samuel")
    # Identity, role and bindings only: no self-model, no memory, no persona fields.
    assert set(record) == {
        "schema_version",
        "id",
        "name",
        "role",
        "created_at",
        "notes",
        "engine",
        "tools",
    }


def test_legacy_records_are_read_without_being_destroyed(office):
    """A v1 record (identity only) still loads; it is not rewritten on read."""
    path = office.home / "residents" / "legacy.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(
            {"schema_version": "1.0", "id": "legacy", "name": "Legacy", "created_at": "2026-01-01T00:00:00Z", "notes": ""}
        ),
        encoding="utf-8",
    )
    record = office.residents.get("legacy")
    assert record["role"] == "developer"
    assert record["engine"]["id"]
    assert json.loads(path.read_text(encoding="utf-8"))["schema_version"] == "1.0"
