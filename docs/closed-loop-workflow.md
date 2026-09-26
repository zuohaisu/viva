# Closed-Loop Workflow Definition

> **Status (2026-09-27):** this is the authoritative operational definition of
> the **Ticket Autopilot delivery subsystem** — now a capability inside the
> Viva product, not the repository-wide product definition. For the product
> North Star see `IDEA.md` and `docs/architecture/viva-transition.md`. This
> document remains the source of truth for the ticket loop; it is a legacy
> product surface with active, reusable capabilities.

This is the authoritative operational definition for the implemented **Phase 1
Plane-first local Web workflow**. `IDEA.md` remains the product charter and
`AGENTS.md` remains the working charter. Historical Engine, CLI and reference
pipeline documents are not evidence that a Web capability is live.

## Phase 1 boundary

The user starts a localhost-only service with:

```bash
./start-ticket-autopilot
```

The service listens only on `127.0.0.1:8765`. The user supplies local Plane,
repository and Agent CLI configuration in the browser. One repository has at
most one active Run. A Run owns one Worktree and one local feature branch under
the repository's `.ticket-autopilot/` directory.

The Phase 1 success endpoint is a Controller-created **local** Commit after
schema-valid QA PASS. It does not Push, create a PR, Merge, deploy, write Plane
state, select another Ticket, run in parallel, or resume agent execution after
the service restarts.

## Implemented stage mapping

| Stage | Current implementation | Evidence gate |
| --- | --- | --- |
| 1. Ticket intake and contract | `connectors/plane.py`, `services/ticket_contract.py`, and `/api/tickets` list unfinished Plane work items and normalize Plane v2 fields. | Expanded Project matches local configuration; required structured sections parse into a schema-valid R0/R1 `ticket-spec`. |
| 2. Prompt preparation | `services/prompt_resolver.py` and `/api/tickets/<id>/run` reuse exact `tasks/AIO-NNN-*` Prompts or call the configured Planner automatically for missing roles. `/prepare` is an explicit human action that atomically materializes generated Prompts in `tasks/` without overwriting an existing file; automatic Run preparation remains artifact-only. Structurally invalid Planner output receives one bounded feedback repair attempt. | Both Developer and Acceptance Prompts are retained in a Run-owned artifact. Explicit Prepare also returns and displays their canonical `tasks/` paths for human inspection and later Run reuse. Planner failure is shown with its artifact path and starts no Developer/QA. |
| 3. Development invocation | `services/web_agent_loop.py` creates an owned Worktree/branch, then invokes the configured Developer in that Worktree. | A run ID, Worktree, branch, base SHA and append-only event exist before background work begins. |
| 4. Deterministic verification and independent QA | Controller runs the Ticket's automated/query verification and required checks, then invokes QA with the Ticket, complete Diff, changed files and check evidence. | All checks exit 0, Diff is nonempty and safe, and QA returns a schema-valid verdict tied to the same run and attempt. |
| 5. Bounded fix loop | QA FAIL provides the original findings to the next Developer invocation; each QA is a fresh attempt. | QA attempt is in `1..5`; fifth FAIL becomes `QA_EXHAUSTED` with no further Developer or Commit call. |
| 6. Local Commit and delivery evidence | After QA PASS, Controller stages only ticket-owned changed files and creates an issue-keyed local Commit. Delivery policy records Owner decisions. | Commit SHA, branch, changed files and Timeline artifacts exist. Owner action records retain actor, action, approved_at and reason without rewriting QA facts. |
| 7. Human escalation and retained evidence | The Web Timeline reads `events.jsonl` and state artifacts; it presents Hard Break, Retry, Stop, Finder and Owner-action controls. | Artifacts retain events, prompts, checks, QA verdicts and Commit evidence; secrets are redacted from API, UI and stored event payloads. |

## User-visible workflow

```text
Plane Ticket
  → readiness gate
  → one Run action
  → existing Prompt or automatic Planner preparation
  → owned Worktree / feature branch
  → Developer → checks → independent QA
  → up to four findings-only fixes and re-QA
  → local Commit after PASS
  → Timeline and human delivery decision
```

`HARD_BREAK`, `BLOCKED`, `QA_EXHAUSTED`, `STOPPED` and normal PASS remain
distinct. A browser closing does not remove the recorded Timeline, but it does
not give a restarted service permission to recreate lost Agent process context.

## Actor-aware delivery authority

Safety boundaries apply to autonomous Agents, not the repository owner:

| Actor | Push / Draft PR / Merge |
| --- | --- |
| Developer or QA Agent | Never authorized. |
| Controller in current Phase 1 Web UI | Records an Owner action only; performs no remote mutation. |
| Repository Owner | May record visual acceptance, override, feature-branch push, Draft PR or merge intent with `actor`, `action`, `approved_at`, `reason`. |

Canonical delivery evidence includes `QA_PENDING`,
`HUMAN_VISUAL_REVIEW_PENDING`, `READY_FOR_REVIEW`,
`DIFF_SPLIT_REQUIRED`, `USER_OVERRIDE_APPROVED`,
`MERGE_AUTHORIZED_BY_USER` and `TECHNICAL_BLOCKED`. An override never changes a
failed or pending QA/visual event into PASS.

## Explicit exclusions and known limitations

Phase 1 excludes remote access, multiple users, webhooks, automatic Ticket
selection, parallel Runs, arbitrary resume, automatic Push/PR/Merge/deploy and
Plane writeback.

Agent CLI subprocesses detach stdin so a background Web Run cannot suspend on
terminal input. The service passes its random identity as one option value even
when it begins with `-`. Stop remains conservative: it terminates an Agent only
when an owned process group has been registered and otherwise asks for human
intervention rather than signaling an unverified PID.

## Deterministic verification

```bash
.venv/bin/python -m pytest \
  tests/test_web_service.py \
  tests/test_local_config.py \
  tests/test_web_tickets.py \
  tests/test_prompt_resolver.py \
  tests/integration/test_web_agent_loop.py \
  tests/test_web_run_tracking.py \
  tests/integration/test_web_hard_break.py -q
```

The test set uses fake Agents and temporary repositories. It proves the local
contract, workflow and safety boundaries; a real Plane/Agent/GitHub delivery
requires a separate live vertical-slice acceptance.
