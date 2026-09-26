"""Atomic, private JSON persistence for Viva state files.

Mirrors the proven pattern used by the delivery subsystem's local config
(temp file + fsync + ``os.replace``, 0o600 files) without importing the legacy
web module: Viva's dependency direction forbids that edge.
"""

from __future__ import annotations

import json
import os
import tempfile
from pathlib import Path
from typing import Any

from viva.core.errors import VivaError
from viva.core.paths import ensure_private_dir


def read_json_object(path: Path) -> dict[str, Any] | None:
    """Return the JSON object at *path*, or None if absent/invalid."""
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    return value if isinstance(value, dict) else None


def atomic_json_write(path: Path, payload: dict[str, Any]) -> None:
    """Atomically replace *path* with *payload*; never expose a partial file."""
    ensure_private_dir(path.parent)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=path.parent
    )
    temporary = Path(temporary_name)
    try:
        os.fchmod(descriptor, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            json.dump(payload, output, ensure_ascii=False, indent=2, sort_keys=True)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        try:
            path.chmod(0o600)
        except OSError:
            pass
    except Exception:
        try:
            temporary.unlink(missing_ok=True)
        except OSError:
            pass
        raise


def require_object(payload: Any, *, what: str) -> dict[str, Any]:
    if not isinstance(payload, dict):
        raise VivaError(f"{what} must be a JSON object")
    return payload
