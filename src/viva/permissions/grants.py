"""Grant ledger: who is allowed to make whom do what, and why.

A **grant** is the only way an AI member (rather than Haisu in person) may
cause work to happen. Every grant records:

* its **source** — ``user`` (Haisu granted it) or ``worker`` (an execution that
  holds a parent grant is passing part of it on);
* its **scope** — the task it applies to, the actions it covers, and the
  strongest work mode it may use;
* its **delegation relationship** — ``delegated_from`` points at the parent
  grant, so any capability can be traced back to the human who granted it.

Two invariants, both enforced here and both tested:

1. a child grant can never be broader than its parent (actions ⊆ parent,
   mode ≤ parent, same task);
2. a refused request is *recorded* with its reason instead of failing silently.

The ledger is append-only JSONL: grants, revocations and refusals are all
evidence. Nothing here is a general policy engine — it is the minimal
structure the AI Office needs to let a coordinator worker act on Haisu's
behalf within a boundary Haisu drew.
"""

from __future__ import annotations

import json
import threading
from pathlib import Path
from typing import Any, Callable, Iterable, Mapping

from viva.core.errors import VivaError
from viva.core.ids import new_grant_id, utc_now
from viva.core.paths import ensure_private_dir, viva_home
from viva.core.redaction import redact

SCHEMA_VERSION = "1.0"
LEDGER_FILENAME = "grants.jsonl"
_LOCK = threading.RLock()

GRANT_ACTIONS = frozenset({"dispatch", "status", "result", "stop"})
MODE_ORDER = {"read_only": 0, "write": 1}
SOURCE_KINDS = frozenset({"user", "worker"})


class GrantError(VivaError):
    """A grant is missing, out of scope, or broader than its parent."""


def _validate_actions(actions: Iterable[str]) -> list[str]:
    cleaned = sorted({str(action).strip() for action in actions if str(action).strip()})
    if not cleaned:
        raise GrantError("a grant must cover at least one action")
    unknown = [action for action in cleaned if action not in GRANT_ACTIONS]
    if unknown:
        raise GrantError(f"unknown grant actions: {unknown}; allowed: {sorted(GRANT_ACTIONS)}")
    return cleaned


def _validate_mode(mode: str) -> str:
    value = str(mode).strip()
    if value not in MODE_ORDER:
        raise GrantError(f"unknown work mode {mode!r}: expected one of {sorted(MODE_ORDER)}")
    return value


