"""Viva TUI — the Phase 1 product surface.

A calm status pane (resident, workspace, worktree, workers, runtime) beside a
live experience journal, with one input line for the user. Every command the
user types is recorded as an experience event; nothing here claims memory,
learning, or evolution. Built on the same VivaContext as the CLI — there is
no second code path.
"""

from __future__ import annotations

import shlex
from typing import Any

from textual.app import App, ComposeResult
from textual.widgets import Footer, Header, Input, RichLog, Static

from viva.context import VivaContext
from viva.core.errors import VivaError
from viva.workers import WorkerError, run_worker

HELP_TEXT = (
    "Commands: help · status · workspace list|add <path> [name]|use <name> · "
    "resident list|add <name>|use <name> · worker list|run <name> <message> · "
    "experience [n] · memory · clear · quit"
)


class VivaTui(App[None]):
    TITLE = "VIVA"
    SUB_TITLE = "a persistent habitat for AI agents"

    CSS = """
    #status-panel {
        width: 46;
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
        self._worker_running = False

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

    # -- rendering helpers ------------------------------------------------------

    def log_line(self, text: str) -> None:
        self._log_lines.append(text)
        self.query_one("#journal", RichLog).write(text)

    def refresh_status(self) -> None:
        status = self.context.status()
        self._status_text = self._render_status(status)
        self.query_one("#status-panel", Static).update(self._status_text)

    def _render_status(self, status: dict[str, Any]) -> str:
        lines = [f"Viva {status['version']}", ""]
        resident = status.get("resident")
        if resident:
            lines.append(f"Resident      {resident['name']}")
            lines.append(f"              id {resident['id']} · since {str(resident['created_at'])[:10]}")
        else:
            lines.append("Resident      none yet")
            lines.append("              viva resident add <name>")
        workspace = status.get("workspace")
        git = status.get("git") or {}
        if workspace:
            lines.append(f"Workspace     {workspace['name']}")
            if git.get("available"):
                current = git.get("current") or {}
                branch = current.get("branch") or git.get("branch") or "?"
                lines.append(
                    f"              git: {branch} @ {git.get('head_sha', '?')} · "
                    f"{git.get('worktree_count', '?')} worktree(s)"
                )
            else:
                lines.append(f"              {workspace['path']}")
        else:
            lines.append("Workspace     none — viva workspace add <path>")
        workers = status.get("workers") or []
        available = [entry["name"] for entry in workers if entry.get("available")]
        lines.append(f"Workers       {len(available)}/{len(workers)} available")
        if workers:
            lines.append("              " + " · ".join(
                f"{entry['name']}{'✓' if entry.get('available') else '✗'}" for entry in workers
            ))
        lines.append("Memory        not implemented (journal = experience, not memory)")
        lines.append(f"State         {status['home']} · persistent")
        return "\n".join(lines)

    @staticmethod
    def _format_event(event: dict[str, Any]) -> str:
        payload = event.get("payload") or {}
        summary = " ".join(
            f"{key}={value}" for key, value in sorted(payload.items())
            if value not in (None, "", {}, [])
        )
        base = (
            f"#{event.get('sequence')} {str(event.get('timestamp'))[:19]} "
            f"{event.get('event_type')}"
        )
        return f"{base} {summary}" if summary else base

    # -- input handling -----------------------------------------------------------

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
            elif command == "help":
                self.log_line(HELP_TEXT)
            elif command == "clear":
                self.action_clear_log()
            elif command == "status":
                self.refresh_status()
                self.log_line("Status refreshed (see left panel).")
            elif command == "memory":
                self.log_line(
                    "Memory is not implemented. What you see is the append-only "
                    "experience journal — events, not memory. Viva will not "
                    "pretend otherwise."
                )
            elif command in ("workspace", "ws"):
                self._handle_workspace(arguments)
            elif command in ("resident", "r"):
                self._handle_resident(arguments)
            elif command in ("worker", "w"):
                self._handle_worker(arguments)
            elif command in ("experience", "exp", "journal"):
                self._handle_experience(arguments)
            else:
                self.log_line(f"Unknown command {command!r}. {HELP_TEXT}")
        except (VivaError, WorkerError) as exc:
            self.log_line(f"error: {exc}")
        if command not in ("quit", "exit", "q"):
            self.refresh_status()

    def _handle_workspace(self, args: list[str]) -> None:
        if not args or args[0] == "list":
            workspaces = self.context.workspaces.list()
            if not workspaces:
                self.log_line("No workspaces yet — workspace add <path> [name]")
                return
            current = self.context.current_workspace()
            for entry in workspaces:
                marker = " *" if current and current["id"] == entry["id"] else ""
                self.log_line(f"  {entry['name']:<12} {entry['path']} ({'git' if entry.get('is_git') else 'dir'}){marker}")
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
            self.log_line(f"Workspace selected: {workspace['name']} — {workspace['path']}")
            return
        raise VivaError("usage: workspace list | add <path> [name] | use <name>")

    def _handle_resident(self, args: list[str]) -> None:
        if not args or args[0] == "list":
            residents = self.context.residents.list()
            if not residents:
                self.log_line("No residents yet — resident add <name>")
                return
            current = self.context.current_resident()
            for record in residents:
                marker = " *" if current and current["id"] == record["id"] else ""
                self.log_line(f"  {record['name']} (id {record['id']}){marker}")
            return
        if args[0] == "add" and len(args) >= 2:
            record = self.context.residents.create(" ".join(args[1:]))
            self.context.journal.append(
                event_type="resident.created", resident_id=record["id"], source="tui", payload={}
            )
            self.log_line(f"Resident created: {record['name']} (id {record['id']})")
            if self.context.current_resident() is None:
                self.context.runtime.set_current_resident(record["id"])
                self.context.journal.append(
                    event_type="resident.selected", resident_id=record["id"], source="tui", payload={}
                )
            return
        if args[0] == "use" and len(args) >= 2:
            record = self.context.residents.get(" ".join(args[1:]))
            if record is None:
                raise VivaError(f"no resident matches {' '.join(args[1:])!r}")
            self.context.runtime.set_current_resident(record["id"])
            self.context.journal.append(
                event_type="resident.selected", resident_id=record["id"], source="tui", payload={}
            )
            self.log_line(f"Resident selected: {record['name']}")
            return
        raise VivaError("usage: resident list | add <name> | use <name>")

    def _handle_worker(self, args: list[str]) -> None:
        if not args or args[0] == "list":
            for entry in self.context.workers.list():
                state = "available" if entry.get("available") else f"unavailable ({entry.get('reason')})"
                self.log_line(f"  {entry['name']:<10} {entry['command']} — {state}")
            return
        if args[0] == "run" and len(args) >= 3:
            if self._worker_running:
                raise VivaError("a worker is already running; wait for it to finish")
            self._run_worker_thread(args[1], " ".join(args[2:]))
            return
        raise VivaError("usage: worker list | run <name> <message>")

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

    # -- worker streaming --------------------------------------------------------

    def _run_worker_thread(self, name: str, message: str) -> None:
        worker = self.context.workers.get(name)
        availability = self.context.workers.probe(worker)
        if not availability["available"]:
            raise WorkerError(f"worker {worker['name']!r} is unavailable: {availability['reason']}")
        workspace = self.context.current_workspace()
        if workspace is None:
            raise VivaError("no current workspace — a worker run needs one: workspace add <path>")
        context = self.context
        session_id = context.current_session_id()
        context.journal.append(
            event_type="worker.invoked",
            resident_id=context.current_resident_id(),
            session_id=session_id,
            workspace=workspace["id"],
            source="tui",
            payload={"worker": worker["name"], "message": message},
        )
        self._worker_running = True
        self.log_line(f"[viva] running {worker['name']} in {workspace['name']}…")

        def stream(stream: str, line: str) -> None:
            self.call_from_thread(self.log_line, f"[{worker['name']}|{stream}] {line}")

        def work() -> None:
            try:
                result = run_worker(
                    worker,
                    message,
                    cwd=workspace["path"],
                    initiated_by="user",
                    on_output=stream,
                )
            except (VivaError, WorkerError) as exc:
                self.call_from_thread(self.log_line, f"error: {exc}")
                self.call_from_thread(self._finish_worker, None)
                return
            self.call_from_thread(self._finish_worker, result)

        self.run_worker(work, thread=True, exclusive=False)

    def _finish_worker(self, result: Any) -> None:
        self._worker_running = False
        if result is None:
            self.refresh_status()
            return
        self.context.journal.append(
            event_type="worker.completed" if result.status == "COMPLETED" else "worker.failed",
            resident_id=self.context.current_resident_id(),
            session_id=self.context.current_session_id(),
            workspace=(self.context.current_workspace() or {}).get("id"),
            source="tui",
            payload={
                "worker": result.worker,
                "status": result.status,
                "exit_code": result.exit_code,
                "duration_ms": result.duration_ms,
            },
        )
        self.log_line(f"[viva] {result.status} exit={result.exit_code} in {result.duration_ms} ms")
        self.refresh_status()

    # -- bindings -----------------------------------------------------------------

    def action_clear_log(self) -> None:
        log = self.query_one("#journal", RichLog)
        log.clear()
        self._log_lines.clear()
        self.log_line("(log cleared; the experience journal itself is append-only)")
