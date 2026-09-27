"""Viva's permission vocabulary and the AI Office's authority rules.

This is a contract plus two enforcement points, not a policy engine.

    READ                observe only
    PROPOSE             present a plan or diff; no mutation
    ACT_WITH_APPROVAL   act after the user — or a live, user-granted scope —
                        explicitly authorized that action
    ACT_AUTONOMOUSLY    act inside a pre-authorized boundary with no per-action
                        approval (never granted in this round)
    FORBIDDEN           never allowed for that actor

Two rules carry the whole product boundary:

1. **A request is never relabelled.** ``require_invocation_authority`` takes the
   request's real source and refuses a worker-originated request that has no
   grant: it may not be recorded, or reasoned about, as if Haisu had asked.
2. **Capability is not authority.** Nothing an AI member or worker does grants
   itself repository-owner power; see :mod:`viva.permissions.authority` and
   :mod:`viva.permissions.grants`.
"""

from __future__ import annotations

from enum import Enum
from typing import Any, Mapping

from viva.core.errors import VivaError
from viva.permissions.authority import (
    OWNER_ACTIONS,
    DeliveryAuthorizationError,
    delivery_decision,
    require_user_authorization,
)
from viva.permissions.grants import (
    GRANT_ACTIONS,
    MODE_ORDER,
    GrantError,
    GrantRegistry,
)


class Permission(str, Enum):
    READ = "READ"
    PROPOSE = "PROPOSE"
    ACT_WITH_APPROVAL = "ACT_WITH_APPROVAL"
    ACT_AUTONOMOUSLY = "ACT_AUTONOMOUSLY"
    FORBIDDEN = "FORBIDDEN"


PHASE1_GRANTS: dict[str, Permission] = {
    # observation
    "observe_status": Permission.READ,
    "read_experience": Permission.READ,
    "probe_worker": Permission.READ,
    "list_worktrees": Permission.READ,
    "read_task": Permission.READ,
    "read_execution": Permission.READ,
    "read_knowledge": Permission.READ,
    "read_github": Permission.READ,
    # user-issued mutations
    "register_workspace": Permission.ACT_WITH_APPROVAL,
    "register_project": Permission.ACT_WITH_APPROVAL,
    "select_resident": Permission.ACT_WITH_APPROVAL,
    "select_workspace": Permission.ACT_WITH_APPROVAL,
    "create_resident": Permission.ACT_WITH_APPROVAL,
    "configure_member": Permission.ACT_WITH_APPROVAL,
    "create_task": Permission.ACT_WITH_APPROVAL,
    "invoke_worker": Permission.ACT_WITH_APPROVAL,
    "create_grant": Permission.ACT_WITH_APPROVAL,
    "link_github_issue": Permission.ACT_WITH_APPROVAL,
    "curate_knowledge": Permission.ACT_WITH_APPROVAL,
    # delegated mutations: allowed only while a live grant covers them
    "dispatch_delegated": Permission.ACT_WITH_APPROVAL,
    "read_delegated": Permission.ACT_WITH_APPROVAL,
    "stop_delegated": Permission.ACT_WITH_APPROVAL,
    "delegate_grant": Permission.ACT_WITH_APPROVAL,
    # never
    "invoke_worker_autonomously": Permission.FORBIDDEN,
    "push_protected_branch": Permission.FORBIDDEN,
    "merge_pull_request": Permission.FORBIDDEN,
    "approve_pull_request": Permission.FORBIDDEN,
    "self_authorize": Permission.FORBIDDEN,
}


def level_for(action: str) -> Permission:
    """Return the permission level for *action*."""
    try:
        return PHASE1_GRANTS[action]
    except KeyError:
        raise VivaError(f"unknown Viva action: {action!r}") from None


def require_user_initiated(initiated_by: str, *, action: str) -> None:
    """Enforce that a mutation was explicitly issued by the user."""
    if initiated_by != "user":
        raise VivaError(
            f"{action} requires user initiation; initiated_by={initiated_by!r} "
            "must carry a live grant instead"
        )


def require_invocation_authority(
    source: Mapping[str, Any] | None,
    *,
    action: str,
    grant: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Authorize one worker invocation from its *real* source.

    ``source`` is ``{"kind": "user", "id": ...}`` for something Haisu issued, or
    ``{"kind": "worker", "id": <execution id>}`` for something a coordinating
    worker issued against a grant. A worker-originated request without a grant
    is FORBIDDEN and, crucially, is never rewritten to look user-originated.
    """
    if not isinstance(source, Mapping):
        raise VivaError(f"{action} requires an explicit request source")
    kind = str(source.get("kind") or "").strip()
    identifier = str(source.get("id") or "").strip()
    if kind not in {"user", "worker"}:
        raise VivaError(f"{action} source kind must be 'user' or 'worker', got {kind!r}")
    if not identifier:
        raise VivaError(f"{action} source id is required")
    if kind == "user":
        if grant is not None:
            raise VivaError(
                f"{action} cannot claim user origin while spending grant {grant.get('id')!r}"
            )
        return {"kind": "user", "id": identifier}
    if grant is None:
        raise VivaError(f"{action} from worker {identifier!r} is FORBIDDEN without a live grant")
    return {"kind": "worker", "id": identifier, "grant_id": grant.get("id")}


__all__ = [
    "DeliveryAuthorizationError",
    "GRANT_ACTIONS",
    "GrantError",
    "GrantRegistry",
    "MODE_ORDER",
    "OWNER_ACTIONS",
    "PHASE1_GRANTS",
    "Permission",
    "delivery_decision",
    "level_for",
    "require_invocation_authority",
    "require_user_authorization",
    "require_user_initiated",
]
