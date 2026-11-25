#!/bin/bash
# Local, registry-free verification of the npm distribution chain.
#
# Stages the packages against a STUB binary (no Rust build needed), then
# installs the platform package + wrapper into a sandbox and asserts that
# typing `viva` execs the native binary with argv and exit codes intact.
# The real npm publish path is exercised by CI (release.yml, npm-publish).
#
# Usage: packaging/npm/test-local.sh [--pack-only]  # skip Darwin install on Linux

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

STUB="$WORK/viva-stub"
cat > "$STUB" <<'EOF'
#!/bin/bash
if [ "$1" = "--version" ]; then echo "viva 9.9.9-npm-stub"; exit 0; fi
if [ "$1" = "--exit-code" ]; then exit "${2:-3}"; fi
if [ "$1" = "--npm-root" ]; then printf '%s\n' "$VIVA_NPM_PACKAGE_ROOT"; exit 0; fi
echo "stub argv: $*"
EOF
chmod +x "$STUB"

echo "== stage packages from two Release fixtures with real notices"
REVISION=$(git -C "$HERE" rev-parse HEAD)
# Keep the pack check registry-free. Exercise the production collector with
# fixture inputs; the release workflow collects from the actual toolchains.
python3 "$HERE/test_dependency_notices.py"
for ARCH in arm64 x64; do
  RELEASE="$WORK/source/release-$ARCH"
  case "$ARCH" in
    arm64) TARGET=aarch64-apple-darwin ;;
    x64) TARGET=x86_64-apple-darwin ;;
  esac
  python3 "$HERE/test_dependency_notices.py" fixture "$RELEASE" "$TARGET"
  python3 "$HERE/release_materials.py" create "$RELEASE" zuohaisu/viva "$REVISION" "$TARGET"
  cp "$STUB" "$RELEASE/viva"
  if [ "$(uname -s)" = Darwin ]; then
    xattr -w com.example.viva-pack-test fixture "$RELEASE/licenses/herdr-LICENSE"
  fi
  python3 "$HERE/release_materials.py" archive "$RELEASE" "$WORK/release-$ARCH.tar.gz"
  # Extract without macOS tar interpreting AppleDouble entries as metadata.
  python3 -m tarfile -e "$WORK/release-$ARCH.tar.gz" "$WORK"
done

# A stray on-disk sidecar must fail before uploading a Release archive.
printf 'sidecar fixture\n' > "$RELEASE/licenses/._herdr-LICENSE"
if python3 "$HERE/release_materials.py" archive "$RELEASE" "$WORK/rejected.tar.gz" \
  >"$WORK/archive.log" 2>&1; then
  echo "FAIL: accepted an AppleDouble sidecar"; exit 1
fi
grep -F 'Unexpected AppleDouble entry' "$WORK/archive.log"

# An incomplete Release must fail before staging either platform.
MISSING="$WORK/release-x64/licenses/herdr-data/x11/COPYING"
mv "$MISSING" "$WORK/missing-notice"
if python3 "$HERE/prepare-packages.py" --version 9.9.9 \
  --release-arm64 "$WORK/release-arm64" --release-x64 "$WORK/release-x64" \
  --out "$WORK/rejected" >"$WORK/stage.log" 2>&1; then
  echo "FAIL: staging accepted a missing nested license"; exit 1
fi
grep -F 'licenses/herdr-data/x11/COPYING' "$WORK/stage.log"
test ! -e "$WORK/rejected"
mv "$WORK/missing-notice" "$MISSING"

if python3 "$HERE/prepare-packages.py" --version 9.9.9 \
  --release-arm64 "$WORK/release-x64" --release-x64 "$WORK/release-arm64" \
  --out "$WORK/rejected" >"$WORK/stage.log" 2>&1; then
  echo "FAIL: staging accepted swapped architectures"; exit 1
