#!/usr/bin/env python3
"""Read-only evidence inventory; NOT a license classifier or clearance gate.

Reuse Git's tracked-file inventory and Cargo's official metadata. This fills
only the reproducibility gap in #190; humans must resolve provenance/scope.
Run from Viva's root, with metadata from the nested workspace:
  (cd upstream/herdr && cargo metadata --locked --format-version 1) > /tmp/metadata.json
  python3 tooling/herdr-audit/inventory.py /tmp/metadata.json > /tmp/inventory.json
Every committed blob gets a path/hash/kind record, including binary files outside
assets/. kind is only a UTF-8/NUL heuristic, never a license classification.
viva_commit is the running checkout's HEAD; it changes after any commit. Compare
source_tree and payload fields when reproducing from a different Viva commit.
No build, installer, updater, or user session is started.
"""

import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
PREFIX = "upstream/herdr/"
SIGNAL = re.compile(
    r"copyright|spdx-license|licensed? under|lesser general public|"
    r"permission (?:is hereby|to use)|redistribution and use|"
    r"derived (?:from|in part)|based on.{0,30}(?:source|chromium)|"
    r"unicode.org/terms|BSD-style license|distributed under GPL",
    re.IGNORECASE,
)


def inspect_file(path, content):
    """Return factual signals only; absence of a match says nothing about rights."""
    try:
        text = content.decode("utf-8")
        kind = "binary" if b"\0" in content else "text"
    except UnicodeDecodeError:
        text = content.decode("utf-8", errors="replace")
        kind = "binary"
    signals = [
        {"line": i, "text": line.strip()}
        for i, line in enumerate(text.splitlines(), 1)
        if SIGNAL.search(line)
    ]
    result = {"path": path, "sha256": hashlib.sha256(content).hexdigest(), "kind": kind}
    if signals:
        result["signals"] = signals
    return result


def cargo_inventory(metadata):
    result = []
    for package in metadata["packages"]:
        manifest = Path(package["manifest_path"])
        # Export only selected Cargo fields, not local manifest/cache paths or full
        # metadata. Historical raw command logs have a separate disclosure policy.
        result.append({
            "name": package["name"],
            "version": package["version"],
            "source": package["source"],
            "declared_license": package["license"],
            "declared_license_file": package["license_file"],
            "license_files_present": sorted(
                p.name for p in manifest.parent.iterdir()
                if p.is_file() and re.match(r"(?i)^(license|copying|notice)([.-]|$)", p.name)
            ),
        })
    return sorted(result, key=lambda p: (p["name"], p["version"]))


def inventory(metadata):
    def git(*args, input_data=None):
        return subprocess.check_output(["git", *args], cwd=ROOT, input=input_data)

    # Read committed blobs, not cache directories or symlink targets.
    tree = git("ls-tree", "-rz", "HEAD:upstream/herdr")
    files, signal_files, assets, zig_manifests = [], [], [], []
    entries = tree.rstrip(b"\0").split(b"\0")
    oids = [entry.split(b"\t", 1)[0].split()[2] for entry in entries]
    # One Git process, not thousands of subprocesses for the full snapshot.
    blobs = git("cat-file", "--batch", input_data=b"\n".join(oids) + b"\n")
    cursor = 0
    for entry in entries:
        header, raw_path = entry.split(b"\t", 1)
        mode, kind, oid = header.decode().split()
        path = raw_path.decode()
        if kind != "blob":
            raise ValueError(f"unexpected non-blob entry: {path}")
        end = blobs.index(b"\n", cursor)
        blob_header = blobs[cursor:end].split()
        if blob_header[:2] != [oid.encode(), b"blob"]:
            raise ValueError(f"unexpected cat-file response for {path}")
        size = int(blob_header[2])
        content = blobs[end + 1:end + 1 + size]
        cursor = end + 2 + size
        fact = inspect_file(PREFIX + path, content)
        if mode == "120000":
            fact["kind"] = "symlink"
        files.append({"path": fact["path"], "sha256": fact["sha256"],
                      "kind": fact["kind"], "git_mode": mode})
        if fact.get("signals"):
            signal_files.append(fact)
        if path.startswith("assets/"):
            assets.append({**fact, "git_mode": mode, "permission_status": "unresolved"})
        if path.endswith("build.zig.zon"):
            zig_manifests.append({**fact, "manifest": content.decode()})
    return {
        "schema_version": 2,
        "warning": "Discovery evidence only. Not exhaustive legal review or proof of binary linkage.",
        "viva_commit": git("rev-parse", "HEAD").decode().strip(),
        "source_tree": git("rev-parse", "HEAD:upstream/herdr").decode().strip(),
        "tracked_files_scanned": len(entries),
        "files": files,
        "cargo_scope": "cargo metadata --locked; all platforms, not a built-artifact SBOM",
        "cargo_packages": cargo_inventory(metadata),
        "source_signals": signal_files,
        "assets": assets,
        "zig_manifests": zig_manifests,
    }


if __name__ == "__main__":
    with open(sys.argv[1]) as source:
        print(json.dumps(inventory(json.load(source)), indent=2, ensure_ascii=False))
