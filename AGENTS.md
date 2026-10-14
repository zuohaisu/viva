# Viva — Agent Working Charter

Viva is Haisu's local-first **personal AI collaboration system**. Several
persistent AI members work through Viva; Viva is **not** an AI member.

# Project North Star and Drift Guard

This repository's North Star is:

> Build Viva so Haisu's AI members keep their continuity
> while working through tasks, worktrees, workers and cognitive engines — and
> where the history of that work outlives every worker session.

Core ontology invariants — every design must preserve them:

```text
Resident ≠ Role ≠ Cognitive Engine ≠ Worker ≠ Execution Session
Workspace ≠ Project ≠ Repository ≠ Worktree ≠ Task
raw event ≠ experience ≠ memory ≠ self-model ≠ identity
Session dies. Resident persists.
```

The product charter and canonical product definition live in the GitHub Wiki
(`Core-Thesis`, `Vision`) — see "Documentation and Project Knowledge" below.

## Documentation and Project Knowledge

**GitHub Wiki is the canonical documentation and long-term knowledge base for
Viva**: <https://github.com/zuohaisu/viva/wiki> — vision, product model,
architecture, roadmap, research, decisions (ADRs), development records,
validation records, release notes and history.

Agents should consult the Wiki before making decisions involving architecture,
roadmap, terminology, research conclusions, experiments, or project direction.
For important tasks, at minimum read:

1. `Home` — what Viva is and where knowledge lives;
2. `Roadmap` — current stage, current milestone, next steps;
3. the relevant domain page;
4. `Core-Thesis` / `ADR-0011` / `ADR-0012` when the task touches foundational
   assumptions.

Long-lived project knowledge is maintained in the Wiki rather than added as
new Markdown documents inside the main repository. Unless there is a clear
reason, do not:

- create new long-lived research or knowledge Markdown in the repo;
- grow a second project knowledge base under `docs/`;
- copy Wiki content back into the repo;
- maintain duplicate canonical documentation in both the repository and Wiki.

The main repository primarily contains implementation, tests, configuration,
code-local documentation, machine-consumed specifications, and raw research
assets (for example `research/`, `docs/validation/evidence/`, `tasks/`
archives, `tooling/`, and the Start-Prompt track under `research/` and
`tooling/start-prompt/`).

When work changes a durable project assumption, architecture decision,
research conclusion, experiment result, roadmap stage, or milestone, update
the relevant Wiki page as part of the same task:

1. read the current page first;
2. decide whether it is an extension, a correction, or a supersession;
3. preserve important provenance (dates, sources, evidence states);
4. never silently promote a working hypothesis to an established fact;
5. update the affected cross-links;
6. update `Roadmap` when the current stage or milestone is affected.

Wiki edits do not go through pull requests. Commit directly to the Wiki's
`master`, one change per commit, never force-push, and post each commit link
on the GitHub issue the change belongs to. The step-by-step procedure is in
[`.agents/prompts/delivery.md`](.agents/prompts/delivery.md) §6.

When historical repository documents (or Git history) conflict with current
Wiki content, the Wiki represents the current canonical understanding — but
historical evidence is never deleted or rewritten: provenance stays traceable
through Git history and each Wiki page's source reference.

Boundary-level decisions are the Wiki's `Decisions` section (ADR 0001–0012;
several early ADRs are explicitly superseded — read each ADR's status line
before relying on it). For host-language, TUI, Pi/member-host integration,
Viva state storage, or Rust migration work, read the accepted target,
direct-replacement policy and implementation status in `ADR-0011`; for the
resident runtime, detach/attach, terminal orchestration API and close/pause
semantics, read `ADR-0012`. The architecture and migration record is
`Transition-Record`. Goal-drift incidents are recorded in `Goal-Drift-Log`.

## On-demand Independent QA

The project-specific, agent-independent review prompt is
[`.agents/prompts/independent-qa.md`](.agents/prompts/independent-qa.md).
It is a machine-consumed task instruction, not a second Wiki knowledge base.
Any agent that can read repository files can use it; no Pi command or
agent-specific automatic discovery is required.

When explicitly asked to perform independent QA, read that file and apply it
to the named PR, branch or commit range. Reading this charter alone does not
activate QA or add a mandatory dev→QA pipeline. Prefer a fresh review session.

