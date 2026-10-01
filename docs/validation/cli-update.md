# CLI update — plan and validation

## Scope and reuse

Add explicit `viva update` and read-only `viva update --check`. Reuse the
existing GitHub release artifacts/checksums and npm wrapper/platform packages;
the reuse/gap entry is in `docs/architecture/viva-transition.md` §6. No update
service, new runtime protocol, automatic update schedule or state migration.

- Compare semantic version precedence, excluding build metadata. Only newer
  stable releases are installed; prereleases/drafts/downgrades are refused.
- Global npm: identify the wrapper and platform package, retain the original
  prefix, query npm `latest`, install the checked version through npm, verify
  the newly installed wrapper's version. No direct edits to npm's binary.
- Standalone macOS: select the compiled binary's Apple Silicon/Intel target,
  fetch official release metadata over HTTPS, validate asset URLs/sizes,
  download archive + checksum, verify SHA-256, copy only the exact regular-file
  binary entry, probe its version, atomically rename it over the old binary.
  Use a persistent private advisory lock and same-filesystem temporary files.
- Parse/reject invalid arguments before any effects. Refuse Cargo build
  outputs, directly invoked package-managed binaries, and known member/grant
  contexts for installation. These CLI gates are not an OS sandbox.
- Never open VIVA_HOME or call the resident server. Running agents are not
  stopped; activating the installed server version is an explicit
  `viva server-restart` followed by reconnecting the TUI.

## Evidence (2026-10-01, this worktree)

| Command | Result |
| --- | --- |
| `cargo test -p viva --test update` | PASS: 4 subprocess tests, including npm channel/prefix handling and no home initialization |
| `cargo test --workspace -- --test-threads=1` | PASS: all non-ignored tests; the existing 5 manual acceptance tests and new opt-in network smoke remain ignored by default |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS |
| `cargo fmt --all -- --check` | PASS |
| `git diff --check` | PASS |
| `bash packaging/npm/test-local.sh` | PASS: stage → pack → local install → wrapper exec, exit codes and trusted npm package-root propagation |
| `node --check packaging/npm/viva/bin/viva.js` | PASS |
| `target/debug/viva update --check` | PASS: official GitHub metadata reported current/latest `0.2.0`; no installation changed |
| `cargo test -p viva --lib update::tests::live_github_release_updates_only_sandbox_binary -- --ignored --nocapture` | PASS: real official `v0.2.0` macOS Intel archive/checksum downloaded, verified, executed and installed in a temporary sandbox only |

The updater has 14 offline unit tests covering version/platform selection,
no-op/check behavior, successful native replacement, corrupt/missing downloads,
unsafe/duplicate/missing archive binary entries, version mismatch, changed
installation/concurrent lock refusal, release URL/size validation, npm layouts,
npm failure/verification, and subprocess deadlines. The subprocess tests use
a fixture npm installer; **a real global npm upgrade was not run**.

### Existing parallel PTY instability — not a clean parallel-suite PASS

`cargo test --workspace` failed in two existing `v05_terminal` tests
(`slow_consumer_backpressure_keeps_the_session_controllable`: EIO;
`high_volume_output_stays_bounded_and_the_log_is_redacted`: zero output).
The isolated `cargo test -p viva --test v05_terminal` rerun passed all 7.
`cargo test --workspace --no-fail-fast` ran all targets but still failed the
slow-consumer test; every other target passed. No PTY source/test was changed.

A clean `git archive origin/main` exported to
`/tmp/viva-cli-update-baseline.FLQa7Y`, followed by
`CARGO_TARGET_DIR="$PWD/target" cargo test --manifest-path
/tmp/viva-cli-update-baseline.FLQa7Y/Cargo.toml --test v05_terminal`, also
failed the unchanged high-volume test (zero output) and real-PTY-input test
(EIO). This reproduces the instability without the updater changes.
Serial full-suite success does not erase those parallel failures.

## Known limitations

- macOS native releases only. Platform mapping is tested for both targets;
  the real artifact smoke in this session exercised Intel only.
- Native upgrade replaces only the executable, not bundled Pi extension
  sources or licenses; refresh those from the matching release separately.
- SHA-256 is corruption/integrity evidence, not an independent signature.
- npm owns installation/failure semantics and may partially change packages
  before reporting an error; the native atomic-replacement guarantee does not
  apply to npm. Local npm installs require a project-level npm command.
- No sudo, automatic server restart, rollback command or release publication.
  Previously published binaries without `update` need a one-time reinstall
  to obtain this feature. No user installation, Viva home or worktree was
  upgraded/deleted during validation.

## v0.2.1 release preflight

The owner requested v0.2.1 publication. The existing PR #52 now carries the
package-version bump in `crates/viva/Cargo.toml` and `Cargo.lock`, with release
notes in `docs/releases/v0.2.1.md`. npm manifests continue to derive their
version from the release tag through the existing packaging workflow.

- `cargo test --workspace --no-fail-fast`: PASS for every non-ignored test,
  including the default parallel PTY tests in this run. Earlier local PTY
  failures above remain recorded; this pass does not erase them.
- `cargo fmt --all -- --check`: PASS.
- `cargo clippy --workspace --all-targets -- -D warnings`: PASS.
- `cargo build --release --locked`: PASS on the current macOS Intel host.
- `target/release/viva --version`: reports `viva 0.2.1`.
- `target/release/viva update --help`: PASS; new command is present.
- `target/release/viva update --check`: PASS; current `0.2.1`, public latest
  `0.2.0`, no downgrade performed.
- `bash packaging/npm/test-local.sh` and
  `node --check packaging/npm/viva/bin/viva.js`: PASS.
- `git diff --check`: PASS; release diff inspected for secrets/artifacts.
- GitHub repository secret metadata includes `NPM_TOKEN`; its value was not
  read. `npm view @zuohaisu/viva dist-tags --json` still reports `latest`
  `0.2.0`; secret presence is not proof that a future publish will succeed.

Publication remains pending the repository owner's merge of PR #52. The
implementation author must not merge/approve its own PR, or bypass that
boundary by releasing an unmerged feature branch. No `v0.2.1` tag, GitHub
Release, npm publication or production installation was created by this
preflight. After owner merge, the requested release can use the existing
`v*` tag workflow and verify both artifact and npm channel results.
