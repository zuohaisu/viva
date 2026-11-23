#!/usr/bin/env python3
"""Prepare and verify the notice payload shared by the release and npm archives."""

from __future__ import annotations

import argparse
import re
import shutil
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ROOT_FILES = ("LICENSE", "NOTICE", "SBOM.txt", "THIRD_PARTY_NOTICES.md")
EXTRA_LICENSES = {
    "herdr-LICENSE": "upstream/herdr/LICENSE",
    "ghostty-LICENSE": "upstream/herdr/vendor/libghostty-vt/LICENSE",
    "portable-pty-LICENSE.md": "upstream/herdr/vendor/portable-pty/LICENSE.md",
    "cargo-notice-payload-baseline.json": "research/herdr-190/license-closure/cargo-notice-payload.json",
}


def create(directory: Path, repository: str, revision: str, target: str) -> None:
    if not re.fullmatch(r"[\w.-]+/[\w.-]+", repository) or not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("Expected a GitHub owner/repo and a full source commit")
    source = f"https://github.com/{repository}/tree/{revision}"
    directory.mkdir(parents=True, exist_ok=True)
    shutil.copytree(ROOT / "licenses", directory / "licenses")
    for name, path in EXTRA_LICENSES.items():
        shutil.copyfile(ROOT / path, directory / "licenses" / name)
    for name in ("LICENSE", "THIRD_PARTY_NOTICES.md"):
        shutil.copyfile(ROOT / name, directory / name)
    (directory / "NOTICE").write_text(
        "Viva bundles modified Herdr 0.9.3 (Apache-2.0, https://github.com/herdrdev/herdr),\n"
        "including changes for Viva runtime embedding, domain integration, and authority handling.\n"
        "The embedded libghostty-vt is MIT-licensed; separately licensed files\n"
        "and the patched MIT portable-pty are documented in THIRD_PARTY_NOTICES.md.\n"
        "This is not an original Herdr distribution or an upstream endorsement.\n\n"
        f"Exact build source (including modifications and original notices): {source}\n"
        "See licenses/ for full retained texts and supplemental notices.\n",
        encoding="utf-8",
    )
    (directory / "SBOM.txt").write_text(
        f"Viva release component summary ({target})\n"
        f"Source: {source}\n\n"
        "Viva: MIT\n"
        "Herdr 0.9.3: Apache-2.0 (modified, embedded)\n"
        "libghostty-vt: MIT (see third-party notices for file-level exceptions)\n"
        "portable-pty: MIT (patched)\n\n"
        f"Resolved Cargo inputs and checksums: {source}/Cargo.lock\n"
        "licenses/cargo-notice-payload-baseline.json preserves full Cargo notice\n"
        "texts captured for the original #190 Intel baseline, not this build's\n"
        "linked dependency inventory. This summary is not a complete linked SBOM.\n",
        encoding="utf-8",
    )
    material_files(directory)


def material_files(directory: Path) -> list[Path]:
    required = [Path(name) for name in ROOT_FILES]
    required.extend(Path("licenses") / name for name in EXTRA_LICENSES)
    required.extend(
        path.relative_to(ROOT) for path in (ROOT / "licenses").rglob("*") if path.is_file()
    )
    for relative in required:
        path = directory / relative
        if not path.is_file() or path.stat().st_size == 0:
            raise SystemExit(f"Required release material is missing or empty: {path}")
    return [Path(name) for name in ROOT_FILES] + sorted(
        path.relative_to(directory) for path in (directory / "licenses").rglob("*") if path.is_file()
    )


def verify_pack(release: Path, archive: Path) -> None:
    with tarfile.open(archive, "r:gz") as packed:
        for relative in material_files(release):
            member = f"package/{relative.as_posix()}"
            try:
                entry = packed.getmember(member)
            except KeyError:
                raise SystemExit(f"Missing from {archive.name}: {member}") from None
            if not entry.isfile():
                raise SystemExit(f"Not a regular file in {archive.name}: {member}")
            with packed.extractfile(entry) as content:
                if content.read() != (release / relative).read_bytes():
                    raise SystemExit(f"Content differs in {archive.name}: {member}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    stage = commands.add_parser("create")
    stage.add_argument("directory", type=Path)
    stage.add_argument("repository")
    stage.add_argument("revision")
    stage.add_argument("target")
    verify = commands.add_parser("verify-pack")
    verify.add_argument("release", type=Path)
    verify.add_argument("archive", type=Path)
    args = parser.parse_args()
    if args.command == "create":
        create(args.directory, args.repository, args.revision, args.target)
    else:
        verify_pack(args.release, args.archive)


if __name__ == "__main__":
    main()
