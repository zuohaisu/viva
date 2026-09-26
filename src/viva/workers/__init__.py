"""Worker — a replaceable agent/tool CLI a resident (or the user) delegates to.

Workers are configuration data, never hard-coded identity: the shipped seeds
in ``workers.json`` are examples the user can edit freely. This registry is
the generic layer; the delivery subsystem's role-bound ``AgentCatalog``
(planner/developer/qa) is untouched and continues to serve ticket runs.
"""

from viva.workers.registry import WorkerError, WorkerRegistry
from viva.workers.runner import WorkerRunResult, run_worker

__all__ = ["WorkerError", "WorkerRegistry", "WorkerRunResult", "run_worker"]
