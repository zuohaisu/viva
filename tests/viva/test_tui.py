"""TUI tests through Textual's headless driver (no real terminal needed)."""

from __future__ import annotations

import asyncio

from textual.widgets import Input

from viva.context import VivaContext
from viva.tui.app import VivaTui

HELP_MARKER = "Commands:"


def _run(coro):
    return asyncio.run(coro)


def test_tui_boots_shows_honest_empty_state(home):
    async def scenario():
        app = VivaTui(context=VivaContext(home))
        async with app.run_test() as pilot:
            await pilot.pause()
            panel = app._status_text
            assert "Members (0)" in panel
            assert "Memory     not implemented" in panel
            assert any(HELP_MARKER in line for line in app._log_lines)
            assert app.query_one("#prompt", Input).has_focus

    _run(scenario())


def test_tui_commands_drive_the_same_domain_layer(home, git_repo, office_tools):
    async def scenario():
        context = VivaContext(home)
        app = VivaTui(context=context)
        async with app.run_test() as pilot:
            await pilot.pause()
            prompt = app.query_one("#prompt", Input)

            prompt.value = "member add Deven --role developer --engine fake-engine"
            await pilot.press("enter")
            await pilot.pause()
            assert any("Member created: Deven" in line for line in app._log_lines)

            prompt.value = f"workspace add {git_repo} Office"
            await pilot.press("enter")
            await pilot.pause()
            assert any("Workspace selected: Office" in line for line in app._log_lines)

            prompt.value = f"project add Viva --repo {git_repo}"
            await pilot.press("enter")
            await pilot.pause()
            assert any("Project registered: viva" in line for line in app._log_lines)

            prompt.value = "task new Deliver the office loop --kind delivery --project viva"
            await pilot.press("enter")
            await pilot.pause()
            assert any("Task created: task-deliver-the-office-loop" in line for line in app._log_lines)

            prompt.value = "task list"
            await pilot.press("enter")
            await pilot.pause()
            assert any("task-deliver-the-office-loop" in line for line in app._log_lines)

            prompt.value = "status"
            await pilot.press("enter")
            await pilot.pause()
            panel = app._status_text
            assert "Deven" in panel
            assert "task-deliver-the-office-loop"[:26] in panel

            prompt.value = "memory"
            await pilot.press("enter")
            await pilot.pause()
            assert any("Memory is not implemented" in line for line in app._log_lines)

    _run(scenario())
    context = VivaContext(home)
    assert context.current_resident()["id"] == "deven"
    assert context.current_workspace()["id"] == "office"
    assert context.tasks.list()[0]["project_id"] == "viva"
    journal_types = [event["event_type"] for event in context.journal.read()]
    assert "user.command" in journal_types
    assert "member.created" in journal_types


def test_tui_dispatch_logs_the_outcome_from_the_execution_record(home, git_repo, office_tools):
    """The regression this TUI was rebuilt for: attribution follows the run."""

    async def scenario():
        context = VivaContext(home)
        app = VivaTui(context=context)
        async with app.run_test() as pilot:
            await pilot.pause()
            app.handle_command("member add Deven --role developer --engine fake-engine")
            app.handle_command("member add Alice --role reviewer --engine fake-engine")
            app.handle_command(f"workspace add {git_repo} Office")
            app.handle_command("task new Deliver it --kind delivery --repo " + str(git_repo))
            task_id = context.tasks.list()[0]["id"]
            app.handle_command(f"dispatch {task_id} deven")

            # While it runs, the human navigates somewhere else entirely.
            app.handle_command("member use Alice")
            assert context.current_resident()["id"] == "alice"

            for _ in range(200):
                await pilot.pause()
                if any("read it with" in line for line in app._log_lines):
                    break

            completion = [line for line in app._log_lines if "read it with" in line]
            assert completion, "the completion line never appeared"
            reported = [
                line for line in app._log_lines if line.startswith("[viva] exec-") and "completed" in line
            ]
            assert reported
            assert "member deven" in reported[-1]
            assert f"task {task_id}" in reported[-1]
            # And the record itself agrees.
            record = context.executions.list()[0]
            assert record["member_id"] == "deven"
            assert record["status"] == "completed"

    _run(scenario())


def test_tui_stop_leaves_other_executions_alone(home, git_repo, office_tools):
    async def scenario():
        context = VivaContext(home)
        app = VivaTui(context=context)
        async with app.run_test() as pilot:
            await pilot.pause()
            app.handle_command("member add Deven --role developer --engine fake-engine")
            app.handle_command(f"workspace add {git_repo} Office")
            app.handle_command("task new First --kind delivery --repo " + str(git_repo))
            app.handle_command("task new Second --kind delivery --repo " + str(git_repo))
            tasks = {task["title"]: task["id"] for task in context.tasks.list()}
            app.handle_command(f"dispatch {tasks['First']} deven sleep long")
            app.handle_command(f"dispatch {tasks['Second']} deven sleep long")
            for _ in range(100):
                await pilot.pause()
                if len(context.executions.running()) == 2:
                    break
            running = context.executions.running()
            assert len(running) == 2

            app.handle_command(f"stop {running[0]['id']} one task only")

            assert context.executions.require(running[0]["id"])["status"] == "stopped"
            assert context.executions.require(running[1]["id"])["status"] == "running"
            app.handle_command(f"stop {running[1]['id']} cleanup")

    _run(scenario())


def test_tui_quit_via_input(home):
    async def scenario():
        app = VivaTui(context=VivaContext(home))
        async with app.run_test() as pilot:
            await pilot.pause()
            prompt = app.query_one("#prompt", Input)
            prompt.value = "quit"
            await pilot.press("enter")
            await pilot.pause()

    _run(scenario())  # returning cleanly proves the app exited without hanging
