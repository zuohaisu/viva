"""Executions — one run of one member on one task, with its own attribution.

An Execution is not a WorkerSession and not a Task:

    Task       the intention (survives every execution)
    Execution  one attempt (member + engine + tool + work location + authority)
    Session    the transient runtime interval a surface is open for

Several executions of the same member may run at once; they are separated by
task, work location and record, never by a shared UI flag.
"""

from viva.executions.registry import (
    FINAL_STATUSES,
    UNRESOLVED_STATUSES,
    ExecutionError,
    ExecutionRegistry,
    process_alive,
)
from viva.executions.runner import ExecutionHandle, ExecutionRunner

__all__ = [
    "FINAL_STATUSES",
    "UNRESOLVED_STATUSES",
    "ExecutionError",
    "ExecutionHandle",
    "ExecutionRegistry",
    "ExecutionRunner",
    "process_alive",
]
