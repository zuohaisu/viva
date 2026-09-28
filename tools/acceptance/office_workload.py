#!/usr/bin/env python3
"""V12/V14 peak-workload scenario against the REAL office control plane.

What this script actually does (all real, no mocks):
- creates an isolated VIVA_HOME and initializes the office store;
- seeds one member, three tasks and three live dispatch grants (the same
  shape the owner would create interactively);
- starts the real `viva office start` host process;
- dispatches SIXTEEN real supervised terminal processes (sleep stand-ins)
  through the real UDS channel with distinct request keys;
- samples the host's process tree at peak, plus whole-machine memory;
- stops two terminals and verifies the neighbors keep running;
- shuts down gracefully and verifies no owned process survives.

Honest labeling: the sixteen workers are REAL supervised processes under the
real control plane (process-tree resource behavior of the host is a real
measurement), but they are NOT real agent CLIs doing model work. Model-CLI
peak performance stays pending; this record must never be presented as the
16-agent product workload budget result.

Usage:
    python tools/acceptance/office_workload.py \
        --binary ./target/debug/viva --out docs/validation/evidence/v12-final/peak.json
"""

from __future__ import annotations

import argparse
import json
import socket
import sqlite3
import subprocess
import sys
import time
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from sampler import (  # noqa: E402
    ExclusiveWindow,
    descendants,
    machine_profile,
    process_table,
    observed_maxima,
    system_memory,
)

WORKERS = 16
SLEEP_SECONDS = 25


def utc_now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime()) + "Z"


def seed_office(home: Path) -> dict:
    """Seed one member + three tasks + three grants via the migrated store."""
    db = home / "office.db"
    conn = sqlite3.connect(db)
    now = utc_now()
    member_id = f"mem-{uuid.uuid4().hex}"
    conn.execute(
        "INSERT INTO members(member_id, display_name, created_at) VALUES (?, ?, ?)",
        (member_id, "Samuel", now),
    )
    conn.execute(
        "INSERT INTO member_bindings(member_id, role, model_binding, tools_json, updated_at)"
        " VALUES (?, ?, ?, ?, ?)",
        (member_id, "developer", "glm-5.3-flash", '["sleep-stand-in"]', now),
    )
    seeded = {"member_id": member_id, "tasks": []}
    for i in range(3):
        task_id = f"task-{uuid.uuid4().hex}"
        grant_id = f"grant-{uuid.uuid4().hex}"
        conn.execute(
            "INSERT INTO tasks(task_id, goal, constraints_json, assignee_member_id,"
            " workspace_id, project_id, status, created_at, updated_at)"
            " VALUES (?, ?, '[]', ?, NULL, NULL, 'open', ?, ?)",
            (task_id, f"peak workload slice {i}", member_id, now, now),
        )
        conn.execute(
            "INSERT INTO grants(grant_id, parent_grant_id, principal_member_id, issued_by,"
            " task_id, actions_json, mode, status, expires_at, created_at, revoked_at, revoke_reason)"
            " VALUES (?, NULL, ?, 'user', ?, ?, 'ACT_AUTONOMOUSLY', 'live', NULL, ?, NULL, NULL)",
            (grant_id, member_id, task_id,
             json.dumps(["dispatch_delegated", "stop_delegated"]), now),
        )
        seeded["tasks"].append({"task_id": task_id, "grant_id": grant_id})
    conn.commit()
    conn.close()
    return seeded


