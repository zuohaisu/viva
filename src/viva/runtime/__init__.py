"""Runtime state: what survives a session, and what identifies a session.

``runtime/state.json`` holds the persistent pointers (current resident,
current workspace, last session). Session ids themselves are transient
labels; when a session dies the pointers remain.
"""

from viva.runtime.state import RuntimeState

__all__ = ["RuntimeState"]
