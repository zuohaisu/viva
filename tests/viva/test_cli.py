"""The CLI: the same domain layer the TUI uses, with real state on disk."""

from __future__ import annotations

import json

from viva.cli.main import main
from viva.context import VivaContext

from .conftest import make_fake_worker, write_config


def test_help_exits_zero(capsys):
    try:
        main(["--help"])
    except SystemExit as exc:  # argparse exits after printing help
        assert exc.code == 0
    assert "Personal AI Office" in capsys.readouterr().out


def test_status_on_fresh_home_is_honest(home, capsys):
    assert main(["status"]) == 0
    output = capsys.readouterr().out
    assert "Members:     0" in output
    assert "Workspace:   none" in output
    assert "Experience:  0 events" in output
    assert "append-only journal" in output


def test_member_workspace_project_task_flow(home, git_repo, office_tools, capsys):
    assert main(["workspace", "add", str(git_repo), "Office"]) == 0
    assert main(["project", "add", "Viva", "--repo", str(git_repo)]) == 0
    assert main(["resident", "add", "Deven", "--role", "developer", "--engine", "fake-engine"]) == 0
    assert main(["task", "new", "Ship the office", "--kind", "delivery", "--project", "viva"]) == 0

    output = capsys.readouterr().out
    assert "Workspace registered: Office" in output
    assert "Project registered: Viva" in output
    assert "Member created: Deven" in output
    assert "Task created: task-ship-the-office" in output

    assert main(["task", "list", "--open"]) == 0
    assert "delivery" in capsys.readouterr().out

    context = VivaContext(home)
    task = context.tasks.list()[0]
    assert task["project_id"] == "viva"
    assert task["repositories"] == [str(git_repo.resolve())]
    assert context.journal.read()  # every mutation is recorded


def test_status_shows_members_tasks_and_executions(home, git_repo, office_tools, capsys):
    main(["workspace", "add", str(git_repo), "Office"])
    main(["resident", "add", "Deven", "--role", "developer", "--engine", "fake-engine"])
    main(["task", "new", "Ship it", "--kind", "delivery", "--repo", str(git_repo)])
    capsys.readouterr()

    assert main(["status"]) == 0
    output = capsys.readouterr().out
    assert "Deven" in output and "developer" in output
    assert "fake-engine" in output
    assert "Tasks:       1 total, 1 open" in output
    assert "Tools:" in output

    assert main(["status", "--json"]) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["residents"][0]["id"] == "deven"
    assert payload["open_tasks"][0]["id"].startswith("task-ship-it")


def test_dispatch_status_result_and_recover(home, git_repo, office_tools, capsys):
    main(["workspace", "add", str(git_repo), "Office"])
    main(["resident", "add", "Deven", "--role", "developer", "--engine", "fake-engine"])
    main(["task", "new", "Ship it", "--kind", "delivery", "--repo", str(git_repo)])
    task_id = VivaContext(home).tasks.list()[0]["id"]
    capsys.readouterr()

    assert main(["office", "dispatch", "--task", task_id, "--to", "deven"]) == 0
    assert "dispatched" in capsys.readouterr().out

    assert main(["office", "status", "--json"]) == 0
    payload = json.loads(capsys.readouterr().out)
    execution_id = payload["executions"][0]["id"]
    assert payload["executions"][0]["requested_by"] == {"kind": "user", "id": "haisu"}

    assert main(["office", "result", execution_id]) == 0
    assert "fakeworker start" in capsys.readouterr().out

    assert main(["office", "recover"]) == 0
    recovered = capsys.readouterr().out
    assert "reconciled:" in recovered
    assert "never restarts" in recovered


def test_foreground_dispatch_waits_and_reports(home, git_repo, office_tools, capsys):
    main(["workspace", "add", str(git_repo), "Office"])
    main(["resident", "add", "Deven", "--role", "developer", "--engine", "fake-engine"])
    main(["task", "new", "Ship it", "--kind", "delivery", "--repo", str(git_repo)])
    task_id = VivaContext(home).tasks.list()[0]["id"]
    capsys.readouterr()

    assert main(["office", "dispatch", "--task", task_id, "--to", "deven", "--wait"]) == 0
    assert "completed exit=0" in capsys.readouterr().out


