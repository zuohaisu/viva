"""Execution records: who ran what, where, with which model, and under what authority.

One JSON file per execution under ``~/.viva/executions/``; raw worker output in
``~/.viva/executions/logs/<id>.log`` (0o600, redacted when the run finalises).

An execution record is the answer to "what is actually running right now":
it fixes its own attribution at launch — member, task, role, engine/model,
tool, work location, and the request that caused it — so switching the UI's
selected member or workspace later cannot rewrite history.

Concurrency is a property of this registry, not of a UI flag: many executions
run at once, each with its own process id and process group, and stopping one
never touches another.

Recovery is deliberately read-only. :meth:`ExecutionRegistry.reconcile`
compares what the records claim against what the operating system says and
*never* starts anything: a completed run stays completed, and an orphaned run
is reported honestly as exited or unknown instead of being silently re-run.
"""

from __future__ import annotations

import json
import os
import signal
import subprocess
import threading
import time
from pathlib import Path
from typing import Any, Callable

from viva.core.errors import VivaError
from viva.core.ids import new_execution_id, utc_now
from viva.core.paths import ensure_private_dir, viva_home
from viva.core.redaction import redact, redact_file
from viva.core.store import atomic_json_write, read_json_object

SCHEMA_VERSION = "1.0"
FINAL_STATUSES = frozenset({"completed", "failed", "stopped"})
UNRESOLVED_STATUSES = frozenset({"exited", "unknown"})
STOP_GRACE_SECONDS = 5.0
_LOCK = threading.RLock()


class ExecutionError(VivaError):
    """An execution record or process could not be handled safely."""


def process_alive(pid: int | None, started_marker: str | None = None) -> bool | None:
    """Is *pid* alive? ``None`` means "cannot tell" — never guess.

    ``started_marker`` is the ``ps -o lstart=`` string captured at launch: if
    the current process with that pid started at a different time, the pid has
    been reused and the original process is gone.
    """
    if not pid:
        return None
    try:
        proc = subprocess.run(
            ["ps", "-o", "lstart=", "-p", str(pid)],
            capture_output=True,
            text=True,
        )
    except OSError:
        return None
    if proc.returncode != 0 or not proc.stdout.strip():
        return False
    current = " ".join(proc.stdout.split())
    if started_marker and current != started_marker:
        return False
    return True


def process_started_marker(pid: int) -> str | None:
    try:
        proc = subprocess.run(
            ["ps", "-o", "lstart=", "-p", str(pid)], capture_output=True, text=True
        )
    except OSError:
        return None
    if proc.returncode != 0 or not proc.stdout.strip():
        return None
    return " ".join(proc.stdout.split())


