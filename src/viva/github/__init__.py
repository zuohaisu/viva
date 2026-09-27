"""GitHub connector — read-only linkage through the ``gh`` CLI."""

from viva.github.client import (
    GitHubClient,
    GitHubError,
    GitHubUnavailable,
    gh_available,
)

__all__ = ["GitHubClient", "GitHubError", "GitHubUnavailable", "gh_available"]