def test_grant_without_origin_is_refused(home, git_repo, office_tools, capsys):
    main(["resident", "add", "Deven", "--role", "developer", "--engine", "fake-engine"])
    main(["task", "new", "Ship it", "--kind", "delivery", "--repo", str(git_repo)])
    task_id = VivaContext(home).tasks.list()[0]["id"]
    capsys.readouterr()

    assert main(["office", "grant", "--to", "deven", "--task", task_id, "--actions", "dispatch",
                 "--mode-max", "write", "--reason", "scope"]) == 0
    grant_id = VivaContext(home).grants.list()[0]["id"]
    capsys.readouterr()

    # A grant may only be spent by a worker that names the execution it came from.
    assert main(["office", "dispatch", "--task", task_id, "--to", "deven", "--grant", grant_id]) == 1
    assert "must name the execution it came from" in capsys.readouterr().err


def test_worker_list_and_worktree_list(home, git_repo, office_tools, capsys):
    main(["workspace", "add", str(git_repo), "Office"])
    capsys.readouterr()
    assert main(["worker", "list"]) == 0
    assert "fakeworker" in capsys.readouterr().out
    assert main(["worktree", "list"]) == 0
    assert "worktree(s)" in capsys.readouterr().out


def test_knowledge_and_github_commands(home, git_repo, office_tools, capsys):
    main(["task", "new", "Ship it", "--kind", "research", "--repo", str(git_repo)])
    task_id = VivaContext(home).tasks.list()[0]["id"]
    capsys.readouterr()

    assert (
        main(
            [
                "knowledge",
                "add",
                "--kind",
                "team_knowledge",
                "--title",
                "Audit first",
                "--body",
                "Read-only pass before edits.",
                "--task",
                task_id,
            ]
        )
        == 0
    )
    assert "Recorded kb-" in capsys.readouterr().out
    assert main(["knowledge", "list"]) == 0
    assert "Audit first" in capsys.readouterr().out
    assert main(["knowledge", "reuse"]) == 0
    assert "No reuse evidence yet" in capsys.readouterr().out

    assert main(["knowledge", "add", "--kind", "team_knowledge", "--title", "No provenance",
                 "--body", "x"]) == 1
    assert "provenance" in capsys.readouterr().err

    assert main(["github", "link", task_id, "--repo", "zuohaisu/viva", "--issue", "150"]) == 0
    assert "linked to" in capsys.readouterr().out


def test_errors_are_reported_not_swallowed(home, capsys):
    assert main(["resident", "show", "nobody"]) == 1
    assert "no AI member matches" in capsys.readouterr().err
    assert main(["office", "result", "exec-missing"]) == 1
    assert "no execution" in capsys.readouterr().err
    assert main(["office", "dispatch", "--task", "task-nope", "--to", "deven"]) == 1
    assert "no task" in capsys.readouterr().err


def test_unavailable_tool_is_reported_and_not_substituted(
    home, git_repo, fake_bin, capsys, monkeypatch
):
    make_fake_worker(fake_bin, "flakyworker")
    write_config(
        home,
        "workers.json",
        {
            "schema_version": "1.0",
            "workers": [{"name": "flakyworker", "command": "flakyworker", "args": []}],
        },
    )
    write_config(
        home,
        "engines.json",
        {
            "schema_version": "1.0",
            "engines": [
                {
                    "id": "flaky",
                    "tool": "flakyworker",
                    "model": "m",
                    "model_flag": "--model",
                    "args": [],
                }
            ],
        },
    )
    main(["resident", "add", "Deven", "--role", "developer", "--engine", "flaky"])
    main(["task", "new", "Ship it", "--kind", "delivery", "--repo", str(git_repo)])
    task_id = VivaContext(home).tasks.list()[0]["id"]
    capsys.readouterr()

    monkeypatch.setenv("PATH", "/nonexistent-bin")
    assert main(["office", "dispatch", "--task", task_id, "--to", "deven"]) == 1
    error = capsys.readouterr().err
    assert "unavailable" in error
    assert "will not silently substitute" in error
    assert VivaContext(home).executions.list() == []
