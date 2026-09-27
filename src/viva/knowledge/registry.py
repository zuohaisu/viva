"""Knowledge: who owns an insight, and who may reuse it.

Four kinds, each with one owner — the boundary is deliberate, because a single
undifferentiated "memory" bucket is how a workspace's facts end up buried
inside one agent's head, and how one agent's habits leak into every project:

    personal_memory        owned by one AI member; survives model changes
    self_model_candidate   a *hypothesis* about that member, with evidence
                           attached; never silently promoted (evolution is not
                           implemented and is not pretended)
    project_knowledge      owned by one project; true even if the member that
                           wrote it disappears
    team_knowledge         shared across the members of the office
    skill                  a reusable procedure (written as a SKILL.md file so
                           it is portable to any tool that reads that format)

Discipline that makes this evidence rather than decoration:

* an entry without **provenance** (the task/execution it came from) is refused;
* an entry is only *reuse evidence* once a later execution records using it —
  :meth:`KnowledgeRegistry.reuse_evidence` lists exactly those, and nothing
  else counts.

Storage is open and human-readable: an append-only ``entries.jsonl`` plus one
Markdown file per skill.
"""

from __future__ import annotations

import json
import threading
from pathlib import Path
from typing import Any, Callable

from viva.core.errors import VivaError
from viva.core.ids import new_knowledge_id, utc_now
from viva.core.paths import ensure_private_dir, viva_home
from viva.core.redaction import redact

SCHEMA_VERSION = "1.0"
ENTRIES_FILENAME = "entries.jsonl"
KINDS = (
    "personal_memory",
    "self_model_candidate",
    "project_knowledge",
    "team_knowledge",
    "skill",
)
OWNERS = {
    "personal_memory": "member",
    "self_model_candidate": "member",
    "project_knowledge": "project",
    "team_knowledge": "team",
    "skill": "team",
}
_LOCK = threading.RLock()


class KnowledgeError(VivaError):
    """A knowledge entry could not be created, read, or attributed."""


