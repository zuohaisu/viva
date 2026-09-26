# Contributing to Viva

Viva is a persistent habitat for AI agents to live, work, remember, and grow.
Repository-wide rules live in [AGENTS.md](AGENTS.md), the product charter in
[IDEA.md](IDEA.md), and the architecture transition record in
[docs/architecture/viva-transition.md](docs/architecture/viva-transition.md).

The ticket-driven delivery loop below applies to work on the **Ticket
Autopilot delivery subsystem** (the automated software-delivery capability and
its legacy Web surface). Use the [agent-ready ticket template](specs/agent-ready-ticket-template.md) to make the ticket contract complete before subsystem work begins. Viva-shell work (`src/viva/`) follows the Viva goal check in [AGENTS.md](AGENTS.md) instead of a ticket key.

## Before starting

- Work from one Plane or Linear ticket; do not combine unrelated tickets.
- Confirm its scope boundary, acceptance criteria, deterministic verification, risk, and human touchpoints are complete.
- Begin each work update with the required `[Goal check]` line from [AGENTS.md](AGENTS.md).
- Treat a missing requirement, unmet dependency, or ambiguous decision as blocked; record evidence rather than guessing.

## Branch naming

Create one branch per ticket using lowercase kebab case:

```text
<ticket-key>/<short-description>
```

For example: `aio-3/project-scaffolding`. Keep the branch limited to that ticket's approved scope.

## Pull-request process

1. Complete the ticket's acceptance criteria and deterministic verification.
2. Prepare the PR description from [`.github/PULL_REQUEST_TEMPLATE.md`](.github/PULL_REQUEST_TEMPLATE.md), including the Plane key, scope, evidence, risks, and rollback plan.
3. A development agent must not create, merge, or approve a PR on its own. It leaves the branch, commit(s), verification output, and completed template content for the Controller or a human to create the PR.
4. The Controller or designated human creates the PR when the workflow supports it. A human reviewer is always the merge gate; no agent self-approval or automatic merge.
5. Merge only after required review and CI evidence are present. The operational workflow remains authoritative for what is currently implemented versus planned.

## Closed-loop status mapping

Use ticket status to report evidence, not intent:

| Ticket status | When to use it | Required evidence |
| --- | --- | --- |
| **In Progress** | The ticket contract is complete and implementation or verification is underway. | Active branch/run and recorded work updates. |
| **In Review** | The approved scope is complete and ready for human review. | Acceptance criteria, deterministic verification, required QA/CI evidence, and PR link when the PR stage is available. |
| **Blocked** | Work cannot safely proceed. | A concise blocker report naming the missing dependency, failed gate, ambiguity, or required human decision; use `BLOCKED_NEEDS_HUMAN` where applicable. |

Do not move a ticket to **In Review** merely because an agent reports completion. The evidence gates in [docs/closed-loop-workflow.md](docs/closed-loop-workflow.md) decide whether a stage is closed.

## Persistent context

Record durable project context and daily execution notes according to [`.workbuddy/memory/README.md`](.workbuddy/memory/README.md). Keep secrets, credentials, and unsupported claims out of memory and PR text.

## Commit convention

Use focused commits and include the ticket key, for example:

```text
AIO-3 add project scaffolding documentation
```

Do not modify product code, dependencies, or unrelated files unless the ticket explicitly includes them.

## Contribution licensing

Unless explicitly agreed otherwise, contributions submitted for inclusion in
this project are licensed under the [MIT License](LICENSE). Contributors
retain copyright in their contributions and must have the right to submit
them under these terms.

When reusing third-party code or assets, identify their source and license,
preserve required copyright and license notices, and confirm that their terms
permit the proposed inclusion. The project's MIT License does not replace
third-party licenses.
