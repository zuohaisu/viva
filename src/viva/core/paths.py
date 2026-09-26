"""Resolve the Viva persistent state root.

All Viva state lives under one private per-user directory (``~/.viva`` by
default, overridable via ``VIVA_HOME`` for tests and exotic setups).

The historical ``~/.ticket-autopilot`` directory belongs to a retired product
(ADR 0008). Viva never reads, writes, migrates or deletes it: old run data is
evidence, not something to clean up.
"""

from __future__ import annotations

import os
from pathlib import Path

VIVA_HOME_ENV = "VIVA_HOME"
DEFAULT_HOME_NAME = ".viva"


def viva_home(override: str | Path | None = None) -> Path:
    """Return the Viva state root (explicit override > env > ~/.viva)."""
    if override is not None:
        return Path(override).expanduser()
    from_env = os.environ.get(VIVA_HOME_ENV)
    if from_env:
        return Path(from_env).expanduser()
    return Path.home() / DEFAULT_HOME_NAME


def ensure_private_dir(path: Path) -> Path:
    """Create *path* owner-only (0o700) if missing and return it."""
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    try:
        path.chmod(0o700)
    except OSError:
        pass
    return path
