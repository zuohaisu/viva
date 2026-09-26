"""Cognitive engines are replaceable organs, not the member.

An **engine** is a model served through an execution tool:

    {"id": "claude-opus", "tool": "claude", "model": "opus",
     "model_flag": "--model", "args": []}

* ``tool`` must name a configured worker (an execution tool: a CLI on PATH);
* ``model`` is passed through ``model_flag`` when both are set;
* ``args`` are extra tool-specific arguments.

The catalogue is configuration data in ``~/.viva/config/engines.json``.
Changing a member's engine rewrites one binding; it never touches the member's
record, history or knowledge.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from viva.core.errors import VivaError
from viva.core.ids import utc_now
from viva.core.paths import ensure_private_dir, viva_home
from viva.core.store import atomic_json_write, read_json_object

SCHEMA_VERSION = "1.0"
ENGINES_FILENAME = "engines.json"

DEFAULT_ENGINES: list[dict[str, Any]] = [
    {
        "id": "claude-default",
        "label": "Claude Code (tool default model)",
        "tool": "claude",
        "model": None,
        "model_flag": "--model",
        "args": [],
    },
    {
        "id": "qoder-default",
        "label": "Qoder CLI (tool default model)",
        "tool": "qodercli",
        "model": None,
        "model_flag": None,
        "args": [],
    },
    {
        "id": "codex-default",
        "label": "Codex exec (tool default model)",
        "tool": "codex",
        "model": None,
        "model_flag": "--model",
        "args": [],
    },
]


class EngineError(VivaError):
    """An engine is unknown or its definition is unusable."""


def _normalize(entry: Any, index: int) -> dict[str, Any]:
    if not isinstance(entry, dict):
        raise EngineError(f"engines[{index}] must be an object")
    engine_id = str(entry.get("id", "")).strip()
    tool = str(entry.get("tool", "")).strip()
    if not engine_id or any(ch.isspace() for ch in engine_id):
        raise EngineError(f"engines[{index}].id must be a non-empty token")
    if not tool or any(ch.isspace() for ch in tool):
        raise EngineError(f"engines[{index}].tool must name a configured worker")
    model = entry.get("model")
    if model is not None and not isinstance(model, str):
        raise EngineError(f"engines[{index}].model must be a string or null")
    model_flag = entry.get("model_flag")
    if model_flag is not None and not isinstance(model_flag, str):
        raise EngineError(f"engines[{index}].model_flag must be a string or null")
    args = entry.get("args", [])
    if not isinstance(args, list) or not all(isinstance(arg, str) for arg in args):
        raise EngineError(f"engines[{index}].args must be a list of strings")
    return {
        "id": engine_id,
        "label": str(entry.get("label") or engine_id),
        "tool": tool,
        "model": model,
        "model_flag": model_flag,
        "args": list(args),
    }


class EngineCatalog:
    """Load ``~/.viva/config/engines.json`` (seeding defaults on first use)."""

    def __init__(self, home: str | Path | None = None):
        self.home = viva_home(home)
        self.path = ensure_private_dir(self.home / "config") / ENGINES_FILENAME

    def _load(self) -> list[dict[str, Any]]:
        stored = read_json_object(self.path)
        if stored is None:
            payload = {
                "schema_version": SCHEMA_VERSION,
                "note": (
                    "Engines are replaceable cognitive bindings: model + the tool "
                    "that serves it. Edit to match the models you actually use."
                ),
                "engines": DEFAULT_ENGINES,
            }
            atomic_json_write(self.path, payload)
            stored = payload
        if stored.get("schema_version") != SCHEMA_VERSION or not isinstance(
            stored.get("engines"), list
        ):
            raise EngineError(f"engine catalogue is unreadable or unsupported: {self.path}")
        engines = [_normalize(entry, index) for index, entry in enumerate(stored["engines"])]
        ids = [engine["id"] for engine in engines]
        if len(ids) != len(set(ids)):
            raise EngineError("engine ids must be unique")
        return engines

    def list(self) -> list[dict[str, Any]]:
        return self._load()

    def get(self, engine_id: str) -> dict[str, Any]:
        needle = str(engine_id).strip().casefold()
        for engine in self._load():
            if engine["id"].casefold() == needle:
                return engine
        raise EngineError(
            f"unknown engine {engine_id!r}; configured engines: "
            f"{[engine['id'] for engine in self._load()]}"
        )

    def as_binding(self, engine_id: str, *, model: str | None = None) -> dict[str, Any]:
        """The binding stored on a member: engine id + concrete model + time."""
        engine = self.get(engine_id)
        return {
            "id": engine["id"],
            "tool": engine["tool"],
            "model": model if model is not None else engine["model"],
            "configured_at": utc_now(),
        }

    def export_config_path(self) -> str:
        return str(self.path)
