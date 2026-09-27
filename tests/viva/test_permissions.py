"""Permission vocabulary, actor-aware authority, and the delegation boundary."""

from __future__ import annotations

import pytest

from viva.core.errors import VivaError
from viva.permissions import (
    PHASE1_GRANTS,
    Permission,
    level_for,
    require_invocation_authority,
    require_user_initiated,
    require_user_authorization,
)
from viva.permissions.authority import DeliveryAuthorizationError, delivery_decision


def test_autonomous_and_self_authorizing_actions_are_forbidden():
    for action in (
        "invoke_worker_autonomously",
        "push_protected_branch",
        "merge_pull_request",
        "approve_pull_request",
        "self_authorize",
    ):
        assert level_for(action) is Permission.FORBIDDEN


def test_delegated_actions_exist_and_require_approval():
    for action in ("dispatch_delegated", "read_delegated", "stop_delegated", "delegate_grant"):
        assert level_for(action) is Permission.ACT_WITH_APPROVAL


def test_read_and_act_levels():
    assert level_for("observe_status") is Permission.READ
    assert level_for("invoke_worker") is Permission.ACT_WITH_APPROVAL
    assert level_for("register_project") is Permission.ACT_WITH_APPROVAL
    assert level_for("read_github") is Permission.READ


def test_no_action_is_silently_autonomous():
    assert all(level is not Permission.ACT_AUTONOMOUSLY for level in PHASE1_GRANTS.values())


def test_unknown_action_raises_instead_of_defaulting_open():
    with pytest.raises(VivaError, match="unknown Viva action"):
        level_for("delete_everything")


def test_require_user_initiated():
    require_user_initiated("user", action="invoke_worker")
    with pytest.raises(VivaError, match="requires user initiation"):
        require_user_initiated("agent", action="invoke_worker")


def test_invocation_authority_keeps_the_real_source():
    grant = {"id": "grant-1"}
    assert require_invocation_authority(
        {"kind": "user", "id": "haisu"}, action="invoke_worker"
    ) == {"kind": "user", "id": "haisu"}
    assert require_invocation_authority(
        {"kind": "worker", "id": "exec-1"}, action="dispatch_delegated", grant=grant
    ) == {"kind": "worker", "id": "exec-1", "grant_id": "grant-1"}
    with pytest.raises(VivaError, match="cannot claim user origin"):
        require_invocation_authority(
            {"kind": "user", "id": "haisu"}, action="dispatch_delegated", grant=grant
        )
    with pytest.raises(VivaError, match="FORBIDDEN without a live grant"):
        require_invocation_authority({"kind": "worker", "id": "exec-1"}, action="dispatch_delegated")
    with pytest.raises(VivaError, match="source kind"):
        require_invocation_authority({"kind": "ghost", "id": "x"}, action="dispatch_delegated")


# -- owner authorization (remote / protected actions) -------------------------


def test_a_remote_action_without_owner_authorization_is_refused():
    with pytest.raises(DeliveryAuthorizationError, match="requires explicit repository-owner"):
        require_user_authorization(None, action="push_feature_branch")
    with pytest.raises(DeliveryAuthorizationError, match="not approved"):
        require_user_authorization({"approved": False}, action="merge")


def test_owner_authorization_returns_an_audit_record():
    audit = require_user_authorization(
        {
            "approved": True,
            "actor_type": "repository_owner",
            "actor": "hzuo",
            "action": "push_feature_branch",
            "approved_at": "2026-09-27T10:00:00Z",
            "reason": "ship the office loop",
        },
        action="push_feature_branch",
    )
    assert audit["actor"] == "hzuo" and audit["action"] == "push_feature_branch"


def test_an_agent_cannot_authorize_a_remote_action():
    with pytest.raises(DeliveryAuthorizationError, match="must be authorized by a repository_owner"):
        require_user_authorization(
            {
                "approved": True,
                "actor_type": "agent",
                "actor": "deven",
                "action": "merge",
                "approved_at": "2026-09-27T10:00:00Z",
                "reason": "I decided",
            },
            action="merge",
        )


def test_merge_always_needs_its_own_authorization():
    clean = delivery_decision(deterministic_status="PASS", qa_status="PASS", visual_status="PASS")
    assert clean["status"] == "READY_FOR_REVIEW"
    assert clean["merge_allowed"] is False
    assert clean["draft_pr_allowed"] is True

    pending = delivery_decision(
        deterministic_status="PASS", qa_status="PENDING", visual_status="PENDING"
    )
    assert pending["status"] == "HUMAN_VISUAL_REVIEW_PENDING"
    assert "QA_PENDING" in pending["warnings"]
    assert pending["merge_allowed"] is False

    qa_only = delivery_decision(
        deterministic_status="PASS", qa_status="PENDING", visual_status="NOT_REQUIRED"
    )
    assert qa_only["status"] == "QA_PENDING"

    blocked = delivery_decision(
        deterministic_status="PASS",
        qa_status="PASS",
        visual_status="PASS",
        technical_blocker="network unreachable",
    )
    assert blocked["status"] == "TECHNICAL_BLOCKED"
    assert blocked["draft_pr_allowed"] is False
