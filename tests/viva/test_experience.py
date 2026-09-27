"""Experience journal: append-only, sequence-checked, secret-redacted."""

from __future__ import annotations

import json

import pytest

from viva.experience import ExperienceError, ExperienceJournal

REQUIRED_FIELDS = (
    "schema_version",
    "id",
    "sequence",
    "timestamp",
    "resident_id",
    "session_id",
    "event_type",
    "workspace",
    "source",
    "payload",
)


def test_append_then_read_has_required_envelope(home):
    journal = ExperienceJournal(home)
    event = journal.append(
        event_type="viva.started",
        resident_id="alice",
        session_id="s-abc",
        workspace="demo",
        source="cli",
        payload={"note": "started"},
    )
    for field in REQUIRED_FIELDS:
        assert field in event
    assert event["sequence"] == 1
    assert event["event_type"] == "viva.started"


def test_sequences_are_monotonic_and_persist(home):
    journal = ExperienceJournal(home)
    journal.append(event_type="viva.started", resident_id="a")
    journal.append(event_type="resident.created", resident_id="a")
    third = journal.append(event_type="user.command", resident_id="a", payload={"command": "help"})

    reloaded = ExperienceJournal(home)
    events = reloaded.read()
    assert [item["sequence"] for item in events] == [1, 2, 3]
    assert third["payload"] == {"command": "help"}
    assert reloaded.tail(2)[0]["event_type"] == "resident.created"


def test_secrets_are_redacted_before_write(home):
    journal = ExperienceJournal(home, known_secrets=("hunter2",))
    journal.append(
        event_type="user.command",
        resident_id="a",
        payload={"api_key": "sk-supersecret123", "password": "hunter2", "note": "token ghp_abcdefghijkl in text"},
    )
    raw = (journal.path.read_text(encoding="utf-8")).splitlines()[0]
    assert "sk-supersecret123" not in raw
    assert "hunter2" not in raw
    assert "ghp_abcdefghijkl" not in raw
    event = json.loads(raw)
    assert event["payload"]["api_key"] == "********"
    assert event["payload"]["password"] == "********"


def test_empty_journal_reads_empty(home):
    assert ExperienceJournal(home).read() == []
    assert ExperienceJournal(home).tail(5) == []


def test_corrupt_line_is_a_loud_error_not_silence(home):
    journal = ExperienceJournal(home)
    journal.append(event_type="viva.started", resident_id="a")
    with journal.path.open("a", encoding="utf-8") as handle:
        handle.write("{not json}\n")
    with pytest.raises(ExperienceError, match="malformed at line 2"):
        journal.read()


def test_sequence_gap_is_rejected(home):
    journal = ExperienceJournal(home)
    journal.append(event_type="viva.started", resident_id="a")
    lines = journal.path.read_text(encoding="utf-8").splitlines()
    event = json.loads(lines[0])
    event["sequence"] = 99
    journal.path.write_text(json.dumps(event) + "\n", encoding="utf-8")
    with pytest.raises(ExperienceError, match="expected sequence 1"):
        journal.read()


def test_append_refuses_empty_event_type(home):
    journal = ExperienceJournal(home)
    with pytest.raises(ExperienceError):
        journal.append(event_type="   ")


def test_experience_is_not_memory(home):
    """The journal is an event stream: nothing is promoted, summarised or recalled."""
    journal = ExperienceJournal(home)
    journal.append(event_type="user.command", resident_id="deven", payload={"command": "status"})
    event = journal.read()[0]
    assert "memory" not in event and "self_model" not in event
    assert set(event) == set(REQUIRED_FIELDS)


def test_execution_events_carry_their_own_attribution(office, git_repo):
    """Experience records what happened, under the attribution of the run itself."""
    from .conftest import dispatch, make_member, make_task

    task = make_task(office, "Recorded work", kind="delivery", repository=git_repo)
    member = make_member(office, "Deven")
    record = dispatch(office, task["id"], member["id"], message="quick")
    office.executions_runner.wait(office.executions_handles[record["id"]])

    started = [
        event for event in office.journal.read() if event["event_type"] == "execution.started"
    ][-1]
    assert started["resident_id"] == "deven"
    assert started["payload"]["execution_id"] == record["id"]
    assert started["payload"]["task_id"] == task["id"]
    assert started["payload"]["work_location"] == record["work_location"]["path"]
    assert started["payload"]["requested_by"] == {"kind": "user", "id": "user"}
