"""Local git worktree isolation, reused from the retired delivery subsystem.

The argument-only Git subset, the protected-branch guard, and the
owner-authorized push behaviour are unchanged: they are the capability Viva
still needs for "different tasks get different writable work locations".

Viva adds no second worktree system on top; discovery (``discovery.py``) and
allocation (``location.py``) are thin readers over this service.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

from viva.permissions.authority import require_user_authorization

PROTECTED_BRANCHES = frozenset({"main", "master"})


class GitWorktreeError(RuntimeError):
    """A deterministic Git precondition or operation failed."""


def is_protected_branch(branch: str) -> bool:
    """Return true only for the branch names that must never be work targets."""
    return branch.removeprefix("refs/heads/").casefold() in PROTECTED_BRANCHES


class GitWorktreeService:
    """Run a deliberately small, argument-only subset of local Git commands."""

    def __init__(self, repository: str | Path):
        repository_path = Path(repository).expanduser().resolve()
        if not repository_path.is_dir():
            raise GitWorktreeError(f"repository does not exist: {repository_path}")
        self.repository = repository_path
        # This is both an early configuration check and the canonical path used below.
        self.repository = Path(self._run("rev-parse", "--show-toplevel")).resolve()

    def _run(self, *args: str, cwd: Path | None = None, check: bool = True) -> str:
        proc = subprocess.run(
            ["git", *args],
            cwd=cwd or self.repository,
            capture_output=True,
            text=True,
        )
        if check and proc.returncode:
            detail = proc.stderr.strip() or proc.stdout.strip() or "unknown git error"
            raise GitWorktreeError(f"git {' '.join(args)} failed: {detail}")
        return proc.stdout.strip()

    def head_sha(self) -> str:
        return self._run("rev-parse", "HEAD")

    def current_branch(self) -> str:
        branch = self._run("symbolic-ref", "--quiet", "--short", "HEAD")
        if not branch:
            raise GitWorktreeError("repository is detached; a named base branch is required")
        return branch

    def default_base(self) -> str:
        """The remote default branch when known, else the current commit.

        A task worktree must never silently start from a stale local branch, so
        the remote-tracking default wins whenever it exists.
        """
        for candidate in ("refs/remotes/origin/HEAD", "refs/remotes/origin/main"):
            proc = subprocess.run(
                ["git", "rev-parse", "--verify", "--quiet", candidate],
                cwd=self.repository,
                capture_output=True,
                text=True,
            )
            if proc.returncode == 0 and proc.stdout.strip():
                sha = self._run("rev-parse", f"{candidate}^{{commit}}")
                remote, _, branch = candidate.removeprefix("refs/remotes/").partition("/")
                if candidate.endswith("/HEAD"):
                    branch = self._run(
                        "symbolic-ref", "--quiet", "--short", candidate
                    ).removeprefix(f"{remote}/") or branch
                return f"{sha} {branch}".strip()
        return f"{self.head_sha()} {self.current_branch()}"

    def create_worktree(self, worktree: str | Path, branch: str, base_sha: str) -> None:
        target = Path(worktree).expanduser().resolve()
        if target.exists():
            raise GitWorktreeError(f"refusing to reuse an existing worktree target: {target}")
        if is_protected_branch(branch):
            raise GitWorktreeError(f"refusing to create a protected branch: {branch}")
        target.parent.mkdir(parents=True, exist_ok=True)
        self._run("worktree", "add", "-b", branch, str(target), base_sha)

    def worktree_branch(self, worktree: str | Path) -> str:
        target = Path(worktree).expanduser().resolve()
        if not target.is_dir():
            raise GitWorktreeError(f"worktree does not exist: {target}")
        return self._run("symbolic-ref", "--quiet", "--short", "HEAD", cwd=target)

    def branch_exists(self, branch: str) -> bool:
        proc = subprocess.run(
            ["git", "show-ref", "--verify", "--quiet", f"refs/heads/{branch}"],
            cwd=self.repository,
            capture_output=True,
            text=True,
        )
        return proc.returncode == 0

    def is_branch_merged(self, branch: str, base_branch: str) -> bool | None:
        """Return true/false, or ``None`` when Git cannot establish the relation."""
        proc = subprocess.run(
            ["git", "merge-base", "--is-ancestor", branch, base_branch],
            cwd=self.repository,
            capture_output=True,
            text=True,
        )
        if proc.returncode == 0:
            return True
        if proc.returncode == 1:
            return False
        return None

    def remove_worktree(self, worktree: str | Path) -> None:
        self._run("worktree", "remove", "--force", str(Path(worktree).expanduser().resolve()))

    def delete_unmerged_branch(self, branch: str) -> None:
        if is_protected_branch(branch):
            raise GitWorktreeError(f"refusing to delete a protected branch: {branch}")
        self._run("branch", "-D", branch)

    def push_feature_branch(
        self,
        branch: str,
        *,
        remote: str = "origin",
        authorization: dict | None = None,
    ) -> dict:
        """Push one existing feature branch after explicit owner authorization."""
        audit = require_user_authorization(authorization, action="push_feature_branch")
        if not remote or remote.startswith("-"):
            raise GitWorktreeError("remote must be a named Git remote")
        if not branch or is_protected_branch(branch):
            raise GitWorktreeError("refusing to push a protected or empty branch")
        if not self.branch_exists(branch):
            raise GitWorktreeError(f"local feature branch does not exist: {branch}")
        destination = f"refs/heads/{branch}:refs/heads/{branch}"
        self._run("push", "--set-upstream", remote, destination)
        return {
            "status": "PUSHED",
            "remote": remote,
            "branch": branch,
            "authorization": audit,
        }
