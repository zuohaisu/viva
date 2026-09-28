# Contributing to Viva

Viva is Haisu's local-first Personal AI Office. Repository-wide rules live in
[AGENTS.md](AGENTS.md), the product charter in [IDEA.md](IDEA.md), and the
architecture/migration record in
[docs/architecture/viva-transition.md](docs/architecture/viva-transition.md).

## Before starting

- Begin each work update with the required `[Goal check]` line from
  [AGENTS.md](AGENTS.md).
- Read the decisions that bind your change: `docs/decisions/` — and check each
  ADR's **Status** line first, because several early ones are explicitly
  superseded (0001 partially, 0002/0003/0004 fully) and only the superseding
  ADR (0006–0010) is current.
- Treat a missing requirement, unmet dependency, or ambiguous decision as
  blocked; record evidence rather than guessing.

## Delivery workflow

Every change is delivered from an isolated worktree through a branch, a pull
request, and a merge into the remote default branch — see
[AGENTS.md](AGENTS.md) §"Concurrent Development" for the full protocol
(branch naming, staging rules, checks to run, PR contents, merge authority).

In short:

```bash
git fetch origin
# work in a dedicated worktree based on origin/main, on a branch named
# agent/<type>-<short-description>
python -m pytest tests/ -q
git diff --check
```

## What to change, and where

| You want to… | Look at |
| --- | --- |
| change a member's identity, role, model or tool binding | `crates/viva/src/members/` |
| change how authority or delegation works | `crates/viva/src/authority/` (and ADR 0007) |
| change where work happens (worktrees, GitHub evidence) | `crates/viva/src/git/` (and ADR 0009) |
| change tasks, results, handoff briefs | `crates/viva/src/tasks/` |
| change dispatch/stop/status/recovery (control plane) | `crates/viva/src/office/` |
| change knowledge ownership or reuse evidence | `crates/viva/src/knowledge/` (and ADR 0010) |
| change conversation trees, forks, handoffs | `crates/viva/src/conversations/` |
| change the surfaces | `crates/viva/src/tui/`, `crates/viva/src/main.rs`, `extensions/pi/` |

Do **not** reintroduce the retired delivery subsystem: no ticket/Plane/run/QA
objects, no fixed pipelines presented as dynamic scheduling (ADR 0008).
Historical run data (`~/.ticket-autopilot/`, `qa-verdict.json`, `tasks/`
archives) must not be deleted or rewritten.

## Tests

`tests/viva/` is the suite. Meaningful coverage means:

- a unit test for the object's rule (state transitions, refusal reasons,
  ownership boundaries);
- an integration test when the behaviour crosses a process boundary —
  dispatch, stopping and recovery use **real** worker processes
  (`tests/viva/conftest.py` provides a controllable fake worker CLI);
- the nine acceptance scenarios in `tests/viva/test_acceptance.py` stay green.

Never weaken or skip a test to make a change pass.

## Commit convention

Focused, imperative commits that describe the delivered behaviour, e.g.:

```text
feat: record execution attribution at launch, not from the UI selection
docs: retire Ticket Autopilot and record what was reused
```

Do not modify unrelated files in the same commit. If a change cannot be cleanly
separated from someone else's work in progress, stop and report it.

## Contribution licensing

Unless explicitly agreed otherwise, contributions submitted for inclusion in
this project are licensed under the [MIT License](LICENSE). Contributors
retain copyright in their contributions and must have the right to submit them
under these terms.

When reusing third-party code or assets, identify their source and license,
preserve required copyright and license notices, and confirm that their terms
permit the proposed inclusion. The project's MIT License does not replace
third-party licenses.
