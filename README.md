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
- **Active control plane** — `viva start` becomes the single
  active host for the home (a second start refuses and names the running
  one) and serves a private Unix-socket channel. From any other process:
  `viva status/dispatch/terminals/stop-terminal/result/handoff/shutdown`.
  Dispatch is grant-checked at the moment of effect, idempotent by request
  key, and launches a real PTY in the task's worktree. With no active
  host, mutations are clean rejections — no daemon is ever started behind
  your back. On a crash, the next host reconciles: interrupted launches and
  orphaned executions get honest recovery records; nothing is re-run,
  nothing is killed, finished tasks refuse re-dispatch.
- **Parallel development workbench** — one Viva instance spans projects, tasks,
  worktrees and terminals (see `crates/viva/src/tui/workbench/`): real git
  facts (branch/dirty/diff), purpose-terminals per worktree with focused
  keyboard input that never crosses neighbors, and a quit protocol that
  stops owned processes while preserving every worktree and record.
- **Conversation metadata** — `viva conversations` keeps the Viva-owned
  tree: display names, fork hierarchy, write-once native session pointers,
  explicit task attachment, and cross-harness handoffs with declared
  capability. The harness (Pi) owns the transcript; Viva owns the tree.
- **Pi as the default conversational host** — members meet in Pi's own UI
  inside a Viva terminal (`crates/viva/src/harness/pi/` +
  [extensions/pi/](extensions/pi/)). The extension injects the member's
  task context and offers status queries, grant-controlled dispatch and
  explicit handoff. It states plainly when no external memory is connected.
- **Data preservation** — `viva data export --out <dir>` dumps every fact
  table read-only to JSON with a manifest. Historical Ticket Autopilot data
  is never read, written or migrated.

**Not implemented, and never faked**: automatic reflection, self-model
evolution, automatic memory promotion, a daemon, automatic remote writes,
multi-user support. Unavailable models/tools/providers fail loudly; Viva
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

Updates do not open/migrate `VIVA_HOME`, alter members/tasks/history, or
restart running servers/agents. After installing, explicitly run
`viva server-restart` to use the new binary in a running server, then reconnect
the TUI. Upgrades are user-invoked, not automatic member maintenance actions.
Older releases without this command need a one-time npm reinstall or release
download before `viva update` becomes available.

**Run Viva:**

```bash
viva start               # the active host for this VIVA_HOME
viva status              # from any second terminal/process
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
  office/       active host, control channel, reconciliation (V07)
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

## License

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

MIT — see [LICENSE](LICENSE). Copyright (c) 2026 zuohaisu