class GrantRegistry:
    """Append-only grant ledger at ``~/.viva/authority/grants.jsonl``."""

    def __init__(self, home: str | Path | None = None, *, journal: Callable[..., Any] | None = None):
        self.home = ensure_private_dir(viva_home(home) / "authority")
        self.path = self.home / LEDGER_FILENAME
        self._journal = journal

    # -- persistence ---------------------------------------------------------

    def _append(self, record: dict[str, Any]) -> dict[str, Any]:
        event = redact({"schema_version": SCHEMA_VERSION, "recorded_at": utc_now(), **record})
        with _LOCK:
            try:
                with self.path.open("a", encoding="utf-8") as output:
                    output.write(json.dumps(event, ensure_ascii=False, sort_keys=True) + "\n")
            except OSError as exc:
                raise GrantError(f"grant ledger append failed: {exc}") from exc
        return event

    def records(self) -> list[dict[str, Any]]:
        if not self.path.exists():
            return []
        try:
            lines = self.path.read_text(encoding="utf-8").splitlines()
        except OSError as exc:
            raise GrantError("grant ledger is unreadable") from exc
        records: list[dict[str, Any]] = []
        for number, line in enumerate(lines, start=1):
            try:
                record = json.loads(line)
            except json.JSONDecodeError as exc:
                raise GrantError(f"grant ledger is malformed at line {number}") from exc
            if not isinstance(record, dict):
                raise GrantError(f"grant ledger is invalid at line {number}")
            records.append(record)
        return records

    # -- reads ---------------------------------------------------------------

    def list(self) -> list[dict[str, Any]]:
        """Every grant with its revocation state folded in."""
        grants: dict[str, dict[str, Any]] = {}
        for record in self.records():
            kind = record.get("record_type")
            if kind == "grant":
                grants[str(record["id"])] = {**record, "revoked_at": None, "revoked_reason": None}
            elif kind == "revocation":
                grant = grants.get(str(record.get("id")))
                if grant is not None:
                    grant["revoked_at"] = record.get("recorded_at")
                    grant["revoked_reason"] = record.get("reason")
        return list(grants.values())

    def get(self, grant_id: str) -> dict[str, Any] | None:
        needle = str(grant_id).strip()
        for grant in self.list():
            if grant["id"] == needle:
                return grant
        return None

    def refusals(self) -> list[dict[str, Any]]:
        return [record for record in self.records() if record.get("record_type") == "refusal"]

    # -- writes --------------------------------------------------------------

    def create(
        self,
        *,
        source: Mapping[str, Any],
        grantee: str,
        task_id: str,
        actions: Iterable[str],
        mode_max: str,
        reason: str,
        delegated_from: str | None = None,
    ) -> dict[str, Any]:
        """Create a grant, refusing any widening of an existing parent grant."""
        source_kind = str(source.get("kind") or "").strip()
        source_id = str(source.get("id") or "").strip()
        if source_kind not in SOURCE_KINDS:
            raise GrantError(f"grant source kind must be one of {sorted(SOURCE_KINDS)}")
        if not source_id:
            raise GrantError("grant source id is required")
        grantee = str(grantee).strip()
        if not grantee:
            raise GrantError("grant grantee is required")
        task_id = str(task_id).strip()
        if not task_id:
            raise GrantError("a grant must name the task it applies to")
        reason = str(reason).strip()
        if not reason:
            raise GrantError("a grant must state why it was granted")
        mode_max = _validate_mode(mode_max)
        wanted = _validate_actions(actions)

        parent = None
        if delegated_from:
            parent = self.get(delegated_from)
            if parent is None:
                return self._refuse(
                    f"delegation refused: parent grant {delegated_from!r} does not exist",
                    attempted={
                        "source": dict(source),
                        "grantee": grantee,
                        "task_id": task_id,
                        "actions": wanted,
                        "mode_max": mode_max,
                        "delegated_from": delegated_from,
                    },
                )
            if source_kind != "worker":
                return self._refuse(
                    "delegation refused: only a worker holding a parent grant may delegate",
                    attempted={"source": dict(source), "delegated_from": delegated_from},
                )
        elif source_kind == "worker":
            return self._refuse(
                "worker-originated grants must name the parent grant they delegate from",
                attempted={"source": dict(source), "task_id": task_id},
            )

        if parent is not None:
            if parent.get("revoked_at"):
                return self._refuse(
                    f"delegation refused: parent grant {parent['id']!r} was revoked",
                    attempted={"delegated_from": parent["id"]},
                )
            wider = sorted(set(wanted) - set(parent.get("actions", [])))
            if wider:
                return self._refuse(
                    f"delegation refused: actions {wider} are outside parent grant {parent['id']!r}",
                    attempted={"delegated_from": parent["id"], "actions": wanted},
                )
            if MODE_ORDER[mode_max] > MODE_ORDER[str(parent.get("mode_max"))]:
                return self._refuse(
                    f"delegation refused: mode {mode_max!r} exceeds parent grant "
                    f"{parent['id']!r} mode {parent.get('mode_max')!r}",
                    attempted={"delegated_from": parent["id"], "mode_max": mode_max},
                )
            if str(parent.get("task_id")) != task_id:
                return self._refuse(
                    f"delegation refused: task {task_id!r} is outside parent grant "
                    f"{parent['id']!r} task {parent.get('task_id')!r}",
                    attempted={"delegated_from": parent["id"], "task_id": task_id},
                )

        grant = self._append(
            {
                "record_type": "grant",
                "id": new_grant_id(),
                "source": {"kind": source_kind, "id": source_id},
                "grantee": grantee,
                "task_id": task_id,
                "actions": wanted,
                "mode_max": mode_max,
                "delegated_from": parent["id"] if parent else None,
                "reason": reason,
            }
        )
        self._event(
            "authority.granted",
            payload={
                "grant_id": grant["id"],
                "source": grant["source"],
                "grantee": grantee,
                "task_id": task_id,
                "actions": wanted,
                "mode_max": mode_max,
                "delegated_from": grant["delegated_from"],
                "reason": reason,
            },
        )
        return grant

    def revoke(self, grant_id: str, *, reason: str) -> dict[str, Any]:
        grant = self.get(grant_id)
        if grant is None:
            raise GrantError(f"no grant {grant_id!r} to revoke")
        reason = str(reason).strip()
        if not reason:
            raise GrantError("revoking a grant requires a reason")
        record = self._append(
            {"record_type": "revocation", "id": grant["id"], "reason": reason}
        )
        self._event("authority.revoked", payload={"grant_id": grant["id"], "reason": reason})
        return record

    def check(
        self,
        grant_id: str,
        *,
        action: str,
        task_id: str | None = None,
        mode: str | None = None,
    ) -> dict[str, Any]:
        """Return the grant if it covers *action*, else refuse with a reason."""
        if action not in GRANT_ACTIONS:
            raise GrantError(f"unknown grant action {action!r}; allowed: {sorted(GRANT_ACTIONS)}")
        grant = self.get(grant_id)
        if grant is None:
            return self._refuse(
                f"refused: no grant {grant_id!r} exists",
                attempted={"grant_id": grant_id, "action": action, "task_id": task_id},
            )
        if grant.get("revoked_at"):
            return self._refuse(
                f"refused: grant {grant['id']!r} was revoked ({grant.get('revoked_reason')})",
                attempted={"grant_id": grant["id"], "action": action, "task_id": task_id},
            )
        if action not in grant.get("actions", []):
            return self._refuse(
                f"refused: grant {grant['id']!r} does not cover action {action!r}",
                attempted={"grant_id": grant["id"], "action": action, "task_id": task_id},
            )
        if task_id is not None and str(grant.get("task_id")) != str(task_id):
            return self._refuse(
                f"refused: grant {grant['id']!r} covers task {grant.get('task_id')!r}, "
                f"not {task_id!r}",
                attempted={"grant_id": grant["id"], "action": action, "task_id": task_id},
            )
        if mode is not None:
            wanted = _validate_mode(mode)
            if MODE_ORDER[wanted] > MODE_ORDER[str(grant.get("mode_max"))]:
                return self._refuse(
                    f"refused: grant {grant['id']!r} allows {grant.get('mode_max')!r}, "
                    f"not {wanted!r}",
                    attempted={"grant_id": grant["id"], "action": action, "mode": wanted},
                )
        return grant

    # -- refusal evidence ----------------------------------------------------

    def _refuse(self, reason: str, *, attempted: dict[str, Any]) -> Any:
        self._append(
            {"record_type": "refusal", "reason": reason, "attempted": attempted}
        )
        self._event("authority.refused", payload={"reason": reason, "attempted": attempted})
        raise GrantError(reason)

    def _event(self, event_type: str, *, payload: dict[str, Any]) -> None:
        if self._journal is None:
            return
        self._journal(event_type=event_type, source="viva", payload=payload)
