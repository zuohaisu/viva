"""Worker — a replaceable execution tool (an agent CLI a member drives).

Workers are configuration data, never hard-coded identity: the shipped seeds
in ``workers.json`` are examples the user can edit freely.

A Worker is not a member and not a model:
``member -> engine (model) -> tool (worker record)``. Execution is handled by
:mod:`viva.executions`, which records attribution and authority per run.
"""

from viva.workers.registry import WorkerError, WorkerRegistry

__all__ = ["WorkerError", "WorkerRegistry"]
