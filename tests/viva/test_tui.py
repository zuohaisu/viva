"""TUI tests through Textual's headless driver (no real terminal needed)."""

from __future__ import annotations

import asyncio

from textual.widgets import Input

from viva.context import VivaContext
from viva.tui.app import VivaTui


def _run(coro):
    return asyncio.run(coro)


def test_tui_boots_shows_honest_empty_state(home):
    async def scenario():
        app = VivaTui(context=VivaContext(home))
        async with app.run_test() as pilot:
            await pilot.pause()
            assert "Resident      none yet" in app._status_text
            assert "not implemented" in app._status_text  # memory honesty
            assert any(HELP_MARKER in line for line in app._log_lines)
            # Startup experience was journaled by the CLI launcher, not the TUI;
            # boot itself must not fabricate events for a session it didn't open.
            assert app.query_one("#prompt", Input).has_focus

    _run(scenario())


HELP_MARKER = "Commands:"


def test_tui_commands_drive_the_same_domain_layer(home, git_repo):
    async def scenario():
        context = VivaContext(home)
        app = VivaTui(context=context)
        async with app.run_test() as pilot:
            await pilot.pause()
            prompt = app.query_one("#prompt", Input)

            prompt.value = "resident add Test Resident"
            await pilot.press("enter")
            await pilot.pause()
            assert any("Resident created: Test Resident" in line for line in app._log_lines)

            prompt.value = f"workspace add {git_repo} Demo"
            await pilot.press("enter")
            await pilot.pause()
            assert any("Workspace selected: Demo" in line for line in app._log_lines)

            prompt.value = "status"
            await pilot.press("enter")
            await pilot.pause()
            assert "Test Resident" in app._status_text
            assert "git: main" in app._status_text

            prompt.value = "workspace list"
            await pilot.press("enter")
            await pilot.pause()
            assert any("Demo" in line and str(git_repo) in line for line in app._log_lines)

            prompt.value = "memory"
            await pilot.press("enter")
            await pilot.pause()
            assert any("Memory is not implemented" in line for line in app._log_lines)

    _run(scenario())
    # State written through the TUI persists to disk for the next session.
    context = VivaContext(home)
    assert context.current_resident()["id"] == "test-resident"
    assert context.current_workspace()["id"] == "demo"
    journal_types = [event["event_type"] for event in context.journal.read()]
    assert "user.command" in journal_types
    assert "resident.created" in journal_types
    assert "workspace.registered" in journal_types


def test_tui_worker_run_streams_and_journals(home, git_repo, fake_bin):
    from .conftest import make_fake_worker, register_worker_config

    make_fake_worker(fake_bin, "tuifake")
    register_worker_config(home, "tuifake", "tuifake")

    async def scenario():
        context = VivaContext(home)
        app = VivaTui(context=context)
        async with app.run_test() as pilot:
            await pilot.pause()
            app.handle_command(f"workspace add {git_repo} Demo")
            await pilot.pause()
            app.handle_command("worker run tuifake hello from the TUI")
            for _ in range(50):
                await pilot.pause()
                if not app._worker_running:
                    break
            assert any("[tuifake|stdout]" in line for line in app._log_lines)
            assert any("COMPLETED" in line for line in app._log_lines)

    _run(scenario())
    events = VivaContext(home).journal.read()
    worker_events = [event["event_type"] for event in events if event["event_type"].startswith("worker.")]
    assert "worker.invoked" in worker_events
    assert "worker.completed" in worker_events


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
