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
  and spawns a detached server first when none runs. Release builds (compiled
  with the embedded Herdr runtime) use that same native workbench for
  `viva`, `viva workbench` and `viva open <path>`, inheriting a live legacy
  server on first start. From another local process:
  `viva status/dispatch/terminals/stop-terminal/result/pause/resume/shutdown`.
  Native `handoff` and member-initiated dispatch remain explicitly unavailable
  pending #191 P2; they never start a legacy host.
  Dispatch is grant-checked at the moment of effect, idempotent by request
  key, and launches a real PTY in the task's worktree. With no server
  running, execution mutations are clean rejections; workbench entry points,
  native workspace navigation and `viva server` can start the native host.
  Native crash reconciliation and member-execution upgrade remain #191 P3; an
  unowned live execution is refused rather than replayed or called complete.
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
permission to dispatch or stop. Scoped cooperative native upgrades are described
below; output persistence redaction, member/crash recovery and final human visual
acceptance remain #191 work.

Native command routing: `workspace` keeps Viva's canonical composition and
`.code-workspace` semantics; `open` then attaches the same Herdr renderer.
`brief/create-task/create-project/grant/result/events` reuse the existing domain store;
mutations authenticate against a live native host and refuse known member
contexts. `stop-terminal` accepts a live execution's public pane locator;
ordinary shell stopping is not yet exposed there. `agent/terminal/scene/plugin/
sync/recovery/handoff` and `server-restart` remain explicit native
refusals (P2/P3), not legacy fallbacks. The old renderer and host library remain
only in non-native development builds, historical handoff fixtures and retained
legacy data readers; native release entry points cannot reach the old UI.

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

The Tailnet development channel remains separate from GitHub Releases/npm
`latest` used by `viva update`. Its installer is tracked in
[`packaging/install.sh`](packaging/install.sh) (macOS, Python 3, curl).
Deploying that file or publishing a package still requires owner authorization;
a repository change does not update the currently served installer.

Serve it over trusted HTTPS with `channel.json` and immutable archives. The
manifest selects `assets.macos-arm64` / `assets.macos-intel`, each with `file`
(a basename ending in `.tar.gz`) and `sha256`. Archives use the complete native
payload from `.github/workflows/release.yml`, including extensions and notices;
checksum integrity does not independently authenticate the publisher.

```bash
BASE=https://your-tailnet-host/viva
curl -fsSL "$BASE/install.sh" | VIVA_INSTALL_BASE="$BASE" \
  VIVA_PREFIX="$HOME/.local/viva-test-install" VIVA_HOME="$HOME/.viva-test" sh
VIVA_HOME="$HOME/.viva-test" "$HOME/.local/viva-test-install/bin/viva"
```

The installer stages and verifies before replacing files, preserves the previous
binary/wrapper, and asks Viva to activate the same installation and VIVA_HOME.
No running runtime means **no daemon is started**. Cooperative native generations
support live PTY/shell transfer and negotiated reconnect of existing compatible
frontends; the frontend executable is not reloaded. The actual target binary
hash, new host identity, old host exit and frontend reconnect are checked, not
just `--version`. Owned stale endpoints with no listener count as no runtime
and are left intact; unsafe or partially live endpoints are refused. A normal
`viva update` also retries deferred/failed activation when files are already
current. Handoff preflight applies only when files will actually change; an
already-active build does not interrupt active work. No-frontend upgrades report
`no_previous_clients`, not a fabricated client recovery.

**Current limits (#201):** 0.3.10 and earlier native hosts lack the cooperative
protocol and are refused before replacement; active canonical member executions
or worker credentials also block upgrade before any handoff. Do not kill live
work to bypass this guard. Those upgrade paths remain unfinished, not successful
cold recovery. Native npm package replacement with a live host remains
unsupported; retrying activation of an already installed executable is separate
from package replacement. Exact-schema attachment performs no database migration; retained files are **binary backups,
not database backups**. An ambiguous post-commit failure retains installed files
and reports failure instead of blindly rolling back behind a new runtime.
Intel installation, user-data migration and human visual acceptance require their
own evidence. PR builds keep the package version; record commit and binary hash.

**Run Viva:**

```bash
viva                     # workbench (native default in release builds)
viva workbench           # same native workbench in release builds
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
