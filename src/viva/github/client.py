"""GitHub linkage, read-only, through the proven ``gh`` CLI.

Viva does not implement a GitHub client: ``gh`` is already installed,
authenticated and maintained here, so it is the connector. This module only

* reads an issue, a branch's pull request, the PR's reviews and checks;
* writes the resulting evidence onto the task that owns it, so
  ``Task -> Repository -> Worktree/branch -> PR`` can be traced in one place.

Every call goes through :meth:`GitHubClient._read`, which refuses anything that
is not an explicitly read-only ``gh`` subcommand. Creating a PR, pushing,
approving or merging is *not* implemented here: those remain
repository-owner actions (``viva.permissions.authority``), and an agent may
never approve or merge its own PR.
"""

from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path
from typing import Any

from viva.core.errors import VivaError

READ_ONLY_COMMANDS = frozenset({"view", "list", "status", "checks", "diff"})
DEFAULT_TIMEOUT_SECONDS = 30


class GitHubError(VivaError):
    """The GitHub connector could not read what was asked for."""


class GitHubUnavailable(GitHubError):
    """``gh`` is missing or not authenticated."""


def gh_available() -> tuple[bool, str]:
    if shutil.which("gh") is None:
        return False, "gh is not on PATH"
    proc = subprocess.run(
        ["gh", "auth", "status"], capture_output=True, text=True, timeout=DEFAULT_TIMEOUT_SECONDS
    )
    if proc.returncode != 0:
        detail = (proc.stderr or proc.stdout or "").strip().splitlines()
        return False, detail[0] if detail else "gh is not authenticated"
    return True, ""


class GitHubClient:
    """Least-privilege read access to GitHub through ``gh``."""

    def __init__(self, *, timeout: float = DEFAULT_TIMEOUT_SECONDS):
        self.timeout = timeout

    # -- plumbing ------------------------------------------------------------

    def _read(self, *args: str, cwd: str | Path | None = None) -> Any:
        if len(args) < 2:
            raise GitHubError("no gh subcommand given")
        verb, subcommand = args[0], args[1]
        if subcommand not in READ_ONLY_COMMANDS:
            raise GitHubError(
                f"refusing non-read-only gh command {verb} {subcommand!r}; Viva's GitHub "
                f"connector only reads ({sorted(READ_ONLY_COMMANDS)})"
            )
        if any(token in {"-X", "--method", "--field", "-f"} for token in args):
            raise GitHubError(f"refusing gh {verb} {subcommand}: mutating flags are not allowed")
        available, reason = gh_available()
        if not available:
            raise GitHubUnavailable(reason)
        command = ["gh", *args]
        try:
            proc = subprocess.run(
                command, capture_output=True, text=True, timeout=self.timeout, cwd=cwd
            )
        except (OSError, subprocess.SubprocessError) as exc:
            raise GitHubError(f"gh {' '.join(args)} failed: {exc}") from exc
        if proc.returncode != 0:
            detail = (proc.stderr or proc.stdout or "").strip().splitlines()
            raise GitHubError(
                f"gh {' '.join(args)} exited {proc.returncode}: "
                f"{detail[0] if detail else 'unknown error'}"
            )
        return proc.stdout

    def _read_json(self, *args: str) -> Any:
        raw = self._read(*args)
        try:
            return json.loads(raw)
        except json.JSONDecodeError as exc:
            raise GitHubError(f"gh {' '.join(args)} did not return JSON") from exc

    # -- reads ---------------------------------------------------------------

    def issue(self, repo: str, number: int) -> dict[str, Any]:
        payload = self._read_json(
            "issue",
            "view",
            str(number),
            "--repo",
            repo,
            "--json",
            "number,title,state,url,labels,assignees,updatedAt",
        )
        return _issue_summary(payload)

    def pull_request(self, repo: str, number: int) -> dict[str, Any]:
        payload = self._read_json(
            "pr",
            "view",
            str(number),
            "--repo",
            repo,
            "--json",
            "number,title,state,url,headRefName,isDraft,reviews,statusCheckRollup,mergeable",
        )
        return _pr_summary(payload)

    def pull_request_for_branch(self, repo: str, branch: str) -> dict[str, Any] | None:
        payload = self._read_json(
            "pr",
            "list",
            "--repo",
            repo,
            "--head",
            branch,
            "--state",
            "all",
            "--json",
            "number,title,state,url,headRefName,isDraft",
            "--limit",
            "5",
        )
        if not payload:
            return None
        entry = payload[0]
        return {
            "number": entry.get("number"),
            "title": entry.get("title"),
            "state": entry.get("state"),
            "url": entry.get("url"),
            "head_ref": entry.get("headRefName"),
            "draft": bool(entry.get("isDraft")),
        }

    # -- evidence ------------------------------------------------------------

    def evidence(self, task: dict[str, Any]) -> dict[str, Any]:
        """Read-only snapshot tying a task to its issue, branch, PR and checks."""
        github = dict(task.get("github") or {})
        repo = github.get("repo")
        if not repo:
            raise GitHubError(
                f"task {task.get('id')!r} has no GitHub repository linked — run: "
                f"viva github link {task.get('id')} --repo <owner/name> --issue <n>"
            )
        evidence: dict[str, Any] = {
            "repo": repo,
            "issue": None,
            "branch": github.get("branch"),
            "pull_request": None,
            "checks": [],
            "reviews": [],
            "check_state": "UNKNOWN",
            "review_state": "UNKNOWN",
        }
        if github.get("issue"):
            evidence["issue"] = self.issue(str(repo), int(github["issue"]))
        branch = github.get("branch") or (task.get("work_location") or {}).get("branch")
        pr_number = github.get("pr")
        if pr_number is None and branch:
            found = self.pull_request_for_branch(str(repo), str(branch))
            pr_number = found.get("number") if found else None
        if pr_number is not None:
            pull = self.pull_request(str(repo), int(pr_number))
            evidence["pull_request"] = pull
            evidence["checks"] = pull["checks"]
            evidence["reviews"] = pull["reviews"]
            evidence["check_state"] = pull["check_state"]
            evidence["review_state"] = pull["review_state"]
            evidence["branch"] = evidence["branch"] or pull.get("head_ref")
        return evidence


