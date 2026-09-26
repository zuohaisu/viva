"""Safe, user-initiated, non-interactive worker invocation.

argv is built without a shell (``[command, *args, message]``), the child runs
inside the current workspace, output streams through a callback, and every
run is bounded by a timeout. Autonomous/background invocation is structurally
refused in Phase 1 (see viva.permissions).
"""

from __future__ import annotations

import queue
import subprocess
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

from viva.permissions import require_user_initiated
from viva.workers.registry import WorkerError


@dataclass
class WorkerRunResult:
    status: str  # COMPLETED | FAILED | TIMEOUT
    worker: str
    exit_code: int | None
    duration_ms: int
    output: list[str] = field(default_factory=list)


def run_worker(
    worker: dict,
    message: str,
    *,
    cwd: str | Path,
    initiated_by: str,
    timeout_seconds: float | None = None,
    on_output: Callable[[str, str], None] | None = None,
) -> WorkerRunResult:
    """Run one worker non-interactively and return an honest result.

    ``initiated_by`` must be ``"user"``: Viva does not invoke workers on its
    own initiative in Phase 1. ``on_output(stream, line)`` receives lines as
    they arrive (stream is "stdout" or "stderr").
    """
    require_user_initiated(initiated_by, action="invoke_worker")
    if not isinstance(message, str) or not message.strip():
        raise WorkerError("worker message must be a non-empty string")
    if not isinstance(worker, dict) or not worker.get("command"):
        raise WorkerError("worker record must include a command")
    workspace = Path(cwd)
    if not workspace.is_dir():
        raise WorkerError(f"working directory does not exist: {workspace}")

    argv = [str(worker["command"]), *[str(arg) for arg in worker.get("args", [])], message]
    timeout = float(timeout_seconds or worker.get("timeout_seconds", 600))
    started = time.monotonic()
    lines: list[tuple[str, str]] = []
    outputs: queue.Queue[tuple[str, str] | None] = queue.Queue()

    def pump(stream_name: str, pipe) -> None:
        try:
            for raw in iter(pipe.readline, ""):
                line = raw.rstrip("\n")
                lines.append((stream_name, line))
                outputs.put((stream_name, line))
        finally:
            outputs.put(None)

    try:
        process = subprocess.Popen(
            argv,
            cwd=str(workspace),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            stdin=subprocess.DEVNULL,
            text=True,
        )
    except OSError as exc:
        raise WorkerError(f"could not start worker {worker['name']!r}: {exc}") from exc

    threads = [
        threading.Thread(target=pump, args=("stdout", process.stdout), daemon=True),
        threading.Thread(target=pump, args=("stderr", process.stderr), daemon=True),
    ]
    for thread in threads:
        thread.start()

    timed_out = False
    finished_senders = 0
    while finished_senders < len(threads):
        remaining = timeout - (time.monotonic() - started)
        if remaining <= 0:
            timed_out = True
            break
        try:
            item = outputs.get(timeout=min(remaining, 0.5))
        except queue.Empty:
            continue
        if item is None:
            finished_senders += 1
            continue
        stream_name, line = item
        if on_output is not None:
            on_output(stream_name, line)

    if timed_out:
        process.kill()
        process.wait(timeout=10)
        return WorkerRunResult(
            status="TIMEOUT",
            worker=str(worker["name"]),
            exit_code=None,
            duration_ms=int((time.monotonic() - started) * 1000),
            output=[line for _, line in lines],
        )

    exit_code = process.wait()
    for thread in threads:
        thread.join(timeout=5)
    return WorkerRunResult(
        status="COMPLETED" if exit_code == 0 else "FAILED",
        worker=str(worker["name"]),
        exit_code=exit_code,
        duration_ms=int((time.monotonic() - started) * 1000),
        output=[line for _, line in lines],
    )
