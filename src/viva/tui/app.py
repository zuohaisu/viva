"""Viva TUI — the office at a glance.

A status pane (members, workspace/project, tasks, executions, tools) beside a
live journal, with one input line. Two design rules, both learned the hard way:

* **the UI's current selection is navigation, not attribution.** What you have
  selected decides where the *next* command goes; it never rewrites the
  attribution of a running execution. Completions are logged from the
  execution record itself.
* **there is no global "a worker is running" flag.** Concurrency is managed by
  the execution registry, so two members (or the same member twice) can work in
  parallel and stopping one touches nothing else.

Nothing here claims memory or growth: the journal is experience, knowledge is
curated by hand, and the pane says so.
"""

from __future__ import annotations

import shlex
from typing import Any

from textual.app import App, ComposeResult
from textual.widgets import Footer, Header, Input, RichLog, Static

from viva.context import VivaContext
from viva.core.errors import VivaError
from viva.executions import ExecutionError
from viva.github import GitHubError
from viva.permissions import GrantError
from viva.residents import InvocationUnavailable, ResidentError
from viva.tasks import TaskError
from viva.workers import WorkerError

HELP_TEXT = (
    "Commands: help · status · member list|add <name> [--role R]|use <name>|show <name> · "
    "workspace list|add <path> [name]|use <name> · project list|add <name> · "
    "task list|new <title> [--kind K]|show <id>|brief <id> · "
    "dispatch <task> <member> [read_only|write] [note] · exec list · result <exec> · "
    "stop <exec> <reason> · recover · grant <member> <task> <actions> <mode> · "
    "knowledge list|reuse · github evidence <task> · experience [n] · memory · clear · quit"
)


