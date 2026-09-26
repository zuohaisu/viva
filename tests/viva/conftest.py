"""Shared fixtures for Viva tests: an isolated state home and a real git repo."""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest

from viva.context import VivaContext


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
    bin_dir: Path,
    name: str = "fakeworker",
    *,
    body: str = 'echo "fake ran: $@"\nexit 0\n',
) -> Path:
    script = bin_dir / name
    script.write_text(f"#!/bin/sh\n{body}", encoding="utf-8")
    script.chmod(0o755)
    return script


def register_worker_config(home: Path, name: str, command: str) -> None:
    """Add a worker entry to the registry config, as a user would."""
    import json

    path = home / "config" / "workers.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        data = json.loads(path.read_text(encoding="utf-8"))
    else:
        data = {"schema_version": "1.0", "workers": []}
    data["workers"].append({"name": name, "command": command, "args": [], "capabilities": ["test"]})
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
