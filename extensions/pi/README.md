# @viva/pi-office-extension

Viva office extension for the [Pi coding agent](https://github.com/earendil-works/pi)
(V09, issue #18). It gives a Pi session the office context of the member it
runs as — identity, current task brief — and controlled office actions over
the fixed `viva office …` CLI envelope.

## What it does

- **Identity injection**: the Rust harness (`crates/viva/src/harness/pi/`)
  launches `pi --extension <this file>` with office context in environment
  variables. The extension surfaces that identity at session start. The
  member is configuration data; no member name is hard-coded here.
- **Office query tools**: `viva_office_status` and `viva_office_task_brief`
  read real office records (read-only).
- **Controlled dispatch**: `viva_office_dispatch` requires a live grant
  reference. Without one it refuses with a proposal to ask the user — chat
  never self-authorizes, and the office's dispatch endpoint re-checks the
  grant server-side at the moment of effect.
- **Explicit handoff**: `viva_office_handoff` records a member-reported
  summary for the current task. It is stored as a fact ("member X reported
  this"), never as a completion verdict and never as acceptance PASS.

## What it does NOT do

- No external memory integration: it states plainly that no memory system
  is connected. No "remembers", no "learned", no fake Holographic.
- No second chat engine, no agent loop: Pi keeps its own UI and loop.
- Sessions started outside the office (no member identity in the
  environment) leave the extension inert — it registers nothing and claims
  nothing.

## Environment contract (set by the Rust harness)

| Variable | Meaning |
| --- | --- |
| `VIVA_OFFICE_MEMBER_ID` | Member id of this session (required; without it the extension stays inert) |
| `VIVA_OFFICE_MEMBER_NAME` | Display name at launch time (required) |
| `VIVA_OFFICE_TASK_ID` | Attached task, when the session is task-scoped |
| `VIVA_OFFICE_GRANT_ID` | Live per-task dispatch grant, when the office holds one |
| `VIVA_HOME` | Office home the CLI calls operate on |
| `VIVA_BIN` | `viva` binary override (tests) |

## Type notes (honest scope)

`pi-api.d.ts` vendors the minimal subset of the upstream extension API this
package uses, pinned to the audited extension documentation (2026-09-28).
It is not the full SDK types. When the real
`@earendil-works/pi-coding-agent` package is added as a dependency, delete
that file, import from the SDK and re-run typecheck.

## Real-Pi integration status (pending, not PASS)

The extension package typechecks and its envelope logic is tested against a
fake `viva` binary (see `npm test`). **Real Pi end-to-end interaction
(model conversations, tool calls inside live Pi) is pending**: it needs a
Pi installation and model credentials, and is recorded by V12 when those
exist. Nothing here claims that integration has been exercised.

## Development

```bash
npm install        # dev deps: typescript, @types/node
npm run typecheck  # tsc --noEmit (strict)
npm test           # node --experimental-strip-types --test
```
