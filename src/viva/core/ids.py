"""Identifier and timestamp helpers shared by Viva domains."""

from __future__ import annotations

import re
import uuid
from datetime import datetime, timezone


def utc_now() -> str:
    """Return an ISO-8601 UTC timestamp with a trailing Z."""
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def new_session_id() -> str:
    return f"s-{uuid.uuid4().hex[:16]}"


def new_event_id() -> str:
    return uuid.uuid4().hex


def new_task_id(title: str) -> str:
    """A readable, unique task id: ``task-<slug>-<4 hex>``."""
    try:
        slug = slugify(title, what="task title")[:32].strip("-")
    except ValueError:
        slug = "task"
    return f"task-{slug}-{uuid.uuid4().hex[:4]}"


def new_execution_id() -> str:
    return f"exec-{uuid.uuid4().hex[:12]}"


def new_grant_id() -> str:
    return f"grant-{uuid.uuid4().hex[:12]}"


def new_knowledge_id() -> str:
    return f"kb-{uuid.uuid4().hex[:10]}"


_SLUG_PATTERN = re.compile(r"[^a-z0-9-]+")


def slugify(name: str, *, what: str = "identifier") -> str:
    """Reduce a display name to a lowercase id token (e.g. ``Test User`` -> ``test-user``)."""
    slug = _SLUG_PATTERN.sub("-", name.strip().casefold()).strip("-")
    slug = re.sub(r"-{2,}", "-", slug)
    if not slug:
        raise ValueError(f"{what} needs at least one letter or digit")
    return slug[:64]
