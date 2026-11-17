# herdr baseline and derived runtime

This is the source base for Viva's workbench replacement (#187 / #191), not a
reference implementation to translate. The **original complete import** remains
at commit `48ce171f11a249de575df71d59d868caf1a1e1c4`, tree `98b3f4e`.
The current `herdr/` is now a derivative: its entry is a callable library with
explicit embedding paths and Viva domain/authority/CLI adapters. It is compiled
into `viva` when `herdr-runtime` is enabled; no external Herdr binary is needed.
The original nested lockfile, vendored sources, assets and frozen endpoint codecs
are retained; the runtime Rust source itself has been modified. Attribution,
tests and license notices are retained.
Upstream docs are third-party source assets, not Viva's canonical knowledge base.
The source commit and tree are recorded in `herdr-source.json`.

## Build and test the original baseline

Use a new archive directory, not the derived checkout or another Git worktree:

```sh
scratch="$(mktemp -d /tmp/herdr-original.XXXXXX)" || exit 1
git archive 48ce171f11a249de575df71d59d868caf1a1e1c4 upstream/herdr | tar -x -C "$scratch"
cd "$scratch/upstream/herdr"
rustup toolchain install
# Install Zig 0.16.0; alternatively set ZIG to its executable.
cargo build --locked
cargo nextest run --workspace --locked
./target/debug/herdr --version
./target/debug/herdr --help
```

Upstream's broader check is `just check` (requires just, cargo-nextest, Bun,
and the Windows cross-compilation prerequisites described in its `AGENTS.md`).
The direct Cargo commands above do not replace those maintenance, integration,
documentation, or Windows checks.

The nested workspace retains its original lockfile, patches and Rust toolchain.
Use its upstream nextest runner: a shared `cargo test` process can inherit signal
handlers changed by another native test. Root default tests do not run these
native tests. Daily `native-runtime` CI builds the native feature, runs core
feature-enabled tests and a real PTY smoke. The manual full CI
(`workflow_dispatch`) adds native/imported Clippy, nested nextest/maintenance and
historical migration/compatibility matrices. Use the full entry for imported
source/dependency or upgrade/migration changes; daily green is not proof those
full checks ran. Markdown-only PRs run the diff check and omit product jobs.
See `.github/workflows/ci.yml` for exact toolchain, fixture and test commands.
Neither package version changed. The root composition has its own combined lock
graph; the original #190 notices are not a final derived-artifact SBOM.

For an isolated build of the **current native composition** (the same feature
used by the release workflow, not a claim that current main has been released):

```sh
# Use the toolchain pinned by upstream/herdr/rust-toolchain.toml.
cargo +1.96.1 build --locked -p viva --features herdr-runtime
out=$(mktemp -d)
python3 tooling/herdr-integration/native_runtime.py --viva target/debug/viva --output "$out/native-runtime.json"
```

