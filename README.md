# Viva

> **Viva is a persistent habitat for AI agents to live, work, remember, and grow.**
>
> Viva is where persistent agents live and work. Viva is not the agent — it is
> the place in which a persistent agent can live and work.
>
> Customer Zero is **Haisu**; the first resident is **Samuel**. Phase 1 serves
> that one collaboration: Haisu's daily development across workspaces,
> worktrees, workers, and models — with the context, history, and capabilities
> of the work accumulating instead of evaporating. The canonical product
> definition: [docs/product/vision.md](docs/product/vision.md).

```bash
$ viva
```

```text
VIVA

Resident      Samuel
Workspace     viva
Worktree      main
Workers       codex · claude
State         persistent (~/.viva)

Haisu >
```

## The core distinction

```text
Resident ≠ Worker ≠ Cognitive Engine
```

| Example | Role |
| --- | --- |
| Samuel | **Resident** — a long-lived AI identity; persists across sessions |
| Codex / Claude Code / Qoder | **Workers** — replaceable agent CLIs a resident delegates work to |
| GPT / Claude / GLM | **Cognitive engines** — the model currently powering cognition |
| `viva`, `VicTrader` | **Workspaces** — long-term working contexts |
| a TUI session | **Session** — transient; it dies, the resident persists |

Sessions end. Workers get upgraded or swapped. Models change. The resident —
its identity, history, and experiences — stays.

## What exists today (Phase 1)

Viva is young and honest about it. Working now:

- **`viva` TUI** — the Phase 1 product surface: resident, workspace, worktree,
  worker, and runtime status, with a live experience journal.
- **Residents** — `viva resident add/list`; persistent identity records.
- **Workspaces** — `viva workspace add/list/use`; registers local git repos,
  state persists across sessions.
- **Worktrees** — discovers a workspace's git worktrees (built on the
  delivery subsystem's audited worktree service).
- **Workers** — `viva worker list/run`; a generic, data-driven registry of
  agent CLIs with availability probing.
- **Experience journal** — append-only, secret-redacted record of what
  happened (`~/.viva/experiences/journal.jsonl`).

Not yet (and never faked): memory formation, self-model, autonomous
operation, daemon, voice/desktop/message channels. The journal is experience,
not memory.

## Quickstart

Requires Python 3.11+ and Git.

```bash
python3.11 -m venv .venv
. .venv/bin/activate
python -m pip install -e '.[dev]'

viva resident add Samuel          # your first resident (any name — Samuel is data, not code)
viva workspace add ~/Documents/code/viva viva
viva status                       # plain summary; --json for machines
viva                              # enter the TUI
```

In the TUI: type `help` for the command list, `Ctrl+Q` to quit. Resident and
workspace selections persist in `~/.viva/` and are restored on the next run.

The CLI also works without the TUI:

```bash
viva workspace list
viva worktree list                # worktrees of the current workspace (read-only)
viva worker list                  # availability probe of configured agent CLIs
viva worker run codex "explain this repository in one paragraph"
viva experience list
```

## Ticket Autopilot is now a capability, not the product

Ticket Autopilot was this project's original product focus: turn one structured
Plane ticket into an isolated worktree run — Developer → deterministic checks →
independent QA → bounded fix loop → auditable local commit — through a
localhost Web controller. It **survives intact** as Viva's automated
software-delivery subsystem and legacy product surface:

```bash
./start-ticket-autopilot          # legacy Web controller, unchanged
ticket-controller --help          # legacy CLI, unchanged
```

Its authoritative workflow definition is [docs/closed-loop-workflow.md](docs/closed-loop-workflow.md);
its state still lives in `~/.ticket-autopilot/` and is not touched by Viva.
This is evolution, not a correction: the delivery loop was the first proven
capability of the habitat.

## Architecture

- [IDEA.md](IDEA.md) — product charter and core invariants.
- [AGENTS.md](AGENTS.md) — working charter, goal checks, drift guard.
- [docs/architecture/viva-transition.md](docs/architecture/viva-transition.md) —
  the canonical transition record: what was reused, what stays
  Ticket-Autopilot-specific, the TUI technology decision, state layout, and
  the permission vocabulary.
- [docs/product/](docs/product/) — the deeper product model: vision, product
  model, customer zero, workflows, and Phase-1 scope.
- [docs/architecture/](docs/architecture/) — conceptual architecture, domain
  model, temporal model, and the transition record above.
- [docs/decisions/](docs/decisions/) — ADRs: product boundary, resident vs
  worker, workspace as primary context, Ticket Autopilot boundary, Hermes
  strategy.
- [docs/research/](docs/research/) — the four source studies: VS Code
  workspace, Orca worktrees, Hermes growth, self-model relationship.

```text
src/viva/                the Viva product shell
  core/ residents/ workspaces/ worktrees/ workers/ experience/ permissions/ runtime/ cli/ tui/
src/ticket_autopilot/    the delivery subsystem (legacy product surface, reused capabilities)
```

Dependency direction: `viva` → (worktree service, redaction) ← `ticket_autopilot`;
nothing in `ticket_autopilot/` imports `viva/`.

## Testing

```bash
.venv/bin/python -m pytest tests/ src/ticket_autopilot/engine/tests/ -q
```

Viva tests (`tests/viva/`) cover the CLI, resident/workspace persistence,
repository detection, worktree discovery, worker configuration and
availability, experience append-only behavior, and TUI startup/shutdown via
Textual's headless driver. Delivery-subsystem tests are unchanged and stay
green.

## Security and boundaries

Agents and workers invoked through Viva never gain repository-owner authority:
no protected-branch pushes, self-authorization, or merges without an explicit,
audited owner decision. All persisted state is private (0o600/0o700), secrets
are redacted before anything is written, and `~/.ticket-autopilot/` legacy
state is never silently migrated or destroyed.

## License

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

This project is released under the [MIT License](LICENSE). You may use,
modify, redistribute, and use it commercially, provided you retain the
copyright and permission notice. Third-party code and assets retain their
own licenses and notices.

Copyright (c) 2026 zuohaisu
