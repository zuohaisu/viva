# Viva — Agent Working Charter

Viva is a persistent habitat for AI agents to live, work, remember, and grow.
Viva is **not** the agent: Viva is the place in which a persistent agent can
live and work.

# Project North Star and Drift Guard

This repository's North Star is:

> Build Viva into a local-first persistent habitat where an AI resident can
> maintain continuity across sessions while working through multiple
> workspaces, worktrees, workers, and cognitive engines.

Core ontology invariants — every design must preserve them:

```text
Resident ≠ Worker ≠ Cognitive Engine ≠ Workspace ≠ Session
raw event ≠ experience ≠ memory ≠ self-model ≠ identity
Session dies. Resident persists.
```

The authoritative product charter is `IDEA.md`; the canonical product
definition and Customer Zero model live in `docs/product/` (vision.md first).
Boundary-level decisions are codified as ADRs in `docs/decisions/`. The
authoritative architecture transition record is
`docs/architecture/viva-transition.md`. The Ticket Autopilot subsystem
(automated software delivery) keeps `docs/closed-loop-workflow.md` as its
operational definition. Goal-drift incidents are recorded in
`logs/goal-drift.md`.

## Mandatory Alignment Check

Before starting research, planning, or implementation, emit one concise line in
the working update:

```text
[Goal check] This work advances Viva's <resident/workspace/worktree/worker/experience/runtime> capability by <measurable evidence>.
```

If that sentence cannot be completed concretely, do not start the work.
Classify it as a side track and ask the user whether to change priorities.
Work inside the Ticket Autopilot subsystem may instead use that subsystem's
ticket-loop goal check (`docs/closed-loop-workflow.md`). Repeat the check
whenever the deliverable type changes, a new subsystem is proposed, or the work
expands beyond the active scope. At completion, report which capability
advanced and what evidence now exists.

## Architecture Order

Use this order and do not skip directly to a custom platform:

1. Reuse existing proven capabilities — inside this repo (worktree service,
   redaction, authorization policy, agent catalog, engine) and outside
   (CLIs, MCP, GitHub integrations).
2. Add the smallest deterministic glue needed to connect proven gaps.
3. Build a minimal new component only where a documented capability gap
   prevents the product from working.

Every custom component must name the existing capability that was checked and
the gap it fills (see `docs/architecture/viva-transition.md` §6 for the
pattern).

## Incremental Migration Rule

`src/ticket_autopilot/` is the legacy delivery subsystem and stays working.
Viva lives in `src/viva/` and absorbs capabilities through explicit interfaces.
No big-bang rewrite, no mass rename, no silent destruction of
`~/.ticket-autopilot/` state.

## Concurrent Development: Worktree → Branch → PR → Merge

Multiple agents and the repository owner develop this repo in parallel. The
main checkout is shared state: never do task work directly in it. Every change
is delivered from an isolated worktree through a branch, a pull request, and a
merge into the remote default branch.

- Treat any user request to implement, fix, refactor, or otherwise change the
  repository as authorization to deliver through the full workflow below. The
  user only describes the desired outcome; they do not need to pick branch
  names, approve routine commits, push, or separately ask for a PR.
  Documentation-only edits follow the same workflow unless the user explicitly
  asks for local-only changes.
- **One task = one worktree + one branch.** Create a dedicated git worktree per
  task (viva's own worktree service may allocate it; `worktrees/` is ignored in
  the main checkout). Derive the base from the remote default branch — run
  `git fetch origin` and start from `origin/main`, never from a possibly stale
  local branch.
- Branch naming: `agent/<type>-<short-kebab-case-description>`, with `<type>`
  normally `feat` / `fix` / `refactor` / `docs` / `test` / `chore`. If the task
  already has an open PR, continue that branch instead of duplicating it.
- **Protect other people's work.** Never discard, overwrite, reset, amend,
  stash, commit, or publish changes that do not belong to the current task.
  In a dirty checkout, identify exactly which paths belong to the request and
  keep everything else out of the commit; if clean separation cannot be
  guaranteed, stop and report the conflict.
- Worktree lifecycle is human-controlled. Do not delete, prune, or remove a
  Git or Orca worktree without explicit human authorization naming the exact
  target in the current request. A merged PR, passing tests, a stale branch,
  or task completion never implies that authorization.
- Before committing, run the closest checks in the worktree:
  `uv run pytest` (or the subset covering the change), `git diff --check`, and
  an inspection of the final diff for secrets and generated artifacts. Report
  blocked or unrun checks honestly; never weaken or skip tests to make them
  pass.
- Stage paths explicitly; do not use `git add .` or `git add -A`. Use concise
  imperative commits describing the delivered behavior. Push the task branch
  to `origin` with upstream tracking. Never force-push, delete remote
  branches, or alter branch protection as part of this workflow.
- Open one PR per task branch, in **Ready for review** state unless the user
  asks for a draft, targeting the remote default branch. The body must contain
  Summary, Validation (exact commands and results), Risk/Safety Boundaries,
  and Known Limitations.
- After pushing, rebase or merge the latest base if the branch conflicts, fix
  task-caused failures, and push follow-up commits.
- **Merge authority is unchanged by this workflow.** Per Actor-Aware Delivery
  Authority, no agent merges or approves its own PR; the repository owner
  performs (or explicitly authorizes the Controller for) the merge.
- If authentication, permissions, or network block push or PR creation,
  complete all safe local work and validation, then report the exact blocker
  and the smallest user action needed. Never work around access controls.
- Handoff must name changed files, commands run and their results, unresolved
  assumptions or skipped checks, and — when the workflow applies — the branch,
  commit, worktree location, and PR URL.

## Boundaries With Sibling Repos

```text
self-model     research on persistent Self  — Viva depends on nothing from it
dsh-ai-soul    generic Soul/DSH reference   — Viva depends on nothing from it
Viva           the local habitat/product
```

Viva implements only contracts the product needs now; extract shared cores only
under real pressure.

## Actor-Aware Delivery Authority

Safety restrictions apply to autonomous Agents, not to the repository owner.
Developer and QA Agents may never push a protected branch, authorize their own
override, or merge a Pull Request. The Controller may push an isolated feature
branch or merge only when the repository owner explicitly authorizes that exact
action and the audit record includes actor, action, timestamp, and reason.
Workers invoked by Viva inherit this rule: they never gain owner authority.

`QA_PENDING` and `HUMAN_VISUAL_REVIEW_PENDING` are evidence states, not
`BLOCKED_REQUIREMENTS`; they may accompany a Draft PR. A user override must
retain the original pending/fail evidence and must not be reported as PASS.
Mixed commits or files are `DIFF_SPLIT_REQUIRED` and should be mechanically
isolated. Use `TECHNICAL_BLOCKED` only for an operation that actually fails
because of credentials, network, conflict, or remote rejection.

Viva's Phase-1 permission vocabulary (READ / PROPOSE / ACT_WITH_APPROVAL /
ACT_AUTONOMOUSLY / FORBIDDEN) is defined in `src/viva/permissions/` and the
transition document; new worker/workspace/worktree actions must not bypass
these principles.

## Honesty Constraints

No persona theater. If memory formation, self-model evolution, or any
capability does not exist, the product must say so — never display "remembers",
"learned", or "evolved" without the capability behind it. Samuel (Customer
Zero's resident) is configuration data, never hard-coded identity:
`Viva ≠ Samuel`.

## Current Focus

The Start Prompt research and comparison tooling remains a parked supporting
track. The next Viva milestones grow the resident/workspace/worker/experience
runtime and the delivery subsystem's evolution into a Viva capability — one
coherent milestone at a time, each with measurable evidence.
