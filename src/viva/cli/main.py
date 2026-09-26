"""`viva` — the Phase 1 CLI for the Viva habitat.

Surfaces:

    viva                    launch the TUI (Phase 1 product surface; needs a TTY)
    viva status             honest snapshot of resident/workspace/workers/experiences
    viva resident add|list|use
    viva workspace add|list|use|current
    viva worker list|run
    viva experience list

All state lives under ~/.viva (override with VIVA_HOME or --home). Read-only
commands write nothing; every mutation is recorded in the experience journal.
"""

from __future__ import annotations

import argparse
import json
import sys
from typing import Any

import viva
from viva.context import VivaContext
from viva.core.errors import VivaError
from viva.workers import WorkerError, run_worker
from viva.worktrees import worktree_summary

EXIT_OK = 0
EXIT_ERROR = 1
EXIT_NO_TTY = 2
EXIT_UNEXPECTED = 3


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="viva",
        description="Viva — a persistent habitat for AI agents to live, work, remember, and grow.",
    )
    parser.add_argument("--version", action="version", version=f"viva {viva.__version__}")
    parser.add_argument("--home", help="Viva state directory (default: ~/.viva or $VIVA_HOME)")
    sub = parser.add_subparsers(dest="command")

    status = sub.add_parser("status", help="show resident, workspace, worktree, worker and runtime state")
    status.add_argument("--json", action="store_true", help="emit machine-readable JSON")

    resident = sub.add_parser("resident", help="manage residents (long-lived identities)")
    resident_sub = resident.add_subparsers(dest="resident_command", required=True)
    resident_add = resident_sub.add_parser("add", help="create a resident")
    resident_add.add_argument("name")
    resident_add.add_argument("--notes", default="", help="free-text notes for this resident")
    resident_sub.add_parser("list", help="list residents")
    resident_use = resident_sub.add_parser("use", help="select the current resident")
    resident_use.add_argument("name")

    workspace = sub.add_parser("workspace", help="manage workspaces (long-term work contexts)")
    workspace_sub = workspace.add_subparsers(dest="workspace_command", required=True)
    workspace_add = workspace_sub.add_parser("add", help="register a local directory")
    workspace_add.add_argument("path")
    workspace_add.add_argument("name", nargs="?", default=None)
    workspace_sub.add_parser("list", help="list registered workspaces")
    workspace_use = workspace_sub.add_parser("use", help="select the current workspace")
    workspace_use.add_argument("name")
    workspace_sub.add_parser("current", help="show the current workspace")

    worker = sub.add_parser("worker", help="manage and invoke worker CLIs")
    worker_sub = worker.add_subparsers(dest="worker_command", required=True)
    worker_sub.add_parser("list", help="list configured workers and availability")
    worker_run = worker_sub.add_parser("run", help="run one worker non-interactively (user-initiated)")
    worker_run.add_argument("name")
    worker_run.add_argument("message", nargs="+", help="message passed to the worker")
    worker_run.add_argument("--timeout", type=float, default=None, help="seconds before the run is killed")

    worktree = sub.add_parser("worktree", help="inspect git worktrees of a workspace (read-only)")
    worktree_sub = worktree.add_subparsers(dest="worktree_command", required=True)
    worktree_list = worktree_sub.add_parser("list", help="list worktrees of the current or named workspace")
    worktree_list.add_argument("--workspace", default=None, help="workspace name/id (default: current)")

    experience = sub.add_parser("experience", help="inspect the append-only experience journal")
    experience_sub = experience.add_subparsers(dest="experience_command", required=True)
    experience_list = experience_sub.add_parser("list", help="show recent experiences")
    experience_list.add_argument("-n", type=int, default=20, help="how many recent events to show")
    experience_list.add_argument("--json", action="store_true", help="emit machine-readable JSON")

    return parser


# -- rendering ---------------------------------------------------------------


def _print_json(payload: Any) -> None:
    print(json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True, default=str))


def _worker_line(entry: dict[str, Any]) -> str:
    state = "available" if entry.get("available") else f"unavailable ({entry.get('reason') or 'not found'})"
    version = f" {entry['version']}" if entry.get("version") else ""
    return f"  {entry['name']:<10} {entry['command']}{version} — {state}"


