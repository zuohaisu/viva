# External memory integration (F04, issue #26)

Viva links the office's member/project namespace, provenance and
recoverable exit on top of the **real Holographic memory already running
on this machine**. Nothing here re-implements FTS, HRR or storage.

## The implementation Viva verified and reuses

Verified on 2026-09-29 against the machine's actual installation:

| What | Value |
| --- | --- |
| Checkout | `~/.hermes/hermes-agent` (bundled plugin, **not** the handoff or community package) |
| Version | `v0.21.4+canary.20260926T065603Z-131-g1c535d9689` (commit `1c535d9689…`) |
| Interface | `HolographicMemoryProvider(config={"db_path": …})` → `initialize(session_id=…)` → `handle_tool_call("fact_store", {action, …})` |
| Store | SQLite `memory_store.db` (tables `facts`, `entities`, `fact_entities`, `facts_fts`, `memory_banks`) |
| Interpreter | the checkout's own venv python (the plugin package imports YAML tooling a bare system python lacks) |
| Deployment note | the user's cron runs a bidirectional content-merge sync to a second machine every 24 h — the store is live, treat it with care |

This resolves the "which variant" question the research doc
(`docs/research/hermes-holographic-memory-2026-09-28.md`) left open: the
user runs the **bundled** provider from a canary checkout, newer than the
research snapshot.

## What Viva adds (the verified gaps)

The bundled schema has no member/project scoping, no provenance and no
recoverable exit (`remove_fact` is a physical SQL DELETE). Viva's Rust
layer (`crates/viva/src/memory/`) adds exactly those, on its own tables:

- `memory_links` — which fact belongs to which member (and optional
  project), with a mandatory `source`. Search answers only the viewer's
  own, active, linked facts; unclaimable hits are counted, not mixed in.
- `memory_usages` — every recall leaves who asked, what was asked, when.
  Reuse claims cite these rows.
- Archive/restore — status flips with recorded reasons. The physical
  delete of the provider is **never exposed**: archive is the exit, and
  it is recoverable.

## Calling convention

The Rust office spawns `extensions/pi/memory/memory_adapter.py` with the
interpreter and paths from `AdapterConfig::from_env()`:

| Env | Default | Meaning |
| --- | --- | --- |
| `VIVA_MEMORY_PYTHON` | `python3` | interpreter (use the checkout's `venv/bin/python` for the real plugin) |
| `VIVA_HERMES_AGENT` | `~/.hermes/hermes-agent` | the checkout whose bundled provider is imported |
| `VIVA_MEMORY_DB` | `~/.hermes/memory_store.db` | the store path, **always passed explicitly** |
| `VIVA_MEMORY_ADAPTER` | `extensions/pi/memory/memory_adapter.py` | adapter script path |

CLI: `viva memory search | remember | archive | restore | status`.

## Safety rules baked in

- **Fail closed on the store path.** The adapter refuses to run without
  an explicit `--db`; the provider's own default is the user's real
  store, and a silent default once wrote a development test fact there
  (2026-09-29, removed within minutes, before any sync — recorded here
  as the reason the guard exists).
- **Tests never touch the real store.** The Rust integration tests
  (`crates/viva/tests/f04_memory.rs`) use a temp db and skip honestly
  when the checkout is absent (CI).
- **No delete.** Not in the adapter, not in the Rust layer, not in the
  Pi extension.
- **Unavailable ≠ empty.** A store that cannot be reached answers
  `unavailable` with a reason; nothing pretends to have remembered.
- **Known retrieval boundary (verified):** recall is FTS-gated — exact
  terms and full CJK runs match; paraphrases and partial CJK substrings
  do not. The office reports this honestly instead of faking recall.