The probe creates private HOME/VIVA_HOME and an immutable binary copy. See
`research/herdr-191/p1/` and the Wiki's
[Herdr-Viva-Integration](https://github.com/zuohaisu/viva/wiki/Herdr-Viva-Integration)
for the subsequent public-entry, control and legacy-import evidence. Earlier
`research/herdr-191/native/` packets describe an earlier read-only preview,
not the current feature's entire capability. Plain Cargo builds without this
feature still compile the legacy development host; `--release` alone does not
select the native workbench.

## Reproduce the #190 fixed-source build after download failures

The follow-up at `research/herdr-190/completion/` records a successful original
Intel macOS build, CLI checks, 3,714 passing Rust tests / 12 ignored profiles,
and an isolated real-PTY TUI smoke. This is not Viva integration, a release,
license clearance, Windows coverage, or human visual acceptance.

Zig 0.16.0's fetcher returned HTTP 400 for some fixed Ghostty URLs in the tested
environment while curl could download those same URLs. Seed the local cache
only after checking the unchanged manifest's content hash. Run `zig fetch`
**inside `upstream/herdr/vendor/libghostty-vt`**, which has a Zig project;
fetching outside a project failed with this toolchain.

The original Codeberg endpoint explicitly denied this machine's IP. Do not
rotate IPs/proxies or bypass that block. A separately accessible GitHub copy
contained the exact fixed commit. Its archive was accepted only because Zig
returned the original pinned content hash, not because the repository name was
trusted. For this specific snapshot, from the repository root:

```sh
# Set ZIG to the absolute, verified Zig 0.16.0 executable first.
# Use your own new scratch directory; these commands do not change any manifest.
scratch="$(mktemp -d /tmp/herdr-fixed-deps.XXXXXX)" || exit 1
# Public exact-commit artifact, not a change to the declared dependency URL:
gh api repos/moderatelyConfused/translate-c/tarball/4e879eb8aba615de112eabd1231ea6e01920cead \
  > "$scratch/translate-c.tar.gz" || exit 1
gh api repos/vancluever/arocc/tarball/f97cdfc3779aec4b242299e2fc9a1c828c3547c6 \
  > "$scratch/aro.tar.gz" || exit 1
(
  cd upstream/herdr/vendor/libghostty-vt || exit 1
  test "$("$ZIG" fetch "$scratch/translate-c.tar.gz")" = \
    'translate_c-0.0.0-Q_BUWhVNBwDOEcIqub4VFPJPB6D9dgwzUMHTX5KWr8Xr' || exit 1
  test "$("$ZIG" fetch "$scratch/aro.tar.gz")" = \
    'aro-0.0.0-JSD1Qk6lNgDdcDV4Vh7Sfy-34m2TluIVOdPzMmj_0BjX' || exit 1
)
```

Other recovered archives used the **original** `deps.files.ghostty.org` URLs
with curl, then the same content-hash check. Full URL/hash records are in
`completion/baseline.json` and `checks.json`; uucode was already seeded in the
previous #190 run. Never copy a different package under the expected cache name
or change a pin merely to get a build. A bundled translate-c candidate with a
different hash was explicitly rejected.

Broader validation used real nextest 0.9.150, Git 2.51.0, uutils sha256sum 0.2.2,
just 1.59.0, Bun 1.4.4 and Python 3.11 installed only in temporary tool paths.
Reduced test concurrency did not filter tests. `just check` reached its Windows
SDK prerequisite and stopped; its Unix stages and standalone docs checks are
recorded separately, not presented as a full PASS. No SDK license was accepted.
The raw PTY capture script is an experiment asset, not a production automation
API; run it only against the original binary in its new isolated HOME/session.

## Compare the import with its source

The fixed import, not the derived current HEAD, must match `herdr-source.json`
(including executable bits and symlinks):

```sh
git rev-parse 48ce171f11a249de575df71d59d868caf1a1e1c4:upstream/herdr
# The following is now a DIFFERENT, derived tree:
git rev-parse HEAD:upstream/herdr
```

This check establishes the original import, not a prohibition on developing
from it. Future modifications must retain upstream attribution and prominently
mark changed files, as required by Apache-2.0. Keep the original source commit
available as the comparison baseline; document each Viva-specific change.

## Safety and scope

The **original standalone baseline** has Herdr's own identity, paths, update
channels, socket API, hooks and worktree actions. It is not a Viva-authorized
worker host. Never attach baseline tests to an existing Herdr server, run its
updater/installers or operate existing worktrees. Interactive tests use a new,
explicitly isolated session and configuration.

The **current embedded runtime** overrides paths into `VIVA_HOME`, disables
Herdr self-update/manifest refresh/plugins and refuses unintegrated operations.
Native `viva`, `workbench` and `open` share the Herdr renderer; top-level domain
controls use the native host or canonical store, not a hidden legacy Host.
It now connects live-grant-checked owner dispatch to real canonical Execution,
LaunchSpec/PID/exit records, supports bound member reports, and can adopt live
legacy PTYs. Adoption is degraded, not a general native-to-native upgrade.
Local controller capability is not human presence or a same-UID sandbox;
explicit member effects still need their own bindings and grants. No Keychain
or interactive login is used. See [README.md](../README.md) for command routing
and source-versus-installed-channel distinctions.

This is **partial integration, not full acceptance**. Member-initiated dispatch,
other unintegrated effects, comprehensive native persistence redaction, restart
reconciliation and final package/license review remain outstanding; pane disk
history is disabled. Automatic native upgrade is tracked by #201. A green
upstream suite does not prove all ingress authorization, data preservation,
installed-product or human UI acceptance. Legacy protocol/data readers and
non-native historical fixtures are retained, not an installed native UI bypass.

No upstream installer/hooks are automatically installed into user tools by this
embedding. Its nested `.github/workflows` do not run as Viva workflows. Tests
must not operate real user data, installations, live sessions or Keychain.

The current route, decision supersession and remaining acceptance live in the
[Wiki Roadmap](https://github.com/zuohaisu/viva/wiki/Roadmap) and #187.
