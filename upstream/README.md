# herdr source baseline

This is the starting source tree for Viva's source-based workbench replacement
(issue [#187](https://github.com/zuohaisu/viva/issues/187)), not another reference
implementation to translate. `herdr/` is an **unmodified, complete Git snapshot**
of the owner's local herdr checkout. Its source, tests, build configuration,
vendored dependencies, docs, and license notices are retained together so that
its original behavior can be built and compared before Viva-specific changes.
Upstream docs are third-party source assets, not Viva's canonical knowledge base.
The source commit and tree are recorded in `herdr-source.json`.

## Build and test the original baseline

```sh
cd upstream/herdr
rustup toolchain install
# Install Zig 0.16.0; alternatively set ZIG to its executable.
cargo build --locked
cargo test --workspace --locked
./target/debug/herdr --version
./target/debug/herdr --help
```

Upstream's broader check is `just check` (requires just, cargo-nextest, Bun,
and the Windows cross-compilation prerequisites described in its `AGENTS.md`).
The direct Cargo commands above do not replace those maintenance, integration,
documentation, or Windows checks.

The nested Cargo workspace intentionally keeps its upstream lockfile, dependency
patches, and Rust toolchain. Run Cargo **from that directory**, not via the root
Viva workspace. Root `cargo test --workspace` and current Viva CI do not test
this nested workspace. Neither workspace's package versions have been changed.

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

After the import is committed, this command must print the `tree` recorded in
`herdr-source.json` (all imported bytes, executable bits and symlinks):

```sh
git rev-parse HEAD:upstream/herdr
```

This check establishes the original import, not a prohibition on developing
from it. Future modifications must retain upstream attribution and prominently
mark changed files, as required by Apache-2.0. Keep the original source commit
available as the comparison baseline; document each Viva-specific change.

## Safety and scope

This first slice does **not** replace the installed `viva` entry or migrate any
user data. The binary still identifies as herdr and has herdr's upstream paths,
update channels, socket API, hooks, and worktree actions. It does **not** enforce
Viva's live grants or write Viva Resident/Task/Execution attribution. Do not use
it as a Viva-authorized worker host, attach it to an existing herdr server, run
its updater/installers, or operate existing worktrees as part of baseline tests.
Any interactive test must use a disposable, explicitly isolated session and
configuration, with the exact scope recorded on #187.

No scripts, hooks or workflows from the nested snapshot are installed or
activated in Viva. In particular, its nested `.github/workflows` do not run as
Viva workflows. Before replacing the default Viva runtime, validate authority,
source attribution, redaction, data preservation and real TUI interactions.
Passing upstream tests alone is not evidence that Viva integration is complete.

The current route, decision supersession and remaining acceptance live in the
[Wiki Roadmap](https://github.com/zuohaisu/viva/wiki/Roadmap) and #187.
