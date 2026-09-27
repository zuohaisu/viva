"""Task — one piece of intended work, owned by a workspace.

A task is the *intention* anchor: it can exist before a worktree does
(planning), it can span several executions (a retry, a second opinion), and it
can be assigned to one or more AI members. It is not a worktree attribute and
not a worker session.

Durable by construction:

* the intent survives any worker session dying;
* every execution that served it is recorded on the task, so a replacement
  worker can be briefed with what was already tried and why it failed;
* outputs and unfinished items are listed explicitly, which is what makes
  after-restart recovery honest rather than optimistic.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any, Callable

from viva.core.errors import VivaError
from viva.core.ids import new_task_id, utc_now
from viva.core.paths import ensure_private_dir, viva_home
from viva.core.store import atomic_json_write, read_json_object
from viva.worktrees.location import READ_ONLY_KINDS, WRITABLE_KINDS

SCHEMA_VERSION = "1.0"
TASK_KINDS = sorted(WRITABLE_KINDS | READ_ONLY_KINDS)
STATUSES = (
    "todo",
    "in_progress",
    "in_review",
    "blocked",
    "done",
    "stopped",
)
OPEN_STATUSES = frozenset({"todo", "in_progress", "in_review", "blocked"})


class TaskError(VivaError):
    """A task could not be created, found, or updated."""


class TaskRegistry:
    """One JSON file per task under ``~/.viva/tasks/``."""

    def __init__(self, home: str | Path | None = None, *, journal: Callable[..., Any] | None = None):
        self.home = ensure_private_dir(viva_home(home) / "tasks")
        self._journal = journal

    # -- paths ---------------------------------------------------------------

    def _path(self, task_id: str) -> Path:
        return self.home / f"{task_id}.json"

    # -- writes --------------------------------------------------------------

    def create(
        self,
        *,
        title: str,
        intent: str = "",
        kind: str = "delivery",
        workspace_id: str | None = None,
        project_id: str | None = None,
        repositories: list[str] | None = None,
        assignees: list[str] | None = None,
        constraints: list[str] | None = None,
        created_by: dict[str, Any] | None = None,
        task_id: str | None = None,
    ) -> dict[str, Any]:
        title = str(title).strip()
        if not title:
            raise TaskError("task title must be a non-empty string")
        kind = str(kind).strip().casefold()
        if kind not in TASK_KINDS:
            raise TaskError(f"unknown task kind {kind!r}: expected one of {TASK_KINDS}")
        identifier = task_id or new_task_id(title)
        if self._path(identifier).exists():
            raise TaskError(f"task {identifier!r} already exists")
        now = utc_now()
        record = {
            "schema_version": SCHEMA_VERSION,
            "id": identifier,
            "title": title,
            "intent": str(intent).strip(),
            "kind": kind,
            "status": "todo",
            "workspace_id": workspace_id,
            "project_id": project_id,
            "repositories": [str(Path(item).expanduser().resolve()) for item in (repositories or [])],
            "work_location": {},
            "assignees": [str(item).strip() for item in (assignees or []) if str(item).strip()],
            "constraints": [str(item).strip() for item in (constraints or []) if str(item).strip()],
            "created_by": created_by or {"kind": "user", "id": "user"},
            "created_at": now,
            "updated_at": now,
            "github": {},
            "executions": [],
            "outputs": [],
            "unfinished": [],
        }
        atomic_json_write(self._path(identifier), record)
        self._event(
            "task.created",
            payload={
                "task_id": identifier,
                "title": title,
                "kind": kind,
                "workspace_id": workspace_id,
                "project_id": project_id,
                "assignees": record["assignees"],
                "requested_by": record["created_by"],
            },
        )
        return record

    def update(self, task_id: str, **fields: Any) -> dict[str, Any]:
        record = self.require(task_id)
        record.update(fields)
        record["updated_at"] = utc_now()
        atomic_json_write(self._path(record["id"]), record)
        return record

    def set_status(self, task_id: str, status: str) -> dict[str, Any]:
        status = str(status).strip().casefold()
        if status not in STATUSES:
            raise TaskError(f"unknown task status {status!r}: expected one of {list(STATUSES)}")
        record = self.require(task_id)
        previous = record["status"]
        record["status"] = status
        record["updated_at"] = utc_now()
        atomic_json_write(self._path(record["id"]), record)
        self._event(
            "task.status_changed",
            payload={"task_id": record["id"], "from": previous, "to": status},
        )
        return record

    def assign(self, task_id: str, member_id: str) -> dict[str, Any]:
        record = self.require(task_id)
        member_id = str(member_id).strip()
        if not member_id:
            raise TaskError("assignee must be a non-empty member id")
        if member_id not in record["assignees"]:
            record["assignees"].append(member_id)
            record["updated_at"] = utc_now()
            atomic_json_write(self._path(record["id"]), record)
            self._event(
                "task.assigned",
                payload={"task_id": record["id"], "member_id": member_id},
            )
        return record

    def set_work_location(self, task_id: str, location: dict[str, Any]) -> dict[str, Any]:
        record = self.update(task_id, work_location=dict(location))
        return record

    def record_execution(self, task_id: str, execution: dict[str, Any]) -> dict[str, Any]:
        """Attach one execution to the task, with the fields a handoff needs."""
        record = self.require(task_id)
        entry = {
            "execution_id": execution["id"],
            "member_id": execution.get("member_id"),
            "role": execution.get("role"),
            "worker": execution.get("tool"),
            "engine": execution.get("engine"),
            "mode": (execution.get("work_location") or {}).get("mode"),
            "requested_by": execution.get("request"),
            "status": execution.get("status"),
            "started_at": execution.get("started_at"),
            "finished_at": execution.get("finished_at"),
            "summary": execution.get("summary"),
            "failure_reason": execution.get("failure_reason"),
        }
        record["executions"] = [item for item in record["executions"] if item.get("execution_id") != entry["execution_id"]]
        record["executions"].append(entry)
        record["updated_at"] = utc_now()
        atomic_json_write(self._path(record["id"]), record)
        return record

    def record_output(
        self, task_id: str, *, execution_id: str, summary: str, artifacts: list[str] | None = None
    ) -> dict[str, Any]:
        record = self.require(task_id)
        record["outputs"].append(
            {
                "execution_id": execution_id,
                "summary": str(summary),
                "artifacts": [str(item) for item in (artifacts or [])],
                "recorded_at": utc_now(),
            }
        )
        record["updated_at"] = utc_now()
        atomic_json_write(self._path(record["id"]), record)
        self._event(
            "task.output_recorded",
            payload={"task_id": record["id"], "execution_id": execution_id, "summary": summary},
        )
        return record

    def set_unfinished(self, task_id: str, items: list[str]) -> dict[str, Any]:
        return self.update(task_id, unfinished=[str(item) for item in items if str(item).strip()])

    def link_github(
        self, task_id: str, *, repo: str | None = None, issue: int | None = None,
        branch: str | None = None, pr: int | None = None,
    ) -> dict[str, Any]:
        record = self.require(task_id)
        github = dict(record.get("github") or {})
        for key, value in (("repo", repo), ("issue", issue), ("branch", branch), ("pr", pr)):
            if value is not None:
                github[key] = value
        record["github"] = github
        record["updated_at"] = utc_now()
        atomic_json_write(self._path(record["id"]), record)
        self._event("task.github_linked", payload={"task_id": record["id"], **github})
        return record

    # -- reads ---------------------------------------------------------------

    def get(self, task_id: str) -> dict[str, Any] | None:
        needle = str(task_id).strip()
        if not needle:
            return None
        record = read_json_object(self._path(needle))
        if record is None:
            return None
        if record.get("schema_version") != SCHEMA_VERSION:
            raise TaskError(
                f"task {needle!r} has unsupported schema_version {record.get('schema_version')!r}"
            )
        return record

    def require(self, task_id: str) -> dict[str, Any]:
        record = self.get(task_id)
        if record is None:
            raise TaskError(f"no task {task_id!r} — run: viva task list")
        return record

    def list(
        self, *, status: str | None = None, workspace_id: str | None = None, open_only: bool = False
    ) -> list[dict[str, Any]]:
        tasks = []
        for path in sorted(self.home.glob("*.json")):
            record = self.get(path.stem)
            if record is not None:
                tasks.append(record)
        if status is not None:
            tasks = [task for task in tasks if task.get("status") == status]
        if workspace_id is not None:
            tasks = [task for task in tasks if task.get("workspace_id") == workspace_id]
        if open_only:
            tasks = [task for task in tasks if task.get("status") in OPEN_STATUSES]
        tasks.sort(key=lambda task: (str(task.get("created_at", "")), str(task.get("id", ""))))
        return tasks

    def unfinished(self) -> list[dict[str, Any]]:
        return [task for task in self.list() if task.get("unfinished")]

    # -- internals -----------------------------------------------------------

    def _event(self, event_type: str, *, payload: dict[str, Any]) -> None:
        if self._journal is None:
            return
        workspace = payload.get("workspace_id")
        self._journal(event_type=event_type, workspace=workspace, source="viva", payload=payload)
