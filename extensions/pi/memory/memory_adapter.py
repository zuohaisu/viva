#!/usr/bin/env python3
"""Viva's thin adapter over the user's real bundled Holographic memory.

This adapter REUSES the implementation that already runs on this machine
(the bundled plugin inside the user's hermes-agent checkout); it does not
re-implement HRR, FTS or storage. Its whole job:

- speak JSON on stdout so the Rust office can call it as a subprocess;
- carry an explicit db path (Viva's tests use a temp store; the real
  store is only touched when the office is actually configured for it);
- NEVER delete: the bundled ``remove_fact`` is a physical SQL DELETE and
  there is no archive in the bundled schema, so exit handling lives on
  the Viva side and this adapter simply does not expose removal;
- degrade honestly: any failure (missing interpreter, missing checkout,
  import error, locked db) comes back as
  ``{"ok": false, "state": "unavailable", "error": ...}`` — never as
  empty-but-successful output.

Usage:
  memory_adapter.py search --agent-dir DIR [--db PATH] --query Q [--limit N]
  memory_adapter.py add    --agent-dir DIR [--db PATH] --content C \
                           [--category C] [--tags T]
  memory_adapter.py status --agent-dir DIR [--db PATH]

This file is never executed directly by hand in production: the Rust
office spawns it with the interpreter it was configured with (the
hermes-agent venv python, because the plugin package imports YAML
tooling that a bare system python lacks).
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def emit(payload: dict) -> None:
    json.dump(payload, sys.stdout, ensure_ascii=False)
    sys.stdout.write("\n")


def unavailable(error: str) -> None:
    emit({"ok": False, "state": "unavailable", "error": error, "facts": []})


def load_provider(agent_dir: str):
    """Import the bundled provider from the real checkout. Returns the
    class or raises — the caller turns any failure into `unavailable`."""
    root = Path(agent_dir).expanduser().resolve()
    plugin_dir = root / "plugins" / "memory" / "holographic"
    if not plugin_dir.is_dir():
        raise FileNotFoundError(f"no bundled holographic plugin at {plugin_dir}")
    sys.path.insert(0, str(root))
    # Import path matches the user's own sync script: the checkout is the
    # package root, the plugin is a subpackage.
    from plugins.memory.holographic import HolographicMemoryProvider  # noqa: PLC0415

    return HolographicMemoryProvider


def make_provider(agent_dir: str, db: str | None):
    if not db:
        # Fail closed. The bundled provider defaults to the user's real
        # memory_store.db when no path is given — during development a
        # test write once landed there because this adapter did not
        # forward an explicit path. That default is now structurally
        # impossible here: the office always passes an explicit store
        # path, and without one this adapter refuses to run.
        raise ValueError(
            "refusing to run without an explicit --db path: the bundled default "
            "would be the user's real memory store"
        )
    cls = load_provider(agent_dir)
    provider = cls(config={"db_path": db})
    provider.initialize(session_id="viva-office-adapter")
    return provider


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    def common(p: argparse.ArgumentParser) -> argparse.ArgumentParser:
        p.add_argument("--agent-dir", required=True, help="hermes-agent checkout root")
        p.add_argument("--db", default=None, help="memory_store.db path override")
        return p

    p_search = common(sub.add_parser("search"))
    p_search.add_argument("--query", required=True)
    p_search.add_argument("--limit", type=int, default=10)

    p_probe = common(sub.add_parser("probe"))
    p_probe.add_argument("--entity", required=True)
    p_probe.add_argument("--limit", type=int, default=10)

    p_add = common(sub.add_parser("add"))
    p_add.add_argument("--content", required=True)
    p_add.add_argument("--category", default="general")
    p_add.add_argument("--tags", default="")

    common(sub.add_parser("status"))

    args = parser.parse_args()

    try:
        provider = make_provider(args.agent_dir, args.db)
    except Exception as err:  # noqa: BLE001 — every failure is honest output
        unavailable(f"provider unavailable: {type(err).__name__}: {err}")
        return 0

    try:
        if args.command == "search":
            raw = provider.handle_tool_call(
                "fact_store",
                {"action": "search", "query": args.query, "limit": max(1, args.limit)},
            )
            payload = json.loads(raw)
            # The provider's search answers under "results"; "list" answers
            # under "facts" — verified against the live checkout.
            emit({"ok": True, "state": "fetched", "facts": payload.get("results", [])})
        elif args.command == "probe":
            raw = provider.handle_tool_call(
                "fact_store",
                {"action": "probe", "entity": args.entity, "limit": max(1, args.limit)},
            )
            payload = json.loads(raw)
            emit({"ok": True, "state": "fetched", "facts": payload.get("results", [])})
        elif args.command == "add":
            raw = provider.handle_tool_call(
                "fact_store",
                {
                    "action": "add",
                    "content": args.content,
                    "category": args.category,
                    "tags": args.tags,
                },
            )
            payload = json.loads(raw)
            emit(
                {
                    "ok": True,
                    "state": "fetched",
                    "fact": payload.get("fact", payload),
                }
            )
        else:  # status
            raw = provider.handle_tool_call(
                "fact_store", {"action": "list", "limit": 1}
            )
            payload = json.loads(raw)
            # Honest degradation visibility: the provider silently falls
            # back to FTS/Jaccard when NumPy is missing — surface that.
            try:
                import numpy  # noqa: F401

                hrr = "active"
            except Exception:  # noqa: BLE001
                hrr = "degraded (NumPy missing: HRR reranking off)"
            emit(
                {
                    "ok": True,
                    "state": "fetched",
                    "store": str(Path(args.db).expanduser()) if args.db else "default",
                    "facts_total": payload.get("count"),
                    "hrr": hrr,
                }
            )
    except Exception as err:  # noqa: BLE001
        unavailable(f"tool call failed: {type(err).__name__}: {err}")
    finally:
        try:
            provider._store.close()  # noqa: SLF001 — single owner, this process
        except Exception:  # noqa: BLE001
            pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
