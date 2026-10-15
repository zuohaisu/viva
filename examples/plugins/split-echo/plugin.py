#!/usr/bin/env python3
"""Example local plugin.

Listens on VIVA_PLUGIN_SOCKET, opens a split pane with `viva scene`, and
writes the matching pane.opened event to received.json. It does not read
credentials and it does not contact a marketplace.
"""

import json
import os
import socket
import subprocess
import sys
import threading
import time


def main() -> None:
    sock_path = os.environ["VIVA_PLUGIN_SOCKET"]
    plugin_dir = os.environ["VIVA_PLUGIN_DIR"]
    viva = os.environ["VIVA_BIN"]
    received_path = os.path.join(plugin_dir, "received.json")
    ready = threading.Event()

    def listen() -> None:
        try:
            os.remove(sock_path)
        except FileNotFoundError:
            pass
        server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        server.bind(sock_path)
        os.chmod(sock_path, 0o600)
        server.listen(8)
        ready.set()
        while True:
            conn, _addr = server.accept()
            buf = b""
            while b"\n" not in buf:
                chunk = conn.recv(4096)
                if not chunk:
                    break
                buf += chunk
            conn.close()
            try:
                event = json.loads(buf.decode())
            except json.JSONDecodeError:
                continue
            if event.get("type") == "pane.opened" and event.get("open") == "split":
                with open(received_path, "w", encoding="utf-8") as handle:
                    json.dump(event, handle)
                    handle.write("\n")
                return

    thread = threading.Thread(target=listen, daemon=True)
    thread.start()
    if not ready.wait(5):
        sys.exit("event socket was not ready")

    def run(args: list[str]) -> None:
        proc = subprocess.run([viva, *args], capture_output=True, text=True)
        if proc.returncode != 0:
            sys.stderr.write(proc.stderr)
            sys.stderr.write(proc.stdout)
            raise SystemExit(proc.returncode)

    run(["scene", "create", "--id", "plugin-demo", "--name", "Plugin"])
    run(["scene", "tab", "create", "--scene", "plugin-demo", "--name", "main"])
    run(
        [
            "scene",
            "pane",
            "split",
            "--scene",
            "plugin-demo",
            "--direction",
            "right",
        ]
    )
    thread.join(timeout=5)
    while True:
        time.sleep(3600)


if __name__ == "__main__":
    main()
