#!/usr/bin/env python3
"""Real-binary startup handoff probe for #191 (not Herdr cutover evidence).

Both inputs must be actual Viva binaries. All hosts, PTYs and homes belong
only to this invocation. No installed binary, user socket or user session is
used. A nonzero result retains the failed checks in the JSON evidence.
"""

import argparse
import hashlib
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import socket
import subprocess
import struct
import tempfile
import termios
import time
import uuid


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def call(home, kind, **extra):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(3)
        stream.connect(str(home / "server.sock"))
        payload = {"request_id": str(uuid.uuid4()), "version": 2, "kind": kind}
        payload.update(extra)
        stream.sendall(json.dumps(payload).encode() + b"\n")
        with stream.makefile("rb") as reader:
            line = reader.readline()
        if not line:
            raise RuntimeError("control socket closed without a response")
        try:
            response = json.loads(line)
        except json.JSONDecodeError as error:
            raise RuntimeError("control socket returned invalid JSON") from error
    if not response["ok"]:
        raise RuntimeError(response.get("error", "request rejected"))
    return response["result"]


def poll(probe, timeout=12):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            value = probe()
            if value:
                return value
        except (OSError, RuntimeError):
            pass
        time.sleep(0.05)
    raise TimeoutError("probe did not become ready")


CLIENT_OUTPUT = {}
CLIENT_ATTACHED = {}
CLIENT_REFUSED = {}


def start_client(binary, environment):
    pid, master = pty.fork()
    if pid == 0:
        os.chdir(environment["VIVA_HOME"])
        os.execve(str(binary), [str(binary)], environment)
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    os.set_blocking(master, False)
    CLIENT_OUTPUT[pid] = b""
    CLIENT_ATTACHED[pid] = False
    CLIENT_REFUSED[pid] = False
    return pid, master


