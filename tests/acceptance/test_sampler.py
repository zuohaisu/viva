"""V12 G1 acceptance-sampler tests (issue #21).

These tests validate the sampling method itself — classification, window
exclusivity, tree walking, real latency probes and budget comparison. They
do not run the full 16-agent workload and do not constitute the V12 final
acceptance; they prove the method is runnable and honest.
"""

from __future__ import annotations

import json
import subprocess
import sys
import time
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "tools" / "acceptance"))

import sampler  # noqa: E402


def test_classification_separates_host_worker_browser_build() -> None:
    assert sampler.classify_process("./target/debug/viva doctor") == "host"
    assert sampler.classify_process("/usr/local/bin/pi --model x") == "worker"
    assert sampler.classify_process("chrome --type=renderer") == "browser"
    assert sampler.classify_process("cargo build --workspace") == "build"
    assert sampler.classify_process("vim notes.md") == "other"


def test_exclusive_window_blocks_a_second_holder(tmp_path: Path) -> None:
    lock = tmp_path / "window.lock"
    with sampler.ExclusiveWindow(lock):
        with pytest.raises(sampler.WindowBusy):
            with sampler.ExclusiveWindow(lock):
                pass  # pragma: no cover - must not be reached
    # After release the window is reusable.
    with sampler.ExclusiveWindow(lock):
        pass


def test_process_table_and_descendants_of_a_real_child(tmp_path: Path) -> None:
    script = tmp_path / "sleeper.sh"
    script.write_text("sleep 5\n")
    child = subprocess.Popen(["/bin/sh", str(script)])
    try:
        time.sleep(0.2)
        table = sampler.process_table()
        tree = sampler.descendants(table, child.pid)
        assert any(row.pid == child.pid for row in tree)
        assert all(row.rss_kb >= 0 for row in tree)
    finally:
        child.terminate()
        child.wait(timeout=10)


def test_role_totals_carry_the_non_additive_note() -> None:
    rows = [
        sampler.ProcessRow(1, 0, "host", 10_000, 1.0, "viva"),
        sampler.ProcessRow(2, 1, "worker", 50_000, 2.0, "pi"),
    ]
    totals = sampler.role_totals(rows)
    assert totals["roles"]["host"]["processes"] == 1
    assert totals["roles"]["worker"]["rss_kb_max"] == 50_000
    assert "NOT total physical memory" in totals["note"]


def test_launch_latency_measures_real_wall_time() -> None:
    result = sampler.launch_latency([sys.executable, "-c", "pass"], repeats=3)
    assert result["repeats"] == 3
    assert all(t > 0 for t in result["seconds"])
    assert result["kind"] == "real"


def test_pty_echo_round_trip_is_real_and_bounded() -> None:
    result = sampler.echo_latency_pty(timeout=5.0)
    assert result["timed_out"] is False, "a healthy PTY echo must answer"
    assert result["seconds"] is not None and result["seconds"] < 5.0
    assert result["kind"] == "real"


def test_budget_comparison_reports_missing_not_passing() -> None:
    record = {"resident_tree": {"totals": {"roles": {"host": {"rss_kb_max": 100}}}}}
    budget = {
        "metrics": {
            "resident_tree.totals.roles.host.rss_kb_max": {"max_bytes": 150_000},
            "response.doctor.median_seconds": {"max_seconds": 0.5},
        }
    }
    checks = sampler.compare_to_budget(record, budget)
    by_metric = {c["metric"]: c for c in checks}
    assert by_metric["resident_tree.totals.roles.host.rss_kb_max"]["status"] == "within"
    assert by_metric["response.doctor.median_seconds"]["status"] == "missing"


def test_observed_maxima_deduplicate_transient_snapshots() -> None:
    rows = [
        sampler.ProcessRow(10, 1, "host", 2_000, 3.0, "viva doctor"),
        sampler.ProcessRow(10, 1, "host", 2_500, 1.0, "viva doctor"),
        sampler.ProcessRow(11, 1, "other", 1_000, 0.0, "/bin/sh -c loop"),
    ]
    totals = sampler.observed_maxima(rows)
    assert totals["roles"]["host"]["pids_observed"] == 1
    assert totals["roles"]["host"]["rss_kb_max"] == 2_500
    assert totals["roles"]["other"]["pids_observed"] == 1
    assert "never additive" in totals["note"]


def test_machine_profile_names_the_target() -> None:
    profile = sampler.machine_profile()
    assert profile["cpu_count"] >= 1
    assert profile["system"] in ("Darwin", "Linux")
    assert "total_memory_bytes" in profile


def test_slice_cli_workload_record_is_schema_valid(tmp_path: Path) -> None:
    binary = REPO_ROOT / "target" / "debug" / "viva"
    if not binary.exists():
        pytest.skip("rust binary not built in this environment")
    record = sampler.make_record(
        workload=sampler.slice_cli_workload(binary, tmp_path / "home"),
        window_path=tmp_path / "window.lock",
    )
    assert record["schema_version"] == sampler.RECORD_SCHEMA_VERSION
    assert record["window"]["exclusive"] is True
    assert record["workload"]["kind"] == "real"
    assert all(step["exit_code"] == 0 for step in record["workload"]["steps"]), (
        json.dumps(record["workload"]["steps"], indent=2)
    )
    assert record["step_peaks"], "per-step peak RSS must be measured"
    for peak in record["step_peaks"]:
        assert peak["supported"], peak
        assert peak["exit_code"] == 0, peak
        assert peak["max_rss_kb"] and peak["max_rss_kb"] > 0, peak
    json.dumps(record)  # the record must be serializable for the evidence file
