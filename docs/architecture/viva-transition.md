# Viva Architecture Transition

Status: **authoritative** for the repository's product boundary as of 2026-09-27.
This document records the transition from *Ticket Autopilot as product* to
*Viva as product* and governs how existing assets are treated. It is written
before any code moved, so it doubles as the archaeology record.

## 1. Product boundary

```text
OLD:
Ticket Autopilot = Product
  (Plane ticket -> isolated worktree -> Developer -> checks -> independent QA
   -> bounded fix loop -> auditable local commit; Web controller surface)

NEW:
Viva = Product
  (short slogan: "a persistent habitat for AI agents to live, work, remember,
   and grow"; the canonical product definition lives in docs/product/vision.md)
Ticket Autopilot = one existing capability / subsystem inside Viva
  (automated software delivery; legacy product surface: localhost Web UI)
```

Viva is **not** an agent. Viva is the place in which a persistent agent can
live and work. The core ontology invariants are:

```text
Resident ≠ Worker ≠ Cognitive Engine ≠ Workspace ≠ Session
raw event ≠ experience ≠ memory ≠ self-model ≠ identity
```

- **Resident** — a long-lived AI identity in Viva (identity, history,
  experiences, skills, governance history). Persists across sessions.
- **Worker** — a replaceable agent/tool CLI that a Resident (or the user)
  delegates concrete work to (Codex, Claude Code, Qoder, ZCode, …).
- **Cognitive Engine** — the model providing cognition at a point in time
  (GPT, Claude, Gemini, GLM, DeepSeek, local). A Resident's substrate, not the
  Resident.
- **Workspace** — a long-term working context. Not necessarily one git repo;
  Phase 1 registers local directories/repositories.
- **Worktree** — a git execution environment under a Workspace.
- **Session** — one transient interaction/runtime interval. Sessions die;
  Residents persist.
- **Experience** — an event a Resident went through, recorded append-only.
  Experience is *not* Memory: no promotion/formation exists yet.

Naming: new product surfaces use **Viva / `viva`**. Historical names
(`AI-Operation`, `Ticket Autopilot`, `AIO-NNN` ticket keys) are history and are
kept where they occurred; they are not used for new repository-level branding.

## 2. Repository archaeology (ground truth before this transition)

The repo was `ticket-autopilot` 0.1.0 (stdlib-only runtime, pytest for dev),
src layout `src/ticket_autopilot/` (~8.3k lines), entry point
`ticket-controller`, plus a localhost Web controller launched by
`./start-ticket-autopilot`. Per-user state lived in `~/.ticket-autopilot/`
(config.json with atomic private writes, service.json, logs). Per-repository
Run artifacts lived in `<repo>/.ticket-autopilot/` (runs, operations). 229
tests green at baseline.

### A. Existing modules that can serve Viva directly (reuse, do not rewrite)

| Existing capability | Module | Viva use |
| --- | --- | --- |
| Safe git worktree ops (create/remove/list branches, protected-branch guard, owner-authorized push) | `ticket_autopilot.services.git_worktree.GitWorktreeService` | Reused **as-is** behind Viva's worktree adapter; Viva adds only read-only `git worktree list --porcelain` discovery on top. No second worktree system. |
| Secret redaction for artifacts | `ticket_autopilot.services.run_events.redact` | Imported directly by Viva's Experience journal before any write. |
| Append-only, sequence-checked JSONL evidence pattern | `ticket_autopilot.services.run_events.RunEventStore` | Pattern (not the run-specific schema) reused for `~/.viva/experiences/journal.jsonl`. |
| Actor-aware authorization (`require_user_authorization`, owner actions) | `ticket_autopilot.services.delivery_policy` | The authority precedent Viva's permission model builds on; still owned by the delivery subsystem. |
| Atomic, private (0o600/0o700) JSON state writes | `ticket_autopilot.web.atomic_json_write` pattern | Replicated in `viva.core.store` for `~/.viva` state (viva must not import the web module; see dependency rules). |
| Agent CLI availability probing and safe argv construction | `ticket_autopilot.services.agent_catalog.AgentCatalog` | Kept for ticket roles. Viva's generic `WorkerRegistry` covers the gap below. |
| Deterministic checks runner, process-output stream, session ledger | `ticket_autopilot.services.development_verifier` / `process_output` / `agent_sessions` | Future Viva work/delivery capabilities; untouched this round. |
| Engine (YAML workflow, cli/llm/script/mock drivers, guardrails) | `ticket_autopilot.engine` | Reusable execution substrate for later Viva capabilities; untouched this round. |

### B. Ticket-Autopilot-specific (stays inside the subsystem)

Plane connector, ticket contract/readiness gates, prompt resolver and planner
adapter, Developer → checks → QA fix loop, run manager and run events (run
schema), ticket Web UI + static assets, `tasks/AIO-NNN-*` prompt artifacts,
`~/.ticket-autopilot/` state, `ticket-controller` CLI. These implement the
delivery loop and are not part of Viva core's ontology.

