"""Data-driven worker registry with local availability probing.

Each worker record:

    {"name": "codex", "command": "codex", "args": ["exec"],
     "capabilities": ["coding"], "probe_args": ["--version"],
     "timeout_seconds": 600}

``command``/``args`` must be plain tokens; they are spliced into argv without
a shell, so no value can smuggle extra flags beyond what the user configures.
"""

from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path
from typing import Any

from viva.core.errors import VivaError
from viva.core.store import atomic_json_write, read_json_object
from viva.core.paths import ensure_private_dir, viva_home

SCHEMA_VERSION = "1.0"
WORKERS_FILENAME = "workers.json"
PROBE_TIMEOUT_SECONDS = 10

# Shipped seeds are *data* (Customer Zero's common CLIs), editable per user.
DEFAULT_WORKERS: list[dict[str, Any]] = [
    {"name": "codex", "command": "codex", "args": ["exec"], "capabilities": ["coding"]},
    {"name": "claude", "command": "claude", "args": ["-p"], "capabilities": ["coding", "writing"]},
    {"name": "qodercli", "command": "qodercli", "args": [], "capabilities": ["coding"]},
    {"name": "zcode", "command": "zcode", "args": [], "capabilities": ["coding"]},
]


class WorkerError(VivaError):
    """A worker is misconfigured, unavailable, or refused."""


def _normalize(entry: Any, index: int) -> dict[str, Any]:
    if not isinstance(entry, dict):
        raise WorkerError(f"workers[{index}] must be an object")
    name = str(entry.get("name", "")).strip()
    command = str(entry.get("command", "")).strip()
    if not name or any(ch.isspace() for ch in name):
        raise WorkerError(f"workers[{index}].name must be a non-empty token without whitespace")
    if not command or any(ch.isspace() for ch in command):
        raise WorkerError(f"workers[{index}].command must be a non-empty token without whitespace")
    args = entry.get("args", [])
    if not isinstance(args, list) or not all(isinstance(arg, str) for arg in args):
        raise WorkerError(f"workers[{index}].args must be a list of strings")
    capabilities = entry.get("capabilities", [])
    if not isinstance(capabilities, list) or not all(isinstance(item, str) for item in capabilities):
        raise WorkerError(f"workers[{index}].capabilities must be a list of strings")
    probe_args = entry.get("probe_args", ["--version"])
    if not isinstance(probe_args, list) or not all(isinstance(arg, str) for arg in probe_args):
        raise WorkerError(f"workers[{index}].probe_args must be a list of strings")
    timeout = entry.get("timeout_seconds", 600)
    if isinstance(timeout, bool) or not isinstance(timeout, (int, float)) or timeout <= 0:
        raise WorkerError(f"workers[{index}].timeout_seconds must be a positive number")
    return {
        "name": name,
        "command": command,
        "args": list(args),
        "capabilities": list(capabilities),
        "probe_args": list(probe_args),
        "timeout_seconds": timeout,
    }


class WorkerRegistry:
    """Load ``~/.viva/config/workers.json`` (seeding defaults on first use)."""

    def __init__(self, home: str | Path | None = None):
        self.home = viva_home(home)
        self.path = ensure_private_dir(self.home / "config") / WORKERS_FILENAME

    def _load_raw(self) -> list[dict[str, Any]]:
        stored = read_json_object(self.path)
        if stored is None:
            payload = {
                "schema_version": SCHEMA_VERSION,
                "note": "Workers are data, not code: edit this file to match your machine.",
                "workers": DEFAULT_WORKERS,
            }
            atomic_json_write(self.path, payload)
            stored = payload
        if stored.get("schema_version") != SCHEMA_VERSION or not isinstance(
            stored.get("workers"), list
        ):
            raise WorkerError(f"worker registry is unreadable or unsupported: {self.path}")
        workers = [_normalize(entry, index) for index, entry in enumerate(stored["workers"])]
        names = [worker["name"] for worker in workers]
        if len(names) != len(set(names)):
            raise WorkerError("worker names must be unique")
        return workers

    def list_unprobed(self) -> list[dict[str, Any]]:
        return self._load_raw()

    def get(self, name: str) -> dict[str, Any]:
        needle = str(name).strip().casefold()
        for worker in self._load_raw():
            if worker["name"].casefold() == needle:
                return worker
        raise WorkerError(f"no worker named {name!r} is configured ({self.path})")

    def probe(self, worker: dict[str, Any]) -> dict[str, Any]:
        """Return an availability entry; probing degrades, never raises."""
        entry: dict[str, Any] = {
            "name": worker["name"],
            "command": worker["command"],
            "capabilities": worker.get("capabilities", []),
            "available": False,
            "reason": "",
            "version": "",
        }
        located = shutil.which(worker["command"])
        if located is None:
            entry["reason"] = f"{worker['command']} is not on PATH"
            return entry
        if not worker["probe_args"]:
            entry["available"] = True
            return entry
        try:
            proc = subprocess.run(
                [worker["command"], *worker["probe_args"]],
                capture_output=True,
                text=True,
                timeout=PROBE_TIMEOUT_SECONDS,
                stdin=subprocess.DEVNULL,
                check=False,
            )
        except (OSError, subprocess.SubprocessError) as exc:
            entry["reason"] = f"probe failed: {exc}"[:200]
            return entry
        if proc.returncode != 0:
            entry["reason"] = f"{worker['command']} {' '.join(worker['probe_args'])} exited {proc.returncode}"
            return entry
        entry["available"] = True
        entry["version"] = (proc.stdout or "").strip().splitlines()[0] if (proc.stdout or "").strip() else ""
        return entry

    def list(self) -> list[dict[str, Any]]:
        """Every configured worker with fresh availability information."""
        return [self.probe(worker) for worker in self._load_raw()]

    def export_config_path(self) -> str:
        return str(self.path)
