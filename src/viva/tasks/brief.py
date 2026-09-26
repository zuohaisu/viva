"""Handoff brief — everything a replacement worker needs, and nothing invented.

When a worker dies, stalls, or is simply replaced, the task context must move
with the *task*, not with the dead process. This module turns a task record
into the brief handed to the next member: goal, constraints, work location,
what was already produced, what was already tried, and why those attempts
failed.

It also carries the office protocol, so a coordinating member can actually
dispatch, observe, read results and stop other members within the scope Haisu
granted — instead of Viva pretending a fixed pipeline is dynamic scheduling.
"""

from __future__ import annotations

from typing import Any, Iterable

PROTOCOL_LINES = (
    "You are acting inside Viva's AI Office. Other AI members can be dispatched "
    "with the commands below; they are real commands, not a fixed pipeline — any "
    "member may be assigned to any task if the role allows the work mode.",
    "",
    "  viva office dispatch --task {task} --to <member> [--mode read_only|write] "
    "[--tool <tool>] [--note <text>]",
    "  viva office status [--task {task}] [--json]",
    "  viva office result <execution-id> [--json]",
    "  viva office stop <execution-id> --reason <text>",
    "",
    "Every dispatch you make must spend the grant Haisu gave this execution "
    "(read it from $VIVA_GRANT_ID). A dispatch without a grant is refused and "
    "recorded; you may not widen the scope of your grant, and your requests are "
    "recorded as coming from this execution, not from Haisu.",
)


def _line(label: str, value: Any) -> str:
    return f"- **{label}:** {value}"


def handoff_brief(
    task: dict[str, Any],
    *,
    executions: Iterable[dict[str, Any]] = (),
    knowledge: Iterable[dict[str, Any]] = (),
    coordinator: bool = False,
) -> dict[str, Any]:
    """Structured brief; the same data drives the text and the JSON surface."""
    executions = list(executions)
    attempts = []
    for execution in executions:
        attempts.append(
            {
                "execution_id": execution.get("id"),
                "member_id": execution.get("member_id"),
                "role": execution.get("role"),
                "worker": execution.get("tool"),
                "engine": execution.get("engine"),
                "mode": (execution.get("work_location") or {}).get("mode"),
                "status": execution.get("status"),
                "requested_by": execution.get("request"),
                "summary": execution.get("summary"),
                "failure_reason": execution.get("failure_reason"),
                "output_path": execution.get("output_path"),
            }
        )
    return {
        "task_id": task.get("id"),
        "title": task.get("title"),
        "goal": task.get("intent") or "",
        "kind": task.get("kind"),
        "status": task.get("status"),
        "constraints": list(task.get("constraints") or []),
        "workspace_id": task.get("workspace_id"),
        "project_id": task.get("project_id"),
        "repositories": list(task.get("repositories") or []),
        "work_location": dict(task.get("work_location") or {}),
        "assignees": list(task.get("assignees") or []),
        "github": dict(task.get("github") or {}),
        "attempts": attempts,
        "outputs": list(task.get("outputs") or []),
        "unfinished": list(task.get("unfinished") or []),
        "knowledge": [
            {
                "id": entry.get("id"),
                "scope": entry.get("scope"),
                "kind": entry.get("kind"),
                "title": entry.get("title"),
                "used_before": len(entry.get("used_in") or []),
            }
            for entry in knowledge
        ],
        "coordinator": bool(coordinator),
    }


def brief_text(brief: dict[str, Any]) -> str:
    """Render the brief as the text handed to a worker (or read by a human)."""
    lines = [
        f"# Task {brief['task_id']} — {brief['title']}",
        "",
        _line("Goal", brief["goal"] or "(no intent recorded)"),
        _line("Kind / status", f"{brief['kind']} / {brief['status']}"),
        _line("Workspace / project", f"{brief['workspace_id'] or '-'} / {brief['project_id'] or '-'}"),
    ]
    location = brief.get("work_location") or {}
    if location.get("path"):
        lines.append(
            _line(
                "Work location",
                f"{location.get('kind')} `{location.get('path')}`"
                + (f" on `{location.get('branch')}`" if location.get("branch") else "")
                + f" (mode: {location.get('mode') or 'read_only'})",
            )
        )
    else:
        lines.append(_line("Work location", "not allocated yet"))
    if brief["repositories"]:
        lines.append(_line("Repositories", ", ".join(f"`{item}`" for item in brief["repositories"])))
    if brief["github"]:
        github = brief["github"]
        parts = [f"{github.get('repo', '-')}#{github.get('issue', '-')}"]
        if github.get("branch"):
            parts.append(f"branch `{github['branch']}`")
        if github.get("pr"):
            parts.append(f"PR #{github['pr']}")
        lines.append(_line("GitHub", " · ".join(parts)))
    lines.append(_line("Assigned members", ", ".join(brief["assignees"]) or "(unassigned)"))
    if brief["constraints"]:
        lines.append(_line("Constraints", "; ".join(str(item) for item in brief["constraints"])))

    lines.extend(["", "## Previous attempts", ""])
    if brief["attempts"]:
        for attempt in brief["attempts"]:
            detail = (
                f"- `{attempt['execution_id']}` by {attempt['member_id']} "
                f"({attempt['role']}) via {attempt['worker']} "
                f"[{attempt['status']}, mode {attempt['mode']}]"
            )
            if attempt.get("failure_reason"):
                detail += f" — failed: {attempt['failure_reason']}"
            elif attempt.get("summary"):
                detail += f" — {attempt['summary']}"
            lines.append(detail)
    else:
        lines.append("- none yet")

    lines.extend(["", "## Outputs", ""])
    if brief["outputs"]:
        for output in brief["outputs"]:
            artifacts = output.get("artifacts") or []
            suffix = f" (artifacts: {', '.join(artifacts)})" if artifacts else ""
            lines.append(f"- {output.get('summary')} [{output.get('execution_id')}]{suffix}")
    else:
        lines.append("- none recorded")

    lines.extend(["", "## Unfinished", ""])
    if brief["unfinished"]:
        lines.extend(f"- {item}" for item in brief["unfinished"])
    else:
        lines.append("- nothing recorded as unfinished")

    if brief["knowledge"]:
        lines.extend(["", "## Knowledge in scope", ""])
        for entry in brief["knowledge"]:
            lines.append(
                f"- [{entry['scope']}/{entry['kind']}] {entry['title']} "
                f"(`{entry['id']}`, used {entry['used_before']}x)"
            )

    if brief["coordinator"]:
        lines.extend(["", "## Office protocol", ""])
        lines.extend(
            line.format(task=brief["task_id"]) if "{task}" in line else line
            for line in PROTOCOL_LINES
        )
    return "\n".join(lines)
