"""The AI Office control surface.

This module is the part a *coordinating worker* actually calls. A worker is a
subprocess; the only honest way for it to reach Viva is a real command, so the
operations below are implemented once and exposed through the CLI
(``viva office dispatch|status|result|stop``), which a worker invokes from its
shell exactly like a human would.

Two rules make this safe rather than theatrical:

* a worker-originated dispatch must spend a **grant** Haisu created; it is
  refused (and the refusal recorded) otherwise, and it is never recorded as if
  Haisu had asked;
* nothing is a fixed pipeline. Any member may be dispatched to any task whose
  role allows the work mode — the coordination graph is what the grant and the
  task actually are, not a hard-coded dev→QA chain.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from viva.core.errors import VivaError
from viva.executions.registry import ExecutionError, ExecutionRegistry
from viva.executions.runner import ExecutionRunner
from viva.permissions import require_invocation_authority


class OfficeError(VivaError):
    """An office operation could not be completed safely."""


class Office:
    """Dispatch, observe, read and stop executions across AI members."""

    def __init__(self, context: Any):
        self.context = context
        self.home = context.home

    # -- authority -----------------------------------------------------------

    def grant(
        self,
        *,
        member: str,
        task: str,
        actions: list[str],
        mode_max: str,
        reason: str,
        delegated_from: str | None = None,
        source: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Create a grant. Only the operator, or a worker delegating from a parent."""
        member_record = self.context.residents.require(member)
        task_record = self.context.tasks.require(task)
        resolved_source = dict(source or self.context.user_source())
        return self.context.grants.create(
            source=resolved_source,
            grantee=str(member_record["id"]),
            task_id=str(task_record["id"]),
            actions=actions,
            mode_max=mode_max,
            reason=reason,
            delegated_from=delegated_from,
        )

    def _authorize(
        self,
        *,
        action: str,
        task_id: str,
        grant_id: str | None,
        origin_execution: str | None,
        mode: str | None,
    ) -> dict[str, Any]:
        """Resolve (and validate) the request source for one delegated action."""
        grant = None
        if grant_id:
            if not origin_execution:
                raise OfficeError(
                    f"a granted {action} must name the execution it came from "
                    "(set VIVA_EXECUTION_ID or pass --origin-execution); Viva will not "
                    "record a worker request as if the operator had issued it"
                )
            grant = self.context.grants.check(
                grant_id, action=action, task_id=task_id, mode=mode
            )
        source = {"kind": "worker", "id": origin_execution} if origin_execution else dict(self.context.user_source())
        return require_invocation_authority(source, action=f"{action}_delegated", grant=grant)

    # -- dispatch ------------------------------------------------------------

    def dispatch(
        self,
        *,
        task: str,
        member: str,
        mode: str | None = None,
        tool: str | None = None,
        message: str | None = None,
        note: str = "",
        grant_id: str | None = None,
        origin_execution: str | None = None,
    ) -> dict[str, Any]:
        """Start one real execution of *member* on *task*."""
        task_record = self.context.tasks.require(task)
        member_record = self.context.residents.require(member)
        request = self._authorize(
            action="dispatch",
            task_id=str(task_record["id"]),
            grant_id=grant_id,
            origin_execution=origin_execution,
            mode=mode,
        )
        invocation = self.context.residents.resolve_invocation(
            member_record, mode=mode, tool=tool, workers=self.context.workers
        )
        work_location = self._work_location(task_record, mode=invocation["mode"])
        task_record = self.context.tasks.set_work_location(str(task_record["id"]), work_location)
        task_record = self.context.tasks.assign(str(task_record["id"]), str(member_record["id"]))
        prompt = message or self.brief_text(
            str(task_record["id"]), coordinator=invocation["role"] == "coordinator"
        )
        if note and not message:
            prompt = f"{prompt}\n\n## Note from the requester\n\n{note}"
        authority = self.authority_for(
            member_id=str(member_record["id"]),
            task_id=str(task_record["id"]),
            mode=invocation["mode"],
        )
        handle = self.context.executions_runner.start(
            task=task_record,
            member=member_record,
            invocation=invocation,
            message=prompt,
            request=request,
            authority=authority,
            execution_mode=invocation["mode"],
            note=note,
        )
        self.context.remember_handle(handle)
        self.context.tasks.record_execution(
            str(task_record["id"]), handle.record
        )
        self.context.tasks.set_status(str(task_record["id"]), "in_progress")
        self.context.journal.append(
            event_type="office.dispatched",
            resident_id=str(member_record["id"]),
            session_id=self.context.current_session_id(),
            workspace=task_record.get("workspace_id"),
            source="viva",
            payload={
                "execution_id": handle.record["id"],
                "task_id": task_record["id"],
                "member_id": member_record["id"],
                "role": invocation["role"],
                "worker": invocation["tool"],
                "engine_id": invocation["engine"]["id"],
                "model": invocation["engine"]["model"],
                "mode": invocation["mode"],
                "work_location": work_location.get("path"),
                "requested_by": request,
                "authority": authority,
                "note": note,
            },
        )
        return handle.record

    def authority_for(
        self, *, member_id: str, task_id: str, mode: str
    ) -> dict[str, Any] | None:
        """The grant an execution of *member_id* on *task_id* runs with.

        A member's authority is not implicit: it is whatever Haisu granted that
        member for that task. The grant travels with the execution (so a
        coordinator can act on it) and is recorded on the execution record, so
        the scope a run could spend is always inspectable afterwards.
        """
        from viva.permissions import MODE_ORDER

        candidates = [
            grant
            for grant in self.context.grants.list()
            if grant.get("grantee") == str(member_id)
            and str(grant.get("task_id")) == str(task_id)
            and not grant.get("revoked_at")
            and MODE_ORDER.get(str(grant.get("mode_max")), -1) >= MODE_ORDER[mode]
        ]
        if not candidates:
            return None
        candidates.sort(key=lambda grant: str(grant.get("recorded_at", "")))
        grant = candidates[-1]
        return {
            "grant_id": grant["id"],
            "grantee": grant["grantee"],
            "task_id": grant["task_id"],
            "actions": list(grant["actions"]),
            "mode_max": grant["mode_max"],
            "source": dict(grant["source"]),
            "delegated_from": grant.get("delegated_from"),
        }

    # -- observe -------------------------------------------------------------

    def status(self, *, task: str | None = None, live: bool = True) -> dict[str, Any]:
        """Tasks, executions and member bindings — with honest run states."""
        task_records = (
            [self.context.tasks.require(task)] if task else self.context.tasks.list()
        )
        execution_records = self.context.executions.list(
            task_id=str(task) if task else None
        )
        if live:
            execution_records = [self._live_status(record) for record in execution_records]
        counts: dict[str, int] = {}
        for record in execution_records:
            counts[record["status"]] = counts.get(record["status"], 0) + 1
        return {
            "tasks": [
                {
                    "id": record["id"],
                    "title": record["title"],
                    "kind": record["kind"],
                    "status": record["status"],
                    "assignees": record["assignees"],
                    "work_location": record.get("work_location") or {},
                    "unfinished": record.get("unfinished") or [],
                }
                for record in task_records
            ],
            "executions": [
                {
                    "id": record["id"],
                    "task_id": record["task_id"],
                    "member_id": record["member_id"],
                    "role": record["role"],
                    "worker": record["tool"],
                    "engine": record.get("engine"),
                    "mode": (record.get("work_location") or {}).get("mode"),
                    "status": record["status"],
                    "pid": record.get("pid"),
                    "started_at": record.get("started_at"),
                    "finished_at": record.get("finished_at"),
                    "requested_by": record.get("request"),
                    "authority": record.get("authority"),
                    "failure_reason": record.get("failure_reason"),
                    "summary": record.get("summary"),
                    "recoverable": bool(record.get("recoverable")),
                }
                for record in execution_records
            ],
            "counts": counts,
        }

    def result(self, execution: str) -> dict[str, Any]:
        return self.context.executions.read_result(execution)

    def github_evidence(self, task: str) -> dict[str, Any]:
        """Read GitHub evidence (read-only) and record it on the task."""
        task_record = self.context.tasks.require(task)
        evidence = self.context.github.evidence(task_record)
        pull = evidence.get("pull_request") or {}
        linked = self.context.tasks.link_github(
            str(task_record["id"]),
            repo=evidence.get("repo"),
            branch=evidence.get("branch"),
            pr=pull.get("number"),
        )
        self.context.tasks.update(str(task_record["id"]), github_evidence=evidence)
        self.context.journal.append(
            event_type="github.evidence_read",
            workspace=linked.get("workspace_id"),
            source="viva",
            payload={
                "task_id": linked["id"],
                "repo": evidence.get("repo"),
                "issue": (evidence.get("issue") or {}).get("number"),
                "pr": pull.get("number"),
                "check_state": evidence.get("check_state"),
                "review_state": evidence.get("review_state"),
            },
        )
        return evidence

    def recover(self) -> dict[str, Any]:
        """Reconcile records with the OS. Never restarts anything."""
        report = self.context.executions.reconcile()
        self.context.journal.append(
            event_type="execution.reconciled",
            session_id=self.context.current_session_id(),
            source="viva",
            payload={
                "running": report["running"],
                "exited": report["exited"],
                "unknown": report["unknown"],
                "recoverable": report["recoverable"],
            },
        )
        return report

    def stop(
        self,
        execution: str,
        *,
        reason: str,
        grant_id: str | None = None,
        origin_execution: str | None = None,
    ) -> dict[str, Any]:
        record = self.context.executions.require(execution)
        self._authorize(
            action="stop",
            task_id=str(record["task_id"]),
            grant_id=grant_id,
            origin_execution=origin_execution,
            mode=(record.get("work_location") or {}).get("mode"),
        )
        stopped = self.context.executions_runner.stop(execution, reason=reason)
        self.context.tasks.record_execution(str(stopped["task_id"]), stopped)
        return stopped

    def wait(self, execution: str, *, timeout: float | None = None) -> dict[str, Any]:
        """Wait for a running execution and record its outcome on the task."""
        handle = self.context.executions_handles.get(execution)
        if handle is None:
            raise OfficeError(
                f"execution {execution!r} is not owned by this process; "
                "observe it with: viva office status"
            )
        finished = self.context.executions_runner.wait(handle, timeout=timeout)
        self.context.tasks.record_execution(str(finished["task_id"]), finished)
        return finished

    # -- briefs --------------------------------------------------------------

    def brief_text(self, task: str, *, coordinator: bool = False) -> str:
        from viva.tasks.brief import brief_text, handoff_brief

        task_record = self.context.tasks.require(task)
        executions = self.context.executions.list(task_id=str(task_record["id"]))
        knowledge = self.context.knowledge.for_task(task_record)
        brief = handoff_brief(
            task_record, executions=executions, knowledge=knowledge, coordinator=coordinator
        )
        return brief_text(brief)

    def brief(self, task: str, *, coordinator: bool = False) -> dict[str, Any]:
        from viva.tasks.brief import handoff_brief

        task_record = self.context.tasks.require(task)
        executions = self.context.executions.list(task_id=str(task_record["id"]))
        knowledge = self.context.knowledge.for_task(task_record)
        return handoff_brief(
            task_record, executions=executions, knowledge=knowledge, coordinator=coordinator
        )

    # -- internals -----------------------------------------------------------

    def _live_status(self, record: dict[str, Any]) -> dict[str, Any]:
        """Report a running execution's real state without mutating anything."""
        from viva.executions.registry import process_alive

        if record.get("status") == "running":
            alive = process_alive(record.get("pid"), record.get("process_started"))
            if alive is False:
                return {**record, "status": "exited (not yet reconciled)"}
            if alive is None:
                return {**record, "status": "unknown"}
        return record

    def _work_location(self, task: dict[str, Any], *, mode: str) -> dict[str, Any]:
        """Where this task's work happens. Writable work needs a worktree."""
        from viva.worktrees.location import WorkLocationError, resolve_work_location

        existing = task.get("work_location") or {}
        if existing.get("path"):
            if mode == "write" and existing.get("mode") != "write":
                raise OfficeError(
                    f"task {task['id']!r} is bound to a read-only work location "
                    f"({existing.get('kind')} {existing.get('path')}); create a task with a "
                    "writable kind (delivery/implementation/fix/chore) for code changes"
                )
            return dict(existing)
        repository = self._repository_for(task)
        try:
            return resolve_work_location(task, repository=repository, home=self.home)
        except WorkLocationError as exc:
            raise OfficeError(f"cannot place task {task['id']!r}: {exc}") from exc

    def _repository_for(self, task: dict[str, Any]) -> str | None:
        repositories = list(task.get("repositories") or [])
        if repositories:
            return str(repositories[0])
        project_id = task.get("project_id")
        if project_id:
            repository = self.context.projects.primary_repository(str(project_id))
            if repository:
                return repository
        workspace_id = task.get("workspace_id")
        if workspace_id:
            workspace = self.context.workspaces.get(str(workspace_id))
            if workspace and workspace.get("is_git"):
                return str(Path(str(workspace["path"])))
        return None
