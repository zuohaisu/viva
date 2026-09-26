"""`viva` — the command surface of Haisu's AI Office.

    viva                     launch the TUI (needs a terminal)
    viva status              members, workspace/project, tasks, executions
    viva resident ...        AI members: add/list/show/use/role/engine/tools
    viva role|engine list    role and cognitive-engine catalogues (data)
    viva workspace ...       long-term work contexts
    viva project ...         projects inside a workspace, and their repositories
    viva task ...            create, inspect, brief, close tasks
    viva office ...          dispatch/status/result/stop/recover + grants
    viva knowledge ...       personal / project / team knowledge and skills
    viva github ...          read-only issue/PR/checks linkage
    viva worker list         execution tools and their availability
    viva worktree list       git worktrees of a workspace (read-only)

All state lives under ``~/.viva`` (override with VIVA_HOME or --home).
Read-only commands write nothing; every mutation is recorded in the experience
journal, and every execution records its own attribution at launch.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from typing import Any

import viva
from viva.context import VivaContext
from viva.core.errors import VivaError
from viva.executions import ExecutionError
from viva.github import GitHubError
from viva.knowledge import KnowledgeError
from viva.permissions import GrantError
from viva.projects import ProjectError
from viva.tasks import TaskError
from viva.workers import WorkerError
from viva.worktrees import worktree_summary

EXIT_OK = 0
EXIT_ERROR = 1
EXIT_NO_TTY = 2
EXIT_UNEXPECTED = 3

KNOWLEDGE_KINDS = (
    "personal_memory",
    "self_model_candidate",
    "project_knowledge",
    "team_knowledge",
    "skill",
)


def _multi(values: list[str] | None) -> list[str]:
    return list(values or [])


def _env(name: str) -> str | None:
    value = os.environ.get(name)
    return value or None


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="viva",
        description=(
            "Viva — Haisu's local-first Personal AI Office: persistent AI members, "
            "tasks, executions and the history that outlives any worker session."
        ),
    )
    parser.add_argument("--version", action="version", version=f"viva {viva.__version__}")
    parser.add_argument("--home", help="Viva state directory (default: ~/.viva or $VIVA_HOME)")
    sub = parser.add_subparsers(dest="command")

    status = sub.add_parser("status", help="members, workspace, tasks, executions, tools")
    status.add_argument("--json", action="store_true", help="emit machine-readable JSON")

    # -- members -------------------------------------------------------------
    resident = sub.add_parser("resident", help="AI members (persistent identities)")
    resident_sub = resident.add_subparsers(dest="resident_command", required=True)
    resident_add = resident_sub.add_parser("add", help="create an AI member")
    resident_add.add_argument("name")
    resident_add.add_argument("--role", default="developer", help="role id (see: viva role list)")
    resident_add.add_argument("--engine", default=None, help="engine id (see: viva engine list)")
    resident_add.add_argument("--model", default=None, help="concrete model for that engine")
    resident_add.add_argument("--tool", action="append", help="execution tool (repeatable)")
    resident_add.add_argument("--notes", default="", help="free-text notes")
    resident_sub.add_parser("list", help="list members")
    resident_use = resident_sub.add_parser("use", help="select the current member")
    resident_use.add_argument("name")
    resident_show = resident_sub.add_parser("show", help="show one member and its binding")
    resident_show.add_argument("name")
    resident_show.add_argument("--json", action="store_true")
    resident_role = resident_sub.add_parser("role", help="set a member's role")
    resident_role.add_argument("name")
    resident_role.add_argument("role")
    resident_engine = resident_sub.add_parser(
        "engine", help="rebind the cognitive engine (history is kept)"
    )
    resident_engine.add_argument("name")
    resident_engine.add_argument("engine")
    resident_engine.add_argument("--model", default=None)
    resident_tools = resident_sub.add_parser("tools", help="bind execution tools")
    resident_tools.add_argument("name")
    resident_tools.add_argument("--tool", action="append", required=True)

    for name, help_text in (
        ("role", "list roles (configuration data)"),
        ("engine", "list cognitive engines (configuration data)"),
    ):
        catalogue = sub.add_parser(name, help=help_text)
        catalogue_sub = catalogue.add_subparsers(dest=f"{name}_command", required=True)
        catalogue_sub.add_parser("list", help=help_text)

    # -- workspaces / projects ----------------------------------------------
    workspace = sub.add_parser("workspace", help="long-term working contexts")
    workspace_sub = workspace.add_subparsers(dest="workspace_command", required=True)
    workspace_add = workspace_sub.add_parser("add", help="register a local directory")
    workspace_add.add_argument("path")
    workspace_add.add_argument("name", nargs="?", default=None)
    workspace_sub.add_parser("list", help="list registered workspaces")
    workspace_use = workspace_sub.add_parser("use", help="select the current workspace")
    workspace_use.add_argument("name")
    workspace_sub.add_parser("current", help="show the current workspace")

    project = sub.add_parser("project", help="projects inside a workspace")
    project_sub = project.add_subparsers(dest="project_command", required=True)
    project_add = project_sub.add_parser("add", help="register a project")
    project_add.add_argument("name")
    project_add.add_argument("--workspace", default=None, help="owning workspace (default: current)")
    project_add.add_argument("--repo", action="append", help="repository path (repeatable)")
    project_list = project_sub.add_parser("list", help="list projects")
    project_list.add_argument("--json", action="store_true")
    project_bind = project_sub.add_parser("bind", help="bind another repository to a project")
    project_bind.add_argument("project")
    project_bind.add_argument("path")

    # -- tasks ---------------------------------------------------------------
    task = sub.add_parser("task", help="tasks: the intention that outlives sessions")
    task_sub = task.add_subparsers(dest="task_command", required=True)
    task_new = task_sub.add_parser("new", help="create a task")
    task_new.add_argument("title")
    task_new.add_argument("--intent", default="", help="what 'done' means")
    task_new.add_argument(
        "--kind",
        default="delivery",
        help=(
            "delivery|implementation|fix|chore (isolated worktree) or "
            "research|planning|review|ops (read-only, no worktree)"
        ),
    )
    task_new.add_argument("--workspace", default=None)
    task_new.add_argument("--project", default=None)
    task_new.add_argument("--repo", action="append", help="repository path (repeatable)")
    task_new.add_argument("--assign", action="append", help="member id (repeatable)")
    task_new.add_argument("--constraint", action="append", help="constraint for the worker")
    task_new.add_argument("--json", action="store_true")
    task_list = task_sub.add_parser("list", help="list tasks")
    task_list.add_argument("--status", default=None)
    task_list.add_argument("--open", action="store_true", help="only open tasks")
    task_list.add_argument("--json", action="store_true")
    task_show = task_sub.add_parser("show", help="show one task with its executions")
    task_show.add_argument("task")
    task_show.add_argument("--json", action="store_true")
    task_status = task_sub.add_parser("status", help="set a task's status")
    task_status.add_argument("task")
    task_status.add_argument("status")
    task_brief = task_sub.add_parser("brief", help="handoff brief for the next worker")
    task_brief.add_argument("task")
    task_brief.add_argument("--coordinator", action="store_true", help="include the office protocol")
    task_brief.add_argument("--json", action="store_true")
    task_output = task_sub.add_parser("output", help="record an output on a task")
    task_output.add_argument("task")
    task_output.add_argument("--execution", required=True)
    task_output.add_argument("--summary", required=True)
    task_output.add_argument("--artifact", action="append")
    task_unfinished = task_sub.add_parser("unfinished", help="record what is still open")
    task_unfinished.add_argument("task")
    task_unfinished.add_argument("items", nargs="+")

    # -- office --------------------------------------------------------------
    office = sub.add_parser("office", help="dispatch, observe, read and stop executions")
    office_sub = office.add_subparsers(dest="office_command", required=True)

    dispatch = office_sub.add_parser("dispatch", help="start an execution for a member on a task")
    dispatch.add_argument("--task", required=True)
    dispatch.add_argument("--to", required=True, dest="member")
    dispatch.add_argument("--mode", default=None, choices=["read_only", "write"])
    dispatch.add_argument("--tool", default=None, help="override the engine's tool")
    dispatch.add_argument("--message", default=None, help="override the generated brief")
    dispatch.add_argument("--note", default="")
    dispatch.add_argument("--grant", default=None, help="grant id (required for worker-originated dispatch)")
    dispatch.add_argument("--origin-execution", default=None)
    dispatch.add_argument("--wait", action="store_true", help="wait for the execution to finish")
    dispatch.add_argument("--json", action="store_true")

    office_status = office_sub.add_parser("status", help="tasks + executions with live states")
    office_status.add_argument("--task", default=None)
    office_status.add_argument("--json", action="store_true")
    office_execs = office_sub.add_parser("execs", help="list executions")
    office_execs.add_argument("--task", default=None)
    office_execs.add_argument("--json", action="store_true")
    office_result = office_sub.add_parser("result", help="read one execution's result")
    office_result.add_argument("execution")
    office_result.add_argument("--tail", type=int, default=40)
    office_result.add_argument("--json", action="store_true")
    office_stop = office_sub.add_parser("stop", help="stop one execution")
    office_stop.add_argument("execution")
    office_stop.add_argument("--reason", required=True)
    office_stop.add_argument("--grant", default=None)
    office_stop.add_argument("--origin-execution", default=None)
    office_recover = office_sub.add_parser("recover", help="reconcile records with the OS")
    office_recover.add_argument("--json", action="store_true")
    office_grant = office_sub.add_parser("grant", help="grant a member authority over a task")
    office_grant.add_argument("--to", required=True, dest="member")
    office_grant.add_argument("--task", required=True)
    office_grant.add_argument(
        "--actions", required=True, help="comma-separated: dispatch,status,result,stop"
    )
    office_grant.add_argument("--mode-max", required=True, choices=["read_only", "write"])
    office_grant.add_argument("--reason", required=True)
    office_grant.add_argument("--delegate-from", default=None, help="parent grant (worker delegation)")
    office_grant.add_argument("--origin-execution", default=None)
    office_grant.add_argument("--json", action="store_true")

    # -- knowledge -----------------------------------------------------------
    knowledge = sub.add_parser("knowledge", help="personal / project / team knowledge and skills")
    knowledge_sub = knowledge.add_subparsers(dest="knowledge_command", required=True)
    knowledge_add = knowledge_sub.add_parser("add", help="record a curated entry")
    knowledge_add.add_argument("--kind", required=True, choices=list(KNOWLEDGE_KINDS))
    knowledge_add.add_argument("--title", required=True)
    knowledge_add.add_argument("--body", required=True)
    knowledge_add.add_argument("--member", default=None, help="owning member (personal/self_model)")
    knowledge_add.add_argument("--project", default=None, help="owning project (project_knowledge)")
    knowledge_add.add_argument("--task", default=None, help="provenance: task id")
    knowledge_add.add_argument("--execution", default=None, help="provenance: execution id")
    knowledge_add.add_argument("--json", action="store_true")
    knowledge_list = knowledge_sub.add_parser("list", help="list entries")
    knowledge_list.add_argument("--kind", default=None, choices=list(KNOWLEDGE_KINDS))
    knowledge_list.add_argument("--json", action="store_true")
    knowledge_show = knowledge_sub.add_parser("show", help="show one entry")
    knowledge_show.add_argument("entry")
    knowledge_show.add_argument("--json", action="store_true")
    knowledge_use = knowledge_sub.add_parser("use", help="record that an execution used an entry")
    knowledge_use.add_argument("entry")
    knowledge_use.add_argument("--execution", required=True)
    knowledge_use.add_argument("--task", default=None)
    knowledge_use.add_argument("--note", default="")
    knowledge_reuse = knowledge_sub.add_parser(
        "reuse", help="entries actually reused (the only reuse evidence)"
    )
    knowledge_reuse.add_argument("--json", action="store_true")

    # -- github --------------------------------------------------------------
    github = sub.add_parser("github", help="read-only GitHub linkage")
    github_sub = github.add_subparsers(dest="github_command", required=True)
    github_link = github_sub.add_parser("link", help="link a task to a repository/issue/PR")
    github_link.add_argument("task")
    github_link.add_argument("--repo", required=True, help="owner/name")
    github_link.add_argument("--issue", type=int, default=None)
    github_link.add_argument("--branch", default=None)
    github_link.add_argument("--pr", type=int, default=None)
    github_evidence = github_sub.add_parser("evidence", help="read issue/PR/checks evidence")
    github_evidence.add_argument("task")
    github_evidence.add_argument("--json", action="store_true")

    # -- tools / worktrees / experience -------------------------------------
    worker = sub.add_parser("worker", help="execution tools (agent CLIs) and availability")
    worker_sub = worker.add_subparsers(dest="worker_command", required=True)
    worker_sub.add_parser("list", help="list configured tools and availability")

    worktree = sub.add_parser("worktree", help="inspect git worktrees (read-only)")
    worktree_sub = worktree.add_subparsers(dest="worktree_command", required=True)
    worktree_list = worktree_sub.add_parser("list", help="list worktrees of a workspace")
    worktree_list.add_argument("--workspace", default=None)

    experience = sub.add_parser("experience", help="append-only experience journal")
    experience_sub = experience.add_subparsers(dest="experience_command", required=True)
    experience_list = experience_sub.add_parser("list", help="show recent experiences")
    experience_list.add_argument("-n", type=int, default=20)
    experience_list.add_argument("--json", action="store_true")

    return parser


# -- rendering ---------------------------------------------------------------


def _print_json(payload: Any) -> None:
    print(json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True, default=str))


def _worker_line(entry: dict[str, Any]) -> str:
    state = (
        "available"
        if entry.get("available")
        else f"unavailable ({entry.get('reason') or 'not found'})"
    )
    version = f" {entry['version']}" if entry.get("version") else ""
    return f"  {entry['name']:<10} {entry['command']}{version} — {state}"


def _render_status(status: dict[str, Any]) -> str:
    lines = [f"Viva {status['version']} — Personal AI Office", f"Home:        {status['home']}"]
    members = status.get("residents") or []
    current = status.get("resident")
    lines.append(
        f"Members:     {len(members)}" + (f" (current: {current['name']})" if current else "")
    )
    for record in members:
        engine = record.get("engine") or {}
        model = engine.get("model") or "(tool default)"
        marker = " *" if current and current["id"] == record["id"] else ""
        lines.append(
            f"  {record['name']:<12} {str(record.get('role')):<12} "
            f"{str(engine.get('id')):<18} {model:<22} "
            f"tools={','.join(record.get('tools') or [])}{marker}"
        )
    workspace = status.get("workspace")
    if workspace:
        git = status.get("git") or {}
        extra = ""
        if git.get("available"):
            extra = f" (git: {git.get('branch')}, {git.get('worktree_count')} worktree(s))"
        lines.append(f"Workspace:   {workspace['name']} — {workspace['path']}{extra}")
    else:
        lines.append("Workspace:   none — run: viva workspace add <path> [name]")
    projects = status.get("projects") or []
    if projects:
        lines.append(f"Projects:    {', '.join(str(item.get('id')) for item in projects)}")
    tasks = status.get("tasks") or []
    open_tasks = status.get("open_tasks") or []
    lines.append(f"Tasks:       {len(tasks)} total, {len(open_tasks)} open")
    for record in open_tasks[:10]:
        location = (record.get("work_location") or {}).get("path") or "-"
        assignees = ",".join(record.get("assignees") or []) or "unassigned"
        lines.append(
            f"  {record['id']:<36} {record['kind']:<12} {record['status']:<12} "
            f"{assignees:<14} {location}"
        )
    running = status.get("running_executions") or []
    unresolved = status.get("unresolved_executions") or []
    lines.append(f"Executions:  {len(running)} running, {len(unresolved)} unresolved")
    for record in running:
        lines.append(
            f"  {record['id']:<16} {record['member_id']:<10} {record['tool']:<10} "
            f"pid={record.get('pid')} task={record['task_id']}"
        )
    for record in unresolved:
        lines.append(
            f"  {record['id']:<16} {record['status']:<10} task={record['task_id']} "
            "— inspect with: viva office result <id>"
        )
    workers = status.get("workers") or []
    available = [entry for entry in workers if entry.get("available")]
    lines.append(f"Tools:       {len(available)}/{len(workers)} available")
    lines.extend(_worker_line(entry) for entry in workers)
    lines.append(
        f"Knowledge:   {status.get('knowledge_count', 0)} entries, "
        f"{status.get('knowledge_reuse_count', 0)} reused in later work"
    )
    lines.append(
        f"Experience:  {status.get('experience_count', 0)} events "
        "(append-only journal; experience is not memory)"
    )
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
        record = context.residents.create(
            args.name,
            role=args.role,
            engine=args.engine,
            model=args.model,
            tools=_multi(args.tool) or None,
            notes=args.notes,
        )
        print(
            f"Member created: {record['name']} (id {record['id']}, role {record['role']}, "
            f"engine {record['engine']['id']}, tools {','.join(record['tools'])})"
        )
        if context.current_resident() is None:
            context.runtime.set_current_resident(record["id"])
            context.journal.append(
                event_type="resident.selected", resident_id=record["id"], source="cli", payload={}
            )
            print(f"Member selected: {record['name']}")
        return EXIT_OK
    if command == "list":
        members = context.residents.list()
        if not members:
            print("No AI members yet — run: viva resident add <name> --role <role>")
            return EXIT_OK
        current = context.current_resident()
        for record in members:
            marker = " *" if current and current["id"] == record["id"] else ""
            engine = record.get("engine") or {}
            model = f", model {engine['model']}" if engine.get("model") else ""
            print(
                f"  {record['name']} (id {record['id']}, role {record.get('role')}, "
                f"engine {engine.get('id')}{model}, "
                f"tools {','.join(record.get('tools') or [])}){marker}"
            )
        return EXIT_OK
    if command == "use":
        record = context.residents.require(args.name)
        context.runtime.set_current_resident(record["id"])
        context.journal.append(
            event_type="resident.selected", resident_id=record["id"], source="cli", payload={}
        )
        print(f"Member selected: {record['name']}")
        return EXIT_OK
    if command == "show":
        record = context.residents.require(args.name)
        history_events = len(
            [
                event
                for event in context.journal.read()
                if event.get("resident_id") == record["id"]
            ]
        )
        payload = {
            **record,
            "role_definition": context.residents.roles.get(record["role"]),
            "engine_definition": context.residents.engines.get(record["engine"]["id"]),
            "history_events": history_events,
            "knowledge_entries": len(context.knowledge.list(member_id=record["id"])),
            "continuity_note": (
                "Records and history only: Viva keeps this member's configuration, events and "
                "knowledge across model changes. It does not claim a proven continuous Self."
            ),
        }
        if getattr(args, "json", False):
            _print_json(payload)
        else:
            print(f"{record['name']} (id {record['id']}) — role {record['role']}")
            print(
                f"  engine:  {record['engine']['id']} · "
                f"model {record['engine'].get('model') or '(tool default)'}"
            )
            print(f"  tools:   {', '.join(record.get('tools') or [])}")
            print(f"  history: {history_events} experience events")
            print(f"  knowledge entries: {payload['knowledge_entries']}")
            print(f"  {payload['continuity_note']}")
        return EXIT_OK
    if command == "role":
        record = context.residents.set_role(args.name, args.role)
        print(f"Member {record['id']} role set to {record['role']}")
        return EXIT_OK
    if command == "engine":
        record = context.residents.set_engine(args.name, engine=args.engine, model=args.model)
        print(
            f"Member {record['id']} engine rebound to {record['engine']['id']} "
            f"(model {record['engine'].get('model') or '(tool default)'}); "
            "record, history and knowledge kept"
        )
        return EXIT_OK
    if command == "tools":
        record = context.residents.set_tools(args.name, _multi(args.tool))
        print(f"Member {record['id']} tools set to {', '.join(record['tools'])}")
        return EXIT_OK
    raise VivaError(f"unknown resident command: {command!r}")


def _cmd_catalogue(context: VivaContext, args: argparse.Namespace) -> int:
    if args.command == "role":
        for role in context.residents.roles.list():
            print(
                f"  {role['id']:<12} {role['title']:<20} "
                f"modes={','.join(role['allowed_modes'])} default={role['default_mode']}"
            )
            print(f"               {role['purpose']}")
        print(f"config: {context.residents.roles.export_config_path()}")
        return EXIT_OK
    for engine in context.residents.engines.list():
        model = engine.get("model") or "(tool default)"
        print(
            f"  {engine['id']:<18} tool={engine['tool']:<10} model={model:<24} {engine['label']}"
        )
    print(f"config: {context.residents.engines.export_config_path()}")
    return EXIT_OK


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
        print(
            f"{workspace['name']} — {workspace['path']}"
            if workspace
            else "No current workspace — run: viva workspace use <name>"
        )
        return EXIT_OK
    raise VivaError(f"unknown workspace command: {command!r}")


def _cmd_project(context: VivaContext, args: argparse.Namespace) -> int:
    command = args.project_command
    if command == "add":
        workspace = (
            context.workspaces.get(args.workspace)
            if args.workspace
            else context.current_workspace()
        )
        if workspace is None:
            raise ProjectError("no workspace to own this project — pass --workspace or select one")
        project = context.projects.register(
            workspace_id=str(workspace["id"]),
            name=args.name,
            repositories=_multi(args.repo),
        )
        print(
            f"Project registered: {project['name']} (id {project['id']}) in workspace "
            f"{project['workspace_id']} with {len(project['repositories'])} repository(ies)"
        )
        return EXIT_OK
    if command == "list":
        projects = context.projects.list()
        if getattr(args, "json", False):
            _print_json(projects)
            return EXIT_OK
        if not projects:
            print("No projects yet — run: viva project add <name> --repo <path>")
            return EXIT_OK
        for project in projects:
            print(
                f"  {project['id']:<16} workspace={project['workspace_id']:<12} "
                f"{len(project.get('repositories') or [])} repo(s)"
            )
            for repository in project.get("repositories") or []:
                print(f"      {repository}")
        return EXIT_OK
    if command == "bind":
        project = context.projects.bind_repository(args.project, args.path)
        print(f"Project {project['id']} now has {len(project['repositories'])} repository(ies)")
        return EXIT_OK
    raise VivaError(f"unknown project command: {command!r}")


def _cmd_task(context: VivaContext, args: argparse.Namespace) -> int:
    command = args.task_command
    if command == "new":
        workspace = (
            context.workspaces.get(args.workspace)
            if args.workspace
            else context.current_workspace()
        )
        project = context.projects.get(args.project) if args.project else None
        repositories = _multi(args.repo) or (
            [context.projects.primary_repository(str(project["id"]))] if project else []
        )
        record = context.tasks.create(
            title=args.title,
            intent=args.intent,
            kind=args.kind,
            workspace_id=str(workspace["id"]) if workspace else None,
            project_id=str(project["id"]) if project else None,
            repositories=[item for item in repositories if item],
            assignees=_multi(args.assign),
            constraints=_multi(args.constraint),
        )
        if getattr(args, "json", False):
            _print_json(record)
        else:
            print(f"Task created: {record['id']} — {record['title']} [{record['kind']}]")
            print(f"  assignees: {', '.join(record['assignees']) or '(unassigned)'}")
        return EXIT_OK
    if command == "list":
        tasks = context.tasks.list(
            status=args.status, open_only=bool(getattr(args, "open", False))
        )
        if getattr(args, "json", False):
            _print_json(tasks)
            return EXIT_OK
        if not tasks:
            print("No tasks — run: viva task new <title> --kind <kind>")
            return EXIT_OK
        for record in tasks:
            location = (record.get("work_location") or {}).get("path") or "-"
            assignees = ",".join(record.get("assignees") or []) or "unassigned"
            print(
                f"  {record['id']:<36} {record['kind']:<12} {record['status']:<12} "
                f"{assignees:<16} {location}"
            )
        return EXIT_OK
    if command == "show":
        record = context.tasks.require(args.task)
        executions = context.executions.list(task_id=str(record["id"]))
        if getattr(args, "json", False):
            _print_json({**record, "execution_records": executions})
            return EXIT_OK
        print(f"{record['id']} — {record['title']} [{record['kind']}, {record['status']}]")
        print(f"  intent:        {record.get('intent') or '(none)'}")
        print(f"  constraints:   {'; '.join(record.get('constraints') or []) or '(none)'}")
        print(
            f"  workspace:     {record.get('workspace_id') or '-'} / "
            f"project {record.get('project_id') or '-'}"
        )
        location = record.get("work_location") or {}
        print(f"  work location: {location.get('kind') or '-'} {location.get('path') or ''}")
        print(f"  assignees:     {', '.join(record.get('assignees') or []) or '(unassigned)'}")
        if record.get("github"):
            print(f"  github:        {record['github']}")
        for output in record.get("outputs") or []:
            print(f"  output:        {output.get('summary')} [{output.get('execution_id')}]")
        for item in record.get("unfinished") or []:
            print(f"  unfinished:    {item}")
        print(f"  executions:    {len(executions)}")
        for execution in executions:
            reason = execution.get("failure_reason")
            print(
                f"    - {execution['id']} {execution['member_id']} {execution['tool']} "
                f"[{execution['status']}]" + (f" — {reason}" if reason else "")
            )
        return EXIT_OK
    if command == "status":
        record = context.tasks.set_status(args.task, args.status)
        print(f"Task {record['id']} is now {record['status']}")
        return EXIT_OK
    if command == "brief":
        if getattr(args, "json", False):
            _print_json(context.office.brief(args.task, coordinator=bool(args.coordinator)))
        else:
            print(context.office.brief_text(args.task, coordinator=bool(args.coordinator)))
        return EXIT_OK
    if command == "output":
        context.tasks.require(args.task)
        record = context.tasks.record_output(
            args.task,
            execution_id=args.execution,
            summary=args.summary,
            artifacts=_multi(args.artifact),
        )
        print(f"Task {record['id']} now has {len(record['outputs'])} recorded output(s)")
        return EXIT_OK
    if command == "unfinished":
        record = context.tasks.set_unfinished(args.task, _multi(args.items))
        print(f"Task {record['id']} unfinished items: {len(record['unfinished'])}")
        return EXIT_OK
    raise VivaError(f"unknown task command: {command!r}")


def _cmd_office(context: VivaContext, args: argparse.Namespace) -> int:
    command = args.office_command
    if command == "dispatch":
        record = context.office.dispatch(
            task=args.task,
            member=args.member,
            mode=args.mode,
            tool=args.tool,
            message=args.message,
            note=args.note,
            grant_id=args.grant or _env("VIVA_GRANT_ID"),
            origin_execution=args.origin_execution or _env("VIVA_EXECUTION_ID"),
        )
        handle = context.executions_handles.get(str(record["id"]))
        if handle is not None:
            context.remember_handle(handle)
        if args.wait and handle is not None:
            finished = context.office.wait(str(record["id"]))
            if getattr(args, "json", False):
                _print_json(finished)
            else:
                print(
                    f"[viva] {finished['id']} {finished['status']} "
                    f"exit={finished.get('exit_code')} in {finished.get('duration_ms')} ms"
                )
            return EXIT_OK if finished["status"] == "completed" else EXIT_ERROR
        if getattr(args, "json", False):
            _print_json(record)
        else:
            print(
                f"[viva] dispatched {record['id']}: {record['member_id']} ({record['role']}) via "
                f"{record['tool']} in {record['work_location'].get('path')} "
                f"[{record['status']}, requested by {record['request']['kind']}]"
            )
            print("        observe: viva office status · result: viva office result <id>")
        return EXIT_OK
    if command in ("status", "execs"):
        payload = context.office.status(task=args.task)
        if getattr(args, "json", False):
            _print_json(payload)
            return EXIT_OK
        if payload["tasks"]:
            print("Tasks:")
            for record in payload["tasks"]:
                print(
                    f"  {record['id']:<36} {record['kind']:<12} {record['status']:<12} "
                    f"{','.join(record['assignees']) or 'unassigned'}"
                )
        if not payload["executions"]:
            print("No executions yet.")
            return EXIT_OK
        print("Executions:")
        for record in payload["executions"]:
            print(
                f"  {record['id']:<16} {record['member_id']:<10} {record['role']:<12} "
                f"{record['worker']:<10} {record['status']:<28} task={record['task_id']}"
            )
            if record.get("failure_reason"):
                print(f"      {record['failure_reason']}")
        print(f"counts: {payload['counts']}")
        return EXIT_OK
    if command == "result":
        result = context.office.result(args.execution)
        if getattr(args, "json", False):
            _print_json(result)
            return EXIT_OK
        print(
            f"{result['id']} [{result['status']}] member={result['member_id']} "
            f"tool={result['tool']} task={result['task_id']}"
        )
        if result.get("failure_reason"):
            print(f"failure: {result['failure_reason']}")
        print(f"log: {result['output_path']} ({result['output_lines']} lines)")
        print("--- tail ---")
        for line in result["output_tail"][-max(args.tail, 0) :]:
            print(line)
        return EXIT_OK
    if command == "stop":
        record = context.office.stop(
            args.execution,
            reason=args.reason,
            grant_id=args.grant or _env("VIVA_GRANT_ID"),
            origin_execution=args.origin_execution or _env("VIVA_EXECUTION_ID"),
        )
        print(f"[viva] stopped {record['id']} ({record['status']}): {record.get('failure_reason')}")
        return EXIT_OK
    if command == "recover":
        report = context.office.recover()
        if getattr(args, "json", False):
            _print_json(report)
            return EXIT_OK
        print(
            f"reconciled: {len(report['running'])} running, {len(report['exited'])} exited, "
            f"{len(report['unknown'])} unknown, {len(report['recoverable'])} recoverable"
        )
        for execution_id in report["recoverable"]:
            record = context.executions.require(execution_id)
            print(
                f"  {execution_id} [{record['status']}] task={record['task_id']} — "
                f"see: viva task brief {record['task_id']}"
            )
        print("(recovery never restarts a finished or completed run)")
        return EXIT_OK
    if command == "grant":
        actions = [item.strip() for item in args.actions.split(",") if item.strip()]
        grant = context.office.grant(
            member=args.member,
            task=args.task,
            actions=actions,
            mode_max=args.mode_max,
            reason=args.reason,
            delegated_from=args.delegate_from,
            source=(
                {
                    "kind": "worker",
                    "id": args.origin_execution or _env("VIVA_EXECUTION_ID") or "unknown",
                }
                if args.delegate_from
                else None
            ),
        )
        if getattr(args, "json", False):
            _print_json(grant)
        else:
            print(
                f"Grant {grant['id']}: {grant['grantee']} may {','.join(grant['actions'])} on "
                f"{grant['task_id']} (max mode {grant['mode_max']}, from {grant['source']['kind']})"
            )
        return EXIT_OK
    raise VivaError(f"unknown office command: {command!r}")


def _cmd_knowledge(context: VivaContext, args: argparse.Namespace) -> int:
    command = args.knowledge_command
    if command == "add":
        provenance: dict[str, Any] = {}
        if args.task:
            provenance["task_id"] = args.task
        if args.execution:
            provenance["execution_id"] = args.execution
        if not provenance:
            raise KnowledgeError("knowledge needs provenance: pass --task and/or --execution")
        entry = context.knowledge.add(
            kind=args.kind,
            title=args.title,
            body=args.body,
            provenance=provenance,
            member_id=args.member,
            project_id=args.project,
        )
        if getattr(args, "json", False):
            _print_json(entry)
        else:
            print(
                f"Recorded {entry['id']} [{entry['kind']}, owner {entry['owner']}] {entry['title']}"
            )
        return EXIT_OK
    if command == "list":
        entries = context.knowledge.list(kind=args.kind)
        if getattr(args, "json", False):
            _print_json(entries)
            return EXIT_OK
        if not entries:
            print("No knowledge recorded yet — run: viva knowledge add --kind ...")
            return EXIT_OK
        for entry in entries:
            used = len(entry.get("used_in") or [])
            print(
                f"  {entry['id']:<14} {entry['kind']:<20} {entry['owner']:<8} "
                f"{entry['title']} (used {used}x)"
            )
        return EXIT_OK
    if command == "show":
        entry = context.knowledge.require(args.entry)
        if getattr(args, "json", False):
            _print_json(entry)
            return EXIT_OK
        print(f"{entry['id']} [{entry['kind']}] {entry['title']}")
        print(
            f"owner: {entry['owner']} member={entry.get('member_id') or '-'} "
            f"project={entry.get('project_id') or '-'}"
        )
        print(f"provenance: {entry.get('provenance')}")
        print(f"used_in: {entry.get('used_in') or []}")
        print("---")
        print(entry["body"])
        return EXIT_OK
    if command == "use":
        entry = context.knowledge.record_usage(
            args.entry, execution_id=args.execution, task_id=args.task, note=args.note
        )
        print(f"{entry['id']} recorded as used by {args.execution} (reuse evidence grows)")
        return EXIT_OK
    if command == "reuse":
        entries = context.knowledge.reuse_evidence()
        if getattr(args, "json", False):
            _print_json(entries)
            return EXIT_OK
        if not entries:
            print(
                "No reuse evidence yet: an entry counts only once a later execution "
                "records using it (viva knowledge use <id> --execution <id>)."
            )
            return EXIT_OK
        for entry in entries:
            print(f"  {entry['id']:<14} {entry['title']} — used {len(entry['used_in'])}x")
        return EXIT_OK
    raise VivaError(f"unknown knowledge command: {command!r}")


def _cmd_github(context: VivaContext, args: argparse.Namespace) -> int:
    command = args.github_command
    if command == "link":
        record = context.tasks.link_github(
            args.task, repo=args.repo, issue=args.issue, branch=args.branch, pr=args.pr
        )
        print(f"Task {record['id']} linked to {record['github']}")
        return EXIT_OK
    if command == "evidence":
        task = context.tasks.require(args.task)
        evidence = context.office.github_evidence(args.task)
        pull = evidence.get("pull_request") or {}
        if getattr(args, "json", False):
            _print_json(evidence)
            return EXIT_OK
        issue = evidence.get("issue") or {}
        print(f"Task {task['id']} → {evidence['repo']}")
        if issue:
            print(f"  issue #{issue.get('number')}: {issue.get('title')} [{issue.get('state')}]")
        if pull:
            print(
                f"  PR #{pull.get('number')}: {pull.get('title')} [{pull.get('state')}, "
                f"{'draft' if pull.get('draft') else 'ready'}]"
            )
            print(f"  checks: {evidence['check_state']} · reviews: {evidence['review_state']}")
            for check in evidence.get("checks") or []:
                print(f"    - {check.get('name')}: {check.get('state')}")
        elif evidence.get("branch"):
            print(f"  branch {evidence['branch']}: no pull request found")
        print("  (read-only: Viva never pushes, approves or merges here)")
        return EXIT_OK
    raise VivaError(f"unknown github command: {command!r}")


def _cmd_worker(context: VivaContext, args: argparse.Namespace) -> int:
    if args.worker_command != "list":  # pragma: no cover — parser restricts this
        raise VivaError(f"unknown worker command: {args.worker_command!r}")
    for entry in context.workers.list():
        print(_worker_line(entry))
    print(f"config: {context.workers.export_config_path()}")
    return EXIT_OK


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
        raise VivaError(
            f"workspace {workspace['name']!r} has no usable git repository: {summary.get('reason')}"
        )
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
        summary = " ".join(
            f"{key}={value}"
            for key, value in sorted(payload.items())
            if value not in (None, "", {}, [])
        )
        print(
            f"#{event.get('sequence')} {event.get('timestamp')} {event.get('event_type')} "
            f"resident={resident} workspace={workspace} source={event.get('source')}"
            + (f" {summary}" if summary else "")
        )
    print("(experience is not memory: nothing is promoted, recalled, or summarized)")
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


COMMANDS = {
    "status": _cmd_status,
    "resident": _cmd_resident,
    "role": _cmd_catalogue,
    "engine": _cmd_catalogue,
    "workspace": _cmd_workspace,
    "project": _cmd_project,
    "task": _cmd_task,
    "office": _cmd_office,
    "knowledge": _cmd_knowledge,
    "github": _cmd_github,
    "worker": _cmd_worker,
    "worktree": _cmd_worktree,
    "experience": _cmd_experience,
}


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        context = VivaContext(args.home)
        if args.command is None:
            return _launch_tui(context)
        handler = COMMANDS.get(args.command)
        if handler is None:  # pragma: no cover — parser restricts this
            parser.error(f"unknown command: {args.command!r}")
            return EXIT_ERROR
        return handler(context, args)
    except (
        VivaError,
        TaskError,
        ExecutionError,
        KnowledgeError,
        GitHubError,
        GrantError,
        ProjectError,
        WorkerError,
    ) as exc:
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