class KnowledgeRegistry:
    """Append-only knowledge ledger at ``~/.viva/knowledge/``."""

    def __init__(self, home: str | Path | None = None, *, journal: Callable[..., Any] | None = None):
        self.home = ensure_private_dir(viva_home(home) / "knowledge")
        self.path = self.home / ENTRIES_FILENAME
        self.skills = ensure_private_dir(self.home / "skills")
        self._journal = journal

    # -- persistence ---------------------------------------------------------

    def _append(self, record: dict[str, Any]) -> dict[str, Any]:
        event = redact({"schema_version": SCHEMA_VERSION, "recorded_at": utc_now(), **record})
        with _LOCK:
            try:
                with self.path.open("a", encoding="utf-8") as output:
                    output.write(json.dumps(event, ensure_ascii=False, sort_keys=True) + "\n")
            except OSError as exc:
                raise KnowledgeError(f"knowledge ledger append failed: {exc}") from exc
        return event

    def records(self) -> list[dict[str, Any]]:
        if not self.path.exists():
            return []
        try:
            lines = self.path.read_text(encoding="utf-8").splitlines()
        except OSError as exc:
            raise KnowledgeError("knowledge ledger is unreadable") from exc
        parsed: list[dict[str, Any]] = []
        for number, line in enumerate(lines, start=1):
            try:
                record = json.loads(line)
            except json.JSONDecodeError as exc:
                raise KnowledgeError(f"knowledge ledger is malformed at line {number}") from exc
            if not isinstance(record, dict):
                raise KnowledgeError(f"knowledge ledger is invalid at line {number}")
            parsed.append(record)
        return parsed

    # -- reads ---------------------------------------------------------------

    def list(
        self,
        *,
        kind: str | None = None,
        owner: str | None = None,
        project_id: str | None = None,
        member_id: str | None = None,
    ) -> list[dict[str, Any]]:
        entries: dict[str, dict[str, Any]] = {}
        for record in self.records():
            record_type = record.get("record_type")
            if record_type == "entry":
                entries[str(record["id"])] = {**record, "used_in": [], "status": "active"}
            elif record_type == "usage":
                entry = entries.get(str(record.get("id")))
                if entry is not None:
                    entry["used_in"].append(
                        {
                            "execution_id": record.get("execution_id"),
                            "task_id": record.get("task_id"),
                            "at": record.get("recorded_at"),
                            "note": record.get("note") or "",
                        }
                    )
            elif record_type == "retraction":
                entry = entries.get(str(record.get("id")))
                if entry is not None:
                    entry["status"] = "retracted"
                    entry["retracted_reason"] = record.get("reason")
        results = list(entries.values())
        if kind is not None:
            results = [entry for entry in results if entry.get("kind") == kind]
        if owner is not None:
            results = [entry for entry in results if entry.get("owner") == owner]
        if project_id is not None:
            results = [entry for entry in results if entry.get("project_id") == project_id]
        if member_id is not None:
            results = [entry for entry in results if entry.get("member_id") == member_id]
        results.sort(key=lambda entry: (str(entry.get("recorded_at", "")), str(entry.get("id", ""))))
        return results

    def get(self, entry_id: str) -> dict[str, Any] | None:
        needle = str(entry_id).strip()
        for entry in self.list():
            if entry["id"] == needle:
                return entry
        return None

    def require(self, entry_id: str) -> dict[str, Any]:
        entry = self.get(entry_id)
        if entry is None:
            raise KnowledgeError(f"no knowledge entry {entry_id!r} — run: viva knowledge list")
        return entry

    def for_task(self, task: dict[str, Any]) -> list[dict[str, Any]]:
        """Which knowledge a task may see, by ownership boundary. Nothing else."""
        project_id = task.get("project_id")
        assignees = set(task.get("assignees") or [])
        selected = []
        for entry in self.list():
            if entry.get("status") != "active":
                continue
            kind = entry.get("kind")
            if OWNERS.get(str(kind)) == "team":
                selected.append(entry)
            elif kind == "project_knowledge" and entry.get("project_id") == project_id:
                selected.append(entry)
            elif OWNERS.get(str(kind)) == "member" and entry.get("member_id") in assignees:
                selected.append(entry)
        return selected

    def reuse_evidence(self) -> list[dict[str, Any]]:
        """Entries a later execution actually used — the only reuse evidence."""
        return [entry for entry in self.list() if entry.get("used_in")]

    # -- writes --------------------------------------------------------------

    def add(
        self,
        *,
        kind: str,
        title: str,
        body: str,
        provenance: dict[str, Any],
        member_id: str | None = None,
        project_id: str | None = None,
        created_by: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        kind = str(kind).strip()
        if kind not in KINDS:
            raise KnowledgeError(f"unknown knowledge kind {kind!r}: expected one of {KINDS}")
        title = str(title).strip()
        if not title:
            raise KnowledgeError("knowledge title must be a non-empty string")
        body = str(body).strip()
        if not body:
            raise KnowledgeError("knowledge body must be a non-empty string")
        if not isinstance(provenance, dict) or not (
            provenance.get("task_id") or provenance.get("execution_id") or provenance.get("source")
        ):
            raise KnowledgeError(
                "a knowledge entry needs provenance (task_id, execution_id or source); "
                "an entry without provenance is not admissible"
            )
        owner = OWNERS[kind]
        if owner == "member" and not member_id:
            raise KnowledgeError(f"{kind!r} must name the member that owns it (--member)")
        if owner == "project" and not project_id:
            raise KnowledgeError(f"{kind!r} must name the project that owns it (--project)")
        entry_id = new_knowledge_id()
        self._append(
            {
                "record_type": "entry",
                "id": entry_id,
                "kind": kind,
                "owner": owner,
                "member_id": member_id,
                "project_id": project_id,
                "title": title,
                "body": body,
                "provenance": dict(provenance),
                "created_by": created_by or {"kind": "user", "id": "user"},
            }
        )
        if kind == "skill":
            self._write_skill_file(entry_id, title, body, provenance)
        self._event(
            "knowledge.recorded",
            payload={
                "entry_id": entry_id,
                "kind": kind,
                "owner": owner,
                "member_id": member_id,
                "project_id": project_id,
                "title": title,
                "provenance": provenance,
            },
        )
        return self.require(entry_id)

    def record_usage(self, entry_id: str, *, execution_id: str, task_id: str | None = None, note: str = "") -> dict[str, Any]:
        entry = self.require(entry_id)
        if not execution_id:
            raise KnowledgeError("recording usage needs the execution that used the entry")
        self._append(
            {
                "record_type": "usage",
                "id": entry["id"],
                "execution_id": str(execution_id),
                "task_id": task_id,
                "note": str(note),
            }
        )
        self._event(
            "knowledge.used",
            payload={
                "entry_id": entry["id"],
                "execution_id": str(execution_id),
                "task_id": task_id,
                "note": str(note),
            },
        )
        return self.require(entry_id)

    def retract(self, entry_id: str, *, reason: str) -> dict[str, Any]:
        entry = self.require(entry_id)
        reason = str(reason).strip()
        if not reason:
            raise KnowledgeError("retracting an entry requires a reason")
        self._append({"record_type": "retraction", "id": entry["id"], "reason": reason})
        self._event("knowledge.retracted", payload={"entry_id": entry["id"], "reason": reason})
        return self.require(entry_id)

    def skill_path(self, entry_id: str) -> Path:
        return self.skills / f"{entry_id}.md"

    def _write_skill_file(
        self, entry_id: str, title: str, body: str, provenance: dict[str, Any]
    ) -> None:
        """Skills are written as SKILL.md so they are portable to other tools."""
        lines = [
            "---",
            f"name: {entry_id}",
            f"title: {title}",
            f"provenance: {json.dumps(provenance, ensure_ascii=False, sort_keys=True)}",
            "---",
            "",
            body,
            "",
        ]
        path = self.skill_path(entry_id)
        path.write_text("\n".join(lines), encoding="utf-8")
        try:
            path.chmod(0o600)
        except OSError:
            pass

    def _event(self, event_type: str, *, payload: dict[str, Any]) -> None:
        if self._journal is None:
            return
        self._journal(event_type=event_type, source="viva", payload=payload)
