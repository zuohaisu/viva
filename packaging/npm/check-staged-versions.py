#!/usr/bin/env python3
"""Fail unless every staged npm manifest uses the requested version.

The release workflow calls this after prepare-packages.py so a template
placeholder cannot be published.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
PACKAGES = ("viva-darwin-arm64", "viva-darwin-x64", "viva")
WRAPPER_DEPS = ("@zuohaisu/viva-darwin-arm64", "@zuohaisu/viva-darwin-x64")


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {Path(sys.argv[0]).name} VERSION")
    version = sys.argv[1]
    root = HERE / "dist"
    for name in PACKAGES:
        manifest = json.loads((root / name / "package.json").read_text(encoding="utf-8"))
        if manifest.get("version") != version:
            raise SystemExit(
                f"{name} staged {manifest.get('version')}, expected {version}"
            )
    wrapper = json.loads((root / "viva" / "package.json").read_text(encoding="utf-8"))
    deps = wrapper.get("optionalDependencies", {})
    for name in WRAPPER_DEPS:
        if deps.get(name) != version:
            raise SystemExit(
                f"wrapper depends on {name}@{deps.get(name)}, expected {version}"
            )


if __name__ == "__main__":
    main()
