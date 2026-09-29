#!/bin/bash
# Local, registry-free verification of the npm distribution chain.
#
# Stages the packages against a STUB binary (no Rust build needed), then
# installs the platform package + wrapper into a sandbox and asserts that
# typing `viva` execs the native binary with argv and exit codes intact.
# The real npm publish path is exercised by CI (release.yml, npm-publish).
#
# Usage: packaging/npm/test-local.sh

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

STUB="$WORK/viva-stub"
cat > "$STUB" <<'EOF'
#!/bin/bash
if [ "$1" = "--version" ]; then echo "viva 9.9.9-npm-stub"; exit 0; fi
if [ "$1" = "--exit-code" ]; then exit "${2:-3}"; fi
echo "stub argv: $*"
EOF
chmod +x "$STUB"

echo "== stage packages with the stub binary"
python3 "$HERE/prepare-packages.py" --version 9.9.9 \
  --bin-arm64 "$STUB" --bin-x64 "$STUB" --out "$WORK/dist"

node --check "$WORK/dist/viva/bin/viva.js"
echo "wrapper syntax ok"

echo "== npm pack + install the tarballs (faithful to registry installs)"
# Local-DIR installs are symlinked by npm, which breaks node resolution in
# a way the registry never does — so pack first and install the tarballs.
ARCH="$(node -p 'process.arch')"
mkdir "$WORK/pkgs"
for pkg in "viva-darwin-$ARCH" viva; do
  (cd "$WORK/dist/$pkg" && npm pack --pack-destination "$WORK/pkgs" >/dev/null)
done
cd "$WORK"
mkdir sandbox && cd sandbox
printf '{"name":"sandbox","private":true}\n' > package.json
npm install --no-audit --no-fund --loglevel=error \
  "$WORK/pkgs/zuohaisu-viva-darwin-$ARCH-9.9.9.tgz" \
  "$WORK/pkgs/zuohaisu-viva-9.9.9.tgz" \
  > "$WORK/npm-install.log" 2>&1 || { cat "$WORK/npm-install.log"; exit 1; }

VIVA="$PWD/node_modules/.bin/viva"
[ -x "$VIVA" ] || { echo "FAIL: .bin/viva not installed"; exit 1; }

echo "== exec: version, argv passthrough, exit code"
"$VIVA" --version | grep -q "viva 9.9.9-npm-stub" || { echo "FAIL: version"; exit 1; }
"$VIVA" status --json | grep -q "stub argv: status --json" || { echo "FAIL: argv"; exit 1; }
set +e
"$VIVA" --exit-code 3
CODE=$?
set -e
[ "$CODE" -eq 3 ] || { echo "FAIL: exit code passthrough (got $CODE)"; exit 1; }

echo "PASS: npm wrapper resolves the platform binary and forwards argv/exit codes"
