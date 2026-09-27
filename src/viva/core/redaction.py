"""Secret redaction for every Viva artifact write.

Reused from the retired delivery subsystem's ``run_events.redact`` (the one
capability in that module Viva still needs). The behaviour is unchanged:
secret-looking keys are masked wholesale, and token-shaped substrings inside
free text are masked wherever they appear, so a transcript or worker message
can never persist a credential.

Viva's own modules must import this implementation — never the legacy path.
"""

from __future__ import annotations

import os
import re
import tempfile
from pathlib import Path
from typing import Any, Iterable

MASK = "********"
_SECRET_MARKERS = ("api_key", "secret", "token", "password", "credential", "authorization")
_TOKEN_PATTERN = re.compile(
    r"\b(?:gh[pousr]_[A-Za-z0-9_]{8,}|github_pat_[A-Za-z0-9_]{8,}|sk-[A-Za-z0-9_-]{8,})\b"
)


def redact(value: Any, known_secrets: Iterable[str] = ()) -> Any:
    """Return a recursively safe copy before any artifact write."""
    secrets = tuple(item for item in known_secrets if isinstance(item, str) and item)

    def clean(item: Any, key: str | None = None) -> Any:
        if key and any(marker in key.casefold() for marker in _SECRET_MARKERS):
            return MASK if item else ""
        if isinstance(item, dict):
            return {str(name): clean(child, str(name)) for name, child in item.items()}
        if isinstance(item, list):
            return [clean(child) for child in item]
        if isinstance(item, tuple):
            return [clean(child) for child in item]
        if isinstance(item, str):
            for secret in secrets:
                item = item.replace(secret, MASK)
            return _TOKEN_PATTERN.sub(MASK, item)
        return item

    return clean(value)


def redact_file(path: Path, known_secrets: Iterable[str] = ()) -> bool:
    """Redact a text artifact in place; return True when it changed.

    Worker output is written straight to a log file while the process runs, so
    it gets one redaction pass when the run finishes (or is stopped). Journal
    payloads, by contrast, are redacted before every write.
    """
    try:
        original = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return False
    cleaned = redact(original, known_secrets)
    if cleaned == original:
        return False
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=path.parent
    )
    temporary = Path(temporary_name)
    try:
        os.fchmod(descriptor, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            output.write(str(cleaned))
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    except OSError:
        temporary.unlink(missing_ok=True)
        return False
    return True