### C. Disposition table

| Disposition | Items |
| --- | --- |
| **Keep** (as live subsystem code) | `src/ticket_autopilot/**` in full: services, connectors, engine, schemas, web UI, tests, `ticket-controller` entry point. |
| **Keep** (reused by Viva via import) | `git_worktree.GitWorktreeService`, `run_events.redact`, `delivery_policy` (referenced as authority precedent). |
| **Adapt** (new thin Viva layer, no legacy edits) | worktree *discovery* added on top of `GitWorktreeService`; generic `WorkerRegistry` in front of the worker-availability gap; experience journal following the `RunEventStore` pattern. |
| **Move later** | Possibly extracting worktree/redaction/atomic-store into a shared `viva.core` library once there is real shared-core pressure. Not now. |
| **Deprecate later** (decision deferred, explicit trigger required) | Ticket Autopilot Web UI as a product surface; `start-ticket-autopilot` launcher; distribution name `ticket-autopilot` → `viva` (done this round at the packaging level; PyPI namespace irrelevant until publishing). |
| **Do not touch yet** | `~/.ticket-autopilot/` state and all legacy Run artifacts; `specs/` history, `tasks/` archive, `research/`, `logs/goal-drift.md`, `tooling/start-prompt/` (parked track); `docs/closed-loop-workflow.md` (remains the subsystem's authoritative workflow). |

No big-bang: `src/ticket_autopilot/` is **not renamed** this round. Viva lives
in a new `src/viva/` package and absorbs capabilities through explicit
interfaces over time.

## 3. New architecture: `src/viva/`

```text
src/viva/
  core/         paths (VIVA_HOME), atomic store, ids, errors
  residents/    Resident records + registry (identity persists)
  workspaces/   Workspace registry, current-workspace state, repo detection
  worktrees/    read-only discovery + adapter over legacy GitWorktreeService
  workers/      generic Worker registry, availability probe, safe run
  experience/   append-only ExperienceJournal (NOT memory)
  permissions/  Phase-1 permission vocabulary contract
  runtime/      session ids, runtime state snapshot
  cli/          argparse CLI: viva, viva status, viva resident|workspace|worktree|worker|experience
  tui/          Textual Phase-1 product surface
```

Dependency direction (enforced by review, not yet by tooling):

```text
tui / cli  ->  residents, workspaces, worktrees, workers, experience, runtime, core
worktrees  ->  ticket_autopilot.services.git_worktree   (single, deliberate edge)
experience ->  ticket_autopilot.services.run_events.redact (single deliberate edge)
core       ->  (stdlib only)
```

Nothing in `ticket_autopilot/` imports `viva/`. Viva never imports
`ticket_autopilot.web`, `ticket_autopilot.engine`, or any ticket-domain
service.

## 4. Persistent state: `~/.viva/`

```text
~/.viva/
  config/
    settings.json      # default_resident, ui options
    workers.json       # worker registry (data; ships with common CLI seeds)
  residents/
    <id>.json          # one record per Resident
  workspaces/
    registry.json      # registered workspaces
  experiences/
    journal.jsonl      # append-only experience journal
  runtime/
    state.json         # current_resident, current_workspace, last_session_id
  logs/
```

Requirements honored: atomic writes (temp file + `os.replace`, 0o600 files /
0o700 dirs), evolvable `schema_version` fields, secrets redacted on write, all
state reloadable, and resident/workspace state survives TUI exit. The legacy
`~/.ticket-autopilot/` directory is **not** read, written, or migrated by Viva;
the delivery subsystem keeps owning it. `VIVA_HOME` overrides the root for
tests.

## 5. TUI technology decision

Chosen: **Textual** (`textual>=8.0`), the only new runtime dependency.

- **Why**: async task support (worker-process streaming), panes/status
  areas/CSS layout, keyboard navigation, macOS Terminal first-class, and a
  built-in headless test driver (`App.run_test()` + `Pilot`) — the testability
  requirement is native, not bolted on.
- **Alternatives considered**: `curses` (stdlib) — no widget model, painful
  streaming/resize, high maintenance burden; `urwid` — mature but older async
  story and weaker docs; `prompt_toolkit` — excellent for REPL-style input but
  not a pane/status application framework. None met streaming + panes +
  test-driver together.
- **Why stdlib is insufficient**: a Phase-1 *product* surface needs live
  streaming panes, focus handling, and headless tests; raw `curses` would mean
  hand-rolling all of that and owning it forever.
- **Burden accepted deliberately**: textual pulls rich/markdown-it-py/pygments
  etc. This trades the previous zero-dependency claim for a maintainable
  product surface, as the transition brief allows. The engine/web subsystem
  keeps running stdlib-only regardless.

## 6. Worker layer: existing capability checked, gap filled

Checked: `AgentCatalog` (AIO-22) already probes codex/claude/qodercli and
builds role-bound agent dicts for the engine's `cli_call` driver.

Gap: it is provider-**fixed** and role-**coupled** (`planner/developer/qa`,
read-only rules, engine whitelist). Viva needs a generic worker concept for any
CLI. Therefore Viva adds `workers.WorkerRegistry` — data-driven worker records
(`name`, `command`, `args`, `capabilities`, `timeout_seconds`) in
`~/.viva/config/workers.json`, availability via `shutil.which` + a probe
command, and a conservative non-interactive runner (argv = command + args +
message, cwd = current workspace, timeout, streamed output, experience events
on start/complete/fail). Ticket roles keep using `AgentCatalog` unchanged; no
worker names are hard-coded in Viva core — shipped seeds are config data.

## 7. Permission model (Phase-1 contract, no policy engine)

Viva will operate the user's real computer, so the vocabulary exists from day
one (`viva/permissions/__init__.py`):

```text
READ                observe only
PROPOSE             present a plan/diff; no mutation
ACT_WITH_APPROVAL   act after an explicit user/owner approval for that action
ACT_AUTONOMOUSLY    act within a pre-authorized boundary (not granted in Phase 1)
FORBIDDEN           never allowed for that actor
```

Phase-1 mapping: all Viva commands are user-initiated (`ACT_WITH_APPROVAL` in
the sense that the user typed them); worker `run` is always user-issued and
records an experience event; autonomous/background worker invocation is
FORBIDDEN in Phase 1. Workers invoked by Viva inherit the delivery subsystem's
actor rule: they never gain repository-owner authority. Future work must not
bypass `delivery_policy`'s authorization principles.

## 8. Relationships to sibling research repos (boundaries only)

```text
self-model     research: what is a persistent Self? (no code dependency)
dsh-ai-soul    generic Soul primitives / DSH reference implementation (no dependency)
Viva           the local habitat/product; implements only contracts the product needs now
```

Viva imports nothing from either repo this round. `self-model` ↔ Viva form a
research ⇄ runtime evidence loop; `dsh-ai-soul` is a reference implementation
to consult, not a library to couple to. Extract a shared core only on real
pressure.

## 9. Honesty constraints (anti-persona-theater)

- No `if resident == "Samuel"` in core; Samuel is Customer-Zero *data*
  (created via `viva resident add Samuel`), never identity.
- The Experience journal is never called Memory. No memory formation,
  promotion, or self-model evolution exists this round; UI text must not claim
  remembering, learning, or evolving.
- Unknown/not-implemented stays visible (e.g. memory surfaces say
  "not implemented").

## 10. Migration safety commitments

- No deletion of existing capability; no mass file moves; no rewrite of
  `src/ticket_autopilot/`.
- Existing Ticket Autopilot tests stay green (any change must be justified in
  the report).
- `~/.ticket-autopilot/` is untouched.
- New `viva` tests must not depend on Samuel-specific data.

## 11. Explicitly out of scope this round

Full memory architecture, self-model, daemon, voice/desktop/browser/message
channels, multi-device sync, cloud, additional worker adapters, GUI, VS Code
integration, and any Ticket-Autopilot → Viva package rename. Each is a later,
separately-scoped milestone.

## 12. Related canon

`docs/product/` (vision, product model, customer-zero, workflows, Phase-1
scope) and `docs/research/` (Orca, Hermes, VS Code workspace, self-model
relationship) hold the extended product thinking authored alongside this
transition. Where they refine definitions (for example vision.md's
collaboration-centered one-liner), they are the deeper canon; this document
remains the architecture and migration record.

## 13. Product-governance addendum (2026-09-27 product round)

Docs-only round; no code changed. Decisions are codified in
`docs/decisions/0001`–`0005`; this section records only what the sections
above did not already cover.

### 13.1 Design ideas promoted from the delivery subsystem into Viva-wide law

1. **Evidence-first / no false success** (a verdict counts only when
   schema-valid) → journal provenance discipline; domain invariant I3
   (`docs/architecture/domain-model.md`).
2. **Actor-aware authority** (capability ≠ authorization; every grant logged)
   → domain invariant I4; operationalized in §7 above.
3. **Bounded autonomy** (retry caps, hard breaks, explicit blocked states) →
   precondition for any structured workflow a Resident may launch.
4. **Append-only evidence, disposable environments** → invariants I2 and I5.
5. **Constrained invocation** (allowlists, read-only default, cwd fences) →
   the Worker invocation baseline.

### 13.2 Anti-list: what must never enter Viva core

Plane schema/semantics, ticket contract and readiness gates, QA verdict
fields, the one-active-Run cadence assumption, and the Web controller's
Plane-first information architecture. These are ticket-domain and stay in the
delivery subsystem (§2B).

### 13.3 North Star record

The repository North Star changed from ticket-driven delivery to Viva with
this transition; recorded as an accepted reprioritization in
`logs/goal-drift.md` and codified in `docs/decisions/0001` and `0004`.
