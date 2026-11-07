#!/usr/bin/env python3
"""Fault injection against a REAL `viva server --resume` (#196 QA F1).

The sender is a wire fixture, not a claim to run an old host. PTYs and shell
processes are real and owned by the fixture. No installed/user state is used.
"""
import argparse
import array
import hashlib
import json
import os
from pathlib import Path
import select
import shutil
import socket
import subprocess
import tempfile
import time
import uuid

from startup_handoff import poll


def echo_from_master(master, pid):
    marker = "rollback-" + uuid.uuid4().hex
    os.write(master, (marker + "\n").encode())
    expected = f"PID:{pid} ECHO:{marker}".encode()
    data = b""
    deadline = time.monotonic() + 4
    while time.monotonic() < deadline:
        if select.select([master], [], [], 0.1)[0]:
            try:
                data += os.read(master, 65536)
            except OSError:
                break
            if expected in data:
                return True
    return False


def request_any(home, kind):
    # This exercises the same bounded endpoint set as the current client.
    for name in ("server.sock", "resume.sock", "resume-next.sock"):
        try:
            with socket.socket(socket.AF_UNIX) as stream:
                stream.settimeout(1)
                stream.connect(str(home / name))
                stream.sendall(json.dumps({"request_id": str(uuid.uuid4()), "version": 2,
                                           "kind": kind}).encode() + b"\n")
                with stream.makefile("rb") as reader:
                    line = reader.readline()
                if not line:
                    continue
                response = json.loads(line)
                if not response["ok"]:
                    raise RuntimeError(response.get("error"))
                return response["result"]
        except (OSError, ValueError):
            pass
    raise RuntimeError("no owned fixture endpoint responded")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--fault", choices=("bad-second-owner", "bind-failure", "alias-obstructed"), required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    evidence = {"scope": "real receiver binary + wire sender fixture + real owned PTYs",
                "binary_name": binary.name,
                "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "fault": args.fault, "checks": {}}
    with tempfile.TemporaryDirectory(prefix="v196-", dir="/tmp") as directory:
        home = Path(directory)
        pinned = home / "receiver-binary"
        shutil.copy2(binary, pinned)
        binary = pinned
        evidence["binary_sha256"] = hashlib.sha256(binary.read_bytes()).hexdigest()
        evidence["binary_execution"] = "private immutable copy"
        environment = {k: v for k, v in os.environ.items()
                       if not k.startswith(("VIVA_", "HERDR_"))}
        environment.update(HOME=directory, VIVA_HOME=directory, TERM="xterm-256color")
        init = subprocess.run([str(binary), "init"], env=environment, capture_output=True, timeout=10)
        if init.returncode:
            raise RuntimeError("real binary init failed")
        receiver = None
        masters, children, blockers = [], [], []
        try:
            entries = []
            for index in range(2):
                master, slave = os.openpty()
                child = subprocess.Popen(
                    ["/bin/sh", "-c", 'while IFS= read -r line; do printf "PID:%s ECHO:%s\\n" "$$" "$line"; done'],
                    stdin=slave, stdout=slave, stderr=slave, start_new_session=True,
                )
                os.close(slave)
                os.set_blocking(master, False)
                masters.append(master)
                children.append(child)
                entries.append({"terminal_id": "term-" + uuid.uuid4().hex,
                                "owner": "bad-owner" if args.fault == "bad-second-owner" and index == 1 else "test_run",
                                "worktree_id": None, "purpose": "owned QA fault probe",
                                "pid": child.pid, "pid_start_marker": f"pid={child.pid}",
                                "cols": 80, "rows": 24, "history": [],
                                "screen_hex": "", "pending_hex": ""})
            if args.fault == "bind-failure":
                for name in ("resume.sock", "resume-next.sock"):
                    blocker = socket.socket(socket.AF_UNIX)
                    blocker.bind(str(home / name))
                    blocker.listen()
                    blockers.append(blocker)
            if args.fault == "alias-obstructed":
                (home / "server.sock").mkdir()
            with socket.socket(socket.AF_UNIX) as listener, (home / "receiver.log").open("wb") as log:
                listener.bind(str(home / "handoff.sock"))
                listener.listen()
                listener.settimeout(10)
                receiver = subprocess.Popen([str(binary), "server", "--resume"], env=environment,
                                            stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                            start_new_session=True)
                connection, _ = listener.accept()
                with connection:
                    connection.settimeout(10)
                    # Protocol 1 deliberately exercises compatibility with the
                    # historical sender, not only two matching current builds.
                    manifest = {"protocol": 1, "host_id": "host-owned-wire-fixture", "entries": entries}
                    connection.sendall(json.dumps(manifest).encode() + b"\n")
                    for master in masters:
                        connection.sendmsg([b"\0"], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", [master]))])
                    with connection.makefile("rb") as reader:
                        line = reader.readline()
                    evidence["ack"] = json.loads(line) if line else None
                if args.fault == "alias-obstructed":
                    evidence["checks"]["prepared_ack"] = bool(evidence["ack"] and evidence["ack"].get("prepared"))
                    # Simulate sender retirement: close its last master refs.
                    for master in masters:
                        os.close(master)
                    masters.clear()
                    status = poll(lambda: request_any(home, {"type": "ping"}), timeout=8)
                    evidence["receiver"] = status
                    terminals = request_any(home, {"type": "terminal_list"})["terminals"]
                    evidence["checks"]["all_sessions_registered"] = {t["terminal_id"] for t in terminals} == {e["terminal_id"] for e in entries}
                    for entry in entries:
                        marker = "commit-" + uuid.uuid4().hex
                        request_any(home, {"type": "terminal_input", "terminal_id": entry["terminal_id"],
                                           "bytes_hex": (marker + "\n").encode().hex()})
                        expected = f'PID:{entry["pid"]} ECHO:{marker}'
                        poll(lambda: expected in json.dumps(request_any(home, {"type": "terminal_snapshot", "terminal_id": entry["terminal_id"]})))
                    evidence["checks"]["sessions_work_after_sender_retirement"] = True
                    evidence["checks"]["obstructing_path_not_removed"] = (home / "server.sock").is_dir()
                else:
                    evidence["checks"]["no_ack_before_full_preparation"] = evidence["ack"] is None
                    evidence["checks"]["sender_sessions_still_interactive"] = all(
                        echo_from_master(master, child.pid) for master, child in zip(masters, children)
                    )
                    if evidence["ack"] is None:
                        receiver.wait(timeout=8)
                    evidence["checks"]["candidate_rejected"] = receiver.poll() not in (None, 0)
        except Exception as error:
            evidence["error"] = f"{type(error).__name__}: {error}"
        finally:
            if receiver is not None and receiver.poll() is None:
                try:
                    request_any(home, {"type": "shutdown", "close_policy": None})
                    receiver.wait(timeout=5)
                except Exception:
                    receiver.kill()
                    receiver.wait(timeout=5)
            for master in masters:
                os.close(master)
            for child in children:
                if child.poll() is None:
                    # The fixture shell has no descendants; signal only the
                    # exact Popen child, never a possibly reused process group.
                    try:
                        child.kill()
                    except ProcessLookupError:
                        pass
                child.wait(timeout=5)
            for blocker in blockers:
                blocker.close()
            evidence["cleanup_receiver_exit"] = receiver.returncode if receiver else None
            evidence["receiver_log"] = (home / "receiver.log").read_text(errors="replace") if (home / "receiver.log").exists() else ""
            evidence["passed"] = "error" not in evidence and bool(evidence["checks"]) and all(evidence["checks"].values())
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(evidence, indent=2) + "\n")
    print(json.dumps({"output": str(args.output), "passed": evidence["passed"], "checks": evidence["checks"]}))
    return 0 if evidence["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
