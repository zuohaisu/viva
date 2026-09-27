"""GitHub: read-only linkage through gh, and the traceability it produces."""

from __future__ import annotations

import json

import pytest

from viva.github import GitHubClient, GitHubError

from .conftest import make_task

ISSUE_JSON = {
    "number": 150,
    "title": "Add the office loop",
    "state": "OPEN",
    "url": "https://github.com/zuohaisu/viva/issues/150",
    "labels": [{"name": "feature"}],
    "assignees": [{"login": "zuohaisu"}],
    "updatedAt": "2026-09-27T00:00:00Z",
}

PR_JSON = {
    "number": 42,
    "title": "feat: office loop",
    "state": "OPEN",
    "url": "https://github.com/zuohaisu/viva/pull/42",
    "headRefName": "task/task-abc",
    "isDraft": False,
    "mergeable": "MERGEABLE",
    "reviews": [{"author": {"login": "alice"}, "state": "APPROVED"}],
    "statusCheckRollup": [
        {"name": "tests", "conclusion": "SUCCESS", "detailsUrl": "https://ci/1"},
        {"name": "lint", "conclusion": "FAILURE", "detailsUrl": "https://ci/2"},
    ],
}

PR_LIST_JSON = [
    {
        "number": 42,
        "title": "feat: office loop",
        "state": "OPEN",
        "url": "https://github.com/zuohaisu/viva/pull/42",
        "headRefName": "task/task-abc",
        "isDraft": False,
    }
]

FAKE_GH = """#!/bin/sh
# A real gh-shaped process: records its argv and prints canned read-only output.
echo "$@" >> "$GH_LOG"
case "$1 $2" in
  "issue view") echo '%s' ;;
  "pr list") echo '%s' ;;
  "pr view") echo '%s' ;;
  "auth status") echo "logged in"; exit 0 ;;
  *) echo "unsupported: $*" >&2; exit 1 ;;
esac
"""


@pytest.fixture
def fake_gh(fake_bin, monkeypatch, tmp_path):
    log = tmp_path / "gh.log"
    log.write_text("", encoding="utf-8")
    script = fake_bin / "gh"
    script.write_text(
        FAKE_GH
        % (
            json.dumps(ISSUE_JSON),
            json.dumps(PR_LIST_JSON),
            json.dumps(PR_JSON),
        ),
        encoding="utf-8",
    )
    script.chmod(0o755)
    monkeypatch.setenv("GH_LOG", str(log))
    return log


def test_evidence_links_task_to_issue_branch_pr_checks_and_reviews(office, git_repo, fake_gh):
    task = make_task(office, "Office loop", kind="delivery", repository=git_repo)
    office.tasks.link_github(task["id"], repo="zuohaisu/viva", issue=150, branch="task/task-abc")

    evidence = office.github.evidence(office.tasks.require(task["id"]))

    assert evidence["issue"]["number"] == 150
    assert evidence["pull_request"]["number"] == 42
    assert evidence["pull_request"]["head_ref"] == "task/task-abc"
    assert evidence["check_state"] == "FAIL"
    assert evidence["review_state"] == "APPROVED"
    assert {check["name"] for check in evidence["checks"]} == {"tests", "lint"}
    assert evidence["branch"] == "task/task-abc"


def test_only_read_only_gh_commands_are_issued(office, git_repo, fake_gh):
    task = make_task(office, "Office loop", kind="delivery", repository=git_repo)
    office.tasks.link_github(task["id"], repo="zuohaisu/viva", issue=150, branch="task/task-abc")
    office.github.evidence(office.tasks.require(task["id"]))

    issued = [line.split() for line in fake_gh.read_text(encoding="utf-8").splitlines() if line.strip()]
    assert issued
    for argv in issued:
        assert argv[0] in {"issue", "pr", "auth"}
        assert argv[1] in {"view", "list", "status"}
    assert not any("merge" in argv or "close" in argv or "create" in argv for argv in issued)


def test_non_read_only_commands_are_refused_before_gh_runs(tmp_path):
    client = GitHubClient()
    for argv in (("pr", "merge", "42"), ("pr", "create"), ("issue", "close", "1"), ("api", "graphql")):
        with pytest.raises(GitHubError, match="refusing non-read-only gh command"):
            client._read(*argv)


def test_a_task_without_a_repository_link_says_so(office, git_repo):
    task = make_task(office, "Unlinked", kind="research", repository=git_repo)
    with pytest.raises(GitHubError, match="no GitHub repository linked"):
        office.github.evidence(task)


def test_missing_gh_is_reported_not_faked(monkeypatch):
    monkeypatch.setenv("PATH", "/nonexistent-bin")
    with pytest.raises(GitHubError, match="gh is not on PATH"):
        GitHubClient()._read("pr", "view", "1", "--repo", "a/b")
