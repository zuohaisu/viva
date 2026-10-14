#!/usr/bin/env python3
"""S2 presentation cross-check (development harness, not acceptance).

Runs a command inside a Viva headless terminal, records the PTY byte stream
via `script`, snapshots the styled grid from Viva, then replays the same
bytes into a fresh tmux pane at the same geometry. tmux is the independent
reference emulator. Per-cell text/style, cursor, and alternate-screen state
are compared.

Requires: built `viva` binary, `tmux`, a throwaway VIVA_HOME under /tmp
(AF_UNIX path length limit).
"""
import argparse
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import unicodedata

ap = argparse.ArgumentParser()
ap.add_argument("--viva", required=True, help="path to viva binary")
ap.add_argument("--cols", type=int, default=100)
ap.add_argument("--rows", type=int, default=30)
ap.add_argument("--wait", type=float, default=8.0)
ap.add_argument(
    "--cwd",
    default=None,
    help="terminal cwd (use a trusted git checkout for codex)",
)
ap.add_argument("--raw-out", default=None, help="keep stripped byte stream")
ap.add_argument("--max-diffs", type=int, default=15)
ap.add_argument("argv", nargs="+")
a = ap.parse_args()

if shutil.which("tmux") is None:
    sys.exit("tmux is required for the independent reference pane")

a.viva = os.path.abspath(a.viva)
if a.cwd:
    a.cwd = os.path.abspath(a.cwd)
else:
    a.cwd = os.getcwd()
argv = list(a.argv)
if os.path.isfile(argv[0]):
    argv[0] = os.path.abspath(argv[0])