def _issue_summary(payload: dict[str, Any]) -> dict[str, Any]:
    return {
        "number": payload.get("number"),
        "title": payload.get("title"),
        "state": payload.get("state"),
        "url": payload.get("url"),
        "labels": [label.get("name") for label in payload.get("labels") or []],
        "assignees": [user.get("login") for user in payload.get("assignees") or []],
        "updated_at": payload.get("updatedAt"),
    }


def _pr_summary(payload: dict[str, Any]) -> dict[str, Any]:
    reviews = payload.get("reviews") or []
    checks = payload.get("statusCheckRollup") or []
    normalised_checks = [
        {
            "name": check.get("name") or check.get("context"),
            "state": check.get("conclusion") or check.get("state"),
            "url": check.get("detailsUrl") or check.get("targetUrl"),
        }
        for check in checks
    ]
    states = {str(check["state"]).upper() for check in normalised_checks if check["state"]}
    if not states:
        check_state = "NONE"
    elif states <= {"SUCCESS", "NEUTRAL", "SKIPPED"}:
        check_state = "PASS"
    elif states & {"FAILURE", "ERROR", "CANCELLED", "TIMED_OUT", "ACTION_REQUIRED"}:
        check_state = "FAIL"
    else:
        check_state = "PENDING"
    review_states = [str(review.get("state") or "").upper() for review in reviews]
    if any(state == "CHANGES_REQUESTED" for state in review_states):
        review_state = "CHANGES_REQUESTED"
    elif any(state == "APPROVED" for state in review_states):
        review_state = "APPROVED"
    elif review_states:
        review_state = "COMMENTED"
    else:
        review_state = "NONE"
    return {
        "number": payload.get("number"),
        "title": payload.get("title"),
        "state": payload.get("state"),
        "url": payload.get("url"),
        "head_ref": payload.get("headRefName"),
        "draft": bool(payload.get("isDraft")),
        "mergeable": payload.get("mergeable"),
        "checks": normalised_checks,
        "check_state": check_state,
        "reviews": [
            {"author": (review.get("author") or {}).get("login"), "state": review.get("state")}
            for review in reviews
        ],
        "review_state": review_state,
    }
