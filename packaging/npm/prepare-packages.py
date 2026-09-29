#!/usr/bin/env python3
"""Stage the npm packages for one viva release (stdlib-only).

Reads the version and the two built macOS release binaries and produces a
ready-to-publish tree:

    dist/
      viva/                  (@zuohaisu/viva wrapper: bin/viva.js shim)
      viva-darwin-arm64/     (@zuohaisu/viva-darwin-arm64 + bin/viva)
      viva-darwin-x64/       (@zuohaisu/viva-darwin-x64 + bin/viva)

Versions come from the release tag (CI) or `cargo metadata` (local). The
templates under packaging/npm/ keep the real metadata; this script only
substitutes versions and copies binaries — nothing is downloaded.

Usage:
    python3 prepare-packages.py [--version 0.1.0]
        [--bin-arm64 PATH] [--bin-x64 PATH] [--out DIR]

Binaries default to the cargo release layout for the matching targets;
the version defaults to the workspace package version.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent


def cargo_version() -> str:
    out = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    meta = json.loads(out)
    for pkg in meta["packages"]:
        if pkg["name"] == "viva":
            return pkg["version"]
    raise SystemExit("package `viva` not found in cargo metadata")


def default_binary(target: str) -> Path:
    root = HERE.parent.parent.parent  # packaging/npm -> repo root
    return root / "target" / target / "release" / "viva"


def stage(version: str, src_pkg: Path, out_root: Path) -> Path:
    dst = out_root / src_pkg.name
    if dst.exists():
        shutil.rmtree(dst)
    shutil.copytree(src_pkg, dst)
    manifest_path = dst / "package.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    manifest["version"] = version
    if "optionalDependencies" in manifest:
        manifest["optionalDependencies"] = {
            name: version for name in manifest["optionalDependencies"]
        }
    manifest_path.write_text(
        json.dumps(manifest, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )
    return dst


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--version", default=None, help="release version (default: cargo)")
    ap.add_argument(
        "--bin-arm64",
        type=Path,
        default=default_binary("aarch64-apple-darwin"),
        help="path to the aarch64-apple-darwin release binary",
    )
    ap.add_argument(
        "--bin-x64",
        type=Path,
        default=default_binary("x86_64-apple-darwin"),
        help="path to the x86_64-apple-darwin release binary",
    )
    ap.add_argument("--out", type=Path, default=HERE / "dist", help="output directory")
    args = ap.parse_args()

    version = args.version or cargo_version()
    if not version.startswith(("0", "1", "2", "3", "4", "5", "6", "7", "8", "9")):
        raise SystemExit(f"version must be bare semver, got {version!r}")

    binaries = {
        "viva-darwin-arm64": args.bin_arm64,
        "viva-darwin-x64": args.bin_x64,
    }
    for pkg_name, binary in binaries.items():
        if not binary.is_file():
            raise SystemExit(f"missing binary for {pkg_name}: {binary}")

    if args.out.exists():
        shutil.rmtree(args.out)
    args.out.mkdir(parents=True)

    staged = {}
    for pkg_name, binary in binaries.items():
        dst = stage(version, HERE / pkg_name, args.out)
        shutil.copy2(binary, dst / "bin" / "viva")
        (dst / "bin" / "viva").chmod(0o755)
        staged[pkg_name] = dst
    staged["viva"] = stage(version, HERE / "viva", args.out)

    for name, dst in staged.items():
        print(f"prepared {name}@{version} -> {dst}")
    print("next: `npm publish` each platform package, then the wrapper")


if __name__ == "__main__":
    sys.exit(main())
