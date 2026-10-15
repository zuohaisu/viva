# External memory (F04, native storage)

Viva stores durable facts in its own SQLite database (memory domain v3).
Member/project scope, provenance, usage evidence, and recoverable archive
live in Viva's link layer on top of those native facts. No Hermes checkout
and no Python runtime are required for recall or writes.

## What Viva stores

| Layer | Tables | Role |
| --- | --- | --- |
| Native facts | `memory_facts`, `memory_facts_fts`, `memory_entities`, … | Content, FTS5 trigram index, minimal HRR vectors (1024-dim), dedupe by content hash |
| Links | `memory_links` | Which fact belongs to which member (optional project), mandatory `source`, `storage_origin` (`native` or `legacy_hermes`) |
| Usage | `memory_usages` | Every recall records viewer, query, timestamp |
| Exit | link status | Archive/restore with reasons; no physical delete of facts |

Legacy rows imported from the old Hermes adapter remain in `memory_links` with
`storage_origin = legacy_hermes`. They are counted in `viva memory status` but
never participate in `search` or `probe`.

## CLI

`viva memory search | probe | link | remember | archive | restore | status`

All commands read and write through the Viva home database (`viva init`).

## Content hash (native dedupe)

Fact bodies are deduplicated with SHA-256 over **NFC-normalized, trim-normalized**
text, encoded as lowercase hex (`crates/viva/src/memory/hash.rs`).

## Provenance source strings

Every write requires a non-empty `source`. For facts written in Viva, use a
human-readable attribution (for example `chat by Samuel (task task-1)`).

For a future Hermes import (not implemented in this slice), document facts with:

```text
hermes:<absolute path to memory_store.db>#<numeric fact_id>
```

Example: `hermes:/Users/me/.hermes/memory_store.db#42`

## Retrieval

Hybrid rerank within the member/project scope: FTS trigram (weight **0.4**),
Jaccard overlap ( **0.3** ), HRR similarity ( **0.3** ). Short queries use a
deterministic substring fallback. Trust scores below **0.3** are filtered at
the FTS stage.

## HRR port (MIT)

Phase encoding, bind/bundle, and float32 blob serialization are ported from
upstream Hermes Agent holographic memory (`plugins/memory/holographic`) at
commit `6f7a799`. Copyright (c) Nous Research / Hermes Agent contributors,
SPDX-License-Identifier: MIT. See `crates/viva/src/memory/hrr.rs`.

Research background:
[Research-Hermes-Holographic-Memory](https://github.com/zuohaisu/viva/wiki/Research-Hermes-Holographic-Memory).

## Safety rules

- **Tests use isolated homes.** Integration tests (`crates/viva/tests/f04_memory.rs`)
  run the real `viva` binary with a temp `VIVA_HOME` — no Hermes, no Python.
- **No delete.** Archive is the exit path; restore is supported for native links.
- **Unavailable ≠ empty.** Operational errors surface as validation or I/O failures;
  an empty search means nothing matched in scope, not a missing backend.
- **Legacy boundary.** `legacy_hermes` links cannot be linked, archived, or restored
  through native exit paths.
