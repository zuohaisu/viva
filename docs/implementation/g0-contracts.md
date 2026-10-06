# V01/G0 Interface Contracts — Rust Foundation

Status: **Delivered by V01 (issue #10), frozen pending owner review** · 2026-09-28

This document is the G0 deliverable other Epic #8 issues code against. It
fixes the shared semantics, the compilable interfaces, the migration-domain
ownership and the root-file maintainership. Per the rollout plan (§2): other
issues may target these interfaces in parallel once G0 merges; their interface
implementations must not merge before this contract lands on the default
branch.

## 1. Checked capabilities and the gap filled (Architecture Order)

- Checked inside the repo: `src/viva/core/store.py` (atomic JSON store),
  `core/paths.py` (`VIVA_HOME` / 0o700 rules), `core/ids.py` (typed ids,
  RFC-3339 timestamps), `permissions/authority.py` + `grants.py` (grant
  vocabulary), `worktrees/service.py`, `core/redaction.py`.
- Checked outside: SQLite (WAL, transactions), `rusqlite`, `serde`,
  `uuid`, `time`, GitHub Actions + `Swatinem/rust-cache`.
- Gap (from issue #10): no Rust entry point, no transactional store, no
  shared interfaces — parallel lanes would each invent their own
  records/messages/ids. V01 fills exactly that gap; no domain logic beyond
  the reference slices named in the issue.

## 2. Frozen semantics (anchor types in `crates/viva/src/foundation/`)

| Contract (rollout §2) | Frozen in | Semantics |
| --- | --- | --- |
| 成员/角色/模型/工具 | `ids.rs` (`MemberId`, …) | Separate typed ids; never one Agent object. Names and bindings are configuration (`Viva ≠ Samuel`); nothing hard-codes a member. |
| 普通对话 / Task Execution | `records.rs` (`SessionKind`) | `SessionKind::Conversation` needs no task; `SessionKind::TaskExecution(TaskId)` cannot exist without its task (type + `office_sessions` CHECK). Harness-native sessions are pointers (`HarnessSessionRef`), never a second copy of chat facts. |
| 启动规格 | `records.rs` (`LaunchSpec`) | Explicit `argv: Vec<String>` + absolute `cwd` + `RequestOrigin` (member/automation dispatch must name a `GrantId`) + optional `ResourceBudget`. No field accepts a joined shell string. |
| 终端接口 | `records.rs` (`TerminalOwner`, `TerminalEventKind`) | V01 freezes ownership and event records only; spawn/input/resize/snapshot/stop/wait execution lands with V05. Terminal state and process events are separate records. |
| 控制请求/结果 | `envelope.rs` | Envelope version, `RequestId`, payload bound (256 KiB), `ChannelRegistry`-issued caller binding, idempotency, machine-readable `RejectionReason`. |
| 任务结果 | `records.rs` (`ExecutionRecord`) | `process_exit` is a process fact stored beside status; `complete(evidence)` is the only path to `Completed`, and the schema CHECK rejects `completed` rows without evidence. Exit code 0 never completes anything. |
| 存储 | `store.rs`, ADR 0011 §8 | SQLite owns facts/relations + append-only audit; files own bodies/raw material (V01 writes no file payloads yet). One fact, one authoritative source. |
| grant | `envelope.rs` (`ControlRequest.grant`) | The envelope carries a `GrantId` reference; scope enforcement belongs to V04. Grants are never self-issued (see caller binding). |
| 工作台/辅助终端 | `records.rs` (`WorkbenchQuery`/`WorkbenchActions`) | Workbench types/trait signatures frozen; implementations land with V02/V05/V08/V07. Pure projections — no second workflow state, no slot statuses. |

G0 supplement (issue #10, 2026-09-28): one Viva instance spans projects/tasks/
worktrees/terminals via these references; one worktree may host many
purpose-terminals; a user auxiliary shell (`TerminalOwner::UserShell`) can
never carry or fake an execution id — unrepresentable in the type and
rejected by the `terminal_events` CHECK.

## 3. Worker source cannot be self-declared

`ChannelRegistry::issue(role)` is the only way a caller role exists: the host
issues a channel when it spawns or serves a caller, and revokes it when the
caller is gone. `CallerIdentity.claimed_role` is informational; `validate`
requires an issued, non-revoked channel whose binding equals the claim. There
is no CLI flag or environment variable that can establish a caller role
(`viva` has none). Rejections: `UntrustedCaller` (unknown channel, revoked,
or forged claim).

## 4. Control-channel error semantics

`ControlDispatcher::submit` never panics on bad input; every outcome is
recorded in `control_requests`:

1. **Replay first.** A request id with a recorded outcome replays that
   outcome (`Replayed { original_status }`) without re-validation — accepted
   and rejected outcomes are both idempotent.
2. `UnsupportedVersion { got, expected }` — envelope version mismatch.
3. `PayloadTooLarge { size, max }` — serialized payload over 256 KiB.
4. `UntrustedCaller { detail }` — unissued/revoked channel or forged role.
5. `IdempotencyConflict { key }` — the idempotency key belongs to another
   request. The conflict rejection is stored without the disputed key.
6. `Accepted` — recorded with the key; the same id replays.

`OfficeError` (error taxonomy) additionally covers storage/io/json/id/
validation/migration/not-found. `office_events` rejects UPDATE and DELETE by
trigger and bounds payloads at 64 KiB.

## 5. Migration registry (per-domain, merge-order safe)

Rules enforced by `MigrationRegistry::freeze`:

- Versions are **per domain**, starting at 1, contiguous — no gaps, no
  duplicates. Domains never share a number sequence, so two lanes can never
  collide on a pre-numbered migration.
- Applied migrations are recorded in `schema_migrations(domain, version)`.
  On every open, pending migrations apply in-domain order, each in its own
  IMMEDIATE transaction — a domain that merges later still applies on the
  next open; a failed migration leaves no half state.
- Engine tables (`schema_meta`, `schema_migrations`) belong to the store
  engine, not to any domain migration.
- **Foundation tables are reference slices.** Domains create their own tables
  and reference foundation tables; they never `ALTER` them. Ownership of the
  foundation tables and the engine passes V01 → V07 (lane A) after G0, per
  the rollout plan §5.

| Domain constant | Owner (planned) | Notes |
| --- | --- | --- |
| `foundation` | **V01 (this issue)** | schema meta/registry, office_events, office_sessions, office_executions, launch_specs, terminal_events, control_requests |
| `members` | V02 (#11) | member records, roles, model/tool bindings |
| `workspaces_projects` | V02 (#11) | workspaces, projects, repository paths |
| `tasks_executions` | V03 (#12) | task records, execution detail/history (references foundation `office_executions`) |
| `authority` | V04 (#13) | grants, delegation records, redaction policy data |
| `git` | V08 (#17) | worktree registry, GitHub evidence links |
| `conversations` | V03/V10 | conversation detail, forks, handoff records (references foundation `office_sessions`) |
| `knowledge` | V11 (#20) | knowledge ownership, sources, validity, usage evidence |

## 6. Root file maintainership

| Path | Maintainer now | Later |
| --- | --- | --- |
| `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` | **V01 (this issue)** | composition/shared root → V07 (lane A); install/release → V13 |
| `crates/viva/src/foundation/` | V01; changes via coordinated contract updates | same handover |
| `.github/workflows/rust.yml` | V01 | V13 (release builds); merged into `ci.yml` (2026-10) |
| Dependency additions | queued as small change requests — no parallel lockfile rewrites | — |

Toolchain: `stable` channel via `rust-toolchain.toml`; crates declare
`rust-version = "1.85"` (edition 2024). CI runs `cargo fmt --check`,
`clippy -D warnings`, `cargo test` with build caching.

## 7. Implemented vs. interface-only

**Implemented and tested in V01:** paths/VIVA_HOME, typed ids, error
taxonomy, store + WAL + per-domain migrations + transactions, append-only
event log, control envelope (issuer/registry/dispatcher/idempotency),
session records, execution records with attribution snapshots, launch specs,
terminal event records, CLI (`init` / `event add` / `event list` /
`doctor`), the workbench trait shapes (mock-tested).

**Deliberately not here (owned by later issues):** real PTY/terminal
registry (V05), TUI (V06), task/execution domain logic beyond the reference
records (V03), grant scope enforcement (V04), Git/worktree services (V08),
Pi hosting (V09), performance baselines (V12). Nothing in this PR claims
those capabilities.

## 8. Validation evidence (V01 PR)

- `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test` — green (36 tests: unit + integration).
- Real binary acceptance: `viva init` and `viva event add` in an isolated
  `VIVA_HOME`, then `viva event list` in a **separate process** — the event
  reloads (`tests/g0_foundation.rs::binary_writes_an_event_and_reloads_it_after_restart`).
- Failed transactions/migrations leave no half state (SQLite tx + trigger +
  CHECK backstops, unit-tested).
- Not run here (pending, not PASS): Intel macOS run, real PTY/Pi integration,
  multi-lane migration concurrency soak — they belong to V05/V09/V12.
