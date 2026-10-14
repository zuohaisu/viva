"""V12 final-acceptance scenario tests (issue #21).

These validate the office peak-workload scenario tooling end to end against
the real binary: seeding, host startup, 16 supervised dispatches, stop
isolation and graceful shutdown. They are NOT a substitute for the real
16-agent model workload — the scenario itself labels its workers as sleep
stand-ins, and model-CLI performance stays pending in the acceptance doc.
"""

from __future__ import annotations

import json
import shlex
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import pytest

from real_viva import REPO_ROOT, real_viva_binary

sys.path.insert(0, str(REPO_ROOT / "tools" / "acceptance"))

import office_workload  # noqa: E402


def test_peak_scenario_runs_the_real_office_control_plane() -> None:
    # A missing binary FAILS here (issue #66); it used to skip, which let CI
    # report green without ever running the scenario.
    binary = real_viva_binary()
    # A SHORT home: unix socket paths cap at ~104 bytes and pytest's default
    # tmp_path nests too deep on macOS.
    base = Path(tempfile.mkdtemp(prefix="viva-peak-"))
    try:
        home = base / "home"
        out = base / "peak.json"
        rc = office_workload.main_argless(
            binary=binary, home=home, window=base / "window.lock", out=out
        )
        assert rc == 0
        record = json.loads(out.read_text())
    finally:
        shutil.rmtree(base, ignore_errors=True)
    assert record["kind"] == "simulated_agents"
    assert record["dispatched"] == office_workload.WORKERS
    assert record["peak_sample"]["sleep_children_observed"] == office_workload.WORKERS
    assert record["stop_isolation"]["survivor_still_live"] is True
    assert record["shutdown"]["host_exit_code"] == 0
    assert record["shutdown"]["socket_removed"] is True


def _binary_with_fake_server(base: Path, real: Path, stderr_line: str, exit_code: int) -> Path:
    """A `viva` that is the real binary for everything except `server`.

    `server` writes one line to stderr and exits, standing in for a host that
    cannot start. `init` and the rest still reach the real binary, so the
    scenario gets as far as starting the host exactly as it does for real.
    """
    script = base / "viva-fake-server"
    script.write_text(
        "#!/bin/sh\n"
        'if [ "$1" = "server" ]; then\n'
        f"  echo {shlex.quote(stderr_line)} >&2\n"
        f"  exit {exit_code}\n"
        "fi\n"
        f'exec {shlex.quote(str(real))} "$@"\n'
    )
    script.chmod(0o755)
    return script


def test_host_that_exits_before_listening_reports_its_exit_code_and_stderr() -> None:
    """Before this, a host that failed to start surfaced only as
    `office socket never became reachable`, discarding the reason it printed."""
    real = real_viva_binary()
    base = Path(tempfile.mkdtemp(prefix="viva-peak-"))
    try:
        fake = _binary_with_fake_server(base, real, "boom: cannot bind the office socket", 3)
        with pytest.raises(Exception) as failure:
            office_workload.main_argless(
                binary=fake,
                home=base / "home",
                window=base / "window.lock",
                out=base / "peak.json",
            )
    finally:
        shutil.rmtree(base, ignore_errors=True)
    message = str(failure.value)
    assert "boom: cannot bind the office socket" in message
    assert "exit code 3" in message


def test_host_that_never_listens_times_out_with_its_stderr() -> None:
    base = Path(tempfile.mkdtemp(prefix="viva-peak-"))
    host = None
    try:
        log = base / "host.stderr.log"
        with log.open("wb") as stderr_file:
            host = subprocess.Popen(
                ["/bin/sh", "-c", "echo 'still starting up' >&2; exec sleep 30"],
                stderr=stderr_file,
            )
        with pytest.raises(Exception) as failure:
            office_workload.wait_for_socket(
                base / "server.sock", timeout=0.5, host=host, stderr_log=log
            )
    finally:
        if host is not None:
            host.kill()
            host.wait(timeout=10)
        shutil.rmtree(base, ignore_errors=True)
    message = str(failure.value)
    assert "still starting up" in message
    assert "still running" in message
