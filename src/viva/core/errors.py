"""Shared Viva error types."""

from __future__ import annotations


class VivaError(RuntimeError):
    """Base class for every deliberate, user-visible Viva failure."""
