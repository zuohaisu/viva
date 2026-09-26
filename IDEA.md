# Viva — Project Charter

## North Star

```text
A persistent agent should be able to live on the user's computer,
work across tools and projects,
retain its own history,
and remain continuous even when its workers or models change.
```

Viva is a persistent habitat for AI agents to live, work, remember, and grow.
Viva is not an agent, not a Claude Code wrapper, not a worktree manager, and
not a memory database. It is the place in which a persistent agent lives.

## Core invariant

```text
Resident ≠ Worker ≠ Cognitive Engine ≠ Workspace ≠ Session
```

A **Resident** is a long-lived AI identity (e.g. Samuel). A **Worker** is a
replaceable agent CLI a resident delegates work to (e.g. Codex, Claude Code).
A **Cognitive Engine** is the model that happens to provide cognition now
(e.g. GPT, Claude, GLM). A **Workspace** is a long-term working context; a
**Worktree** is a git execution environment inside one; a **Session** is one
transient runtime interval.

```text
Session dies. Resident persists.
```

> **Deeper product model:** `docs/product/` (vision, product model, customer
> zero, workflows, Phase-1 scope) and `docs/research/` carry the extended,
> canonical product thinking. This charter is the repository-level summary.

A resident's experiences are recorded as events. Experiences are not memory:
memory formation is a future capability and must never be faked. Unknown and
not-implemented stay honest.

## Customer Zero

```text
Haisu / Samuel
```

Haisu is Customer Zero; Samuel is the first real resident. Samuel is data — a
record a user creates — never Viva's default persona or hard-coded identity.
Other users will have their own residents (Alice, Maya, Alfred, …), so
`Viva ≠ Samuel` must always hold in code.

## Architecture principle

```text
Samuel must exist before Samuel thinks.
```

Generally: a Resident's persistent state (identity, history, experiences) must
exist independently of the cognitive engine currently expressing it, and of
the worker currently acting for it. Workers and engines are replaceable;
replacing them must not erase the resident.

## Product shape

```text
Viva Core  →  surfaces: CLI · TUI (Phase 1 primary) · Desktop (future)
Viva Core  →  capabilities: delivery (Ticket Autopilot) · memory (future) · …
```

Ticket Autopilot was this project's original product focus (ticket-driven
automated software delivery) and now survives as an existing
software-delivery subsystem inside the broader Viva direction. Its workflow
definition remains `docs/closed-loop-workflow.md`; its history is retained and
is not rewritten as a mistake — it is evolution.

## Reuse first

The first design question is not "what should we build?" It is: which proven
capability — in this repository or outside it — already does this? Compose
existing capabilities, add thin deterministic glue for verified gaps, and
build new components only for documented gaps. Every custom component names
the capability it was weighed against (`docs/architecture/viva-transition.md`).

## Definition of progress

Progress is not the number of features, agents, or documents. Progress is
evidence that a resident's continuity survives one more boundary — another
session, another workspace, another worker, another engine — safely and
honestly, or that a specific blocker to that continuity has been removed.
