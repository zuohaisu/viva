"""`viva` CLI behavior: help, status, the full mutation flow, exit codes."""

from __future__ import annotations

import json

import pytest

from viva.cli.main import EXIT_ERROR, EXIT_NO_TTY, EXIT_OK, main
from viva.context import VivaContext

from .conftest import make_fake_worker, register_worker_config


def test_help_exits_zero(capsys):
    with pytest.raises(SystemExit) as exit_info:
        main(["--help"])
    assert exit_info.value.code == 0
    assert "persistent habitat" in capsys.readouterr().out


def test_version_flag(capsys):
    with pytest.raises(SystemExit) as exit_info:
        main(["--version"])
    assert exit_info.value.code == 0
    assert capsys.readouterr().out.startswith("viva ")


def test_bare_viva_without_tty_is_refused_not_crashed(home, capsys, monkeypatch):
    monkeypatch.setattr("sys.stdout.isatty", lambda: False)
    code = main([])
    assert code == EXIT_NO_TTY
    assert "needs a terminal" in capsys.readouterr().err


def test_status_on_fresh_home_is_honest(home, capsys):
    assert main(["status"]) == EXIT_OK
    out = capsys.readouterr().out
    assert "none yet" in out
    assert "not memory" in out


def test_status_json_is_machine_readable(home, capsys):
    assert main(["status", "--json"]) == EXIT_OK
    payload = json.loads(capsys.readouterr().out)
    assert {"version", "home", "resident", "workspace", "workers", "experience_count"} <= set(payload)


def test_full_flow_resident_workspace_status(home, capsys, git_repo):
    assert main(["resident", "add", "Alice"]) == EXIT_OK
    assert main(["workspace", "add", str(git_repo), "Demo"]) == EXIT_OK
    out = capsys.readouterr().out
    assert "Resident created: Alice" in out
    assert "Workspace selected: Demo" in out

    assert main(["status"]) == EXIT_OK
    status_out = capsys.readouterr().out
    assert "Alice" in status_out
    assert "Demo" in status_out
    assert "git: main" in status_out

    context = VivaContext(None)  # a brand-new instance, same home: state survives
    assert context.current_resident()["id"] == "alice"
    assert context.current_workspace()["id"] == "demo"


def test_resident_use_switches_current(home, capsys):
    main(["resident", "add", "Alice"])
    main(["resident", "add", "Maya"])
    capsys.readouterr()
    assert main(["resident", "use", "maya"]) == EXIT_OK
    assert VivaContext(None).current_resident()["name"] == "Maya"
    assert main(["resident", "use", "ghost"]) == EXIT_ERROR


def test_home_flag_is_accepted_before_subcommand(home, tmp_path, capsys):
    alternate = tmp_path / "alternate-home"
    assert main(["--home", str(alternate), "status", "--json"]) == EXIT_OK
    assert json.loads(capsys.readouterr().out)["home"] == str(alternate)
    assert alternate.exists()


def test_worker_run_end_to_end_records_experience(home, capsys, fake_bin, git_repo):
    make_fake_worker(fake_bin, "clifake")
    register_worker_config(home, "clifake", "clifake")
    main(["resident", "add", "Alice"])
    main(["workspace", "add", str(git_repo), "Demo"])
    capsys.readouterr()

    assert main(["worker", "run", "clifake", "say", "hello"]) == EXIT_OK
    out = capsys.readouterr().out
    assert "COMPLETED" in out

    assert main(["experience", "list", "--json"]) == EXIT_OK
    events = json.loads(capsys.readouterr().out)
    types = [event["event_type"] for event in events]
    assert "worker.invoked" in types and "worker.completed" in types
    assert all(event["source"] == "cli" for event in events if event["event_type"].startswith("worker"))


def test_worker_run_refused_without_workspace(home, capsys, fake_bin):
    make_fake_worker(fake_bin, "clifake")
    register_worker_config(home, "clifake", "clifake")
    assert main(["worker", "run", "clifake", "hi"]) == EXIT_ERROR
    assert "no current workspace" in capsys.readouterr().err


def test_worker_run_refused_when_unavailable(home, capsys, tmp_path):
    main(["resident", "add", "Alice"])
    main(["workspace", "add", str(tmp_path), "Somewhere"])
    capsys.readouterr()
    assert main(["worker", "run", "definitely-missing-worker", "hi"]) == EXIT_ERROR
    err = capsys.readouterr().err
    assert "no worker named" in err


def test_worker_run_failure_is_exit_error_but_still_journaled(home, capsys, fake_bin, git_repo):
    make_fake_worker(
        fake_bin,
        "sadcli",
        body='[ "$1" = "--version" ] && { echo "sadcli 1.0"; exit 0; }\nexit 9\n',
    )
    register_worker_config(home, "sadcli", "sadcli")
    main(["resident", "add", "Alice"])
    main(["workspace", "add", str(git_repo), "Demo"])
    capsys.readouterr()
    assert main(["worker", "run", "sadcli", "go"]) == EXIT_ERROR
    capsys.readouterr()  # discard the streamed run output before parsing JSON
    assert main(["experience", "list", "--json"]) == EXIT_OK
    events = json.loads(capsys.readouterr().out)
    failed = [event for event in events if event["event_type"] == "worker.failed"]
    assert failed and failed[0]["payload"]["exit_code"] == 9


def test_worktree_list_reports_workspace_worktrees(home, capsys, git_repo):
    from ticket_autopilot.services.git_worktree import GitWorktreeService

    main(["workspace", "add", str(git_repo), "Demo"])
    service = GitWorktreeService(git_repo)
    service.create_worktree(git_repo.parent / "wt-x", "feature/x", service.head_sha())
    capsys.readouterr()

    assert main(["worktree", "list"]) == EXIT_OK
    out = capsys.readouterr().out
    assert "[main" in out and "wt-x" in out and "feature/x" in out
    assert "2 worktree(s)" in out
    assert " *" in out  # the workspace root itself is marked current

    assert main(["worktree", "list", "--workspace", "Demo"]) == EXIT_OK
    assert "wt-x" in capsys.readouterr().out


def test_unknown_resident_error_is_clean_not_traceback(home, capsys):
    assert main(["resident", "use", "nobody"]) == EXIT_ERROR
    err = capsys.readouterr().err
    assert err.startswith("viva: ")
    assert "Traceback" not in err