def _render_status(status: dict[str, Any]) -> str:
    lines = [f"Viva {status['version']}", f"Home:        {status['home']}"]
    resident = status.get("resident")
    if resident:
        lines.append(f"Resident:    {resident['name']} (id {resident['id']}, since {resident['created_at'][:10]})")
    else:
        lines.append("Resident:    none yet — run: viva resident add <name>")
    workspace = status.get("workspace")
    if workspace:
        git = status.get("git") or {}
        if git.get("available"):
            current = git.get("current") or {}
            branch = current.get("branch") or git.get("branch")
            lines.append(
                f"Workspace:   {workspace['name']} — {workspace['path']} "
                f"(git: {branch or '?'} @ {git.get('head_sha', '?')}, {git.get('worktree_count', '?')} worktree(s))"
            )
        else:
            lines.append(f"Workspace:   {workspace['name']} — {workspace['path']} (not a git repository)")
    else:
        lines.append("Workspace:   none — run: viva workspace add <path> [name]")
    workers = status.get("workers") or []
    available = [entry for entry in workers if entry.get("available")]
    lines.append(f"Workers:     {len(available)}/{len(workers)} available")
    lines.extend(_worker_line(entry) for entry in workers)
    lines.append(f"Experiences: {status.get('experience_count', 0)} events (append-only journal, not memory)")
    if status.get("last_session_id"):
        lines.append(f"Last session: {status['last_session_id']}")
    return "\n".join(lines)


# -- commands -----------------------------------------------------------------


def _cmd_status(context: VivaContext, args: argparse.Namespace) -> int:
    status = context.status()
    if getattr(args, "json", False):
        _print_json(status)
    else:
        print(_render_status(status))
    return EXIT_OK


def _cmd_resident(context: VivaContext, args: argparse.Namespace) -> int:
    command = args.resident_command
    if command == "add":
        record = context.residents.create(args.name, notes=args.notes)
        context.journal.append(
            event_type="resident.created",
            resident_id=record["id"],
            source="cli",
            payload={"name": record["name"], "notes": bool(record["notes"])},
        )
        print(f"Resident created: {record['name']} (id {record['id']})")
        if context.current_resident() is None:
            context.runtime.set_current_resident(record["id"])
            context.journal.append(
                event_type="resident.selected", resident_id=record["id"], source="cli", payload={}
            )
            print(f"Resident selected: {record['name']}")
        return EXIT_OK
    if command == "list":
        residents = context.residents.list()
        if not residents:
            print("No residents yet — run: viva resident add <name>")
            return EXIT_OK
        current = context.current_resident()
        for record in residents:
            marker = " *" if current and current["id"] == record["id"] else ""
            notes = f" — {record['notes']}" if record.get("notes") else ""
            print(f"  {record['name']} (id {record['id']}, since {record['created_at'][:10]}){notes}{marker}")
        return EXIT_OK
    if command == "use":
        record = context.residents.get(args.name)
        if record is None:
            raise VivaError(f"no resident matches {args.name!r} — run: viva resident list")
        context.runtime.set_current_resident(record["id"])
        context.journal.append(
            event_type="resident.selected", resident_id=record["id"], source="cli", payload={}
        )
        print(f"Resident selected: {record['name']}")
        return EXIT_OK
    raise VivaError(f"unknown resident command: {command!r}")


def _cmd_workspace(context: VivaContext, args: argparse.Namespace) -> int:
    command = args.workspace_command
    if command == "add":
        workspace = context.workspaces.register(args.path, args.name)
        print(f"Workspace registered: {workspace['name']} — {workspace['path']}")
        if context.current_workspace() is None:
            workspace = context.workspaces.use(workspace["id"])
            print(f"Workspace selected: {workspace['name']}")
        return EXIT_OK
    if command == "list":
        workspaces = context.workspaces.list()
        if not workspaces:
            print("No workspaces yet — run: viva workspace add <path> [name]")
            return EXIT_OK
        current = context.current_workspace()
        for entry in workspaces:
            marker = " *" if current and current["id"] == entry["id"] else ""
            git = "git" if entry.get("is_git") else "dir"
            print(f"  {entry['name']:<12} {entry['path']} ({git}){marker}")
        return EXIT_OK
    if command == "use":
        workspace = context.workspaces.use(args.name)
        print(f"Workspace selected: {workspace['name']} — {workspace['path']}")
        return EXIT_OK
    if command == "current":
        workspace = context.current_workspace()
        if workspace is None:
            print("No current workspace — run: viva workspace use <name>")
            return EXIT_OK
        print(f"{workspace['name']} — {workspace['path']}")
        return EXIT_OK
    raise VivaError(f"unknown workspace command: {command!r}")


