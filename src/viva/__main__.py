"""Allow ``python -m viva`` (used by workers that call back into Viva)."""

from __future__ import annotations

import sys

from viva.cli import main

if __name__ == "__main__":
    sys.exit(main())
