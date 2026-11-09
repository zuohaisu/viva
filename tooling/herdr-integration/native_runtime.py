#!/usr/bin/env python3
"""Real library-linked Viva/Herdr client probe; NOT human visual/install acceptance.

Reuse the imported client/daemon/PTY implementation. Only a private temporary
HOME/VIVA_HOME is used. Detach is measured separately from forced fixture cleanup.
No member execution/task completion or old-Viva -> Herdr inheritance is claimed.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import pty
import sqlite3
import select
import signal
import shutil
import socket
import struct
import subprocess
import tempfile
import time
import fcntl
import termios


def rpc(home, method, params=None):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(3)
        connection.connect(str(home / "runtime/workbench/herdr.sock"))
        connection.sendall((json.dumps({"id": "native-fixture", "method": method, "params": params or {}}) + "\n").encode())
        with connection.makefile("rb") as source:
            return json.loads(source.readline())


def process_identity(pid):
    try:
        return subprocess.check_output(
            ["ps", "-p", str(pid), "-o", "lstart=", "-o", "command="],
            stderr=subprocess.DEVNULL, timeout=3).decode().strip()
    except subprocess.CalledProcessError:
        return None


def probe(binary):
    packet = {"schema": 1, "binary": binary.name,
              "human_visual": "HUMAN_VISUAL_REVIEW_PENDING",
              "cross_backend_inheritance": "not_tested", "passed": False}
    with tempfile.TemporaryDirectory(prefix="vnui-", dir="/tmp") as folder:
        root = Path(folder)
        private_binary = root / "viva"
        shutil.copyfile(binary, private_binary)
        private_binary.chmod(0o700)
        binary = private_binary
        packet["binary_sha256"] = hashlib.sha256(binary.read_bytes()).hexdigest()
        packet["immutable_private_binary"] = True
        home = root / "v"
        runtime = home / "runtime/workbench"
        runtime.mkdir(parents=True)
        (runtime / "config.toml").write_text(
            "[terminal]\ndefault_shell = '/bin/sh'\nshell_mode = 'non_login'\n"
            "[ui.sound]\nenabled = false\n")
        user_home = root / "h"
        user_home.mkdir()
        environment = {"PATH": "/usr/bin:/bin", "HOME": str(user_home),
                       "VIVA_HOME": str(home), "SHELL": "/bin/sh",
                       "TERM": "xterm-256color", "LANG": "en_US.UTF-8"}
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 120, 0, 0))
        client = None
        server_pid = None
        server_identity = None
        frame = bytearray()

        def drain(seconds):
            end = time.monotonic() + seconds
            while time.monotonic() < end:
                if select.select([master], [], [], min(.1, max(0, end - time.monotonic())))[0]:
                    try:
                        data = os.read(master, 65536)
                        if not data:
                            break
                        frame.extend(data)
                    except OSError:
                        break

        try:
            client = subprocess.Popen([str(binary), "runtime"], env=environment,
                                      cwd=root, stdin=slave, stdout=slave, stderr=slave,
                                      start_new_session=True)
            os.close(slave)
            slave = None
            deadline = time.monotonic() + 20
            snapshot = None
            while time.monotonic() < deadline:
                drain(.1)
                if client.poll() is not None:
                    raise AssertionError("client exited before ready: " + frame.decode(errors="replace")[-2500:])
                try:
                    snapshot = rpc(home, "viva.snapshot")["result"]["snapshot"]
                    server_pid = snapshot["runtime_pid"]
                    break
                except (OSError, KeyError, ValueError):
                    continue
            assert snapshot and snapshot["backend"] == "herdr-source-library", "native domain endpoint not ready"
            assert server_pid != client.pid, "client is not a detached native host"
            server_identity = process_identity(server_pid)
            assert server_identity and str(binary) + " runtime server" in server_identity, "unexpected private host identity"
            packet["native_autospawn"] = True
            packet["canonical_domain_endpoint"] = True
            invalid = rpc(home, "viva.client.authenticate", {"credential": "fixture-bootstrap-never-log", "claimed_role": "user"})
            packet["invalid_capability_response_code"] = invalid.get("error", {}).get("code")
            packet["invalid_capability_refused"] = packet["invalid_capability_response_code"] == "untrusted_client"
            # A different, real same-UID process (this controller) has no client
            # binding. EOF/reset is evidence; a timeout is NOT a refusal/PASS.
            with socket.socket(socket.AF_UNIX) as unbound:
                unbound.settimeout(1)
                unbound.connect(str(runtime / "herdr-client.sock"))
                try:
                    packet["unbound_raw_client_refused"] = unbound.recv(1) == b""
                except ConnectionResetError:
                    packet["unbound_raw_client_refused"] = True
                except socket.timeout:
                    packet["unbound_raw_client_refused"] = False
            drain(2)
            # Command bytes do not contain the marker: seeing it in a rendered
            # frame proves execution output, not just PTY input echo.
            os.write(master, b"VN_STATE=restored; printf '\\126\\111\\126\\101_\\116\\101\\124\\111\\126\\105_\\120\\124\\131_\\117\\113\\n'\r")
            deadline = time.monotonic() + 12
            while time.monotonic() < deadline and b"VIVA_NATIVE_PTY_OK" not in frame:
                drain(.2)
            packet["native_pty_output"] = b"VIVA_NATIVE_PTY_OK" in frame
            assert packet["native_pty_output"], "native PTY output marker not rendered"
            packet["ansi_bytes"] = len(frame)
            assert b"\x1b[" in frame, "native renderer produced no ANSI frame"
            # Imported local detach contract is ctrl+b q, not a host stop.
            os.write(master, b"\x02")
            time.sleep(.08)
            os.write(master, b"q")
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline and client.poll() is None:
                drain(.1)
            packet["detach_exit_code"] = client.poll()
            assert client.poll() == 0, "native client did not detach cleanly"
            after = rpc(home, "viva.snapshot")["result"]["snapshot"]
            packet["same_host_after_detach"] = (after["runtime_pid"] == server_pid and
                                                  after["runtime_host_id"] == snapshot["runtime_host_id"])
            assert packet["same_host_after_detach"], "detach replaced or stopped the host"
            os.close(master)
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 120, 0, 0))
            client = subprocess.Popen([str(binary), "runtime"], env=environment,
                                      cwd=root, stdin=slave, stdout=slave, stderr=slave,
                                      start_new_session=True)
            os.close(slave)
            slave = None
            frame.clear()
            drain(2)
            os.write(master, b"printf '\\126\\111\\126\\101_\\116\\101\\124\\111\\126\\105_\\122\\105\\101\\124\\124\\101\\103\\110_%s\\n' \"$VN_STATE\"\r")
            deadline = time.monotonic() + 12
            while time.monotonic() < deadline and b"VIVA_NATIVE_REATTACH_restored" not in frame:
                drain(.2)
            packet["reattach_retains_shell_state"] = b"VIVA_NATIVE_REATTACH_restored" in frame
            assert packet["reattach_retains_shell_state"], "native reattach lost live shell state"
            reattached = rpc(home, "viva.snapshot")["result"]["snapshot"]
            assert reattached["runtime_host_id"] == snapshot["runtime_host_id"], "reattach changed host incarnation"
            os.write(master, b"\x02")
            time.sleep(.08)
            os.write(master, b"q")
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline and client.poll() is None:
                drain(.1)
            assert client.poll() == 0, "reattached client did not detach cleanly"
            # Assertions come after actual PTY/reattach so the recorded pre-fix
            # run can show working native input without credential admission.
            assert packet["unbound_raw_client_refused"], "unbound same-UID raw client was not closed before hello/input"
            assert packet["invalid_capability_refused"], "invalid capability did not receive the authentication refusal contract"
            with sqlite3.connect(str(home / "viva.db")) as database:
                rows = database.execute("SELECT payload FROM events WHERE kind='native_client_admitted'").fetchall()
            packet["owner_frontend_admitted_processes"] = len({json.loads(row[0])["transport_peer"]["pid"] for row in rows})
            assert packet["owner_frontend_admitted_processes"] >= 2, "initial and reattached frontends lack canonical admission evidence"
            packet["passed"] = True
        except Exception as error:
            packet["error"] = str(error)
            # Task-owned /tmp paths are disclosed; no production data is loaded.
            packet["frame_tail"] = frame.decode(errors="replace")[-2500:]
        finally:
            if client and client.poll() is None:
                client.kill()
                client.wait(timeout=5)
                packet["client_forced_cleanup"] = True
            if server_pid is None and (home / "viva.db").exists():
                # The real host records attach before binding. Recover its exact
                # fixture PID even when readiness/API decoding failed.
                try:
                    with sqlite3.connect(str(home / "viva.db")) as database:
                        row = database.execute("SELECT payload FROM events WHERE kind='native_domain_attached' ORDER BY seq DESC LIMIT 1").fetchone()
                    if row:
                        server_pid = json.loads(row[0])["pid"]
                        server_identity = process_identity(server_pid)
                except (sqlite3.Error, KeyError, ValueError):
                    pass
            if server_pid:
                identity = process_identity(server_pid)
                if identity is None:
                    packet["server_fixture_cleanup"] = "already exited"
                elif identity != server_identity or str(binary) + " runtime server" not in identity:
                    packet["server_fixture_cleanup"] = "identity changed; no signal sent"
                    packet["passed"] = False
                else:
                    os.kill(server_pid, signal.SIGTERM)
                    deadline = time.monotonic() + 5
                    while time.monotonic() < deadline and process_identity(server_pid) == identity:
                        time.sleep(.05)
                    if process_identity(server_pid) == identity:
                        os.kill(server_pid, signal.SIGKILL)
                        packet["server_forced_cleanup"] = True
                        deadline = time.monotonic() + 5
                        while time.monotonic() < deadline and process_identity(server_pid) == identity:
                            time.sleep(.05)
                    packet["server_fixture_exited"] = process_identity(server_pid) != identity
                    packet["server_fixture_cleanup"] = "exact private PID/start/argv verified; not a lifecycle acceptance claim"
                    if not packet["server_fixture_exited"]:
                        packet["passed"] = False
            if slave is not None:
                os.close(slave)
            os.close(master)
    return packet


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--viva", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    result = probe(arguments.viva.resolve())
    arguments.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
    raise SystemExit(0 if result["passed"] else 1)
