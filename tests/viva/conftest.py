"""Shared fixtures: an isolated state home, a real git repo, and fake tools."""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest

from viva.context import VivaContext

FAKE_WORKER_BODY = """#!/bin/sh
# A real worker process for tests: it prints, can sleep, and can fail.
message="$*"
echo "fakeworker start: $message"
case "$message" in
  *sleep*) sleep 4 ;;
  *fail*) echo "fakeworker failing on purpose"; exit 3 ;;
  *token*) echo "leaked token ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345" ;;
esac
echo "fakeworker done"
"""


@pytest.fixture
def home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """An isolated VIVA_HOME so tests never touch the developer's ~/.viva."""
    state = tmp_path / "viva-home"
    monkeypatch.setenv("VIVA_HOME", str(state))
    return state


@pytest.fixture
def context(home: Path) -> VivaContext:
    return VivaContext(home)


@pytest.fixture
def git_repo(tmp_path: Path) -> Path:
    repo = tmp_path / "test-repo"
    repo.mkdir()

    def git(*args: str) -> None:
        subprocess.run(["git", *args], cwd=repo, check=True, capture_output=True, text=True)

    git("init", "-b", "main")
    git("config", "user.email", "test@example.com")
    git("config", "user.name", "Test Owner")
    (repo / "README.md").write_text("test repository\n", encoding="utf-8")
    git("add", ".")
    git("commit", "-m", "init")
    return repo


@pytest.fixture
def fake_bin(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A PATH-isolated bin dir with a controllable fake worker CLI.

    The real PATH is preserved after the fake dir so tools like git keep
    resolving; the fake dir takes precedence.
    """
    bin_dir = tmp_path / "fakebin"
    bin_dir.mkdir()
    monkeypatch.setenv("PATH", f"{bin_dir}:{os.environ.get('PATH', '')}")
    return bin_dir


def make_fake_worker(
    bin_dir: Path, name: str = "fakeworker", *, body: str = FAKE_WORKER_BODY
) -> Path:
    script = bin_dir / name
    script.write_text(body if body.startswith("#!") else f"#!/bin/sh\n{body}", encoding="utf-8")
    script.chmod(0o755)
    return script


def write_config(home: Path, filename: str, payload: dict) -> Path:
    path = home / "config" / filename
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    return path


def register_worker_config(
    home: Path, name: str, command: str, *, args: list[str] | None = None
) -> None:
    """Add a worker entry to the registry config, as a user would."""
    path = home / "config" / "workers.json"
    if path.exists():
        data = json.loads(path.read_text(encoding="utf-8"))
    else:
        data = {"schema_version": "1.0", "workers": []}
    data["workers"].append(
        {"name": name, "command": command, "args": list(args or []), "capabilities": ["test"]}
    )
    write_config(home, "workers.json", data)


@pytest.fixture
def office_tools(home: Path, fake_bin: Path) -> dict:
    """One fake execution tool plus two engines bound to it.

    This is configuration data, exactly as a user would write it: a real
    subprocess the office can dispatch, and model bindings on top of it.
    """
    make_fake_worker(fake_bin, "fakeworker")
    write_config(
        home,
        "workers.json",
        {
            "schema_version": "1.0",
            "workers": [
                {
                    "name": "fakeworker",
                    "command": "fakeworker",
                    "args": [],
                    "capabilities": ["test"],
                }
            ],
        },
    )
    write_config(
        home,
        "engines.json",
        {
            "schema_version": "1.0",
            "engines": [
                {
                    "id": "fake-engine",
                    "label": "Fake engine (test)",
                    "tool": "fakeworker",
                    "model": "fake-1",
                    "model_flag": "--model",
                    "args": [],
                },
                {
                    "id": "secondary-engine",
                    "label": "Second fake engine (test)",
                    "tool": "fakeworker",
                    "model": "fake-2",
                    "model_flag": "--model",
                    "args": [],
                },
            ],
        },
    )
    return {"tool": "fakeworker", "engine": "fake-engine", "secondary_engine": "secondary-engine"}


@pytest.fixture
def office(context: VivaContext, office_tools: dict) -> VivaContext:
    """A context whose configured tool/engine pair is a working fake worker."""
    return context


def make_member(
    context: VivaContext, name: str, *, role: str = "developer", engine: str = "fake-engine"
) -> dict:
    return context.residents.create(name, role=role, engine=engine)


def make_task(
    context: VivaContext,
    title: str,
    *,
    kind: str = "delivery",
    repository: Path | None = None,
    **kwargs,
) -> dict:
    return context.tasks.create(
        title=title,
        kind=kind,
        repositories=[str(repository)] if repository else [],
        **kwargs,
    )


def dispatch(context: VivaContext, task_id: str, member_id: str, **kwargs) -> dict:
    """Shortcut for the office control surface, as a caller would use it."""
    return context.office.dispatch(task=task_id, member=member_id, **kwargs)


def wait_for(context: VivaContext, execution_id: str, *, timeout: float | None = 30.0) -> dict:
    """Wait for an execution this process started and return its final record."""
    handle = context.executions_handles[str(execution_id)]
    return context.executions_runner.wait(handle, timeout=timeout)


def wait_until(predicate, *, timeout: float = 30.0, interval: float = 0.05) -> bool:
    """Poll a predicate; used for process-state assertions, never as a sleep."""
    import time

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return predicate()