fi
grep -F 'do not match aarch64-apple-darwin' "$WORK/stage.log"
test ! -e "$WORK/rejected"

python3 "$HERE/prepare-packages.py" --version 9.9.9 \
  --release-arm64 "$WORK/release-arm64" --release-x64 "$WORK/release-x64" --out "$WORK/dist"

node --check "$WORK/dist/viva/bin/viva.js"
echo "wrapper syntax ok"

echo "== npm pack and compare both platform tarballs with the Release payload"
mkdir "$WORK/pkgs"
for pkg in viva-darwin-arm64 viva-darwin-x64 viva; do
  npm pack "$WORK/dist/$pkg" --ignore-scripts --pack-destination "$WORK/pkgs" --loglevel=error
  if [ "$pkg" != viva ]; then
    python3 "$HERE/release_materials.py" verify-pack "$WORK/release-${pkg#viva-darwin-}" \
      "$WORK/pkgs/zuohaisu-$pkg-9.9.9.tgz"
  fi
done
tar -xOf "$WORK/pkgs/zuohaisu-viva-9.9.9.tgz" package/LICENSE | cmp "$WORK/release-arm64/LICENSE" -

# Reproduce the old npm allowlist: staged NOTICE exists, but pack drops it.
mkdir "$WORK/incomplete-pack"
node -e 'const fs = require("fs"), p = process.argv[1], m = require(p); m.files = ["bin"]; fs.writeFileSync(p, JSON.stringify(m));' \
  "$WORK/dist/viva-darwin-arm64/package.json"
npm pack "$WORK/dist/viva-darwin-arm64" --ignore-scripts --loglevel=error \
  --pack-destination "$WORK/incomplete-pack"
if python3 "$HERE/release_materials.py" verify-pack "$WORK/dist/viva-darwin-arm64" \
  "$WORK/incomplete-pack/zuohaisu-viva-darwin-arm64-9.9.9.tgz" >"$WORK/pack.log" 2>&1; then
  echo "FAIL: accepted a tarball with no NOTICE"; exit 1
fi
grep -F 'package/NOTICE' "$WORK/pack.log"
if [ "${1:-}" = --pack-only ]; then
  echo "PASS: both npm platform tarballs preserve the Release notices; Darwin install not run"
  exit 0
fi

# Install tarballs, not symlinked local directories, to exercise resolution.
ARCH="$(node -p 'process.arch')"
cd "$WORK"
mkdir sandbox && cd sandbox
printf '{"name":"sandbox","private":true}\n' > package.json
npm install --offline --ignore-scripts --no-audit --no-fund --loglevel=error \
  "$WORK/pkgs/zuohaisu-viva-darwin-$ARCH-9.9.9.tgz" \
  "$WORK/pkgs/zuohaisu-viva-9.9.9.tgz" \
  > "$WORK/npm-install.log" 2>&1 || { cat "$WORK/npm-install.log"; exit 1; }

VIVA="$PWD/node_modules/.bin/viva"
[ -x "$VIVA" ] || { echo "FAIL: .bin/viva not installed"; exit 1; }

echo "== exec: version, argv passthrough, exit code"
"$VIVA" --version | grep -q "viva 9.9.9-npm-stub" || { echo "FAIL: version"; exit 1; }
"$VIVA" status --json | grep -q "stub argv: status --json" || { echo "FAIL: argv"; exit 1; }
ROOT="$(VIVA_NPM_PACKAGE_ROOT=forged-root "$VIVA" --npm-root)"
[ "$ROOT" = "$(pwd -P)/node_modules/@zuohaisu/viva" ] || { echo "FAIL: npm updater channel root (got $ROOT)"; exit 1; }
set +e
"$VIVA" --exit-code 3
CODE=$?
set -e
[ "$CODE" -eq 3 ] || { echo "FAIL: exit code passthrough (got $CODE)"; exit 1; }

echo "PASS: npm wrapper resolves the platform binary and forwards argv/exit codes"