def _cmd_worker(context: VivaContext, args: argparse.Namespace) -> int:
    command = args.worker_command
    if command == "list":
        for entry in context.workers.list():
            print(_worker_line(entry))
        print(f"config: {context.workers.export_config_path()}")
        return EXIT_OK
    if command == "run":
        workspace = context.current_workspace()
        if workspace is None:
            raise WorkerError("no current workspace — a worker run needs one: viva workspace use <name>")
        worker = context.workers.get(args.name)
        availability = context.workers.probe(worker)
        if not availability["available"]:
            raise WorkerError(f"worker {worker['name']!r} is unavailable: {availability['reason']}")
        message = " ".join(args.message)
        session_id = context.current_session_id()
        context.journal.append(
            event_type="worker.invoked",
            resident_id=context.current_resident_id(),
            session_id=session_id,
            workspace=workspace["id"],
            source="cli",
            payload={"worker": worker["name"], "message": message, "command": worker["command"]},
        )
        print(f"[viva] running {worker['name']} in {workspace['name']}…")

        def observe(stream: str, line: str) -> None:
            print(f"[{worker['name']}|{stream}] {line}", flush=True)

        result = run_worker(
            worker,
            message,
            cwd=workspace["path"],
            initiated_by="user",
            timeout_seconds=args.timeout,
            on_output=observe,
        )
        context.journal.append(
            event_type="worker.completed" if result.status == "COMPLETED" else "worker.failed",
            resident_id=context.current_resident_id(),
            session_id=session_id,
            workspace=workspace["id"],
            source="cli",
            payload={
                "worker": worker["name"],
                "status": result.status,
                "exit_code": result.exit_code,
                "duration_ms": result.duration_ms,
            },
        )
        print(f"[viva] {result.status} exit={result.exit_code} in {result.duration_ms} ms")
        return EXIT_OK if result.status == "COMPLETED" else EXIT_ERROR
    raise VivaError(f"unknown worker command: {command!r}")


def _cmd_worktree(context: VivaContext, args: argparse.Namespace) -> int:
    if args.worktree_command != "list":  # pragma: no cover — parser restricts this
        raise VivaError(f"unknown worktree command: {args.worktree_command!r}")
    if args.workspace:
        workspace = context.workspaces.get(args.workspace)
        if workspace is None:
            raise VivaError(f"no registered workspace matches {args.workspace!r}")
    else:
        workspace = context.current_workspace()
        if workspace is None:
            raise VivaError("no current workspace — run: viva workspace use <name>")
    summary = worktree_summary(workspace["path"])
    if not summary.get("available"):
        raise VivaError(f"workspace {workspace['name']!r} has no usable git repository: {summary.get('reason')}")
    current = summary.get("current") or {}
    for entry in summary["worktrees"]:
        marker = " *" if entry["path"] == current.get("path") else ""
        branch = entry["branch"] or "(detached)"
        print(f"  {entry['path']}  [{branch} @ {str(entry['head'])[:12]}]{marker}")
    print(f"({summary['worktree_count']} worktree(s) in {workspace['name']})")
    return EXIT_OK


def _cmd_experience(context: VivaContext, args: argparse.Namespace) -> int:
    if args.experience_command != "list":  # pragma: no cover — parser restricts this
        raise VivaError(f"unknown experience command: {args.experience_command!r}")
    events = context.journal.tail(max(args.n, 0))
    if getattr(args, "json", False):
        _print_json(events)
        return EXIT_OK
    if not events:
        print("No experiences recorded yet.")
        return EXIT_OK
    for event in events:
        resident = event.get("resident_id") or "-"
        workspace = event.get("workspace") or "-"
        payload = event.get("payload") or {}
        summary = " ".join(f"{key}={value}" for key, value in sorted(payload.items()) if value not in (None, "", {}, []))
        print(
            f"#{event.get('sequence')} {event.get('timestamp')} {event.get('event_type')} "
            f"resident={resident} workspace={workspace} source={event.get('source')}"
            + (f" {summary}" if summary else "")
        )
    print("(experience is not memory: nothing is promoted, recalled, or summarized yet)")
    return EXIT_OK


def _launch_tui(context: VivaContext) -> int:
    if not sys.stdout.isatty() or not sys.stdin.isatty():
        print(
            "viva: the TUI needs a terminal. Use `viva status` for a plain snapshot.",
            file=sys.stderr,
        )
        return EXIT_NO_TTY
    from viva.tui.app import VivaTui

    context.begin_session()
    VivaTui(context=context).run()
    return EXIT_OK


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        context = VivaContext(args.home)
        if args.command is None:
            return _launch_tui(context)
        if args.command == "status":
            return _cmd_status(context, args)
        if args.command == "resident":
            return _cmd_resident(context, args)
        if args.command == "workspace":
            return _cmd_workspace(context, args)
        if args.command == "worker":
            return _cmd_worker(context, args)
        if args.command == "worktree":
            return _cmd_worktree(context, args)
        if args.command == "experience":
            return _cmd_experience(context, args)
        parser.error(f"unknown command: {args.command!r}")  # pragma: no cover
        return EXIT_ERROR
    except VivaError as exc:
        print(f"viva: {exc}", file=sys.stderr)
        return EXIT_ERROR
    except KeyboardInterrupt:  # a cancelled run is not a crash
        print("viva: interrupted", file=sys.stderr)
        return EXIT_ERROR
    except Exception as exc:  # surfaced, never reported as success
        print(f"viva: unexpected error: {exc!r}", file=sys.stderr)
        return EXIT_UNEXPECTED


if __name__ == "__main__":
    sys.exit(main())