def drain(clients):
    for pid, master in clients:
        if select.select([master], [], [], 0)[0]:
            try:
                data = CLIENT_OUTPUT[pid] + os.read(master, 65536)
                CLIENT_ATTACHED[pid] |= b"workbench: attached to the resident server" in data
                CLIENT_REFUSED[pid] |= (b"prepared-adoption protocol 2" in data
                                        or b"startup handoff did not complete" in data)
                CLIENT_OUTPUT[pid] = data[-4096:]
            except OSError:
                pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server-bin", type=Path, required=True)
    parser.add_argument("--candidate-bin", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--expect", choices=("handoff", "attach", "refused"), required=True)
    parser.add_argument("--occupy-prepared-endpoints", action="store_true")
    parser.add_argument("--inject-old-receiver", type=Path)
    parser.add_argument("--clients", type=int, choices=(1, 2), default=1)
    parser.add_argument("--replace-server-on-disk", type=Path)
    args = parser.parse_args()
    candidate = args.candidate_bin.resolve(strict=True)
    source = args.server_bin.resolve(strict=True)
    evidence = {
        "scope": "existing Viva runtime startup; NOT Herdr cross-backend handoff",
        "not_asserted": ["old workbench interaction, detach or visual acceptance",
                         "Herdr backend adoption", "member/worker authorization integration"],
        "server_source_name": source.name,
        "server_sha256": digest(source),
        "candidate_name": candidate.name,
        "candidate_sha256": digest(candidate),
        "expected": args.expect,
        "clients": args.clients,
        "checks": {},
    }
    clients = []
    server = None
    blockers = []
    # /tmp keeps Darwin's Unix socket path below sockaddr_un's limit.
    with tempfile.TemporaryDirectory(prefix="v191-", dir="/tmp") as directory:
        root = Path(directory)
        home = root / "state"
        home.mkdir(mode=0o700)
        environment = os.environ.copy()
        for key in list(environment):
            if key.startswith(("VIVA_", "HERDR_")):
                del environment[key]
        environment.update(
            HOME=str(root), VIVA_HOME=str(home), TERM="xterm-256color",
            SHELL="/bin/sh", XDG_CONFIG_HOME=str(root / "config"),
            XDG_DATA_HOME=str(root / "data"), XDG_CACHE_HOME=str(root / "cache"),
        )
        executable = root / "server-generation"
        shutil.copy2(source, executable)
        candidate_copy = root / "candidate-generation"
        shutil.copy2(candidate, candidate_copy)
        # A concurrent cargo build may atomically replace target/debug/viva.
        # Pin the bytes actually executed, not a digest of a mutable path.
        evidence["server_sha256"] = digest(executable)
        evidence["candidate_sha256"] = digest(candidate_copy)
        evidence["candidate_execution"] = "private immutable copy"
        with (root / "server.log").open("wb") as log:
            try:
                server = subprocess.Popen(
                    [str(executable), "server"], env=environment, cwd=home,
                    stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                    start_new_session=True,
                )
                before = poll(lambda: call(home, {"type": "ping"}))
                evidence["before"] = before
                terminal = call(home, {
                    "type": "terminal_create",
                    "argv": ["/bin/sh", "-c", 'printf "READY:%s\\n" "$$"; while IFS= read -r line; do printf "PID:%s ECHO:%s\\n" "$$" "$line"; done'],
                    "cwd": str(home), "env": [], "cols": 100, "rows": 30,
                    "purpose": "issue-191-isolated-handoff-probe", "owner": "test_run",
                    "worktree_id": None,
                })
                terminal_id = terminal["terminal_id"]
                evidence["terminal_before"] = terminal
                if args.inject_old_receiver:
                    incompatible = root / "incompatible-receiver"
                    shutil.copy2(args.inject_old_receiver.resolve(strict=True), incompatible)
                    evidence["injected_receiver_sha256"] = digest(incompatible)
                    call(home, {"type": "server_restart"}, handoff_protocol=2)
                    rejected = subprocess.run([str(incompatible), "server", "--resume"],
                        env=environment, cwd=home, stdin=subprocess.DEVNULL,
                        capture_output=True, timeout=12)
                    evidence["injected_receiver_exit"] = rejected.returncode
                    evidence["injected_receiver_error"] = rejected.stderr.decode(errors="replace")
                    evidence["checks"]["injected_old_receiver_rejected"] = rejected.returncode != 0
                    poll(lambda: call(home, {"type": "status"})["paused"] is False)
                if args.replace_server_on_disk:
                    replacement = root / "replacement"
                    shutil.copy2(args.replace_server_on_disk.resolve(strict=True), replacement)
                    os.replace(replacement, executable)
                    evidence["replacement_sha256"] = digest(executable)
                if args.occupy_prepared_endpoints:
                    for name in ("resume.sock", "resume-next.sock"):
                        blocker = socket.socket(socket.AF_UNIX)
                        blocker.bind(str(home / name))
                        blocker.listen()
                        blockers.append(blocker)
                for _ in range(args.clients):
                    clients.append(start_client(candidate_copy, environment))
                deadline = time.monotonic() + (60 if args.expect == "refused" else 15)
                after = None
                while time.monotonic() < deadline:
                    drain(clients)
                    try:
                        after = call(home, {"type": "ping"})
                        changed = after["pid"] != before["pid"]
                        ready = all(CLIENT_REFUSED.values()) if args.expect == "refused" else all(CLIENT_ATTACHED.values())
                        if changed == (args.expect == "handoff") and ready:
                            break
                    except (OSError, RuntimeError):
                        pass
                    time.sleep(0.05)
                if after is None:
                    raise RuntimeError("no healthy host after startup")
                evidence["after"] = after
                if args.expect == "refused":
                    evidence["checks"]["candidate_explicitly_refused"] = all(CLIENT_REFUSED.values())
                    evidence["checks"]["no_old_workbench_fallback"] = not any(CLIENT_ATTACHED.values())
                    evidence["candidate_diagnostics"] = [CLIENT_OUTPUT[pid].decode(errors="replace") for pid, _ in clients]
                    if args.occupy_prepared_endpoints:
                        evidence["checks"]["actionable_failure_instructions"] = all(
                            b"viva status" in CLIENT_OUTPUT[pid] for pid, _ in clients)
                else:
                    evidence["checks"]["all_client_startups_attached"] = all(CLIENT_ATTACHED.values())
                evidence["checks"]["expected_host_transition"] = (
                    (after["pid"] != before["pid"]) == (args.expect == "handoff")
                )
                evidence["checks"]["candidate_host_identity"] = (
                    after.get("binary_sha256") == evidence["server_sha256" if args.expect == "refused" else "candidate_sha256"]
                )
                # The same terminal must still accept input and return output.
                marker = "handoff-" + uuid.uuid4().hex
                call(home, {"type": "terminal_input", "terminal_id": terminal_id,
                            "bytes_hex": (marker + "\n").encode().hex()})
                snapshot = poll(lambda: matching_snapshot(home, terminal_id, marker))
                evidence["snapshot_after"] = snapshot
                evidence["checks"]["original_terminal_interactive"] = True
                evidence["checks"]["original_process_preserved"] = (
                    f'PID:{terminal["pid"]} ECHO:{marker}' in json.dumps(snapshot)
                )
                terminal_after = call(home, {"type": "terminal_list"})
                evidence["terminals_after"] = terminal_after
                if args.expect == "handoff":
                    if after["pid"] != before["pid"]:
                        server.wait(timeout=8)
                    evidence["checks"]["original_host_exited"] = server.poll() == 0
                else:
                    evidence["checks"]["original_host_still_serving"] = server.poll() is None
            except Exception as error:
                evidence["error"] = f"{type(error).__name__}: {error}"
            finally:
                for pid, master in clients:
                    try:
                        os.write(master, b"\x02q")
                    except OSError:
                        pass
                evidence["client_exit_statuses"] = []
                for pid, master in clients:
                    deadline = time.monotonic() + 3
                    status = None
                    while time.monotonic() < deadline:
                        drain([(pid, master)])
                        found, value = os.waitpid(pid, os.WNOHANG)
                        if found:
                            status = value
                            break
                        time.sleep(0.05)
                    if status is None:
                        # Bound fixture cleanup even if its TUI is wedged.
                        # Only this probe's exact child PID is signalled.
                        os.kill(pid, signal.SIGKILL)
                        kill_deadline = time.monotonic() + 5
                        while time.monotonic() < kill_deadline:
                            drain([(pid, master)])
                            found, value = os.waitpid(pid, os.WNOHANG)
                            if found:
                                status = value
                                break
                            time.sleep(0.05)
                    if status is None:
                        evidence["cleanup_error"] = f"owned client {pid} did not reap after SIGKILL"
                        evidence["client_exit_statuses"].append(None)
                    else:
                        evidence["client_exit_statuses"].append(os.waitstatus_to_exitcode(status))
                    os.close(master)
                try:
                    evidence["cleanup"] = call(home, {"type": "shutdown", "close_policy": None})
                except Exception as error:
                    evidence["cleanup_error"] = str(error)
                if server is not None and server.poll() is None:
                    try:
                        server.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        server.terminate()
                        server.wait(timeout=5)
                for blocker in blockers:
                    blocker.close()
                evidence["server_log"] = (root / "server.log").read_text(errors="replace")
                resumed_log = home / "server.log"
                if resumed_log.exists():
                    evidence["resumed_server_log"] = resumed_log.read_text(errors="replace")
                active = True
                deadline = time.monotonic() + 5
                while active and time.monotonic() < deadline:
                    try:
                        call(home, {"type": "ping"})
                        time.sleep(0.05)
                    except (OSError, RuntimeError):
                        active = False
                evidence["checks"]["owned_control_socket_stopped"] = not active
                evidence["passed"] = (
                    "error" not in evidence and "cleanup_error" not in evidence
                    and bool(evidence["checks"]) and all(evidence["checks"].values())
                )
                args.output.parent.mkdir(parents=True, exist_ok=True)
                args.output.write_text(json.dumps(evidence, indent=2) + "\n")
    print(json.dumps({"output": str(args.output), "passed": evidence["passed"],
                      "checks": evidence["checks"]}))
    return 0 if evidence["passed"] else 1


def matching_snapshot(home, terminal_id, marker):
    snapshot = call(home, {"type": "terminal_snapshot", "terminal_id": terminal_id})
    return snapshot if "ECHO:" + marker in json.dumps(snapshot) else None


if __name__ == "__main__":
    raise SystemExit(main())
