#!/usr/bin/env python3
"""V12 acceptance sampler (issue #21, gate G1).

A runnable, repeatable sampling method for Viva's resource, response and
fault-recovery baselines. It is deliberately stdlib-only so it runs on any
macOS/Linux host without installing the product.

Honesty rules encoded here (issue #21):
- Host, worker/descendant, browser and whole-machine pressure are sampled
  separately. RSS of one process includes pages shared with others, so role
  totals are reported per role and never presented as additive physical
  memory. The whole-machine view comes from the OS (vm_stat / meminfo), not
  from adding process RSS.
- Every sampling run holds an exclusive window (flock). Two samplers — or a
  sampler and another agent's build — must not overlap; contention is an
  error, not a silent merge.
- Response metrics are real wall-clock measurements of real processes. A
  simulated agent only validates control logic; it is labelled `simulated`
  in the record and can never stand in for real-agent performance evidence.

Usage:
    python tools/acceptance/sampler.py profile
    python tools/acceptance/sampler.py sample --workload slice-cli \
        --binary ./target/debug/viva --home /tmp/viva-v12 --out record.json
    python tools/acceptance/sampler.py build-times --binary ./target/debug/viva
"""

from __future__ import annotations

import argparse
import fcntl
import json
import os
import platform
import pty
import select
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path

RECORD_SCHEMA_VERSION = 1
WINDOW_POLL_SECONDS = 0.2
DEFAULT_WINDOW_SECONDS = 120


class WindowBusy(RuntimeError):
    """Another sampler holds the exclusive window."""


