"""Starting, waiting on and stopping real worker processes.

Every execution is launched as its own process group with stdout and stderr
redirected into the execution's log file. That gives Viva three things the
previous in-process design could not:

* **real concurrency** — N executions are N processes, not a boolean flag;
* **real stopping** — one signal reaches the target execution's group and no
  other execution's;
* **real recovery** — a launcher that dies (Ctrl-C, crash, reboot) leaves a
  process that either kept running or is gone; either way the record is
  reconciled honestly instead of being forgotten.

A coordinating worker gets ``VIVA_HOME``, ``VIVA_EXECUTION_ID`` and
``VIVA_GRANT_ID`` in its environment, which is what lets it call
``viva office ...`` back into Viva.
"""

from __future__ import annotations

import os
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from viva.executions.registry import (
    ExecutionError,
    ExecutionRegistry,
    process_started_marker,
    stop_process,
)
from viva.residents.registry import ResidentRegistry


@dataclass
class ExecutionHandle:
    """A live execution: its record and the process that is running it."""

    record: dict[str, Any]
    process: subprocess.Popen


class ExecutionRunner:
    """Launch and supervise worker processes for one Viva home."""

    def __init__(
        self,
        registry: ExecutionRegistry,
        *,
        viva_home_root: str | Path | None = None,
    ):
        self.registry = registry
        self.viva_home_root = Path(viva_home_root) if viva_home_root else registry.home.parent

    # -- launch --------------------------------------------------------------

    def start(
        self,
        *,
        task: dict[str, Any],
        member: dict[str, Any],
        invocation: dict[str, Any],
        message: str,
        request: dict[str, Any],
        authority: dict[str, Any] | None = None,
        execution_mode: str | None = None,
        note: str = "",
    ) -> ExecutionHandle:
        if not isinstance(message, str) or not message.strip():
            raise ExecutionError("execution message must be a non-empty string")
        work_location = dict(task.get("work_location") or {})
        # The mode belongs to the execution, not to the place: a reviewer runs
        # read-only inside the same worktree a developer may write to.
        work_location["mode"] = execution_mode or work_location.get("mode") or "read_only"
        cwd = work_location.get("path")
        if not cwd or not Path(str(cwd)).is_dir():
            raise ExecutionError(
                f"task {task.get('id')!r} has no usable work location"
                + (f": {cwd}" if cwd else " (allocate one before dispatching)")
            )
        record = self.registry.create(
            task_id=str(task["id"]),
            member_id=str(member["id"]),
            role=str(invocation.get("role") or member.get("role") or ""),
            engine=dict(invocation.get("engine") or {}),
            tool=str(invocation["tool"]),
            work_location=work_location,
            request=request,
            authority=authority,
            worker=invocation.get("worker"),
            workspace_id=task.get("workspace_id"),
            project_id=task.get("project_id"),
            note=note,
        )
        argv = ResidentRegistry.build_argv(invocation, message)
        log_path = self.registry.log_path(str(record["id"]))
        log = log_path.open("a", encoding="utf-8", errors="replace")
        try:
            os.chmod(log_path, 0o600)
        except OSError:
            pass
        env = dict(os.environ)
        env["VIVA_HOME"] = str(self.viva_home_root)
        env["VIVA_EXECUTION_ID"] = str(record["id"])
        env["VIVA_TASK_ID"] = str(task["id"])
        if authority and authority.get("grant_id"):
            env["VIVA_GRANT_ID"] = str(authority["grant_id"])
        else:
            env.pop("VIVA_GRANT_ID", None)
        try:
            process = subprocess.Popen(
                argv,
                cwd=str(cwd),
                stdout=log,
                stderr=subprocess.STDOUT,
                stdin=subprocess.DEVNULL,
                env=env,
                start_new_session=True,
            )
        except OSError as exc:
            log.close()
            self.registry.finalize(
                str(record["id"]),
                exit_code=None,
                status="failed",
                failure_reason=f"could not start {invocation['tool']!r}: {exc}",
            )
            raise ExecutionError(
                f"could not start {invocation['tool']!r} for member {member['id']!r}: {exc}"
            ) from exc
        finally:
            log.close()
        record.update(
            {
                "pid": process.pid,
                "pgid": process.pid,
                "process_started": process_started_marker(process.pid),
            }
        )
        record = self.registry.save(record)
        self.registry.record_event(  # single audit point for "this execution started"
            "execution.started",
            record,
            extra={"pid": process.pid, "note": note},
        )
        return ExecutionHandle(record=record, process=process)

    # -- observe -------------------------------------------------------------

    def wait(self, handle: ExecutionHandle, *, timeout: float | None = None) -> dict[str, Any]:
        started = time.monotonic()
        try:
            exit_code = handle.process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            return self.registry.require(str(handle.record["id"]))
        duration_ms = int((time.monotonic() - started) * 1000)
        return self.registry.finalize(
            str(handle.record["id"]), exit_code=exit_code, duration_ms=duration_ms
        )

    def stop(self, execution_id: str, *, reason: str) -> dict[str, Any]:
        """Stop exactly one execution; refuse politely when it is not running.

        The record is marked *before* the process is signalled: a supervisor
        thread that is waiting on the same process must never be able to relabel
        a deliberate stop as a failure.
        """
        record = self.registry.require(execution_id)
        if record.get("status") != "running":
            raise ExecutionError(
                f"execution {execution_id!r} is {record.get('status')!r}, not running"
            )
        alive = process_alive_checked(record)
        if alive is False:
            return self.registry.finalize(
                execution_id,
                exit_code=None,
                status="failed",
                failure_reason="process was already gone when stop was requested",
            )
        stopped = self.registry.mark_stopped(execution_id, reason=reason)
        stop_process(int(record["pid"]), record.get("pgid"))
        exit_code = handle_exit_code(int(record["pid"]))
        if exit_code is not None:
            stopped = self.registry.save({**stopped, "exit_code": exit_code})
        return stopped


def process_alive_checked(record: dict[str, Any]) -> bool | None:
    from viva.executions.registry import process_alive

    return process_alive(record.get("pid"), record.get("process_started"))


def handle_exit_code(pid: int) -> int | None:  # pragma: no cover - signal detail
    """Exit status of a reaped child, when this process is its parent."""
    try:
        _, status = os.waitpid(pid, os.WNOHANG)
    except (ChildProcessError, OSError):
        return None
    if status == 0:
        return None
    if os.WIFSIGNALED(status):
        return -os.WTERMSIG(status)
    return os.WEXITSTATUS(status)
