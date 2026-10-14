# @viva/pi-extension

Viva member-context extension for the [Pi coding agent](https://github.com/earendil-works/pi)
(V09, issue #18). It gives a Pi session its member identity and current task
brief, and offers controlled actions through the `viva` CLI.

## What it does

- **Identity injection**: the Rust harness (`crates/viva/src/harness/pi/`)
  launches `pi --extension <this file>` with member/task context in environment
  variables. The extension surfaces that identity at session start. The
  member is configuration data; no member name is hard-coded here.
- **Status query tools**: `viva_status` and `viva_task_brief`
  read real Viva records (read-only).
- **Controlled dispatch**: `viva_dispatch` requires a live grant
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
  called; nothing is recalled or written automatically. Since #101 they
  register against the real Pi API; of all the tools, only `viva_status`
  and `viva_task_brief` have been exercised end to end in real Pi so
  far. Real-provider tests are pending
  independent re-verification, and no memory backend has been chosen as the
  default.
- **Explicit handoff**: `viva_handoff` records a member-reported
  summary for the current task. It is stored as a fact ("member X reported
  this"), never as a completion verdict and never as acceptance PASS.

## What it does NOT do

- `conversations/index.ts` (fork / rename / handoff tools) is **not loaded in
  production**: the harness passes only `viva.ts` to `pi --extension`,
  and that file does not register the conversations tools. Its three tools
  are written against the real Pi API and checked by CI, but a Pi session
  launched by Viva does not have them. Wiring it in changes what members can
  do in Pi, so it is a product decision, not part of this package's tests.
  Even when loaded, `fork` refuses in Pi 1.2.0: the tool context's session
  manager is read-only and has no fork, so it proposes a handoff instead.
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
| `VIVA_MEMBER_ID` | Member id of this session (required; without it the extension stays inert) |
| `VIVA_MEMBER_NAME` | Display name at launch time (required) |
| `VIVA_TASK_ID` | Attached task, when the session is task-scoped |
| `VIVA_GRANT_ID` | Live per-task dispatch grant, when Viva holds one |
| `VIVA_HOME` | Viva home the CLI calls operate on |
| `VIVA_BIN` | `viva` binary override (tests) |

## Pi version and types

The extension is written against the real Pi SDK, not a copy of it. Types come
from the `@earendil-works/pi-coding-agent` devDependency and tool parameter
schemas from `typebox`; both are pinned to exact versions in `package.json`
(`1.2.0` and `1.3.27`) and in `package-lock.json`. There is no vendored type
file. Tools use the real single-object form,
`pi.registerTool({ name, label, description, parameters, execute })`, and
return `{ content: [{ type: "text", text }], details }` (plus `isError: true`
on failure).

To move to a newer Pi, change both pins together and run `npm run typecheck`
and `npm test`: the type check fails where the API moved, and the real-Pi load
test fails where Pi stops accepting a tool.

## Real-Pi integration status

What is checked, by whom:

- **Loading and tool registration, by CI, on the pinned real Pi.**
  `test/real-pi-load.test.ts` loads `viva.ts` (the file the harness
  passes to `pi --extension`) and `conversations/index.ts` through Pi's own
  extension loader, with the environment variables the harness injects, and
  compares the registered tool names exactly. It needs no network and no model
  credentials, and it fails (never skips) if Pi cannot be loaded. It uses the
  SDK rather than `pi -p`, because without an API key `pi -p` exits 1 whether
  or not the extension loaded.
- **Tool calls made by a model inside a live Pi session, by independent QA,
  manually.** That needs model credentials, so it is not in CI. It is not
  claimed as exercised by anything in this package.

The commands the tools emit are checked against the real `viva` binary by
`test/viva.contract.test.ts`; envelope logic and refusals are checked
against a fake `viva` and a fake Pi (`test/fake-pi.ts`, typed against the real
SDK).

## Development

```bash
npm ci             # dev deps: typescript, @types/node, pinned real Pi + typebox
npm run typecheck  # tsc --noEmit (strict, against the real Pi types)
npm test           # node --experimental-strip-types --test
```

`npm test` needs the real `viva` binary: run `cargo build -p viva` first (the
contract test uses `target/debug/viva`, or the path in `VIVA_BIN`). It fails,
rather than skips, when the binary is missing.
