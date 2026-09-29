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
> Canonical product definition: [docs/product/vision.md](docs/product/vision.md).

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

**From source:**

```bash
cargo install --path crates/viva    # or: cargo build --release
viva init && viva doctor
```

**Run Viva:**

```bash
viva start               # the active host for this VIVA_HOME
viva status              # from any second terminal/process
```

## Technology decision

The approved target is **Rust + Ratatui/Crossterm** for the Viva host and
terminal surface, **Pi + a small TypeScript extension** for member
conversation, and **SQLite + ordinary files** for Viva state.
[ADR 0011](docs/decisions/0011-rust-host-and-tui.md) is the authoritative
selection. The Python runtime was retired in V13
([retirement record](docs/validation/v13-python-retirement.md) maps every
retired behavior to its Rust evidence); the Rust binary is the only `viva`
entry.

## Architecture

- [IDEA.md](IDEA.md) — the charter and the ontology invariants.
- [AGENTS.md](AGENTS.md) — the working charter for agents in this repo.
- [docs/product/](docs/product/) — vision, product model, customer zero,
  workflows, phase-1 scope, first-usable-version gate.
- [docs/architecture/](docs/architecture/) — conceptual architecture, domain
  model, temporal model, and the migration/retirement record.
- [docs/decisions/](docs/decisions/) — ADRs 0001–0011.
- [docs/validation/](docs/validation/) — V12 baselines and final acceptance
  records with raw evidence.

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
