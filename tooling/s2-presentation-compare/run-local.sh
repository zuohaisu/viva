#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VIVA="${VIVA_BIN:-$ROOT/target/release/viva}"
PY="$ROOT/tooling/s2-presentation-compare/compare.py"
SYN="$ROOT/tooling/s2-presentation-compare/synthetic.sh"
chmod +x "$SYN" "$PY"

if [[ ! -x "$VIVA" ]]; then
  echo "building release viva…" >&2
  (cd "$ROOT" && cargo build --release -p viva)
fi

echo "== synthetic fixture ==" >&2
python3 "$PY" --viva "$VIVA" --cwd "$ROOT" --cols 100 --rows 30 --wait 9 -- "$SYN"

if command -v codex >/dev/null 2>&1; then
  echo "== codex exec (trusted cwd) ==" >&2
  python3 "$PY" --viva "$VIVA" --cwd "$ROOT" --cols 100 --rows 30 --wait 25 -- \
    /bin/bash -lc 'script -qefc "timeout 18 codex exec \"say hi in one short line\"" /dev/null'
else
  echo "skip codex: not in PATH" >&2
fi

echo "all configured checks passed" >&2
