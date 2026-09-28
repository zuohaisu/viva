"""V12 final-acceptance scenario tests (issue #21).

These validate the office peak-workload scenario tooling end to end against
the real binary: seeding, host startup, 16 supervised dispatches, stop
isolation and graceful shutdown. They are NOT a substitute for the real
16-agent model workload — the scenario itself labels its workers as sleep
stand-ins, and model-CLI performance stays pending in the acceptance doc.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "tools" / "acceptance"))

import office_workload  # noqa: E402


def test_peak_scenario_runs_the_real_office_control_plane() -> None:
    binary = REPO_ROOT / "target" / "debug" / "viva"
    if not binary.exists():
        pytest.skip("rust binary not built in this environment")
    # A SHORT home: unix socket paths cap at ~104 bytes and pytest's default
    # tmp_path nests too deep on macOS.
    import shutil
    import tempfile

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