class ExecutionRegistry:
    """Persist and reconcile every execution Viva has started."""

    def __init__(self, home: str | Path | None = None, *, journal: Callable[..., Any] | None = None):
        self.home = ensure_private_dir(viva_home(home) / "executions")
        self.logs = ensure_private_dir(self.home / "logs")
        self._journal = journal

    # -- paths ---------------------------------------------------------------

    def _path(self, execution_id: str) -> Path:
        return self.home / f"{execution_id}.json"

    def log_path(self, execution_id: str) -> Path:
        return self.logs / f"{execution_id}.log"

    # -- writes --------------------------------------------------------------

    def create(
        self,
        *,
        task_id: str,
        member_id: str,
        role: str,
        engine: dict[str, Any],
        tool: str,
        work_location: dict[str, Any],
        request: dict[str, Any],
        worker: dict[str, Any] | None = None,
        authority: dict[str, Any] | None = None,
        workspace_id: str | None = None,
        project_id: str | None = None,
        note: str = "",
    ) -> dict[str, Any]:
        if not isinstance(request, dict) or request.get("kind") not in {"user", "worker"}:
            raise ExecutionError("an execution requires an explicit request source")
        execution_id = new_execution_id()
        record = {
            "schema_version": SCHEMA_VERSION,
            "id": execution_id,
            "task_id": str(task_id),
            "workspace_id": workspace_id,
            "project_id": project_id,
            "member_id": str(member_id),
            "role": str(role),
            "engine": dict(engine),
            "tool": str(tool),
            "worker": {
                "command": (worker or {}).get("command"),
                "args": list((worker or {}).get("args") or []),
            },
            "work_location": dict(work_location),
            "request": dict(request),
            "authority": dict(authority) if authority else None,
            "note": str(note),
            "status": "running",
            "pid": None,
            "process_started": None,
            "pgid": None,
            "started_at": utc_now(),
            "finished_at": None,
            "exit_code": None,
            "duration_ms": None,
            "output_path": str(self.log_path(execution_id)),
            "output_bytes": 0,
            "summary": None,
            "failure_reason": None,
            "recovered_at": None,
            "recoverable": False,
        }
        self.save(record)
        return record

    def save(self, record: dict[str, Any]) -> dict[str, Any]:
        record = redact(dict(record))
        with _LOCK:
            atomic_json_write(self._path(str(record["id"])), record)
        return record

    def get(self, execution_id: str) -> dict[str, Any] | None:
        needle = str(execution_id).strip()
        if not needle:
            return None
        record = read_json_object(self._path(needle))
        if record is None:
            return None
        if record.get("schema_version") != SCHEMA_VERSION:
            raise ExecutionError(
                f"execution {needle!r} has unsupported schema_version "
                f"{record.get('schema_version')!r}"
            )
        return record

    def require(self, execution_id: str) -> dict[str, Any]:
        record = self.get(execution_id)
        if record is None:
            raise ExecutionError(f"no execution {execution_id!r} — run: viva office execs")
        return record

    def list(
        self, *, task_id: str | None = None, status: str | None = None, member_id: str | None = None
    ) -> list[dict[str, Any]]:
        records = []
        for path in sorted(self.home.glob("*.json")):
            record = self.get(path.stem)
            if record is not None:
                records.append(record)
        if task_id is not None:
            records = [record for record in records if record.get("task_id") == task_id]
        if status is not None:
            records = [record for record in records if record.get("status") == status]
        if member_id is not None:
            records = [record for record in records if record.get("member_id") == member_id]
        records.sort(key=lambda record: (str(record.get("started_at", "")), str(record.get("id", ""))))
        return records

    def running(self, *, member_id: str | None = None, task_id: str | None = None) -> list[dict[str, Any]]:
        return self.list(status="running", member_id=member_id, task_id=task_id)

    # -- finalisation --------------------------------------------------------

    def finalize(
        self,
        execution_id: str,
        *,
        exit_code: int | None,
        status: str | None = None,
        summary: str | None = None,
        failure_reason: str | None = None,
        duration_ms: int | None = None,
    ) -> dict[str, Any]:
        """Record the outcome once. A stopped execution is never overwritten."""
        record = self.require(execution_id)
        if record["status"] in FINAL_STATUSES:
            return record
        resolved = status or ("completed" if exit_code == 0 else "failed")
        if resolved not in {"completed", "failed"}:
            raise ExecutionError(f"finalize status must be completed or failed, got {resolved!r}")
        record.update(
            {
                "status": resolved,
                "exit_code": exit_code,
                "finished_at": utc_now(),
                "duration_ms": duration_ms,
                "summary": summary if summary is not None else record.get("summary"),
                "failure_reason": failure_reason,
            }
        )
        self.save(record)
        redact_file(self.log_path(execution_id))
        self.record_event(
            "execution.completed" if resolved == "completed" else "execution.failed",
            record,
            extra={"exit_code": exit_code, "duration_ms": duration_ms},
        )
        return record

    def mark_stopped(self, execution_id: str, *, reason: str, exit_code: int | None = None) -> dict[str, Any]:
        record = self.require(execution_id)
        if record["status"] in FINAL_STATUSES:
            return record
        record.update(
            {
                "status": "stopped",
                "exit_code": exit_code,
                "finished_at": utc_now(),
                "failure_reason": str(reason),
            }
        )
        self.save(record)
        redact_file(self.log_path(execution_id))
        self.record_event("execution.stopped", record, extra={"reason": reason})
        return record

    # -- recovery (read-only) ------------------------------------------------

    def reconcile(self) -> dict[str, Any]:
        """Compare records against the OS. Never starts or restarts anything.

        ``exited`` and ``unknown`` list what this pass *newly* classified, so
        running the reconciliation twice reports the same thing and changes
        nothing. ``recoverable`` lists every execution that ended without a
        successful result and can therefore be resumed by a new attempt.
        """
        report: dict[str, Any] = {
            "running": [],
            "exited": [],
            "unknown": [],
            "recoverable": [],
            "resolved_at": utc_now(),
        }
        for record in self.list():
            status = record.get("status")
            transitioned = False
            if status == "running":
                alive = process_alive(record.get("pid"), record.get("process_started"))
                if alive is True:
                    report["running"].append(record["id"])
                    continue
                record["status"] = "exited" if alive is False else "unknown"
                record["recovered_at"] = utc_now()
                record["failure_reason"] = (
                    "process ended without a recorded result (Viva was not running or was "
                    "interrupted); outcome unknown — inspect the output log"
                    if alive is False
                    else "no process id was recorded; cannot tell whether this run is alive"
                )
                self.save(record)
                status = record["status"]
                transitioned = True
            if transitioned:
                report["exited" if status == "exited" else "unknown"].append(record["id"])
            if status in UNRESOLVED_STATUSES or status in {"stopped", "failed"}:
                record["recoverable"] = True
                self.save(record)
                report["recoverable"].append(record["id"])
        return report

    # -- results -------------------------------------------------------------

    def read_result(self, execution_id: str) -> dict[str, Any]:
        record = self.require(execution_id)
        log = self.log_path(execution_id)
        lines: list[str] = []
        if log.exists():
            try:
                lines = redact(log.read_text(encoding="utf-8", errors="replace")).splitlines()
            except OSError as exc:
                raise ExecutionError(f"execution output is unreadable: {exc}") from exc
        return {
            **record,
            "output_lines": len(lines),
            "output_tail": lines[-40:],
        }

    # -- internals -----------------------------------------------------------

    def record_event(
        self, event_type: str, record: dict[str, Any], *, extra: dict[str, Any] | None = None
    ) -> None:
        """Emit an event from the *record* — never from the caller's live context."""
        if self._journal is None:
            return
        payload = {
            "execution_id": record["id"],
            "task_id": record["task_id"],
            "member_id": record["member_id"],
            "role": record["role"],
            "worker": record["tool"],
            "engine_id": (record.get("engine") or {}).get("id"),
            "model": (record.get("engine") or {}).get("model"),
            "work_location": (record.get("work_location") or {}).get("path"),
            "mode": (record.get("work_location") or {}).get("mode"),
            "requested_by": record.get("request"),
            "authority": record.get("authority"),
        }
        payload.update(extra or {})
        self._journal(
            event_type=event_type,
            resident_id=record["member_id"],
            workspace=(record.get("workspace_id") or None),
            source="viva",
            payload=payload,
        )


def stop_process(pid: int, pgid: int | None, *, grace: float = STOP_GRACE_SECONDS) -> None:
    """Terminate one execution's process group, then insist."""
    target = pgid or pid
    try:
        os.killpg(target, signal.SIGTERM)
    except (ProcessLookupError, PermissionError, OSError):
        try:
            os.kill(pid, signal.SIGTERM)
        except (ProcessLookupError, PermissionError, OSError):
            return
    deadline = time.monotonic() + grace
    while time.monotonic() < deadline:
        if process_alive(pid) is not True:
            return
        time.sleep(0.1)
    try:
        os.killpg(target, signal.SIGKILL)
    except (ProcessLookupError, PermissionError, OSError):
        try:
            os.kill(pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError, OSError):
            pass