One-line invocation:

> 读取 `.agents/prompts/independent-qa.md`，按其中要求对 PR #编号进行独立 QA；只读审核，不修改代码，报告发现、证据和未验证项。

## Mandatory Alignment Check

Before starting research, planning, or implementation, emit one concise line in
the working update:

```text
[Goal check] This work advances Viva's <member/task/execution/knowledge/runtime> capability by <measurable evidence>.
```

If that sentence cannot be completed concretely, do not start the work.
Classify it as a side track and ask the user whether to change priorities.
Repeat the check whenever the deliverable type changes, a new subsystem is
proposed, or the work expands beyond the active scope. At completion, report
which capability advanced and what evidence now exists.

## Architecture Order

Do not skip directly to a custom platform:

1. Reuse existing proven capabilities — inside this repo (worktree service,
   redaction, authority policy, worker registry) and outside (CLIs, `gh`, MCP).
2. Add the smallest deterministic glue needed to connect proven gaps.
3. Build a minimal new component only where a documented capability gap
   prevents the product from working.

Every custom component must name the existing capability that was checked and
the gap it fills (pattern and the current table: Wiki `Transition-Record` §6).

## Retired Subsystem Rule

`src/ticket_autopilot/` was **retired** on 2026-09-27 (ADR 0008). Do not
reintroduce it, do not re-add ticket/Plane/run/QA-verdict objects, and do not
recreate a fixed delivery pipeline and call it dynamic scheduling. The three
capabilities that were still useful live in the Rust implementation
(`crates/viva/src/git/`, `crates/viva/src/redaction/`,
`crates/viva/src/authority/`) — change them there. The Python runtime was
retired in V13 (issue #22); the Rust binary is the only `viva` entry. Historical run data (`~/.ticket-autopilot/`, `qa-verdict.json`,
`tasks/` archives) is preserved and must not be deleted or rewritten.

## GitHub Issues Are the Source of Truth

Every development task has a GitHub issue in `zuohaisu/viva`. Conclusions —
specifications, PR links, check evidence, QA verdicts and retrospectives — are
posted on that issue. Tasks and comments on other platforms (for example
Multica) are copies of, or pointers to, the GitHub issue. Do not open a PR for
work that has no GitHub issue.

## Concurrent Development: Worktree(+Branch) → Commits → PR → Merge

Multiple agents and the repository owner develop this repo in parallel. The
main checkout is shared state: never do task work directly in it. Every change
is delivered from an isolated worktree as one or more commits on its branch,
then through a pull request and a merge into the remote default branch.
`main` is branch-protected: it changes only through merged pull requests.

