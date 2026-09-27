"""Execution tools: a data-driven registry with honest availability probing."""

from __future__ import annotations

import json

import pytest

from viva.workers import WorkerError, WorkerRegistry
from viva.workers.registry import DEFAULT_WORKERS

from .conftest import make_fake_worker


def test_first_use_seeds_defaults_as_data(home):
    registry = WorkerRegistry(home)
    names = [worker["name"] for worker in registry.list_unprobed()]
    assert names == [worker["name"] for worker in DEFAULT_WORKERS]
    stored = json.loads((home / "config" / "workers.json").read_text(encoding="utf-8"))
    assert stored["schema_version"] == "1.0"
    # Seeds are editable data, not hard-coded behavior: removing one sticks.
    stored["workers"] = stored["workers"][:1]
    (home / "config" / "workers.json").write_text(json.dumps(stored), encoding="utf-8")
    assert [w["name"] for w in WorkerRegistry(home).list_unprobed()] == [DEFAULT_WORKERS[0]["name"]]


def test_get_is_case_insensitive_and_missing_raises(home):
    registry = WorkerRegistry(home)
    assert registry.get("CODEX")["command"] == "codex"
    with pytest.raises(WorkerError, match="no worker named"):
        registry.get("nonexistent")


def test_probe_reports_availability_honestly(home, fake_bin):
    make_fake_worker(fake_bin, "probecli", body='echo "probecli 1.2.3"\nexit 0\n')
    registry = WorkerRegistry(home)
    worker = {"name": "probe", "command": "probecli", "args": [], "probe_args": ["--version"]}
    entry = registry.probe(worker)
    assert entry["available"] is True
    assert entry["version"] == "probecli 1.2.3"

    missing = registry.probe({"name": "ghost", "command": "definitely-not-here", "args": []})
    assert missing["available"] is False
    assert "not on PATH" in missing["reason"]


def test_probe_survives_failing_probe_command(home, fake_bin):
    make_fake_worker(fake_bin, "brokencli", body="exit 3\n")
    registry = WorkerRegistry(home)
    entry = registry.probe({"name": "broken", "command": "brokencli", "probe_args": ["--version"]})
    assert entry["available"] is False
    assert "exited 3" in entry["reason"]


def test_normalize_rejects_bad_records(home):
    for bad in (
        {"name": "has space", "command": "x"},
        {"name": "ok", "command": ""},
        {"name": "ok", "command": "x", "args": "not-a-list"},
        {"name": "ok", "command": "x", "timeout_seconds": -1},
        "not-an-object",
    ):
        stored = {"schema_version": "1.0", "workers": [bad]}
        (home / "config").mkdir(parents=True, exist_ok=True)
        (home / "config" / "workers.json").write_text(json.dumps(stored), encoding="utf-8")
        with pytest.raises(WorkerError):
            WorkerRegistry(home).list_unprobed()
