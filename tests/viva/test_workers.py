"""Worker registry and safe, user-initiated invocation."""

from __future__ import annotations

import json
import time

import pytest

from viva.core.errors import VivaError
from viva.workers import WorkerError, WorkerRegistry, run_worker
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


def test_run_worker_streams_output_and_reports_success(home, fake_bin, git_repo):
    make_fake_worker(fake_bin, "echocli")
    result = run_worker(
        {"name": "echo", "command": "echocli", "args": [], "timeout_seconds": 30},
        "hello from the user",
        cwd=git_repo,
        initiated_by="user",
    )
    assert result.status == "COMPLETED"
    assert result.exit_code == 0
    assert "hello from the user" in "\n".join(result.output)


def test_run_worker_records_streamed_lines_via_callback(home, fake_bin, git_repo):
    make_fake_worker(fake_bin, "streamy", body='for i in 1 2 3; do echo "line $i"; done\nexit 0\n')
    seen: list[str] = []
    result = run_worker(
        {"name": "streamy", "command": "streamy", "args": [], "timeout_seconds": 30},
        "go",
        cwd=git_repo,
        initiated_by="user",
        on_output=lambda stream, line: seen.append(line),
    )
    assert result.status == "COMPLETED"
    assert len(seen) == 3


def test_run_worker_reports_failure_and_timeout(home, fake_bin, git_repo):
    make_fake_worker(fake_bin, "failcli", body="echo boom >&2\nexit 1\n")
    failed = run_worker(
        {"name": "fail", "command": "failcli", "args": [], "timeout_seconds": 30},
        "go",
        cwd=git_repo,
        initiated_by="user",
    )
    assert failed.status == "FAILED" and failed.exit_code == 1
    assert "boom" in "\n".join(failed.output)

    make_fake_worker(fake_bin, "slowcli", body="sleep 30\n")
    started = time.monotonic()
    timed_out = run_worker(
        {"name": "slow", "command": "slowcli", "args": [], "timeout_seconds": 30},
        "go",
        cwd=git_repo,
        initiated_by="user",
        timeout_seconds=0.5,
    )
    assert timed_out.status == "TIMEOUT" and timed_out.exit_code is None
    assert time.monotonic() - started < 10  # actually bounded, not 30s


def test_autonomous_invocation_is_structurally_forbidden(home, fake_bin, git_repo):
    make_fake_worker(fake_bin, "echocli")
    with pytest.raises(VivaError, match="FORBIDDEN in Phase 1"):
        run_worker(
            {"name": "echo", "command": "echocli", "args": [], "timeout_seconds": 30},
            "go",
            cwd=git_repo,
            initiated_by="agent",
        )


def test_run_worker_needs_a_real_directory(home, fake_bin, tmp_path):
    make_fake_worker(fake_bin, "echocli")
    with pytest.raises(WorkerError, match="working directory does not exist"):
        run_worker(
            {"name": "echo", "command": "echocli", "args": [], "timeout_seconds": 30},
            "go",
            cwd=tmp_path / "missing",
            initiated_by="user",
        )
