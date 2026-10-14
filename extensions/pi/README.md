# @viva/pi-office-extension

Viva member-context extension for the [Pi coding agent](https://github.com/earendil-works/pi)
(V09, issue #18). It gives a Pi session its member identity and current task
brief, and offers controlled actions through the `viva` CLI.

## What it does

- **Identity injection**: the Rust harness (`crates/viva/src/harness/pi/`)
  launches `pi --extension <this file>` with member/task context in environment
  variables. The extension surfaces that identity at session start. The
  member is configuration data; no member name is hard-coded here.
- **Status query tools**: `viva_office_status` and `viva_office_task_brief`
  read real Viva records (read-only).
- **Controlled dispatch**: `viva_office_dispatch` requires a live grant
  reference. Without one it refuses with a proposal to ask the user — chat
  never self-authorizes, and Viva's dispatch endpoint re-checks the
  grant server-side at the moment of effect.
- **External memory tools (F04, issue #26)**: `viva_memory_search`
  (read-only recall scoped to this session's member) and
  `viva_memory_remember` (an explicit write that always records provenance)
  go through `viva memory`, Viva's link layer over the real Holographic
  store (see [memory/README.md](memory/README.md)). The store is the
  user's local Hermes checkout, which Viva does not ship; without it every
  call returns `unavailable`. Recall and writes happen only when a tool is
  called; nothing is recalled or written automatically. These tools cannot
  be used in real Pi until #101 is fixed. Real-provider tests are pending
  independent re-verification, and no memory backend has been chosen as the
  default.
- **Explicit handoff**: `viva_office_handoff` records a member-reported
  summary for the current task. It is stored as a fact ("member X reported
  this"), never as a completion verdict and never as acceptance PASS.

## What it does NOT do

- No memory beyond the linked store: archive and restore are not chat tools,
  the provider's physical delete is never offered, and nothing claims
  "remembers" or "learned" without a recorded fact behind it.
- No second chat engine, no agent loop: Pi keeps its own UI and loop.
- Sessions started outside Viva (no member identity in the
  environment) leave the extension inert — it registers nothing and claims
  nothing.

## Environment contract (set by the Rust harness)

| Variable | Meaning |
| --- | --- |
| `VIVA_OFFICE_MEMBER_ID` | Member id of this session (required; without it the extension stays inert) |
| `VIVA_OFFICE_MEMBER_NAME` | Display name at launch time (required) |
| `VIVA_OFFICE_TASK_ID` | Attached task, when the session is task-scoped |
| `VIVA_OFFICE_GRANT_ID` | Live per-task dispatch grant, when Viva holds one |
| `VIVA_HOME` | Viva home the CLI calls operate on |
| `VIVA_BIN` | `viva` binary override (tests) |

## Type notes (honest scope)

`pi-api.d.ts` vendors the minimal subset of the upstream extension API this
package uses, pinned to the audited extension documentation (2026-09-28).
It is not the full SDK types. When the real
`@earendil-works/pi-coding-agent` package is added as a dependency, delete
that file, import from the SDK and re-run typecheck.

## Real-Pi integration status (pending, not PASS)

The extension package typechecks and its envelope logic is tested against a
fake `viva` binary (see `npm test`), and the commands it emits are checked
against the real `viva` binary by `test/viva-office.contract.test.ts`.
**Real Pi end-to-end interaction
(model conversations, tool calls inside live Pi) is pending**: it needs a
Pi installation and model credentials, and is recorded by V12 when those
exist. Nothing here claims that integration has been exercised.

## Development

```bash
npm install        # dev deps: typescript, @types/node
npm run typecheck  # tsc --noEmit (strict)
npm test           # node --experimental-strip-types --test
```

`npm test` needs the real `viva` binary: run `cargo build -p viva` first (the
contract test uses `target/debug/viva`, or the path in `VIVA_BIN`). It fails,
rather than skips, when the binary is missing.
