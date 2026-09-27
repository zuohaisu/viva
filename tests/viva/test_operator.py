"""The operator is configuration, never an identity Viva ships with.

Viva used to print a fixed name on the TUI prompt while the audit trail
attributed actions to the same name written into source. Both now resolve from
one configured principal, so these tests hold the two halves together.
"""

from __future__ import annotations

import pytest

from viva.context import VivaContext
from viva.core.errors import VivaError

from .conftest import make_member, make_task


def test_unset_operator_stays_neutral(context):
    assert context.operator() is None
    assert context.operator_label() == "you"
    assert context.user_source() == {"kind": "user", "id": "user"}


def test_configured_operator_drives_label_and_attribution(context):
    assert context.set_operator("Haisu Zuo") == "Haisu Zuo"
    assert context.operator_label() == "Haisu Zuo"
    assert context.user_source() == {"kind": "user", "id": "haisu-zuo"}
    # A fresh handle on the same home resolves the same principal.
    reloaded = VivaContext(context.home)
    assert reloaded.operator_label() == "Haisu Zuo"
    assert reloaded.user_source() == {"kind": "user", "id": "haisu-zuo"}


def test_environment_supplies_the_operator(home, monkeypatch):
    monkeypatch.setenv("VIVA_OPERATOR", "Zuo Hai")
    context = VivaContext(home)
    assert context.operator_label() == "Zuo Hai"
    assert context.user_source()["id"] == "zuo-hai"
    # Recorded state wins over the environment.
    context.set_operator("Haisu Zuo")
    assert context.operator() == "Haisu Zuo"


def test_clearing_returns_to_neutral_and_journals_both_ways(context):
    context.set_operator("Haisu Zuo")
    assert context.set_operator(None) is None
    assert context.operator_label() == "you"
    types = [event["event_type"] for event in context.journal.read()]
    assert types.count("operator.set") == 1
    assert types.count("operator.cleared") == 1


@pytest.mark.parametrize("blank", [" ", "\t", "!!!"])
def test_operator_without_a_letter_or_digit_is_rejected(context, blank):
    with pytest.raises(VivaError):
        context.set_operator(blank)
    assert context.operator() is None


def test_a_bad_stored_operator_fails_loudly_instead_of_silently(home):
    context = VivaContext(home)
    context.runtime.set_operator("   !!!   ")
    with pytest.raises(VivaError):
        context.user_source()


def test_grant_attribution_follows_the_operator_not_its_own_default(office, git_repo):
    office.set_operator("Haisu Zuo")
    coordinator = make_member(office, "Samuel", role="coordinator")
    task = make_task(office, "Deliver the thing", kind="delivery", repository=git_repo)
    grant = office.office.grant(
        member=coordinator["id"],
        task=task["id"],
        actions=["dispatch"],
        mode_max="read_only",
        reason="coordinating work",
    )
    assert grant["source"] == office.user_source() == {"kind": "user", "id": "haisu-zuo"}