class ExclusiveWindow:
    """An exclusive sampling window, enforced with an flock on a lock file.

    The issue requires that sampling windows are exclusive: no other agent's
    build or sampler may run inside the measured window. The lock is
    advisory-but-enforced between samplers that use this module, and the
    record notes the window id so a reader can verify exclusivity was
    attempted and for how long the window was held.
    """

    def __init__(self, path: Path, hold_hint_seconds: int = DEFAULT_WINDOW_SECONDS) -> None:
        self.path = Path(path)
        self.hold_hint_seconds = hold_hint_seconds
        self._fd: int | None = None
        self.opened_at: float | None = None

    def __enter__(self) -> "ExclusiveWindow":
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._fd = os.open(self.path, os.O_CREAT | os.O_RDWR, 0o600)
        deadline = time.monotonic() + 2.0
        while True:
            try:
                fcntl.flock(self._fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except OSError:
                if time.monotonic() >= deadline:
                    os.close(self._fd)
                    self._fd = None
                    raise WindowBusy(
                        f"sampling window {self.path} is held by another run; "
                        "sampling must not overlap other agents' builds"
                    ) from None
                time.sleep(WINDOW_POLL_SECONDS)
        os.ftruncate(self._fd, 0)
        os.write(self._fd, f"pid={os.getpid()} started={time.time()}\n".encode())
        self.opened_at = time.monotonic()
        return self

    def __exit__(self, *_exc: object) -> None:
        if self._fd is not None:
            fcntl.flock(self._fd, fcntl.LOCK_UN)
            os.close(self._fd)
            self._fd = None


# ---------------------------------------------------------------------------
# Machine profile
# ---------------------------------------------------------------------------


def machine_profile() -> dict:
    """Identify the target machine: chip, OS, memory, CPU count."""
    profile = {
        "system": platform.system(),
        "release": platform.release(),
        "machine": platform.machine(),
        "python": platform.python_version(),
        "cpu_count": os.cpu_count(),
    }
    if platform.system() == "Darwin":
        model = _sysctl("hw.model")
        chip = _sysctl("machdep.cpu.brand_string")
        mem = _sysctl("hw.memsize")
        profile["model"] = model
        profile["cpu_brand"] = chip
        if mem and mem.isdigit():
            profile["total_memory_bytes"] = int(mem)
    elif platform.system() == "Linux":
        mem = _meminfo_kb().get("MemTotal")
        if mem is not None:
            profile["total_memory_bytes"] = mem * 1024
    return {k: v for k, v in profile.items() if v is not None}


def _sysctl(name: str) -> str | None:
    try:
        out = subprocess.run(
            ["sysctl", "-n", name], capture_output=True, text=True, check=True
        )
        return out.stdout.strip() or None
    except (subprocess.SubprocessError, OSError):
        return None


# ---------------------------------------------------------------------------
# Process sampling: roles are separate, RSS is never summed into a total
# ---------------------------------------------------------------------------

# Role classification patterns, matched against argv[0] basename. Order
# matters: first match wins. Workers are the agent CLIs and helper processes
# a member execution may spawn; browsers are sampled separately because they
# carry their own (large, shared) pages.
ROLE_PATTERNS: list[tuple[str, tuple[str, ...]]] = [
    ("host", ("viva",)),
    ("worker", ("pi", "codex", "claude", "aider", "gemini", "cursor-agent")),
    ("browser", ("chrome", "google-chrome", "chromium", "safari", "firefox")),
    ("build", ("cargo", "rustc", "clang", "cc", "ld", "make")),
]


def classify_process(args_line: str) -> str:
    """Classify by argv[0] basename. Matching on the whole command line would
    mislabel a shell driver that merely mentions the binary in its script."""
    argv0 = args_line.split()[0] if args_line.split() else ""
    basename = argv0.rsplit("/", 1)[-1].lower()
    for role, patterns in ROLE_PATTERNS:
        for pattern in patterns:
            if basename == pattern or basename.startswith(pattern + "-"):
                return role
    return "other"


@dataclass
class ProcessRow:
    pid: int
    ppid: int
    role: str
    rss_kb: int
    cpu_pct: float
    command: str


def process_table() -> list[ProcessRow]:
    out = subprocess.run(
        ["ps", "-eo", "pid=,ppid=,rss=,pcpu=,args="],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    rows: list[ProcessRow] = []
    for line in out.splitlines():
        parts = line.split(None, 5)
        if len(parts) < 6:
            continue
        try:
            rows.append(
                ProcessRow(
                    pid=int(parts[0]),
                    ppid=int(parts[1]),
                    rss_kb=int(parts[2]),
                    cpu_pct=float(parts[3]),
                    role=classify_process(parts[5]),
                    command=parts[5].strip(),
                )
            )
        except ValueError:
            continue
    return rows


def descendants(table: list[ProcessRow], root_pid: int) -> list[ProcessRow]:
    """All processes in the tree rooted at `root_pid`, including the root."""
    children: dict[int, list[ProcessRow]] = {}
    for row in table:
        children.setdefault(row.ppid, []).append(row)
    found: list[ProcessRow] = []
    stack = [row for row in table if row.pid == root_pid]
    seen: set[int] = set()
    while stack:
        row = stack.pop()
        if row.pid in seen:
            continue
        seen.add(row.pid)
        found.append(row)
        stack.extend(children.get(row.pid, []))
    return found


def role_totals(rows: list[ProcessRow]) -> dict:
    """Per-role aggregates. `note` is mandatory in the output: role totals
    must never be presented as additive physical memory."""
    totals: dict[str, dict] = {}
    for role in ("host", "worker", "browser", "build", "other"):
        members = [r for r in rows if r.role == role]
        totals[role] = {
            "processes": len(members),
            "rss_kb_max": max((r.rss_kb for r in members), default=0),
            "rss_kb_sum_of_shared": sum(r.rss_kb for r in members),
            "cpu_pct_sum": round(sum(r.cpu_pct for r in members), 2),
        }
    return {
        "roles": totals,
        "note": (
            "RSS includes pages shared between processes. rss_kb_sum_of_shared "
            "is NOT total physical memory; the whole-machine view lives in "
            "system_memory and is authoritative for pressure."
        ),
    }


def observed_maxima(rows: list[ProcessRow]) -> dict:
    """Per-role maxima over repeated snapshots of (possibly transient)
    processes: distinct pids observed, peak RSS, peak CPU%%. This is the
    honest aggregate for short-lived processes; sums are meaningless across
    snapshots and are deliberately not reported."""
    per_role: dict[str, dict] = {}
    for role in ("host", "worker", "browser", "build", "other"):
        members = [r for r in rows if r.role == role]
        pids = {r.pid for r in members}
        per_role[role] = {
            "pids_observed": len(pids),
            "rss_kb_max": max((r.rss_kb for r in members), default=0),
            "cpu_pct_max": max((r.cpu_pct for r in members), default=0.0),
        }
    return {
        "roles": per_role,
        "note": (
            "Repeated-snapshot maxima. RSS includes shared pages; role "
            "maxima are per-process peaks, never additive physical memory. "
            "The whole-machine view lives in system_memory."
        ),
    }


def system_memory() -> dict:
    """Whole-machine memory pressure. macOS: vm_stat; Linux: /proc/meminfo."""
    if platform.system() == "Darwin":
        page_size = 4096
        out = subprocess.run(
            ["vm_stat"], capture_output=True, text=True, check=True
        ).stdout
        stats: dict[str, int] = {}
        for line in out.splitlines():
            if ":" not in line:
                continue
            key, _, value = line.partition(":")
            value = value.strip().rstrip(".")
            if value.isdigit():
                stats[key.strip()] = int(value) * page_size
        swap_usage = _macos_swap()
        return {
            "source": "vm_stat",
            "free_bytes": stats.get("Pages free", 0),
            "inactive_bytes": stats.get("Pages inactive", 0),
            "purgeable_bytes": stats.get("Pages purgeable", 0),
            "wired_bytes": stats.get("Pages wired down", 0),
            "compressed_bytes": stats.get("Pages stored in compressor", 0),
            **swap_usage,
        }
    meminfo = _meminfo_kb()
    return {
        "source": "/proc/meminfo",
        "free_bytes": meminfo.get("MemFree", 0) * 1024,
        "available_bytes": meminfo.get("MemAvailable", 0) * 1024,
        "swap_total_bytes": meminfo.get("SwapTotal", 0) * 1024,
        "swap_free_bytes": meminfo.get("SwapFree", 0) * 1024,
    }


def _macos_swap() -> dict:
    try:
        out = subprocess.run(
            ["sysctl", "vm.swapusage"], capture_output=True, text=True, check=True
        ).stdout
        # e.g. "vm.swapusage: total = 1024.00M  used = 312.25M  free = 711.75M"
        text = out.split(":", 1)[-1]
        return {f"swap_{label}_bytes": value for label, value in _pairs(text)}
    except (subprocess.SubprocessError, OSError, ValueError):
        return {}


def _pairs(text: str) -> list[tuple[str, int]]:
    """Parse `label = value` pairs ("total = 12288.00M used = 312.25M …")."""
    pairs = []
    tokens = text.replace("\n", " ").split()
    for i, token in enumerate(tokens):
        if (
            token in ("total", "used", "free")
            and i + 2 < len(tokens)
            and tokens[i + 1] == "="
        ):
            pairs.append((token, _size_to_bytes(tokens[i + 2])))
    return pairs


def _size_to_bytes(text: str) -> int:
    text = text.strip()
    if text.endswith("G"):
        return int(float(text[:-1]) * 1024**3)
    if text.endswith("M"):
        return int(float(text[:-1]) * 1024**2)
    if text.endswith("K"):
        return int(float(text[:-1]) * 1024)
    if text.endswith("B"):
        return int(float(text[:-1]))
    return int(float(text))


def _meminfo_kb() -> dict[str, int]:
    try:
        text = Path("/proc/meminfo").read_text()
    except OSError:
        return {}
    values: dict[str, int] = {}
    for line in text.splitlines():
        key, _, rest = line.partition(":")
        number = rest.strip().split()[0] if rest.strip() else ""
        if number.isdigit():
            values[key.strip()] = int(number)
    return values


def peak_rss(argv: list[str], env: dict | None = None, cwd: str | None = None) -> dict:
    """Peak RSS of one short-lived command, measured by the OS at exit
    (`/usr/bin/time -l` on macOS, `/usr/bin/time -v` on Linux). This is the
    honest number for a process too short-lived for ps snapshots: the kernel
    accounts every page the child touched."""
    if platform.system() == "Darwin":
        # macOS time -l reports max RSS in BYTES; normalize to KiB.
        flag, parse = "-l", lambda out: _scaled(_max_rss_line(out), 1 / 1024)
    else:
        # Linux time -v reports max RSS in KiB already.
        flag, parse = "-v", lambda out: _max_rss_line(out)
    time_bin = "/usr/bin/time"
    if not Path(time_bin).exists():
        return {"supported": False, "reason": f"{time_bin} not available"}
    start = time.monotonic()
    proc = subprocess.run(
        [time_bin, flag] + argv, env=env, cwd=cwd, capture_output=True, text=True
    )
    seconds = round(time.monotonic() - start, 4)
    if proc.returncode != 0:
        return {
            "supported": True,
            "seconds": seconds,
            "exit_code": proc.returncode,
            "stderr_tail": proc.stderr[-400:],
        }
    return {
        "supported": True,
        "seconds": seconds,
        "exit_code": 0,
        "max_rss_kb": parse(proc.stderr),
    }


def _max_rss_line(stderr: str) -> int | None:
    for line in stderr.splitlines():
        if "maximum resident set size" in line.lower():
            digits = "".join(ch for ch in line if ch.isdigit())
            if digits:
                return int(digits)
    return None


def _scaled(value: int | None, factor: float) -> int | None:
    return None if value is None else int(value * factor)


# ---------------------------------------------------------------------------
# Response latencies
# ---------------------------------------------------------------------------


def launch_latency(argv: list[str], repeats: int = 5) -> dict:
    """Wall-clock time of running a real command to completion, repeated."""
    timings = []
    for _ in range(max(1, repeats)):
        start = time.monotonic()
        subprocess.run(argv, check=True, capture_output=True)
        timings.append(round(time.monotonic() - start, 4))
    return {
        "argv": argv,
        "repeats": len(timings),
        "seconds": timings,
        "median_seconds": sorted(timings)[len(timings) // 2],
        "kind": "real",
    }


def echo_latency_pty(payload: bytes = b"echo-probe\n", timeout: float = 5.0) -> dict:
    """Round-trip time for one byte of input through a real PTY to `cat`
    and back. This is the minimal honest input-response probe: a real
    terminal device, a real child process, a real round trip."""
    pid, fd = pty.fork()
    if pid == 0:  # child
        os.execvp("cat", ["cat"])
    start = time.monotonic()
    os.write(fd, payload)
    seen = 0
    status = None
    elapsed = None
    while time.monotonic() - start < timeout:
        ready, _, _ = select.select([fd], [], [], 0.1)
        if ready:
            try:
                chunk = os.read(fd, 4096)
            except OSError:
                break
            seen += len(chunk)
            if seen >= len(payload):
                elapsed = round(time.monotonic() - start, 5)
                break
    else:
        elapsed = None
    os.write(fd, b"\x04")  # EOF for cat
    deadline = time.monotonic() + 2.0
    while status is None and time.monotonic() < deadline:
        try:
            status = os.waitpid(pid, os.WNOHANG)[1]
        except ChildProcessError:
            status = 0
            break
        time.sleep(0.02)
    if status is None:
        import signal as _signal

        os.kill(pid, _signal.SIGKILL)
        status = 0
    os.close(fd)
    return {
        "probe": "pty-echo-cat",
        "payload_bytes": len(payload),
        "seconds": elapsed,
        "timed_out": elapsed is None,
        "child_exit_status": status,
        "kind": "real",
        "note": "single round trip, cold child; not an aggregate benchmark",
    }


# ---------------------------------------------------------------------------
# Workloads
# ---------------------------------------------------------------------------


@dataclass
class Workload:
    """A named workload: what to run inside the window, honestly labelled."""

    name: str
    description: str
    kind: str = "simulated"  # or "real-agents" when drivers are real CLIs
    steps: list[dict] = field(default_factory=list)


def slice_cli_workload(binary: Path, home: Path) -> Workload:
    """The first available slice: the viva binary against an isolated
    VIVA_HOME — init, one event, doctor. This is the G1 baseline slice."""
    env = {"VIVA_HOME": str(home)}
    return Workload(
        name="slice-cli",
        description=(
            "viva init + event add + doctor against an isolated VIVA_HOME "
            "(first available slice; not the full office)"
        ),
        kind="real",
        steps=[
            {"argv": [str(binary), "init"], "env": env, "cwd": str(home.parent)},
            {
                "argv": [
                    str(binary),
                    "event",
                    "add",
                    "foundation",
                    "acceptance_sample",
                    "tool",
                    "v12",
                    '{"source":"sampler"}',
                ],
                "env": env,
                "cwd": str(home.parent),
            },
            {"argv": [str(binary), "doctor"], "env": env, "cwd": str(home.parent)},
        ],
    )


def run_workload(workload: Workload) -> dict:
    """Run every step of a workload, capturing per-step wall time."""
    steps = []
    for step in workload.steps:
        env = {**os.environ, **step.get("env", {})}
        cwd = step.get("cwd")
        start = time.monotonic()
        proc = subprocess.run(
            step["argv"], env=env, cwd=cwd, capture_output=True, text=True
        )
        steps.append(
            {
                "argv": step["argv"],
                "seconds": round(time.monotonic() - start, 4),
                "exit_code": proc.returncode,
                "stderr_tail": proc.stderr[-400:] if proc.returncode else "",
            }
        )
    return {"workload": workload.name, "kind": workload.kind, "steps": steps}


# ---------------------------------------------------------------------------
# Build timing
# ---------------------------------------------------------------------------


def build_times(repo_root: Path) -> dict:
    """Cold vs incremental build wall time for the Rust workspace. Cold means
    `cargo clean` first — run only inside an exclusive window."""
    cargo = shutil.which("cargo")
    if cargo is None:
        return {"error": "cargo not on PATH"}
    start = time.monotonic()
    subprocess.run(["cargo", "clean"], cwd=repo_root, check=True, capture_output=True)
    cold = subprocess.run(
        ["cargo", "build", "--workspace"], cwd=repo_root, capture_output=True, text=True
    )
    cold_seconds = round(time.monotonic() - start, 1)
    start = time.monotonic()
    incremental = subprocess.run(
        ["cargo", "build", "--workspace"], cwd=repo_root, capture_output=True, text=True
    )
    incremental_seconds = round(time.monotonic() - start, 1)
    return {
        "cold_seconds": cold_seconds if cold.returncode == 0 else None,
        "incremental_seconds": incremental_seconds
        if incremental.returncode == 0
        else None,
        "cold_ok": cold.returncode == 0,
        "incremental_ok": incremental.returncode == 0,
        "cold_stderr_tail": cold.stderr[-400:] if cold.returncode else "",
    }


# ---------------------------------------------------------------------------
# Budget comparison
# ---------------------------------------------------------------------------


def compare_to_budget(record: dict, budget: dict) -> list[dict]:
    """Compare dotted paths in the record against budget bounds. Missing
    measurements are reported as `missing`, never as within-budget."""
    def lookup(path: str):
        node = record
        for part in path.split("."):
            if not isinstance(node, dict) or part not in node:
                return None
            node = node[part]
        return node

    checks = []
    for metric, bound in budget.get("metrics", {}).items():
        value = lookup(metric)
        limit = bound.get("max_seconds") or bound.get("max_bytes")
        if value is None:
            checks.append(
                {"metric": metric, "value": None, "status": "missing", "limit": limit}
            )
            continue
        within = value <= limit
        checks.append(
            {
                "metric": metric,
                "value": value,
                "limit": limit,
                "status": "within" if within else "over",
            }
        )
    return checks


# ---------------------------------------------------------------------------
# The sample command: one full record
# ---------------------------------------------------------------------------


def make_record(workload: Workload, window_path: Path) -> dict:
    """One full sampling record for a workload, inside an exclusive window."""
    with ExclusiveWindow(window_path):
        profile = machine_profile()
        workload_result = run_workload(workload)

        # Peak RSS per step, measured by the OS at exit. The CLI steps are
        # milliseconds long — ps snapshots cannot catch them; /usr/bin/time
        # accounts every page the child touched.
        step_peaks = []
        for step in workload.steps:
            env = {**os.environ, **step.get("env", {})}
            step_peaks.append(
                {
                    "argv": step["argv"],
                    **peak_rss(step["argv"], env=env, cwd=step.get("cwd")),
                }
            )

        return {
            "schema_version": RECORD_SCHEMA_VERSION,
            "created_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
            "machine": profile,
            "window": {
                "lock": str(window_path),
                "exclusive": True,
                "hold_hint_seconds": DEFAULT_WINDOW_SECONDS,
            },
            "workload": workload_result,
            "step_peaks": step_peaks,
            "system_memory": system_memory(),
            "honesty_notes": [
                "workload kind is recorded per workload; simulated workloads "
                "validate control logic only",
                "max_rss_kb is a per-process peak; it is never summed across "
                "processes into a physical-memory claim",
                "pending platforms (Intel Mac, 16 real agents) are not "
                "covered by this record",
            ],
        }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("profile")

    p_sample = sub.add_parser("sample")
    p_sample.add_argument("--workload", default="slice-cli", choices=["slice-cli"])
    p_sample.add_argument("--binary", type=Path, required=True)
    p_sample.add_argument("--home", type=Path, required=True)
    p_sample.add_argument("--window", type=Path, required=True)
    p_sample.add_argument("--out", type=Path, default=None)

    p_launch = sub.add_parser("launch")
    p_launch.add_argument("argv", nargs="+")
    p_launch.add_argument("--repeats", type=int, default=5)

    p_build = sub.add_parser("build-times")
    p_build.add_argument("--repo", type=Path, default=Path.cwd())

    args = parser.parse_args(argv)

    if args.command == "profile":
        print(json.dumps(machine_profile(), indent=2))
        return 0
    if args.command == "launch":
        print(json.dumps(launch_latency(args.argv, args.repeats), indent=2))
        return 0
    if args.command == "build-times":
        with ExclusiveWindow(Path(tempfile_dir()) / "viva-v12-window.lock"):
            print(json.dumps(build_times(args.repo), indent=2))
        return 0
    if args.command == "sample":
        binary = args.binary.resolve()
        workload = slice_cli_workload(binary, args.home)
        record = make_record(workload, args.window)
        text = json.dumps(record, indent=2)
        if args.out:
            args.out.parent.mkdir(parents=True, exist_ok=True)
            args.out.write_text(text)
            print(f"wrote {args.out}")
        else:
            print(text)
        return 0
    return 1


def tempfile_dir() -> str:
    import tempfile

    return tempfile.gettempdir()


if __name__ == "__main__":
    sys.exit(main())
