"""The Experience journal: one append-only, secret-redacted JSONL stream.

Event types used by Viva (open set; readers must tolerate unknown types):

    viva.started, session.started, resident.created, resident.loaded,
    resident.selected, workspace.registered, workspace.selected,
    worktree.observed, worker.invoked, worker.completed, worker.failed,
    user.command, runtime.state_changed

Every event carries: id, sequence, timestamp, schema_version, resident_id,
session_id, event_type, workspace, source, payload. Writes reuse the delivery
subsystem's proven ``redact`` so secrets never enter the journal.
"""

from __future__ import annotations

import json
import threading
from pathlib import Path
from typing import Any, Iterable

from ticket_autopilot.services.run_events import redact
from viva.core.errors import VivaError
from viva.core.ids import new_event_id, utc_now
from viva.core.paths import ensure_private_dir, viva_home

SCHEMA_VERSION = "1.0"
JOURNAL_FILENAME = "journal.jsonl"
_LOCK = threading.RLock()


class ExperienceError(VivaError):
    """The experience journal cannot be safely appended to or read."""


class ExperienceJournal:
    """Append-only event stream at ``~/.viva/experiences/journal.jsonl``."""

    def __init__(self, home: str | Path | None = None, *, known_secrets: Iterable[str] = ()):
        self.home = viva_home(home)
        self.path = ensure_private_dir(self.home / "experiences") / JOURNAL_FILENAME
        self.known_secrets = tuple(
            item for item in known_secrets if isinstance(item, str) and item
        )

    def append(
        self,
        *,
        event_type: str,
        resident_id: str | None = None,
        session_id: str | None = None,
        workspace: str | None = None,
        source: str = "viva",
        payload: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        if not isinstance(event_type, str) or not event_type.strip():
            raise ExperienceError("event_type must be a non-empty string")
        if payload is not None and not isinstance(payload, dict):
            raise ExperienceError("payload must be an object")
        with _LOCK:
            event = redact(
                {
                    "schema_version": SCHEMA_VERSION,
                    "id": new_event_id(),
                    "sequence": len(self.read()) + 1,
                    "timestamp": utc_now(),
                    "resident_id": resident_id,
                    "session_id": session_id,
                    "event_type": event_type.strip(),
                    "workspace": workspace,
                    "source": source,
                    "payload": payload or {},
                },
                self.known_secrets,
            )
            try:
                with self.path.open("a", encoding="utf-8") as output:
                    output.write(json.dumps(event, ensure_ascii=False, sort_keys=True) + "\n")
                    output.flush()
            except OSError as exc:
                raise ExperienceError(f"experience append failed: {exc}") from exc
            return event

    def read(self) -> list[dict[str, Any]]:
        if not self.path.exists():
            return []
        try:
            lines = self.path.read_text(encoding="utf-8").splitlines()
        except OSError as exc:
            raise ExperienceError("experience journal is unreadable") from exc
        events: list[dict[str, Any]] = []
        for line_number, line in enumerate(lines, start=1):
            try:
                event = json.loads(line)
            except json.JSONDecodeError as exc:
                raise ExperienceError(
                    f"experience journal is malformed at line {line_number}"
                ) from exc
            if not isinstance(event, dict):
                raise ExperienceError(
                    f"experience journal is invalid at line {line_number}: event must be an object"
                )
            expected = line_number
            if event.get("sequence") != expected:
                raise ExperienceError(
                    f"experience journal is invalid at line {line_number}: "
                    f"expected sequence {expected}, found {event.get('sequence')!r}"
                )
            events.append(event)
        return events

    def tail(self, limit: int = 20) -> list[dict[str, Any]]:
        if limit <= 0:
            return []
        return self.read()[-limit:]
