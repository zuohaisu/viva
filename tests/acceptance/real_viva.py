"""Locate the REAL `viva` binary for the acceptance tests (issue #66).

These tests used to `pytest.skip(...)` when the binary was missing. CI did not
build it, so two of them were skipped on every run, and a skipped test is not
evidence: the suite stayed green while proving nothing about the product.
A missing binary is now a failure with the command that fixes it.

The binary comes from `VIVA_BIN`, else `target/debug/viva` in this checkout:
the same convention as the Pi extension's contract test.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]


def real_viva_binary() -> Path:
    """The real binary, or a test FAILURE (never a skip) when it is missing."""
    configured = os.environ.get("VIVA_BIN")
    binary = Path(configured).resolve() if configured else REPO_ROOT / "target" / "debug" / "viva"
    if not binary.is_file():
        pytest.fail(
            f"the real viva binary was not found at {binary}. Build it with "
            "`cargo build -p viva` or point VIVA_BIN at one. This test never skips: "
            "a skipped test would re-create the green-suite / unverified-product "
            "failure it exists to catch.",
            pytrace=False,
        )
    return binary
