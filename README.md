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

`viva` is a single native binary (Rust + SQLite). All facts live in
`$VIVA_HOME` (default `~/.viva`, 0700); member names and model/tool bindings
are configuration, and every completion claim carries recorded evidence.

- **Foundation** — `viva init` creates the home and applies the full Viva
  schema; `viva doctor` reports state, schema versions and table counts;
  `viva event add/list` persists and reloads events across restarts.
- **Resident server** — `viva server` (alias `viva start`) runs the
  single resident server for the home (a second start refuses and names the
  running one); it owns the store, every terminal and a private Unix-socket
  channel until `viva shutdown` ends it. Plain `viva` attaches the workbench
  and spawns a detached server first when none runs. From any other process:
  `viva status/dispatch/terminals/stop-terminal/result/handoff/shutdown`.
  Dispatch is grant-checked at the moment of effect, idempotent by request
  key, and launches a real PTY in the task's worktree. With no server
  running, mutations are clean rejections; only `viva` and `viva server`
  start one. On a crash, the next server reconciles: interrupted launches and
  orphaned executions get honest recovery records; nothing is re-run,
  nothing is killed, finished tasks refuse re-dispatch.
- **Parallel development workbench** — one Viva instance spans projects, tasks,
  worktrees and terminals (see `crates/viva/src/tui/workbench/`): real git
  facts (branch/dirty/diff), purpose-terminals per worktree with focused
  keyboard input that never crosses neighbors. `q` detaches the TUI while
  the server keeps every terminal running; `viva shutdown` stops owned
  processes while preserving every worktree and record.
- **Conversation metadata** — `viva conversations` keeps the Viva-owned
  tree: display names, fork hierarchy, write-once native session pointers,
  explicit task attachment, and cross-harness handoffs with declared
  capability. The harness (Pi) owns the transcript; Viva owns the tree.
- **Pi as the default conversational host** — members meet in Pi's own UI
  inside a Viva terminal (`crates/viva/src/harness/pi/` +
  [extensions/pi/](extensions/pi/)). The extension injects the member's
  task context and offers status queries, grant-controlled dispatch and
  explicit handoff. External memory (F04) stores native facts in the Viva
  database with member/project scope, provenance, usage evidence, and
  recoverable archive (`viva memory …`; no Hermes checkout or Python
  required). Legacy Hermes link rows remain as `legacy_hermes` for provenance
  but do not recall. There is no automatic recall or write-back in chat.
  Since #101 the Pi memory tools register against the real Pi API; end-to-end
  Pi exercise is tracked separately.
- **Data preservation** — `viva data export --out <dir>` dumps every fact
  table read-only to JSON with a manifest. Historical Ticket Autopilot data
  is never read, written or migrated.

**Not implemented, and never faked**: automatic reflection, self-model
evolution, automatic memory promotion, starting at login (planned, to be
configurable), an automatic task-scheduling daemon (a required goal; today
the server only runs scheduled recovery and auto-sync), automatic remote
writes, multi-user support. Unavailable models/tools/providers fail loudly; Viva
never substitutes one silently.

## Install

Requires **macOS** (Apple Silicon or Intel). External tools, when you use the
matching features: `git` (required for worktrees; fetched read-only), `gh`
(optional, GitHub evidence), [Pi](https://github.com/earendil-works/pi)
(optional, member conversation host) and your own model-CLI credentials —
Viva never stores credentials.

**From a release (no Rust toolchain needed):** grab `viva-macos-<arch>.tar.gz`
from the [releases page](https://github.com/zuohaisu/viva/releases), unpack,
and run `./viva`. Each artifact ships with a sha256 checksum and includes the
Pi extension sources and licenses.

**With npm (prebuilt binary; needs Node ≥ 18):**

```bash
npm i -g @zuohaisu/viva    # installs the `viva` command
```

**From source:**

```bash
cargo install --path crates/viva    # or: cargo build --release
viva init && viva doctor
```

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

Updates do not open/migrate `VIVA_HOME` or alter members/tasks/history.
After installing, a running resident server is restarted through the live
handoff: the new binary takes over and the terminals survive; reconnect the
TUI afterwards. `viva update --no-restart` skips this (`viva server-restart`
does it later); with no server running, nothing is started. Upgrades are
user-invoked, not automatic member maintenance actions.
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

Pack an archive the same way as `.github/workflows/release.yml`. On the Mac,
from a checkout of the commit under test:

```bash
cargo build --release -p viva
DIR=viva-macos-arm64   # Intel: viva-macos-intel
mkdir -p "$DIR/extensions"
cp target/release/viva "$DIR/viva"
cp -R extensions/pi "$DIR/extensions/pi"
rm -rf "$DIR/extensions/pi/node_modules"
cp LICENSE README.md "$DIR/"
tar -czf "$DIR.tar.gz" "$DIR"
if command -v shasum >/dev/null 2>&1; then
  digest=$(shasum -a 256 "$DIR.tar.gz" | awk '{ print $1 }')
else
  digest=$(sha256sum "$DIR.tar.gz" | awk '{ print $1 }')
fi
printf '%s  %s\n' "$digest" "$DIR.tar.gz" > "$DIR.tar.gz.sha256"
```

Publish the architectures you built. Copy the archive and the checksum into
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

The script replaces `~/.local/viva-test` only. It does not change `~/.viva`
or a `viva` already on `PATH`. If a test server is already running, stop it
with the same `VIVA_HOME` before installing again:

```bash
VIVA_HOME="$HOME/.viva-test" ~/.local/viva-test/viva shutdown
```

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
viva                     # attach the workbench (starts the resident server if needed)
viva status              # from any second terminal/process
viva shutdown            # stop the resident server and its owned processes
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
  terminal/     real PTY sessions (spawn/input/resize/snapshot/stop/wait)
  git/          worktrees (discover/create/adopt), read-only GitHub evidence
  knowledge/    sources, lifecycle, selection, usage evidence
  conversations/                        tree, forks, handoffs (V10)
  server/       active host, control channel, reconciliation (V07)
  harness/      explicit launch combinations; Pi specialization (V09)
  tui/          shell, parallel-development workbench (V14), conversations
extensions/pi/  the Pi member extension (TypeScript, independently tested)
```

## Testing

```bash
cargo test --workspace          # Rust: units + per-issue acceptance tests
python -m pytest tests/acceptance -q   # acceptance tooling (stdlib-only)
cd extensions/pi && npm ci && npm run typecheck && npm test   # Pi extension
```

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
channel; nothing self-declares. Every persisted artifact is private
(0o600/0o700) and secret-redacted before it is written. Viva never reads,
writes or migrates the historical `~/.ticket-autopilot/` state.

## Acknowledgements

Viva reuses proven tools. The workbench replacement now starts directly from
herdr source rather than reimplementing its behavior (tracking [#187](https://github.com/zuohaisu/viva/issues/187)).

- **[herdr](https://github.com/herdrdev/herdr)** (Apache-2.0) — the mature
  terminal-multiplexer reference behind Viva's resident runtime and workbench
  chrome: a resident server that holds all terminal sessions with
  detach/attach, a workspace/tab/pane layout, mouse-first pane interaction,
  and sidebar-based agent awareness. The unmodified source baseline is retained
  in [`upstream/herdr/`](upstream/herdr/) with its original licenses; build and
  scope instructions are in [`upstream/README.md`](upstream/README.md). This
  initial import does not yet replace the default Viva runtime or integrate
  Viva's authority and member/task/execution model. The alignment
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