def sh(*cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def width(ch):
    if unicodedata.combining(ch) or unicodedata.category(ch) in ("Mn", "Me", "Cf"):
        return 0
    return 2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1


BLANK = (" ", 0, 0, 0)


def place(row, col, ch, st, cols):
    w = width(ch)
    if w == 0:
        if col > 0 and row[col - 1] is not None:
            c = row[col - 1]
            row[col - 1] = (c[0] + ch,) + c[1:]
        return col
    if col + w > cols:
        return col
    row[col] = (ch,) + st
    if w == 2:
        row[col + 1] = ("", 0, 0, 0)
    return col + w


home = os.environ.get("VIVA_HOME")
if not home or len(home) > 80:
    home = tempfile.mkdtemp(prefix="vs2.", dir="/tmp")
    os.environ["VIVA_HOME"] = home
    sh(a.viva, "init")
elif not os.path.isfile(os.path.join(home, "viva.db")):
    sh(a.viva, "init")

raw = a.raw_out or tempfile.mktemp(prefix="s2raw.", dir="/tmp")
inner = "env TERM=xterm-256color COLORTERM=truecolor " + " ".join(
    shlex.quote(x) for x in argv
)
create = sh(
    a.viva,
    "terminal",
    "create",
    "--cols",
    str(a.cols),
    "--rows",
    str(a.rows),
    "--cwd",
    a.cwd,
    "--purpose",
    "s2-compare",
    "--owner",
    "test_run",
    "--",
    "script",
    "-qefc",
    inner,
    raw,
)
if create.returncode:
    sys.exit("viva terminal create failed: " + create.stderr + create.stdout)
tid = json.loads(create.stdout)["terminal_id"]
time.sleep(a.wait)
snap = json.loads(sh(a.viva, "terminal", "snapshot", tid).stdout)
sh(a.viva, "stop-terminal", tid)
if snap.get("screen") is None:
    sys.exit("snapshot has no styled screen (screen=null)")

data = open(raw, "rb").read()
data = re.sub(rb"\AScript started on [^\n]*\n", b"", data)
data = re.sub(rb"\nScript done on [^\n]*\n?\Z", b"", data)
data = re.sub(rb"Script done on [^\n]*\n?\Z", b"", data)
with open(raw, "wb") as f:
    f.write(data)

screen = snap["screen"]
vgrid = []
for runs in screen["lines"]:
    row = [BLANK] * a.cols
    col = 0
    for text, _w, fg, bg, at in runs:
        for ch in text:
            col = place(row, col, ch, (fg, bg, at), a.cols)
    vgrid.append(row)

sock = f"s2ref{os.getpid()}"
T = ["tmux", "-L", sock]
sh(
    *T,
    "new-session",
    "-d",
    "-s",
    "r",
    "-x",
    str(a.cols),
    "-y",
    str(a.rows),
    f"cat {shlex.quote(raw)}; sleep 3600",
)
sh(*T, "set", "-g", "status", "off")
sh(*T, "resize-window", "-t", "r", "-x", str(a.cols), "-y", str(a.rows))
time.sleep(1.5)
cap = sh(*T, "capture-pane", "-p", "-e", "-N", "-t", "r").stdout
info = sh(
    *T,
    "display",
    "-p",
    "-t",
    "r",
    "#{cursor_x} #{cursor_y} #{cursor_flag} #{alternate_on}",
).stdout.split()
sh(*T, "kill-server")
cx, cy, cflag, alt = (int(x) for x in info)

SGR = re.compile(r"\x1b\[([0-9;:]*)m")
unsupported = set()


def parse_sgr(params, st):
    fg, bg, at = st
    ps = params.replace(":", ";").split(";") if params else ["0"]
    i = 0
    while i < len(ps):
        p = int(ps[i] or 0)
        if p == 0:
            fg, bg, at = 0, 0, 0
        elif p == 1:
            at |= 1
        elif p == 2:
            at |= 2
        elif p == 3:
            at |= 4
        elif p == 4:
            at |= 8
            if i + 1 < len(ps) and params.startswith("4:"):
                unsupported.add("underline-style")
                i += 1
        elif p == 7:
            at |= 16
        elif p in (5, 6):
            unsupported.add("blink")
        elif p == 8:
            unsupported.add("conceal")
        elif p == 9:
            unsupported.add("strikethrough")
        elif p == 21:
            unsupported.add("double-underline")
        elif p == 22:
            at &= ~3
        elif p == 23:
            at &= ~4
        elif p == 24:
            at &= ~8
        elif p == 27:
            at &= ~16
        elif 30 <= p <= 37:
            fg = p - 30 + 1
        elif p == 39:
            fg = 0
        elif 40 <= p <= 47:
            bg = p - 40 + 1
        elif p == 49:
            bg = 0
        elif 90 <= p <= 97:
            fg = p - 90 + 8 + 1
        elif 100 <= p <= 107:
            bg = p - 100 + 8 + 1
        elif p in (38, 48):
            kind = int(ps[i + 1])
            if kind == 5:
                v = int(ps[i + 2]) + 1
                i += 2
            else:
                r, g, b = (int(x) for x in ps[i + 2 : i + 5])
                i += 4
                v = 0x1000000 | (r << 16) | (g << 8) | b
            if p == 38:
                fg = v
            else:
                bg = v
        i += 1
    return (fg, bg, at)


tgrid = []
for line in cap.split("\n")[: a.rows]:
    row = [BLANK] * a.cols
    st = (0, 0, 0)
    col = 0
    pos = 0
    while pos < len(line):
        m = SGR.match(line, pos)
        if m:
            st = parse_sgr(m.group(1), st)
            pos = m.end()
            continue
        if line[pos] == "\x1b":
            m2 = re.compile(r"\x1b[\[\]()][0-9;?]*[ -/]*[@-~]?").match(line, pos)
            pos = m2.end() if m2 else pos + 1
            continue
        col = place(row, col, line[pos], st, a.cols)
        pos += 1
    tgrid.append(row)
while len(tgrid) < a.rows:
    tgrid.append([BLANK] * a.cols)


def norm(c):
    ch, fg, bg, at = c
    if ch == " " and fg == 0 and bg == 0 and not (at & (16 | 8)):
        return BLANK
    return c


text_diffs, style_diffs = [], []
for y in range(a.rows):
    for x in range(a.cols):
        v, t = norm(vgrid[y][x]), norm(tgrid[y][x])
        if v == t:
            continue
        if v[0] != t[0]:
            text_diffs.append((y, x, v, t))
        else:
            style_diffs.append((y, x, v, t))

m = screen["modes"]
vcy, vcx = screen["cursor"]
alt_ok = m["alternate"] == bool(alt)
cur_ok = (vcy, vcx) == (cy, cx) and screen["cursor_visible"] == bool(cflag)
ok = alt_ok and cur_ok and not text_diffs and not style_diffs

print(
    f"command        : {' '.join(argv)}  ({a.cols}x{a.rows}, waited {a.wait}s, {len(data)} raw bytes)"
)
print(
    f"alternate      : viva={m['alternate']} tmux={bool(alt)}  {'OK' if alt_ok else 'DIFF'}"
)
print(
    f"cursor         : viva=({vcy},{vcx}) visible={screen['cursor_visible']}  "
    f"tmux=({cy},{cx}) visible={bool(cflag)}  {'OK' if cur_ok else 'DIFF'}"
)
print(f"modes (viva)   : {m}")
print(f"text cells diff: {len(text_diffs)}   style-only cells diff: {len(style_diffs)}")
print(f"tmux SGR features vt100 does not project: {sorted(unsupported) or 'none seen'}")
for label, ds in (("TEXT", text_diffs), ("STYLE", style_diffs)):
    for y, x, v, t in ds[: a.max_diffs]:
        print(f"  {label} r{y} c{x}: viva={v!r} tmux={t!r}")
if a.raw_out:
    print("raw stream kept:", raw)
elif not a.raw_out:
    os.unlink(raw)

sys.exit(0 if ok else 1)
