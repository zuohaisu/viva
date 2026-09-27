"""Resident — one persistent AI member of Haisu's office.

A Resident is not a Role, not a Cognitive Engine, and not a Worker:

    Resident  who persists (identity, role, history, knowledge)
    Role      what it is for (configuration data)
    Engine    which model currently serves it (replaceable)
    Tool      which CLI executes it (replaceable; a Worker record)
    Execution one run, with its own task, work location and authorization

Viva never branches on a member's name; Samuel/Deven/Alice are records a user
creates.
"""

from viva.residents.engines import EngineCatalog, EngineError
from viva.residents.registry import (
    WORK_MODES,
    InvocationUnavailable,
    ResidentError,
    ResidentRegistry,
)
from viva.residents.roles import RoleCatalog, RoleError

__all__ = [
    "WORK_MODES",
    "EngineCatalog",
    "EngineError",
    "InvocationUnavailable",
    "ResidentError",
    "ResidentRegistry",
    "RoleCatalog",
    "RoleError",
]
