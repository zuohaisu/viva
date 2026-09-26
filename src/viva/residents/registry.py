"""Persistent AI-member records under ``~/.viva/residents/``.

A member record is *configuration plus identity*, deliberately thin:

    {"schema_version": "2.0", "id": "deven", "name": "Deven",
     "role": "developer",
     "engine": {"id": "claude-default", "model": null, "configured_at": ...},
     "tools": ["claude"], "created_at": ..., "notes": ...}

What that record **is**: a stable id, the role the user gave this member, and
the replaceable model/tool bindings it currently uses. What it is **not**: a
claim of Self continuity. Viva keeps history (the append-only experience
journal and the member's knowledge entries) and can prove those survived; it
does not claim a continuous Self, and nothing here should read as one.

Changing the engine rewrites one field and appends an event. It never deletes
the record, the history or the knowledge.

One human user owns many members; one member runs many concurrent executions.
``Viva != Samuel`` still holds: no member name appears in Viva's logic.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Callable

from viva.core.errors import VivaError
from viva.core.ids import slugify, utc_now
from viva.core.paths import ensure_private_dir, viva_home
from viva.core.store import atomic_json_write, read_json_object
from viva.residents.engines import EngineCatalog, EngineError
from viva.residents.roles import RoleCatalog, RoleError

SCHEMA_VERSION = "2.0"
LEGACY_SCHEMA_VERSION = "1.0"
WORK_MODES = ("read_only", "write")


class ResidentError(VivaError):
    """A member record could not be created, read, or configured."""


class InvocationUnavailable(ResidentError):
    """The member's configured model or tool cannot be used as configured.

    Viva never substitutes a different model or tool here: a member configured
    to use something unavailable fails loudly instead of silently becoming a
    different member.
    """


class ResidentRegistry:
    """Create, load and configure persistent AI members."""

    def __init__(
        self, home: str | Path | None = None, *, journal: Callable[..., Any] | None = None
    ):
        self.home = ensure_private_dir(viva_home(home) / "residents")
        self.roles = RoleCatalog(home)
        self.engines = EngineCatalog(home)
        self._journal = journal

    # -- paths ---------------------------------------------------------------

    def _path(self, resident_id: str) -> Path:
        return self.home / f"{resident_id}.json"

    # -- writes --------------------------------------------------------------

    def create(
        self,
        name: str,
        *,
        role: str = "developer",
        engine: str | None = None,
        model: str | None = None,
        tools: list[str] | None = None,
        notes: str = "",
        resident_id: str | None = None,
    ) -> dict[str, Any]:
        if not isinstance(name, str) or not name.strip():
            raise ResidentError("member name must be a non-empty string")
        if not isinstance(notes, str):
            raise ResidentError("member notes must be a string")
        try:
            rid = resident_id or slugify(name, what="member name")
        except ValueError as exc:
            raise ResidentError(str(exc)) from exc
        if rid != slugify(rid, what="member id"):
            raise ResidentError(
                f"member id may contain only a-z, 0-9 and '-', got {resident_id!r}"
            )
        path = self._path(rid)
        if path.exists():
            raise ResidentError(f"member {rid!r} already exists")
        try:
            role_record = self.roles.get(role)
        except RoleError as exc:
            raise ResidentError(str(exc)) from exc
        engine_binding = self._binding(engine_id=engine, model=model)
        configured_tools = self._tools(engine_binding, role=role_record, tools=tools)
        record = {
            "schema_version": SCHEMA_VERSION,
            "id": rid,
            "name": name.strip(),
            "role": role_record["id"],
            "created_at": utc_now(),
            "notes": notes,
            "engine": engine_binding,
            "tools": configured_tools,
        }
        atomic_json_write(path, record)
        self._event(
            "member.created",
            resident_id=rid,
            payload={
                "member_id": rid,
                "name": record["name"],
                "role": record["role"],
                "engine": engine_binding["id"],
                "model": engine_binding["model"],
                "tools": configured_tools,
            },
        )
        return record

    def set_role(self, resident_id: str, role: str) -> dict[str, Any]:
        record = self._require(resident_id)
        try:
            role_record = self.roles.get(role)
        except RoleError as exc:
            raise ResidentError(str(exc)) from exc
        previous = record.get("role")
        record["role"] = role_record["id"]
        atomic_json_write(self._path(record["id"]), record)
        self._event(
            "member.role_changed",
            resident_id=record["id"],
            payload={"member_id": record["id"], "from": previous, "to": role_record["id"]},
        )
        return record

    def set_engine(
        self, resident_id: str, *, engine: str, model: str | None = None
    ) -> dict[str, Any]:
        """Rebind the cognitive engine, keeping every other field untouched."""
        record = self._require(resident_id)
        previous = dict(record.get("engine") or {})
        binding = self._binding(engine_id=engine, model=model)
        tools = list(record.get("tools") or [])
        if binding["tool"] not in tools:
            tools.append(binding["tool"])
        record["engine"] = binding
        record["tools"] = tools
        atomic_json_write(self._path(record["id"]), record)
        self._event(
            "member.engine_changed",
            resident_id=record["id"],
            payload={
                "member_id": record["id"],
                "from": previous.get("id"),
                "from_model": previous.get("model"),
                "to": binding["id"],
                "to_model": binding["model"],
                "history_retained": True,
            },
        )
        return record

    def set_tools(self, resident_id: str, tools: list[str]) -> dict[str, Any]:
        record = self._require(resident_id)
        engine_binding = record.get("engine") or {}
        cleaned = self._tools(
            engine_binding, role=self.roles.get(record["role"]), tools=tools
        )
        previous = list(record.get("tools") or [])
        record["tools"] = cleaned
        atomic_json_write(self._path(record["id"]), record)
        self._event(
            "member.tools_changed",
            resident_id=record["id"],
            payload={"member_id": record["id"], "from": previous, "to": cleaned},
        )
        return record

    # -- reads ---------------------------------------------------------------

    def get(self, resident_id_or_name: str) -> dict[str, Any] | None:
        """Look up a member by exact id, then by case-insensitive name."""
        if not isinstance(resident_id_or_name, str) or not resident_id_or_name.strip():
            return None
        needle = resident_id_or_name.strip()
        direct = read_json_object(self._path(needle))
        if direct is not None and direct.get("id"):
            return self._migrate(direct)
        for record in self.list():
            if str(record.get("name", "")).casefold() == needle.casefold():
                return record
        return None

    def require(self, resident_id_or_name: str) -> dict[str, Any]:
        record = self.get(resident_id_or_name)
        if record is None:
            raise ResidentError(
                f"no AI member matches {resident_id_or_name!r} — create it with: "
                "viva resident add <name> --role <role>"
            )
        return record

    def list(self) -> list[dict[str, Any]]:
        members = []
        for path in sorted(self.home.glob("*.json")):
            record = read_json_object(path)
            if record is None:
                raise ResidentError(f"member record is unreadable: {path.name}")
            members.append(self._migrate(record))
        members.sort(
            key=lambda record: (str(record.get("created_at", "")), str(record.get("id", "")))
        )
        return members

    def role_of(self, record: dict[str, Any]) -> dict[str, Any]:
        return self.roles.get(str(record.get("role", "developer")))

    # -- invocation ----------------------------------------------------------

    def resolve_invocation(
        self,
        member: dict[str, Any],
        *,
        mode: str | None = None,
        tool: str | None = None,
        workers: Any = None,
    ) -> dict[str, Any]:
        """Resolve the exact model + tool an execution would use, or fail loudly.

        ``workers`` is a :class:`viva.workers.WorkerRegistry`. Passing it makes
        this a real availability check: an unavailable tool raises
        :class:`InvocationUnavailable` instead of quietly using another one.
        """
        member = self._migrate(member)
        try:
            role = self.roles.get(str(member.get("role", "")))
        except RoleError as exc:
            raise InvocationUnavailable(str(exc)) from exc
        wanted_mode = str(mode or role["default_mode"])
        if wanted_mode not in WORK_MODES:
            raise InvocationUnavailable(
                f"unknown work mode {wanted_mode!r}: expected one of {list(WORK_MODES)}"
            )
        if wanted_mode not in role["allowed_modes"]:
            raise InvocationUnavailable(
                f"role {role['id']!r} may only run {role['allowed_modes']}; "
                f"{wanted_mode!r} is not allowed"
            )
        binding = member.get("engine") or {}
        if not binding.get("id"):
            raise InvocationUnavailable(
                f"member {member['id']!r} has no cognitive engine configured — "
                "set one with: viva resident engine <member> <engine-id>"
            )
        try:
            engine = self.engines.get(str(binding["id"]))
        except EngineError as exc:
            raise InvocationUnavailable(str(exc)) from exc
        configured_tools = [str(item) for item in member.get("tools") or []]
        tool_name = str(tool or engine["tool"])
        if tool_name not in configured_tools:
            raise InvocationUnavailable(
                f"tool {tool_name!r} is not bound to member {member['id']!r} "
                f"(bound tools: {configured_tools}) — bind it with: "
                f"viva resident tools {member['id']} --tool {tool_name}"
            )
        invocation: dict[str, Any] = {
            "member_id": member["id"],
            "member_name": member.get("name") or member["id"],
            "role": role["id"],
            "engine": {"id": engine["id"], "model": binding.get("model") or engine["model"]},
            "engine_catalog": engine,
            "tool": tool_name,
            "mode": wanted_mode,
            "available": True,
        }
        if workers is None:
            return invocation
        worker = workers.get(tool_name)
        availability = workers.probe(worker)
        if not availability.get("available"):
            raise InvocationUnavailable(
                f"member {member['id']!r} is configured for engine {engine['id']!r} "
                f"via tool {tool_name!r}, which is unavailable: "
                f"{availability.get('reason')}. Viva will not silently substitute "
                "another model or tool."
            )
        invocation["available"] = True
        invocation["tool_version"] = availability.get("version") or ""
        invocation["worker"] = worker
        return invocation

    @staticmethod
    def build_argv(invocation: dict[str, Any], message: str) -> list[str]:
        """argv for one invocation: tool argv + engine model args + message."""
        worker = invocation["worker"]
        argv = [str(worker["command"]), *[str(arg) for arg in worker.get("args", [])]]
        engine = invocation["engine"]
        catalog = invocation.get("engine_catalog") or {}
        argv.extend(str(arg) for arg in catalog.get("args", []))
        model_flag = catalog.get("model_flag")
        if engine.get("model") and model_flag:
            argv.extend([str(model_flag), str(engine["model"])])
        argv.append(message)
        return argv

    # -- internals -----------------------------------------------------------

    def _binding(self, *, engine_id: str | None, model: str | None) -> dict[str, Any]:
        if engine_id:
            try:
                return self.engines.as_binding(engine_id, model=model)
            except EngineError as exc:
                raise ResidentError(str(exc)) from exc
        catalog = self.engines.list()
        if not catalog:
            raise ResidentError("no engines are configured; add one to ~/.viva/config/engines.json")
        return {
            "id": catalog[0]["id"],
            "tool": catalog[0]["tool"],
            "model": model if model is not None else catalog[0]["model"],
            "configured_at": utc_now(),
            "configured_by": "catalogue-default",
        }

    def _tools(
        self, binding: dict[str, Any], *, role: dict[str, Any], tools: list[str] | None
    ) -> list[str]:
        try:
            engine = self.engines.get(str(binding["id"]))
        except EngineError as exc:
            raise ResidentError(str(exc)) from exc
        cleaned = [str(item).strip() for item in (tools or [engine["tool"]]) if str(item).strip()]
        if not cleaned:
            raise ResidentError(f"role {role['id']!r} needs at least one execution tool")
        if engine["tool"] not in cleaned:
            cleaned.insert(0, engine["tool"])
        return cleaned

    def _migrate(self, record: dict[str, Any]) -> dict[str, Any]:
        """Read v1 records (no role/engine) without destroying them."""
        version = record.get("schema_version")
        if version == SCHEMA_VERSION:
            return record
        if version != LEGACY_SCHEMA_VERSION:
            raise ResidentError(
                f"member record {record.get('id')!r} has unsupported schema_version {version!r}"
            )
        binding = self._binding(engine_id=None, model=None)
        tool = self.engines.get(str(binding["id"]))["tool"]
        return {
            **record,
            "schema_version": SCHEMA_VERSION,
            "role": record.get("role") or "developer",
            "engine": record.get("engine") or binding,
            "tools": record.get("tools") or [tool],
        }

    def _require(self, resident_id_or_name: str) -> dict[str, Any]:
        return self.require(resident_id_or_name)

    def export(self, record: dict[str, Any]) -> str:
        return json.dumps(record, ensure_ascii=False, sort_keys=True)

    def _event(self, event_type: str, *, resident_id: str, payload: dict[str, Any]) -> None:
        if self._journal is None:
            return
        self._journal(event_type=event_type, resident_id=resident_id, source="viva", payload=payload)
