# Viva

> **Viva is Haisu's local-first personal AI collaboration system.**
>
> Several persistent AI members work through Viva — each with its own role,
> history and knowledge — and drive real tools to get work done. Viva is not an
> AI member: it preserves their working relationships and history.
>
> Customer Zero is **Haisu**; Viva ships with no built-in personas.
> `Samuel`, `Deven` and `Alice` are records you create — configuration data,
> never hard-coded identity.
>
> Canonical product definition and project knowledge: the
> [GitHub Wiki](https://github.com/zuohaisu/viva/wiki) — see
> [Vision](https://github.com/zuohaisu/viva/wiki/Vision).

## The core distinctions

```text
Resident ≠ Role ≠ Cognitive Engine ≠ Worker ≠ Execution Session
Workspace ≠ Project ≠ Repository ≠ Worktree ≠ Task
raw event ≠ experience ≠ memory ≠ self-model ≠ identity
Session dies. Resident persists.
```

## What Viva does today

This section describes **current source**, not every published binary.
Public GitHub/npm `0.3.9` predates the embedded Herdr runtime; the internal
Tailnet `0.3.10` build includes Keychain removal but predates the unified
public commands in PR #202. Neither version number proves current-main
behavior. Source, channel and acceptance records are in the
[Wiki Releases](https://github.com/zuohaisu/viva/wiki/Releases). References to
#191's P0–P5 below identify implementation/acceptance records; that issue is now
closed, not a claim that all remaining gates passed. Current follow-up scope is
in [Roadmap](https://github.com/zuohaisu/viva/wiki/Roadmap) and #201.

`viva` is a single native binary (Rust + SQLite). Viva's domain records live in
`$VIVA_HOME` (default `~/.viva`, private directory); original harness transcripts
may live outside it. Member names and model/tool bindings are configuration.
The native behavior below requires `--features herdr-runtime`; Cargo's default
feature set still builds the legacy development host.

- **Foundation** — `viva init` creates the home and applies the full Viva
  schema; `viva doctor` reports state, schema versions and table counts;
  `viva event add/list` persists and reloads events across restarts.
- **Resident server** — `viva server` (alias `viva start`) runs the
  single resident server for the home (a second start refuses and names the
  running one); it owns the store, every terminal and a private Unix-socket
  channel until `viva shutdown` ends it. Plain `viva` attaches the workbench
  and spawns a detached server first when none runs. Release builds (compiled
  with the embedded Herdr runtime) use that same native workbench for
  `viva`, `viva workbench` and `viva open <path>`, inheriting a live legacy
  server on first start. From another local process:
  `viva status/dispatch/terminals/stop-terminal/result/pause/resume/shutdown`.
  Native member `dispatch` and `handoff` require a host-issued credential,
  the actual bound execution process, and live authority; they never acquire
  controller authority or start a legacy host. Cross-member dispatch requires
  a live target grant descended from the caller's authorized source grant.
  Dispatch is grant-checked at the moment of effect, idempotent by request
  key, and launches a real PTY in the task's worktree. With no server
  running, execution mutations are clean rejections; workbench entry points,
  native workspace navigation and `viva server` can start the native host.
  Native crash reconciliation and in-place restart remain #191 P3; an
  unowned live execution is refused rather than replayed or called complete.
- **Parallel development workbench** — the native build directly embeds
  Herdr's renderer, server and PTYs from `upstream/herdr/`, connected through
  `crates/viva/src/native_runtime/`. `Ctrl+B`, then `q` detaches the TUI;
  the host and live terminals keep running. `viva shutdown` stops that home's
  native host and owned terminals; it does not delete worktrees or domain
  records. The older `tui/workbench/` renderer is retained for non-native
  development and historical checks, not as a native UI fallback.
- **Conversation metadata** — `viva conversations` keeps the Viva-owned
  tree: display names, fork hierarchy, write-once native session pointers,
  explicit task attachment, and cross-harness handoffs with declared
  capability. The harness (Pi) owns the transcript; Viva owns the tree.
- **Pi integration** — Pi is the selected conversational host, using its own
  UI plus `crates/viva/src/harness/pi/` and
  [extensions/pi/](extensions/pi/). The extension supplies member/task context
  and tools for status, dispatch and handoff. Tool registration is not proof
  that every end-to-end scenario works: native member dispatch/handoff use the
  process-bound path above, while the top-level `agent` interface remains
  unintegrated. Real Pi acceptance is still tracked in
  [#18](https://github.com/zuohaisu/viva/issues/18).
  External memory (F04) stores native facts in the Viva
  database with member/project scope, provenance, usage evidence, and
  recoverable archive (`viva memory …`; no Hermes checkout or Python
  required). Legacy Hermes link rows remain as `legacy_hermes` for provenance
  but do not recall. There is no automatic recall or write-back in chat.
  Since #101 the Pi memory tools register against the real Pi API; end-to-end
  Pi exercise is tracked separately.
- **Data preservation** — `viva data export --out <dir>` dumps every fact
  table read-only to JSON with a manifest. Historical Ticket Autopilot data
  is never read, written or migrated.

**Native entry boundaries.** The embedded Herdr entry is Unix-only and a
**degraded import**, not a seamless takeover: live child processes, screen and
scrollback, rows/columns and scene/tab names survive, while terminal ids, pane
cwd/purpose, split ratios, focus/zoom, cell size and agent/worktree metadata
do not. Local control uses an incarnation-bound capability, delivered over
an inherited pipe or loaded by a fresh controller from the private local runtime
file. No Keychain, login or password interaction is used. Capability possession
is recorded as `local_runtime_controller` / `AgentCli`, **not human presence**.
Same-account programs can read same-account files: this is not a same-UID
sandbox. Explicit member execution channels still require process binding and
live grants; unrelated or expired credentials do not confer member authority.
`pane.list` / `pane.read` / `pane.process_info` are same-UID observation, not
permission to dispatch or stop. Native snapshots, trace writes, derived launch
argv, handoffs and generated briefs apply Viva redaction. Configure literal
secrets in `$VIVA_HOME/redaction.secrets` (one per line; `#` comments), loaded
at native host start, without hot reload. Current runtime capability and retained
worker credentials are registered automatically; revoked worker credentials stay
redacted. This protects new writes, not a retrospective scrub of old logs.
Raw PTY interaction remains unchanged. Pane history is still disabled, and
original user inputs/files and the private credential ledger are not rewritten. Native host update/recovery and final human visual
acceptance remain #191 work.

Native command routing: `workspace` keeps Viva's canonical composition and
`.code-workspace` semantics; `open` then attaches the same Herdr renderer.
`brief/create-task/create-project/grant/result/events` reuse the existing domain store;
mutations authenticate against a live native host and refuse known member
contexts. `stop-terminal` accepts a live execution's public pane locator;
ordinary shell stopping is not yet exposed there. `agent/terminal/scene/plugin/
sync/recovery` and `server-restart` remain explicit native
refusals (P2/P3), not legacy fallbacks. The old renderer and host library remain
only in non-native development builds, historical handoff fixtures and retained
legacy data readers; native release entry points cannot reach the old UI.
Member handoffs are attributed reports included in briefs, not User/QA results,
Task completion, or memory formation. Native prompt/wait/terminal-control member
effects remain a separate unfinished P2 slice; this is not all of #191.

**Not implemented, and never faked**: automatic reflection, self-model
evolution, automatic memory promotion, starting at login (planned, to be
configurable), an automatic task-scheduling daemon, automatic remote writes,
multi-user support. Scheduled recovery and auto-sync exist in the legacy host;
their native CLI routes currently refuse rather than silently starting it.
Unavailable models/tools/providers fail loudly; Viva never substitutes one
silently.

## Install

Requires **macOS** (Apple Silicon or Intel). External tools, when you use the
matching features: `git` (required for worktrees; fetched read-only), `gh`
(optional, GitHub evidence), [Pi](https://github.com/earendil-works/pi)
(optional, member conversation host) and your own model-CLI credentials —
model-provider credentials remain in those tools' environments. Viva does store
its own local runtime capability; that is not a model-provider credential.

**Public stable channel (currently the legacy 0.3.9, not current main):**
No Rust toolchain is needed. Grab `viva-macos-<arch>.tar.gz`
from the [releases page](https://github.com/zuohaisu/viva/releases), unpack,
and run `./viva`. Each artifact ships with a sha256 checksum and includes the
Pi extension sources and licenses.

**With npm (prebuilt binary; needs Node ≥ 18):**

```bash
npm i -g @zuohaisu/viva    # installs the `viva` command
```

**From source (native workbench):** run from the repository root on macOS.
Install the Rust version pinned in `upstream/herdr/rust-toolchain.toml`
(currently `1.96.1`), Zig `0.16.0` and Xcode Command Line Tools. Put `zig` on
PATH or set `ZIG` to its executable. The root stable Rust toolchain alone may
be older than the native crate's requirement. Native build/cache troubleshooting
is in [upstream/README.md](upstream/README.md).

```bash
rustup toolchain install 1.96.1
cargo +1.96.1 build --release --locked -p viva --features herdr-runtime
# Optional installation: do not overwrite an existing installation unintentionally.
cargo +1.96.1 install --locked --path crates/viva --features herdr-runtime
viva init && viva doctor
viva runtime --version   # confirms the embedded runtime is available
```

Omitting `--features herdr-runtime` builds the legacy development host, **even
with `--release`**. `viva --version` alone cannot distinguish those builds.
The native feature's release-workflow wiring does not retroactively change
already-published archives.

### Update

```bash
viva update --check      # check the latest stable version without installing
viva update              # upgrade this installation
```

Global npm installs update through npm at the original prefix, keeping the
wrapper and platform package in sync. Local npm installs should use
`npm install @zuohaisu/viva@latest` from their project instead.

Standalone macOS binaries (including `cargo install`) update from the official
GitHub Release for their architecture: HTTPS download → SHA-256 verification
→ version probe → atomic executable replacement. Equal/older releases are not
installed; failed native downloads/validation leave the executable unchanged.
The installation directory must be writable; Viva never invokes `sudo`.
Cargo build outputs and directly invoked package-manager binaries are refused.
Native updates replace **only the executable**; refresh bundled Pi extension
sources separately from the matching release if you use them. Checksums verify
integrity, not an independent publisher signature.

The install step does not migrate the domain database. Activation is a separate
capability, not proved by the installed executable's `--version`:

| Running host → target | Current behavior / evidence |
| --- | --- |
| No host | Installs files; does not start a host. |
| Compatible legacy → legacy | The legacy updater's S3 live handoff carries terminals to a new legacy host; reconnect the TUI. `--no-restart` skips activation. |
| Compatible legacy → native | Starting the native workbench can import live PTYs and retire the legacy host after adoption. This is the degraded import described above, not a guarantee for arbitrary old versions or an end-to-end installer upgrade. |
| Native → newer native | In-place activation is **not implemented**. The updater reports that files were installed but the old host is still running; native `server-restart` refuses. Reattaching does not prove the host uses the new build. |

Do not stop live work just to satisfy an upgrade example. Automatic runtime
switching, client recovery and failure/rollback acceptance remain required by
[#201](https://github.com/zuohaisu/viva/issues/201); these limitations do not
cancel that requirement. An updater install result/exit status is not full
runtime-upgrade acceptance. `--no-restart` also leaves any native host on its
old in-memory code. Upgrades are user-invoked, not automatic member maintenance.
Older releases without this command need a one-time npm reinstall or release
download before `viva update` becomes available.

### Self-hosted test install

`viva update` installs the latest stable GitHub Release or the npm `latest`
tag. It does not read a private file server. To click-test a commit without
publishing, serve that commit's archive from a Linux host and install it on
the Mac that runs the TUI. The Linux host is only a file server.

Publish this directory over HTTP or HTTPS:

```text
install.sh
channel/viva-macos-arm64.tar.gz
channel/viva-macos-arm64.tar.gz.sha256
channel/viva-macos-intel.tar.gz
channel/viva-macos-intel.tar.gz.sha256
```

This is an **isolated test-package recipe**, not the deployed Tailnet installer
or release clearance. Use the native toolchain prerequisites above. It includes
more notice material than the current release workflow; the actual linked
Cargo/Zig dependency closure and final tar/npm license payload still require
review (the P4 work recorded on #191). Do not distribute a candidate until its
applicable full terms, copyright and source obligations have been checked.

On the Mac, from a clean checkout of the commit under test, with the default
Cargo target directory (no `CARGO_TARGET_DIR` override):

```bash
set -eu
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) DIR=viva-macos-arm64 ;;
  Darwin-x86_64) DIR=viva-macos-intel ;;
  *) printf 'Build on the matching macOS architecture\n' >&2; exit 1 ;;
esac
cargo +1.96.1 build --release --locked -p viva --features herdr-runtime
stage=$(mktemp -d)
mkdir -p "$stage/$DIR/extensions"
cp target/release/viva "$stage/$DIR/viva"
cp -R extensions/pi "$stage/$DIR/extensions/pi"
rm -rf "$stage/$DIR/extensions/pi/node_modules"
cp LICENSE README.md THIRD_PARTY_NOTICES.md "$stage/$DIR/"
cp -R licenses "$stage/$DIR/licenses"
cp upstream/herdr/LICENSE "$stage/$DIR/licenses/herdr-LICENSE"
cp upstream/herdr/vendor/libghostty-vt/LICENSE "$stage/$DIR/licenses/ghostty-LICENSE"
cp upstream/herdr/vendor/portable-pty/LICENSE.md "$stage/$DIR/licenses/portable-pty-LICENSE"
cp Cargo.lock "$stage/$DIR/Cargo.lock"
commit=$(git rev-parse HEAD)
printf '%s\n' \
  'Viva test build includes modified Herdr 0.9.3 source (Apache-2.0).' \
  "Source: https://github.com/zuohaisu/viva/tree/$commit" \
  'Modifications: library embedding, Viva domain/authority and CLI integration.' \
  'Not an original Herdr distribution or an endorsement by its authors.' \
  > "$stage/$DIR/NOTICE"
printf '%s\n' 'Component inventory (not a complete linked SBOM):' \
  'viva: MIT; herdr: Apache-2.0; ghostty-vt/portable-pty: MIT.' \
  'Exact Cargo graph: Cargo.lock. Native/source notices: THIRD_PARTY_NOTICES.md.' \
  > "$stage/$DIR/SBOM.txt"
tar -czf "$stage/$DIR.tar.gz" -C "$stage" "$DIR"
digest=$(shasum -a 256 "$stage/$DIR.tar.gz" | awk '{ print $1 }')
printf '%s  %s\n' "$digest" "$DIR.tar.gz" > "$stage/$DIR.tar.gz.sha256"
printf 'Candidate archives: %s\nSource commit: %s\n' "$stage" "$commit"
```

Keep that commit available with all modified source. The recipe assumes no
uncommitted source changes; if present, record the patch/hash too. Shipping a
source link or this notice superset alone is not proof of license compliance.

After the distribution review and deployment authorization, copy only archives
for the architectures actually built and their checksums from `$stage` into
`channel/`. The checksum line is the digest, two spaces, then the archive
name. Take the digest from `shasum` or from `sha256sum` and write the line
with `printf`, as above. Do not redirect `sha256sum` output unchanged:
binary mode writes `digest *name`, and macOS `shasum -c` rejects a line whose
spacing is not its own.

On a Tailscale network, bind the server to that node's `100.x.x.x` address and
serve plain HTTP. A bare IP has no trusted HTTPS certificate, so
`https://100.x.x.x` fails TLS verification. Tailscale already encrypts the
path. Do not listen on `0.0.0.0` when the machine also has a public address.
Caddy, with the `http://` prefix so it does not request a certificate:

```text
http://100.x.x.x {
  root * /var/www/viva-test
  file_server
}
```

`install.sh` on that host. It contains no server address. Set
`VIVA_TEST_BASE` to the origin you curl, with no trailing path:

```sh
#!/bin/sh
set -eu
if [ -z "${VIVA_TEST_BASE:-}" ]; then
  printf 'Set VIVA_TEST_BASE to the file server origin, with no trailing path\n' >&2
  exit 1
fi
BASE=${VIVA_TEST_BASE%/}
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) ASSET=viva-macos-arm64.tar.gz ;;
  Darwin-x86_64) ASSET=viva-macos-intel.tar.gz ;;
  *) printf 'macOS Apple Silicon or Intel only\n' >&2; exit 1 ;;
esac
if [ "$(id -u)" -eq 0 ]; then
  printf 'run as your user, not root\n' >&2
  exit 1
fi
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -fsSL "$BASE/channel/$ASSET" -o "$tmp/$ASSET"
curl -fsSL "$BASE/channel/$ASSET.sha256" -o "$tmp/$ASSET.sha256"
expected=$(awk 'NR==1 && $1 ~ /^[0-9a-fA-F]{64}$/ { print tolower($1); exit }' "$tmp/$ASSET.sha256")
if [ -z "$expected" ]; then
  printf 'checksum file must start with a 64-hex SHA-256\n' >&2
  exit 1
fi
actual=$(shasum -a 256 "$tmp/$ASSET" | awk '{ print tolower($1) }')
if [ "$expected" != "$actual" ]; then
  printf 'SHA-256 mismatch\n' >&2
  exit 1
fi
dest="$HOME/.local/viva-test"
rm -rf "$dest"
mkdir -p "$dest"
tar -xzf "$tmp/$ASSET" -C "$dest" --strip-components=1
chmod 755 "$dest/viva"
printf 'Installed %s\n' "$dest/viva"
printf 'export VIVA_HOME="$HOME/.viva-test"\n'
printf '%s\n' "$dest/viva"
```

On the Mac, with Tailscale connected (the same origin in both places):

```bash
curl -fsSL http://100.x.x.x/install.sh | VIVA_TEST_BASE=http://100.x.x.x sh
export VIVA_HOME="$HOME/.viva-test"
~/.local/viva-test/viva
```

The example script replaces `~/.local/viva-test` only (including anything
previously installed there). It does not change `~/.viva` or a `viva` already
on `PATH`. Use it only for a disposable test installation; it has no coordinated
runtime upgrade. Before replacing an existing test installation, finish its
work and use the **currently installed binary** with the **same `VIVA_HOME`**
to stop that test host, then confirm it has exited. For a native build from
PR #202 or later:

```bash
VIVA_HOME="$HOME/.viva-test" ~/.local/viva-test/viva shutdown
```

Earlier native builds (including the internal `0.3.10`) do not route top-level
`shutdown` to the native host. Their native command is `viva runtime server stop`
with that same binary/home. Non-native legacy builds use `viva shutdown`.
Stopping ends live processes; none of these commands is a seamless upgrade.
The deployed Tailnet manifest/installer and its existing evidence are tracked in
[Herdr-Viva-Integration](https://github.com/zuohaisu/viva/wiki/Herdr-Viva-Integration)
§18.3; they are not the generic script above. Full live upgrades remain #201.

Replacing the files in `channel/` publishes the next build. Run the same curl
command again. Leave `install.sh` unchanged when the origin changes or a new
archive is published: the origin is only in that command, and the script
trusts the checksum file's first field. It compares that digest to
`shasum -a 256` of the downloaded archive, so a GNU `sha256sum` line
(`digest *name` or `digest  name`) still verifies. `viva update` still
follows the stable channels, so do not run it against `~/.local/viva-test`.
A PR build can still print the version in `Cargo.toml`; record
`git rev-parse HEAD` with the bug report.

**Run Viva:**

```bash
# Current source, built with --features herdr-runtime:
viva                     # native workbench; auto-starts host if needed
viva workbench           # same native workbench
viva open ./project      # register/select workspace, then the same workbench
viva status              # native runtime status, from a second process
viva shutdown            # stop native executions and the resident host
```

## Documentation

Project documentation — product definition, architecture, roadmap, research,
decisions (ADRs), development records, validation records and release history
— is maintained in the **GitHub Wiki**, Viva's canonical documentation and
project knowledge source:

**<https://github.com/zuohaisu/viva/wiki>**

Start with [Home](https://github.com/zuohaisu/viva/wiki/Home) and
[Roadmap](https://github.com/zuohaisu/viva/wiki/Roadmap). The working charter
for agents — including how to read and update the Wiki — is
[AGENTS.md](AGENTS.md).

## Technology decision

The approved target is **Rust + Ratatui/Crossterm** for the Viva host and
terminal surface, **Pi + a small TypeScript extension** for member
conversation, and **SQLite + ordinary files** for Viva state.
[ADR 0011](https://github.com/zuohaisu/viva/wiki/ADR-0011) is the authoritative
selection. The Python runtime was retired in V13
([retirement record](https://github.com/zuohaisu/viva/wiki/V13-Python-Retirement)
maps every retired behavior to its Rust evidence); the Rust binary is the only
`viva` entry.

## Code layout

```text
crates/viva/src/
  foundation/   ids, paths, store (SQLite/migrations), events, envelope, records
  members/ workspaces/ projects/          configuration and context domains
  tasks/        task registry, launch-intent protocol, results, briefs
  authority/ redaction/                grants, denial ledger, redaction policy
  native_runtime.rs + native_runtime/    native composition, CLI, admission, import
  terminal/     legacy PTY + transferable handoff/data primitives
  git/          worktrees (discover/create/adopt), read-only GitHub evidence
  knowledge/    sources, lifecycle, selection, usage evidence
  conversations/                        tree, forks, handoffs (V10)
  server/       legacy Host/protocol, shared migrations and durable event query
  harness/      explicit launch combinations; Pi specialization (V09)
  tui/          legacy renderer and reusable scene/data readers
upstream/herdr/ embedded native renderer, headless host, VT/PTY (nested workspace)
extensions/pi/  the Pi member extension (TypeScript, independently tested)
```

## Testing

```bash
cargo test --workspace          # default (non-native) Rust suite
python -m pytest tests/acceptance -q   # acceptance tooling (stdlib-only)
cd extensions/pi && npm ci && npm run typecheck && npm test   # Pi extension
```

The default root suite does **not** exercise the embedded Herdr runtime.
Daily code CI runs feature-enabled core CLI/lifecycle tests and a real PTY
smoke. The manual full CI (`workflow_dispatch`) additionally runs native/imported
Clippy, nested nextest/maintenance and historical handoff matrices. Run that
entry for imported-source/dependency or upgrade/migration changes; a daily green
run is not full-suite evidence. Markdown-only PRs run the diff check and omit
product jobs. Exact targets, legacy fixtures and probes are in
[`.github/workflows/ci.yml`](.github/workflows/ci.yml) (`native-runtime`) and
[upstream/README.md](upstream/README.md).
Pi contract tests need a built `viva` binary (or `VIVA_BIN`); registration/tests
are separate from real model-driven Pi acceptance. Ignored/manual tests and
human UI checks are not passed evidence.

### Independent QA

Any agent can read the project-specific
[QA prompt](.agents/prompts/independent-qa.md) in a fresh session:

> 读取 `.agents/prompts/independent-qa.md`，按其中要求对 PR #编号进行独立 QA；只读审核，不修改代码，报告发现、证据和未验证项。

This is an on-demand review instruction, not an automatic CI gate or approval.

## Security and boundaries

Members and workers never gain repository-owner authority: no protected-branch
pushes, no merges, no approving their own PR, no self-authorization — the
vocabulary lives in `crates/viva/src/authority/` and protected actions are
refused to every grant. Caller roles exist only because Viva issued a
channel; nothing self-declares. Core domain writers use Viva's private-storage
and redaction helpers, but that is **not a universal guarantee for every native
artifact**. In particular, native session metadata is serialized by the imported
runtime; its raw paths/names/launch metadata have no blanket Viva redaction
coverage, and file modes are not uniformly set to 0600 by that writer. The
runtime directory is private; the local capability has explicit 0700/0600
checks. Native pane history is forcibly disabled because output-persistence
redaction is not integrated. Live pane observation remains accessible within
the same account. Complete persistence/redaction coverage remains the P2/P4
work recorded on #191; no secret-leak reproduction or clearance is claimed here.
Viva never reads, writes or migrates historical `~/.ticket-autopilot/` state.

## Acknowledgements

Viva reuses proven tools. The workbench replacement now starts directly from
herdr source rather than reimplementing its behavior (tracking [#187](https://github.com/zuohaisu/viva/issues/187)).

- **[herdr](https://github.com/herdrdev/herdr)** (Apache-2.0) — the mature
  terminal-multiplexer reference behind Viva's resident runtime and workbench
  chrome: a resident server that holds all terminal sessions with
  detach/attach, a workspace/tab/pane layout, mouse-first pane interaction,
  and sidebar-based agent awareness. [`upstream/herdr/`](upstream/herdr/) now
  contains the modified, embedded runtime with canonical member/task queries,
  real execution attribution/exit reporting, member reports and native public
  command routing. The original unmodified import is preserved at commit
  `48ce171f11a249de575df71d59d868caf1a1e1c4`; its tests/notices do not certify the
  later integration. Build and remaining safety/acceptance limits are in
  [`upstream/README.md`](upstream/README.md). The alignment
  analysis and decision records live in the Wiki
  ([Research-Herdr-Runtime-Parity](https://github.com/zuohaisu/viva/wiki/Research-Herdr-Runtime-Parity)
  and [ADR-0012](https://github.com/zuohaisu/viva/wiki/ADR-0012)).
- **[Orca](https://github.com/stablyai/orca)** (MIT) — the day-to-day
  workbench experience reference for running several agents in parallel
  across git worktrees. Also a design reference only; no source code is
  reused.

herdr source and its vendored dependencies retain their original license and
copyright notices; see [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).
Orca remains a design reference only.

## License

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Viva-authored code: MIT — see [LICENSE](LICENSE). Copyright (c) 2026 zuohaisu.
Third-party source retains its own licenses; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
