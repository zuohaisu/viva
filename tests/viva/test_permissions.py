"""Phase-1 permission vocabulary contract."""

from __future__ import annotations

import pytest

from viva.core.errors import VivaError
from viva.permissions import PHASE1_GRANTS, Permission, level_for, require_user_initiated


def test_every_mutable_action_requires_user_initiation():
    for action, level in PHASE1_GRANTS.items():
        if level is Permission.ACT_WITH_APPROVAL:
            assert "autonom" not in action


def test_autonomous_and_protected_actions_are_forbidden():
    assert level_for("invoke_worker_autonomously") is Permission.FORBIDDEN
    assert level_for("push_protected_branch") is Permission.FORBIDDEN
    assert level_for("merge_pull_request") is Permission.FORBIDDEN
    assert level_for("modify_ticket_autopilot_state") is Permission.FORBIDDEN


def test_read_and_act_levels():
    assert level_for("observe_status") is Permission.READ
    assert level_for("invoke_worker") is Permission.ACT_WITH_APPROVAL
    assert level_for("register_workspace") is Permission.ACT_WITH_APPROVAL


def test_unknown_action_raises_instead_of_defaulting_open():
    with pytest.raises(VivaError, match="unknown Viva action"):
        level_for("delete_everything")


def test_require_user_initiated():
    require_user_initiated("user", action="invoke_worker")
    with pytest.raises(VivaError, match="requires user initiation"):
        require_user_initiated("agent", action="invoke_worker")
