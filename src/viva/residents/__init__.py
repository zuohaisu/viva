"""Resident — a long-lived AI identity that persists across sessions.

A Resident is not a Worker and not a Cognitive Engine. Resident records are
pure identity data: nothing here (or anywhere in Viva core) branches on a
specific resident name.
"""

from viva.residents.registry import ResidentError, ResidentRegistry

__all__ = ["ResidentError", "ResidentRegistry"]
