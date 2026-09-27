"""Experience — append-only events a Resident went through.

Experience is NOT memory. There is no memory formation, promotion, or recall
in Viva yet; this journal only records what happened, redacted, in order.
Never present journal contents as "remembering".
"""

from viva.experience.journal import ExperienceError, ExperienceJournal

__all__ = ["ExperienceError", "ExperienceJournal"]