def wait_for_socket(sock_path: Path, timeout: float = 15.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.settimeout(1.0)
            s.connect(str(sock_path))
            s.close()
            return
        except OSError:
            time.sleep(0.05)
    raise TimeoutError(f"office socket never became reachable: {sock_path}")


def viva(binary: Path, home: Path, *args: str, check: bool = True) -> dict | str:
    proc = subprocess.run(
        [str(binary), *args],
        env={"VIVA_HOME": str(home), "PATH": "/usr/bin:/bin:/usr/sbin:/sbin"},
        capture_output=True,
        text=True,
        timeout=60,
    )
    if check and proc.returncode != 0:
        raise RuntimeError(f"viva {' '.join(args)} failed: {proc.stderr.strip()}")
    if proc.stdout.strip().startswith("{") or proc.stdout.strip().startswith("["):
        return json.loads(proc.stdout)
    return proc.stdout


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--home", type=Path, required=True)
    parser.add_argument("--window", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    return _run(
        binary=args.binary.resolve(),
        home=args.home.resolve(),
        window=args.window,
        out=args.out,
    )


def _run(binary: Path, home: Path, window: Path, out: Path) -> int:
    if not binary.exists():
        raise SystemExit(f"binary missing: {binary}")
    if home.exists():
        raise SystemExit(f"refusing to reuse an existing home: {home}")

    record: dict = {
        "schema_version": 1,
        "created_at": utc_now(),
        "machine": machine_profile(),
        "scenario": "office-peak-16",
        "kind": "simulated_agents",
        "honesty_notes": [
            "16 REAL supervised PTY processes through the REAL control plane; "
            "sleep stand-ins, NOT real agent CLIs — model workload performance "
            "stays pending (no credentials/harness in this environment)",
            "role RSS maxima are per-process peaks, never additive memory",
        ],
    }

    with ExclusiveWindow(window):
        # 1. Init + seed.
        viva(binary, home, "init")
        seeded = seed_office(home)

        # 2. Start the real host. Unix sockets cannot exceed ~104 bytes on
        #    macOS; refuse early with the real reason instead of a mystery
        #    timeout.
        socket_path = home / "office.sock"
        if len(str(socket_path)) >= 104:
            raise SystemExit(
                f"VIVA_HOME path too long for a unix socket ({len(str(socket_path))} bytes): "
                f"{socket_path} — use a shorter home"
            )
        home.mkdir(parents=True, exist_ok=True)
        host = subprocess.Popen(
            [str(binary), "office", "start"],
            env={"VIVA_HOME": str(home), "PATH": "/usr/bin:/bin"},
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        try:
            wait_for_socket(socket_path)

            # 3. Dispatch 16 real supervised processes across 3 tasks
            #    (round-robin), timing every control-plane round trip.
            dispatch_seconds = []
            terminal_ids = []
            for i in range(WORKERS):
                task = seeded["tasks"][i % 3]
                start = time.monotonic()
                result = viva(
                    binary, home, "office", "dispatch",
                    "--task", task["task_id"],
                    "--member", seeded["member_id"],
                    "--grant", task["grant_id"],
                    "--request-key", f"peak-{i}",
                    "--cwd", "/tmp",
                    "--",
                    "/bin/sleep", str(SLEEP_SECONDS),
                )
                dispatch_seconds.append(round(time.monotonic() - start, 4))
                assert result.get("replayed") is False, f"dispatch {i} was a replay?!"
                terminal_ids.append(result["terminal_id"])
            record["dispatch_seconds"] = dispatch_seconds
            record["dispatch_median_seconds"] = sorted(dispatch_seconds)[len(dispatch_seconds) // 2]
            record["dispatched"] = WORKERS

            # 4. Sample at peak: host tree with all 16 children alive.
            time.sleep(0.5)
            tree = []
            snapshots = 0
            deadline = time.monotonic() + 2.0
            while time.monotonic() < deadline:
                table = process_table()
                tree.extend(descendants(table, host.pid))
                snapshots += 1
                time.sleep(0.1)
            sleeps = [r for r in tree if r.command.startswith("/bin/sleep")]
            observed_sleep_pids = {r.pid for r in sleeps}
            record["peak_sample"] = {
                "snapshot_count": snapshots,
                "totals": observed_maxima(tree),
                "system_memory": system_memory(),
                # The pids themselves, so the post-shutdown check below can
                # verify THESE processes are gone (pid-reuse caveat: the
                # check runs immediately after shutdown).
                "sleep_pids_observed": sorted(observed_sleep_pids),
                "sleep_children_observed": len(observed_sleep_pids),
            }

            # 5. Stop isolation: stop two, verify a third still live.
            status = viva(binary, home, "office", "status")
            live_before = {t["terminal_id"] for t in status["terminals"] if t["live"]}
            viva(binary, home, "office", "stop-terminal", terminal_ids[0])
            viva(binary, home, "office", "stop-terminal", terminal_ids[1])
            time.sleep(0.5)
            status = viva(binary, home, "office", "status")
            live_after = {t["terminal_id"] for t in status["terminals"] if t["live"]}
            survivor = terminal_ids[2]
            record["stop_isolation"] = {
                "stopped": [terminal_ids[0], terminal_ids[1]],
                "survivor_still_live": survivor in live_after,
                "live_before": len(live_before & set(terminal_ids)),
                "live_after": len(live_after & set(terminal_ids)),
            }
            assert record["stop_isolation"]["survivor_still_live"], "neighbor was killed"

            # 6. Graceful shutdown; verify every owned process is reaped.
            viva(binary, home, "office", "shutdown")
            host.wait(timeout=30)
            time.sleep(1.0)
            table = process_table()
            still_alive = {r.pid for r in table} & observed_sleep_pids
            record["shutdown"] = {
                "host_exit_code": host.returncode,
                "socket_removed": not socket_path.exists(),
                "owned_sleeps_still_alive": len(still_alive),
                "note": (
                    "owned_sleeps_still_alive counts the pids observed at "
                    "peak that are still running after shutdown; the office "
                    "must have reaped every one of them"
                ),
            }
            assert not still_alive, (
                f"graceful shutdown left owned processes alive: {still_alive}"
            )

            record["offline_status_after"] = viva(binary, home, "office", "status", check=False)
        finally:
            if host.poll() is None:
                host.kill()
                host.wait(timeout=10)

    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(record, indent=2))
    print(f"wrote {out}")
    return 0


def main_argless(binary: Path, home: Path, window: Path, out: Path) -> int:
    """Entry for tests: same run with explicit arguments, no argv parsing."""
    return _run(binary=binary, home=home, window=window, out=out)


if __name__ == "__main__":
    sys.exit(main())
