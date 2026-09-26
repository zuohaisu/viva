"""Viva's permission vocabulary and Phase-1 grants.

This is a contract, not a policy engine. Five levels:

    READ                observe only
    PROPOSE             present a plan or diff; no mutation
    ACT_WITH_APPROVAL   act after the user explicitly issues that action
    ACT_AUTONOMOUSLY    act inside a pre-authorized boundary (never granted in Phase 1)
    FORBIDDEN           never allowed for that actor

Phase 1 semantics: Viva only acts when the user issues the command, so every
mutable action maps to ACT_WITH_APPROVAL. Workers invoked through Viva are
user-initiated by definition and never gain repository-owner authority; the
delivery subsystem's owner-authorization rules (delivery_policy) remain the
authority for anything remote or protected.
"""

from __future__ import annotations

from enum import Enum

from viva.core.errors import VivaError


class Permission(str, Enum):
    READ = "READ"
    PROPOSE = "PROPOSE"
    ACT_WITH_APPROVAL = "ACT_WITH_APPROVAL"
    ACT_AUTONOMOUSLY = "ACT_AUTONOMOUSLY"
    FORBIDDEN = "FORBIDDEN"


PHASE1_GRANTS: dict[str, Permission] = {
    "observe_status": Permission.READ,
    "read_experience": Permission.READ,
    "probe_worker": Permission.READ,
    "list_worktrees": Permission.READ,
    "register_workspace": Permission.ACT_WITH_APPROVAL,
    "select_resident": Permission.ACT_WITH_APPROVAL,
    "select_workspace": Permission.ACT_WITH_APPROVAL,
    "create_resident": Permission.ACT_WITH_APPROVAL,
    "invoke_worker": Permission.ACT_WITH_APPROVAL,
    "invoke_worker_autonomously": Permission.FORBIDDEN,
    "push_protected_branch": Permission.FORBIDDEN,
    "merge_pull_request": Permission.FORBIDDEN,
    "modify_ticket_autopilot_state": Permission.FORBIDDEN,
}


def level_for(action: str) -> Permission:
    """Return the Phase-1 permission level for *action*."""
    try:
        return PHASE1_GRANTS[action]
    except KeyError:
        raise VivaError(f"unknown Viva action: {action!r}") from None


def require_user_initiated(initiated_by: str, *, action: str) -> None:
    """Enforce that *action* was explicitly issued by the user."""
    if initiated_by != "user":
        raise VivaError(
            f"{action} requires user initiation; initiated_by={initiated_by!r} "
            "is FORBIDDEN in Phase 1"
        )
