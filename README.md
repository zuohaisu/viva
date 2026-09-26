# Viva

> **Viva is Haisu's local-first Personal AI Office.**
>
> Several persistent AI members live and work here — each with its own role,
> history and knowledge — and drive real tools to get work done. Viva is not an
> AI member: it is the office the members work in.
>
> Customer Zero is **Haisu**; the office ships with no built-in personas.
> `Samuel`, `Deven`, `Alice`, `Oliver` and `Richard` are records you create.
>
> Canonical product definition: [docs/product/vision.md](docs/product/vision.md).

```bash
$ viva
```

```text
VIVA                                    Office
Members (3)                             Workspace   office · git main · 4 wt
 * Samuel   coordinator                 Project     viva (1 repo)
   Deven    developer                   Tasks (2 open)
   Alice    reviewer                      task-issue-150-...  delivery in_progress
Executions (1 running, 0 unresolved)      task-issue-151-...  delivery in_progress
   exec-1a2b3c4d   deven  fakeworker      Tools       2/3: claude qodercli
```

## The core distinctions

```text
Resident ≠ Role ≠ Cognitive Engine ≠ Worker ≠ Execution Session
Workspace ≠ Project ≠ Repository ≠ Worktree ≠ Task
raw event ≠ experience ≠ memory ≠ self-model ≠ identity
```

| Example | What it is |
| --- | --- |
| Samuel / Deven / Alice | **Members (Residents)** — persistent AI identities; records, not code |
| coordinator / developer / reviewer | **Roles** — configuration data in `~/.viva/config/roles.json` |
| Claude / GPT / GLM / local | **Cognitive engines** — replaceable model bindings |
| Claude Code / Qoder CLI / Codex | **Workers** — replaceable execution tools |
| `exec-1a2b3c4d` | **Execution** — one real run with its own attribution and authority |
| office → Viva → VicTrader | **Workspace → Project → Repository** |
| `task/issue-150` worktree | **Worktree** — an isolated writable checkout per task |

Changing a member's model, tool or role never deletes its record, history or
knowledge. Sessions die; members, tasks and executions persist.

## What works today

- **Members** — `viva resident add/list/show`, roles (`viva role list`), engines
  (`viva engine list`), per-member model + tool bindings that can be rebound
  without losing anything. An unavailable model or tool fails loudly; Viva
  never substitutes one silently.
- **Work** — `viva workspace add`, `viva project add/bind`, `viva task new`
  (intent, constraints, assignees, outputs, unfinished items) and
  `viva task brief` — the handoff pack the next member actually needs.
- **Executions** — `viva office dispatch/status/result/stop/recover`. Real
  subprocesses in real worktrees; several run at once (the same member can work
  on two tasks in parallel); stopping one leaves the others alone; every run
  records its member, task, model, tool, work location, requester and authority
  at launch, so switching what the UI shows cannot rewrite history.
- **Delegation** — a coordinating member may dispatch, observe, read results
  and stop other members *inside a scope Haisu granted*
  (`viva office grant`). A child grant can never widen its parent, an
  ungranted worker request is refused, and the refusal is recorded with its
  reason. Requests are never relabelled as Haisu's own.
- **Recovery** — after a restart, `viva office recover` reports honestly what
  is running, what exited without a recorded result, what cannot be told apart,
  and what can be resumed. It never restarts a finished run.
- **GitHub (read-only)** — `viva github link/evidence` ties a task to its
  repository, issue, branch, PR, checks and reviews through `gh`.
- **Knowledge** — `viva knowledge add/list/show/use/reuse`: personal memory,
  self-model *candidates*, project knowledge, team knowledge and skills, each
  with one owner and mandatory provenance. Only entries a later execution
  actually used count as reuse evidence.
- **Experience journal** — append-only, secret-redacted
  (`~/.viva/experiences/journal.jsonl`).

Not implemented, and never faked: automatic reflection, self-model evolution,
automatic memory promotion, a daemon, automatic remote writes, multi-user
support, Jev integration. See [docs/product/phase-1.md](docs/product/phase-1.md) §4.

## Quickstart

Requires Python 3.11+ and Git.

```bash
python3 -m venv .venv
. .venv/bin/activate
pip install -e '.[dev]'

viva resident add Samuel --role coordinator --engine claude-default
viva resident add Deven  --role developer   --engine claude-default
viva resident add Alice  --role reviewer    --engine claude-default
viva workspace add ~/Documents/code/viva office
viva project add viva --repo ~/Documents/code/viva
viva task new "Fix issue 150" --kind delivery --project viva --repo ~/Documents/code/viva \
     --assign deven --constraint "never touch main"

viva office dispatch --task <task-id> --to deven --wait
viva office status
viva task brief <task-id>
```

In the TUI: `help` lists the commands, `Ctrl+Q` quits. Member/workspace
selection persists in `~/.viva/`; the selection is navigation only.

## Architecture

- [IDEA.md](IDEA.md) — the charter and the ontology invariants.
- [AGENTS.md](AGENTS.md) — the working charter for agents in this repo.
- [docs/product/](docs/product/) — vision, product model, customer zero,
  workflows, phase-1 scope.
- [docs/architecture/](docs/architecture/) — conceptual architecture (the seven
  relations), domain model (objects, states, invariants), temporal model
  (event → experience → knowledge), and the migration/retirement record.
- [docs/decisions/](docs/decisions/) — ADRs 0001–0010, including the decisions
  this round superseded and why.

```text
src/viva/
  core/         store, paths, ids, redaction
  residents/    members + role/engine catalogues (config data)
  permissions/  permission vocabulary, owner authority, grant ledger
  workspaces/ projects/ worktrees/
  tasks/        task registry + handoff brief
  executions/   execution registry + real process runner
  office/       dispatch / status / result / stop / grant / recover
  knowledge/    four knowledge kinds + reuse evidence
  github/       read-only GitHub linkage through gh
  experience/   append-only journal
  cli/ tui/     surfaces over one composition root
```

Dependency direction: `cli/tui → context → domain → platform (worktrees,
workers, store)`; `core` is stdlib-only.

## Testing

```bash
python -m pytest tests/ -q
```

`tests/viva/` covers the members, tasks, executions, authority, knowledge,
GitHub linkage, CLI, TUI (headless Textual driver) and the nine acceptance
scenarios end-to-end with real worker processes
(`tests/viva/test_acceptance.py`).

## Security and boundaries

Members and workers never gain repository-owner authority: no protected-branch
pushes, no merges, no approving their own PR, no self-authorization. Remote
actions require an explicit, audited owner decision
(`src/viva/permissions/authority.py`). Every persisted artifact is private
(0o600/0o700) and secret-redacted before it is written. Viva never reads,
writes or migrates the historical `~/.ticket-autopilot/` state.

## License

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

MIT — see [LICENSE](LICENSE). Copyright (c) 2026 zuohaisu