class VivaTui(App[None]):
    TITLE = "VIVA"
    SUB_TITLE = "Personal AI Office — Haisu's local-first office"

    CSS = """
    #status-panel {
        width: 52;
        border: round $accent;
        padding: 0 1;
        margin: 0 1 0 1;
    }
    #journal {
        border: round $panel;
        margin: 0 1 0 0;
    }
    #prompt {
        margin: 0 1;
    }
    """

    BINDINGS = [
        ("ctrl+q", "quit", "Quit"),
        ("ctrl+c", "quit", "Quit"),
        ("ctrl+l", "clear_log", "Clear log"),
    ]

    def __init__(self, context: VivaContext):
        super().__init__()
        self.context = context
        self._log_lines: list[str] = []
        self._status_text = ""
        self._watched: set[str] = set()

    # -- layout ---------------------------------------------------------------

    def compose(self) -> ComposeResult:
        yield Header(show_clock=True)
        yield Static("", id="status-panel")
        yield RichLog(id="journal", highlight=False, markup=False, max_lines=5000)
        yield Input(placeholder="Haisu >  (type help)", id="prompt")
        yield Footer()

    def on_mount(self) -> None:
        self.query_one("#prompt", Input).focus()
        self.refresh_status()
        self.log_line(f"Viva — session {self.context.current_session_id() or '(no session)'}.")
        self.log_line(HELP_TEXT)
        for event in self.context.journal.tail(10):
            self.log_line(self._format_event(event))
        self.log_line("— journal tail ends; new experiences will appear below —")
        self.set_interval(2.0, self.refresh_status)

    # -- rendering helpers ----------------------------------------------------

    def log_line(self, text: str) -> None:
        self._log_lines.append(text)
        self.query_one("#journal", RichLog).write(text)

    def refresh_status(self) -> None:
        status = self.context.status()
        self._status_text = self._render_status(status)
        self.query_one("#status-panel", Static).update(self._status_text)

    def _render_status(self, status: dict[str, Any]) -> str:
        lines = [f"Viva {status['version']}", ""]
        current = status.get("resident")
        members = status.get("residents") or []
        lines.append(f"Members ({len(members)})")
        for record in members:
            engine = record.get("engine") or {}
            marker = "*" if current and current["id"] == record["id"] else " "
            lines.append(f" {marker} {record['name']:<10} {str(record.get('role')):<11}")
            lines.append(
                f"    {str(engine.get('id')):<16} "
                f"{str(engine.get('model') or '(tool default)')}"
            )
        if not members:
            lines.append("   none — member add <name> --role <role>")
        workspace = status.get("workspace")
        projects = status.get("projects") or []
        lines.append("")
        if workspace:
            git = status.get("git") or {}
            detail = ""
            if git.get("available"):
                detail = f" · git {git.get('branch')} · {git.get('worktree_count')} wt"
            lines.append(f"Workspace  {workspace['name']}{detail}")
            if projects:
                lines.append(
                    f"Project    {projects[0].get('id')} "
                    f"({len(projects[0].get('repositories') or [])} repo)"
                )
        else:
            lines.append("Workspace  none")
        open_tasks = status.get("open_tasks") or []
        lines.append("")
        lines.append(f"Tasks ({len(open_tasks)} open)")
        for record in open_tasks[:6]:
            lines.append(
                f"   {record['id'][:26]:<26} {record['kind'][:10]:<10} {record['status'][:11]}"
            )
        if not open_tasks:
            lines.append("   none")
        running = status.get("running_executions") or []
        unresolved = status.get("unresolved_executions") or []
        lines.append("")
        lines.append(f"Executions ({len(running)} running, {len(unresolved)} unresolved)")
        for record in running[:6]:
            lines.append(
                f"   {record['id'][:14]:<14} {record['member_id'][:9]:<9} "
                f"{record['tool'][:8]:<8} pid {record.get('pid')}"
            )
        for record in unresolved[:4]:
            lines.append(f"   {record['id'][:14]:<14} {record['status']}")
        workers = status.get("workers") or []
        available = [entry["name"] for entry in workers if entry.get("available")]
        lines.append("")
        lines.append(f"Tools      {len(available)}/{len(workers)}: " + " ".join(available))
        lines.append(
            f"Knowledge  {status.get('knowledge_count', 0)} entries · "
            f"{status.get('knowledge_reuse_count', 0)} reused"
        )
        lines.append(f"Journal    {status.get('experience_count', 0)} events")
        lines.append("Memory     not implemented (journal = experience)")
        return "\n".join(lines)

    @staticmethod
    def _format_event(event: dict[str, Any]) -> str:
        payload = event.get("payload") or {}
        summary = " ".join(
            f"{key}={value}"
            for key, value in sorted(payload.items())
            if value not in (None, "", {}, [])
        )
        base = (
            f"#{event.get('sequence')} {str(event.get('timestamp'))[:19]} "
            f"{event.get('event_type')}"
        )
        return f"{base} {summary}" if summary else base

    # -- input handling -------------------------------------------------------

    def on_input_submitted(self, event: Input.Submitted) -> None:
        raw = event.value.strip()
        event.input.value = ""
        if not raw:
            return
        self.log_line(f"Haisu > {raw}")
        try:
            self.context.journal.append(
                event_type="user.command",
                resident_id=self.context.current_resident_id(),
                session_id=self.context.current_session_id(),
                workspace=(self.context.current_workspace() or {}).get("id"),
                source="tui",
                payload={"command": raw},
            )
        except VivaError as exc:
            self.log_line(f"journal error: {exc}")
        self.handle_command(raw)

    def handle_command(self, raw: str) -> None:
        try:
            tokens = shlex.split(raw)
        except ValueError:
            tokens = raw.split()
        if not tokens:
            return
        command, arguments = tokens[0].casefold(), tokens[1:]
        try:
            if command in ("quit", "exit", "q"):
                self.exit()
                return
            if command == "help":
                self.log_line(HELP_TEXT)
            elif command == "clear":
                self.action_clear_log()
            elif command == "status":
                self.refresh_status()
                self.log_line("Status refreshed (see left panel).")
            elif command == "memory":
                self.log_line(
                    "Memory is not implemented. What you see is the experience journal — "
                    "events, not memory. Curated knowledge (viva knowledge) is manual and "
                    "records its provenance; Viva will not pretend otherwise."
                )
            elif command in ("member", "resident", "r"):
                self._handle_member(arguments)
            elif command in ("workspace", "ws"):
                self._handle_workspace(arguments)
            elif command == "project":
                self._handle_project(arguments)
            elif command == "task":
                self._handle_task(arguments)
            elif command == "dispatch":
                self._handle_dispatch(arguments)
            elif command in ("exec", "execs"):
                self._handle_exec(arguments)
            elif command == "result":
                self._handle_result(arguments)
            elif command == "stop":
                self._handle_stop(arguments)
            elif command == "recover":
                report = self.context.office.recover()
                self.log_line(
                    f"reconciled: {len(report['running'])} running, "
                    f"{len(report['exited'])} exited, {len(report['unknown'])} unknown, "
                    f"{len(report['recoverable'])} recoverable (nothing is restarted)"
                )
            elif command == "grant":
                self._handle_grant(arguments)
            elif command == "knowledge":
                self._handle_knowledge(arguments)
            elif command == "github":
                self._handle_github(arguments)
            elif command in ("experience", "exp", "journal"):
                self._handle_experience(arguments)
            else:
                self.log_line(f"Unknown command {command!r}. {HELP_TEXT}")
        except (
            VivaError,
            ResidentError,
            TaskError,
            ExecutionError,
            GrantError,
            WorkerError,
            GitHubError,
            InvocationUnavailable,
        ) as exc:
            self.log_line(f"error: {exc}")
        self.refresh_status()

    # -- handlers -------------------------------------------------------------

    def _handle_member(self, args: list[str]) -> None:
        if not args or args[0] == "list":
            members = self.context.residents.list()
            if not members:
                self.log_line("No AI members yet — member add <name> --role <role>")
                return
            current = self.context.current_resident()
            for record in members:
                marker = " *" if current and current["id"] == record["id"] else ""
                engine = record.get("engine") or {}
                self.log_line(
                    f"  {record['name']:<10} {record.get('role', '?'):<12} "
                    f"{engine.get('id', '?')} / {engine.get('model') or '(tool default)'}{marker}"
                )
            return
        if args[0] == "add" and len(args) >= 2:
            positional = args[1 : _flag_index(args)]
            record = self.context.residents.create(
                " ".join(positional),
                role=_flag_value(args, "--role", "developer"),
                engine=_flag_value(args, "--engine", None),
            )
            self.log_line(
                f"Member created: {record['name']} (id {record['id']}, role {record['role']}, "
                f"engine {record['engine']['id']})"
            )
            if self.context.current_resident() is None:
                self.context.runtime.set_current_resident(record["id"])
                self.log_line(f"Member selected: {record['name']}")
            return
        if args[0] == "use" and len(args) >= 2:
            record = self.context.residents.require(" ".join(args[1:]))
            self.context.runtime.set_current_resident(record["id"])
            self.context.journal.append(
                event_type="resident.selected", resident_id=record["id"], source="tui", payload={}
            )
            self.log_line(f"Member selected: {record['name']} (this only changes navigation)")
            return
        if args[0] == "show" and len(args) >= 2:
            record = self.context.residents.require(" ".join(args[1:]))
            engine = record.get("engine") or {}
            self.log_line(
                f"{record['name']} (id {record['id']}) role={record['role']} "
                f"engine={engine.get('id')} model={engine.get('model') or '(tool default)'} "
                f"tools={','.join(record.get('tools') or [])}"
            )
            self.log_line(
                "Records and history only — Viva does not claim a proven continuous Self."
            )
            return
        if args[0] == "role" and len(args) >= 3:
            record = self.context.residents.set_role(args[1], args[2])
            self.log_line(f"Member {record['id']} role set to {record['role']}")
            return
        if args[0] == "engine" and len(args) >= 3:
            model = args[3] if len(args) >= 4 else None
            record = self.context.residents.set_engine(args[1], engine=args[2], model=model)
            self.log_line(
                f"Member {record['id']} engine rebound to {record['engine']['id']}; "
                "record and history kept"
            )
            return
        raise ResidentError(
            "usage: member list | add <name> [--role R] [--engine E] | use <name> | "
            "show <name> | role <name> <role> | engine <name> <engine> [model]"
        )

    def _handle_workspace(self, args: list[str]) -> None:
        if not args or args[0] == "list":
            workspaces = self.context.workspaces.list()
            if not workspaces:
                self.log_line("No workspaces yet — workspace add <path> [name]")
                return
            current = self.context.current_workspace()
            for entry in workspaces:
                marker = " *" if current and current["id"] == entry["id"] else ""
                self.log_line(f"  {entry['name']:<12} {entry['path']}{marker}")
            return
        if args[0] == "add" and len(args) >= 2:
            name = args[2] if len(args) >= 3 else None
            workspace = self.context.workspaces.register(args[1], name)
            self.log_line(f"Workspace registered: {workspace['name']} — {workspace['path']}")
            if self.context.current_workspace() is None:
                self.context.workspaces.use(workspace["id"])
                self.log_line(f"Workspace selected: {workspace['name']}")
            return
        if args[0] == "use" and len(args) >= 2:
            workspace = self.context.workspaces.use(" ".join(args[1:]))
            self.log_line(f"Workspace selected: {workspace['name']} (this only changes navigation)")
            return
        raise VivaError("usage: workspace list | add <path> [name] | use <name>")

    def _handle_project(self, args: list[str]) -> None:
        if not args or args[0] == "list":
            for project in self.context.projects.list():
                self.log_line(
                    f"  {project['id']:<16} {len(project.get('repositories') or [])} repo(s): "
                    + ", ".join(project.get("repositories") or [])
                )
            return
        if args[0] == "add" and len(args) >= 2:
            workspace = self.context.current_workspace()
            if workspace is None:
                raise VivaError("no current workspace — workspace add/use first")
            repositories = _flag_values(args, "--repo")
            project = self.context.projects.register(
                workspace_id=str(workspace["id"]),
                name=" ".join(args[1 : _flag_index(args)]),
                repositories=repositories,
            )
            self.log_line(f"Project registered: {project['id']}")
            return
        raise VivaError("usage: project list | add <name> [--repo <path>]")

    def _handle_task(self, args: list[str]) -> None:
        if not args or args[0] == "list":
            for record in self.context.tasks.list(open_only=True):
                self.log_line(
                    f"  {record['id']:<36} {record['kind']:<12} {record['status']:<12} "
                    f"{','.join(record.get('assignees') or []) or 'unassigned'}"
                )
            return
        if args[0] == "new" and len(args) >= 2:
            workspace = self.context.current_workspace()
            project = None
            project_name = _flag_value(args, "--project", None)
            if project_name:
                project = self.context.projects.get(project_name)
            repositories = _flag_values(args, "--repo")
            if not repositories and project:
                primary = self.context.projects.primary_repository(str(project["id"]))
                repositories = [primary] if primary else []
            record = self.context.tasks.create(
                title=" ".join(args[1 : _flag_index(args)]),
                kind=_flag_value(args, "--kind", "delivery"),
                workspace_id=str(workspace["id"]) if workspace else None,
                project_id=str(project["id"]) if project else None,
                repositories=repositories,
            )
            self.log_line(f"Task created: {record['id']} — {record['title']} [{record['kind']}]")
            return
        if args[0] == "show" and len(args) >= 2:
            record = self.context.tasks.require(args[1])
            self.log_line(
                f"{record['id']} [{record['kind']}, {record['status']}] "
                f"location={(record.get('work_location') or {}).get('path') or '-'}"
            )
            for execution in self.context.executions.list(task_id=str(record["id"])):
                self.log_line(
                    f"   exec {execution['id']} {execution['member_id']} [{execution['status']}]"
                )
            return
        if args[0] == "brief" and len(args) >= 2:
            self.log_line(self.context.office.brief_text(args[1], coordinator=True))
            return
        raise VivaError("usage: task list | new <title> [--kind K] | show <id> | brief <id>")

    def _handle_dispatch(self, args: list[str]) -> None:
        if len(args) < 2:
            raise VivaError("usage: dispatch <task-id> <member> [read_only|write] [note...]")
        task_id, member_id = args[0], args[1]
        mode = args[2] if len(args) >= 3 and args[2] in ("read_only", "write") else None
        note = " ".join(args[3:] if mode else args[2:])
        record = self.context.office.dispatch(
            task=task_id, member=member_id, mode=mode, note=note
        )
        handle = self.context.executions_handles.get(str(record["id"]))
        if handle is not None:
            self.context.remember_handle(handle)
            self._watch(handle)
        self.log_line(
            f"[viva] dispatched {record['id']}: {record['member_id']} ({record['role']}) via "
            f"{record['tool']} in {record['work_location'].get('path')}"
        )

    def _watch(self, handle: Any) -> None:
        """Wait for an execution in a thread; report from its own record."""
        execution_id = str(handle.record["id"])
        if execution_id in self._watched:
            return
        self._watched.add(execution_id)

        def work() -> None:
            try:
                finished = self.context.executions_runner.wait(handle)
            except ExecutionError as exc:  # pragma: no cover - defensive
                self.call_from_thread(self.log_line, f"execution error: {exc}")
                return
            self.call_from_thread(self._on_execution_finished, finished)

        self.run_worker(work, thread=True, exclusive=False)

    def _on_execution_finished(self, record: dict[str, Any]) -> None:
        """Log the outcome from the execution's own record — never from the UI."""
        self._watched.discard(str(record["id"]))
        location = (record.get("work_location") or {}).get("path") or "-"
        self.context.tasks.record_execution(str(record["task_id"]), record)
        self.log_line(
            f"[viva] {record['id']} {record['status']} "
            f"(member {record['member_id']}, task {record['task_id']}, {location}) "
            f"exit={record.get('exit_code')} in {record.get('duration_ms')} ms"
        )
        self.log_line(f"        read it with: viva office result {record['id']}")
        self.refresh_status()

    def _handle_exec(self, args: list[str]) -> None:
        task_id = args[1] if len(args) >= 2 else None
        for record in self.context.executions.list(task_id=task_id):
            self.log_line(
                f"  {record['id']:<16} {record['member_id']:<10} {record['tool']:<10} "
                f"{record['status']:<12} task={record['task_id']}"
            )
        return

    def _handle_result(self, args: list[str]) -> None:
        if not args:
            raise ExecutionError("usage: result <execution-id>")
        result = self.context.office.result(args[0])
        self.log_line(
            f"{result['id']} [{result['status']}] member={result['member_id']} "
            f"task={result['task_id']}"
        )
        if result.get("failure_reason"):
            self.log_line(f"failure: {result['failure_reason']}")
        for line in result["output_tail"][-20:]:
            self.log_line(f"  | {line}")
        return

    def _handle_stop(self, args: list[str]) -> None:
        if len(args) < 2:
            raise ExecutionError("usage: stop <execution-id> <reason...>")
        record = self.context.office.stop(args[0], reason=" ".join(args[1:]))
        self.log_line(f"[viva] stopped {record['id']} ({record['status']})")

    def _handle_grant(self, args: list[str]) -> None:
        if len(args) < 4:
            raise GrantError(
                "usage: grant <member> <task> <actions,csv> <read_only|write> [reason...]"
            )
        grant = self.context.office.grant(
            member=args[0],
            task=args[1],
            actions=[item.strip() for item in args[2].split(",") if item.strip()],
            mode_max=args[3],
            reason=" ".join(args[4:]) or "granted from the TUI",
        )
        self.log_line(
            f"Grant {grant['id']}: {grant['grantee']} may {','.join(grant['actions'])} on "
            f"{grant['task_id']} (max {grant['mode_max']})"
        )

    def _handle_knowledge(self, args: list[str]) -> None:
        if args and args[0] == "reuse":
            entries = self.context.knowledge.reuse_evidence()
            if not entries:
                self.log_line("No reuse evidence yet (an entry counts only after later use).")
                return
            for entry in entries:
                self.log_line(f"  {entry['id']} {entry['title']} — used {len(entry['used_in'])}x")
            return
        for entry in self.context.knowledge.list():
            self.log_line(
                f"  {entry['id']:<14} {entry['kind']:<20} {entry['owner']:<8} {entry['title']}"
            )
        return

    def _handle_github(self, args: list[str]) -> None:
        if len(args) >= 2 and args[0] == "evidence":
            task = self.context.tasks.require(args[1])
            evidence = self.context.github.evidence(task)
            pull = evidence.get("pull_request") or {}
            self.log_line(
                f"{task['id']} → {evidence['repo']} · PR #{pull.get('number')} · "
                f"checks {evidence['check_state']} · reviews {evidence['review_state']}"
            )
            return
        raise GitHubError("usage: github evidence <task-id>")

    def _handle_experience(self, args: list[str]) -> None:
        limit = 10
        if args:
            try:
                limit = max(1, int(args[0]))
            except ValueError as exc:
                raise VivaError("usage: experience [n]") from exc
        events = self.context.journal.tail(limit)
        if not events:
            self.log_line("No experiences recorded yet.")
            return
        for event in events:
            self.log_line(self._format_event(event))
        self.log_line("— experience is not memory —")

    # -- bindings -------------------------------------------------------------

    def action_clear_log(self) -> None:
        query = self.query_one("#journal", RichLog)
        query.clear()
        self._log_lines.clear()
        self.log_line("(log cleared; the experience journal itself is append-only)")


def _flag_index(args: list[str]) -> int:
    """Index of the first ``--flag`` in *args* (len(args) when there is none)."""
    for index, token in enumerate(args):
        if token.startswith("--"):
            return index
    return len(args)


def _flag_value(args: list[str], flag: str, default: Any) -> Any:
    if flag in args and args.index(flag) + 1 < len(args):
        return args[args.index(flag) + 1]
    return default


def _flag_values(args: list[str], flag: str) -> list[str]:
    return [
        args[index + 1]
        for index, token in enumerate(args)
        if token == flag and index + 1 < len(args)
    ]