- Treat any user request to implement, fix, refactor, or otherwise change the
  repository as authorization to deliver through the full workflow below. The
  user only describes the desired outcome; they do not need to pick branch
  names, approve routine commits, push, or separately ask for a PR.
  Repository documentation-only edits follow the same workflow unless the
  user explicitly asks for local-only changes. Wiki edits follow the Wiki
  rule in "Documentation and Project Knowledge" instead.
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
  `cargo test --workspace` (or the subset covering the change; the Python
  product runtime was retired in V13 — `tests/acceptance` is pytest-only
  tooling and `extensions/pi` has its own npm suite), `git diff --check`,
  and an inspection of the final diff for secrets and generated
  artifacts. Report blocked or unrun checks honestly; never weaken
  or skip tests to make them pass.

  #### Docs-only fast path

  When a delivery branch's entire diff — `git diff --name-only
  origin/main...HEAD`, after a fresh `git fetch origin` — contains only
  `.md` documentation files, the executed code is identical to a state
  whose required CI is already green (for a docs-only diff that is
  current `origin/main`, and usually the branch's latest head run too).
  In that case the local requirement drops from a full
  `cargo test --workspace` run to:

      git diff --check    # whitespace damage and stray conflict markers
      git status --short  # confirm only intentional files changed

  and full validation is delegated to required CI, which runs the
  complete suite on every PR regardless of changed paths (`ci.yml` carries
  no `paths` filter on purpose). This fast path relaxes only the local
  suite run — never the branch/PR workflow, worktree isolation, scope
  boundaries, the inspection of the final diff for secrets and generated
  artifacts, or the CI gate itself. As soon as the diff gains any
  non-`.md` path, or the branch's CI baseline is red or unknown, the
  full local check requirement applies again.
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

## npm Releases and Version Policy

- **Releases are human-managed; agent delivery ends at an open PR.** Haisu
  decides when to release and which version to publish, and performs the
  release. A merged PR does not oblige any agent to publish, tag, or prepare
  a release. An agent may operate a release only when Haisu explicitly
  authorizes that specific release; the authorization covers that one
  release and names its version, and it never authorizes an agent to merge
  or approve its own PR, push a protected branch, or bypass release checks.
- **PRs never change version numbers.** Do not bump `crates/viva/Cargo.toml`,
  the Viva entry in `Cargo.lock`, or any package `version` field (npm
  wrapper or platform packages) in a delivery PR, and do not open
  version-prep PRs. Agents do not choose or propose SemVer components.
- Never reuse, overwrite, or force-update a released version or tag.
- Reference for human-run (or explicitly authorized) releases: publish only
  after merge, from the validated merged source, using the existing
  `v<version>` tag flow in `.github/workflows/release.yml` and
  `packaging/npm/README.md`. Tag, binary, npm wrapper, platform package, and
  wrapper dependency versions must agree. Publish platform packages before
  the wrapper; do not add a parallel publishing pipeline or change token
  permissions to work around a failure.
- When an agent operates an authorized release, verify the official npm
  registry exposes the new version and expected `latest` tags for all
  release packages, then perform a clean install and confirm
  `viva --version`. npm upload acceptance, queued processing, or a green
  workflow alone is **not** proof of a usable release. Report pending
  propagation, missing credentials, failed checks, or rejected publication
  honestly, with the release/PR links and the smallest required owner
  action; do not blindly republish an accepted version.

## Boundaries With Sibling Repos

```text
self-model     research on persistent Self  — Viva depends on nothing from it
dsh-ai-soul    generic Soul/DSH reference   — Viva depends on nothing from it
Viva           the local collaboration product
```

Viva implements only contracts the product needs now; extract shared cores only
under real pressure.

## Actor-Aware Delivery Authority

Safety restrictions apply to autonomous Agents, not to the repository owner.
Developer and QA Agents may never push a protected branch, authorize their own
override, approve their own work, or merge a Pull Request. The Controller may
push an isolated feature branch or merge only when the repository owner
explicitly authorizes that exact action and the audit record includes actor,
action, timestamp, and reason. Workers and AI members invoked by Viva inherit
this rule: they never gain owner authority.

`QA_PENDING` and `HUMAN_VISUAL_REVIEW_PENDING` are evidence states, not
`BLOCKED_REQUIREMENTS`; they may accompany a Draft PR. A user override must
retain the original pending/fail evidence and must not be reported as PASS.
Mixed commits or files are `DIFF_SPLIT_REQUIRED` and should be mechanically
isolated. Use `TECHNICAL_BLOCKED` only for an operation that actually fails
because of credentials, network, conflict, or remote rejection.

The permission vocabulary (READ / PROPOSE / ACT_WITH_APPROVAL /
ACT_AUTONOMOUSLY / FORBIDDEN) lives in `crates/viva/src/authority/`. Member-initiated
dispatch and stopping are allowed **only** through a live grant (ADR 0007); a
worker request must never be recorded as if Haisu had issued it, and a child
grant may never widen its parent. New member/task/execution actions must not
bypass these rules.

## Honesty Constraints

No persona theater. If memory formation, self-model evolution, or any
capability does not exist, the product must say so — never display "remembers",
"learned", or "evolved" without the capability behind it. Member records prove
that configuration, history and knowledge were kept; they do not claim a proven
continuous Self. Member names are configuration data, never hard-coded identity:
`Viva ≠ Samuel`.

A second honesty rule applies to work you do here: an execution record, a task
output or a "reused knowledge" claim is evidence only when the record exists.
Report what the tests and the real runs show, and name what was not run.

## Current Focus

The current stage, milestone and next steps are maintained only in the Wiki's
`Roadmap`; this charter does not restate them. The Start Prompt research and
comparison tooling remains a parked supporting track.
