#!/usr/bin/env python3
"""Collect full notices from current Cargo inputs and the release toolchains.

This is a conservative input inventory, not a linker trace or legal opinion.
Release CI supplies target-filtered Cargo metadata for the same locked build.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PAYLOAD = Path("licenses/dependency-notices.json")
NOTICE_FILE = re.compile(
    r"(^|[._-])(unlicense|licen[cs]es?|copying|copyrights?|notices?)([._-]|$)", re.I
)
OBJC2 = "herdr-native/objc2/LICENSE.md"
OBJC2_TERMS = tuple(f"herdr-supplemental/objc2/{name}" for name in (
    "LICENSE-MIT.txt", "LICENSE-APACHE.txt", "LICENSE-ZLIB.txt", "rust-objc-LICENSE.txt",
))
# Only use these workspace-level notices for the verified published revision.
# GitHub revision comes from the package's .cargo_vcs_info.json, not a moving tag.
WORKSPACE_NOTICES = {
    ("block2", "0.6.2"): ("madsmtm/objc2", "b4167b582b2f75f9a1be75495c41b765344fd03c", OBJC2, *OBJC2_TERMS),
    ("dispatch2", "0.3.1"): ("madsmtm/objc2", "8852b424193ca41602281b3d7540d7c8ed51e49a", OBJC2, *OBJC2_TERMS),
    ("objc2", "0.6.5"): ("madsmtm/objc2", "d7d2fa23ceaa5e6096c923b081040e5d81b3b9df", OBJC2, *OBJC2_TERMS),
    ("objc2-encode", "4.1.0"): ("madsmtm/objc2", "8d214f5477365ffcbcbb7de058c86ed9a518efb7", OBJC2, *OBJC2_TERMS),
    ("ratatui-termina", "0.1.0"): ("ratatui/ratatui", "e665c36cb14752a61cd777fbd06dbef8474f2add", "herdr-native/ratatui/LICENSE"),
    ("vtparse", "0.6.2"): ("wez/wezterm", "edeae72b5fc55c7fa4aa1d08bbefd08c5493f757", "wezterm/LICENSE.md"),
    ("wezterm-input-types", "0.1.0"): ("wez/wezterm", "d5ca5509b95edc31291ba29bd1fcbf3f0b12c92d", "wezterm/LICENSE.md"),
}
ZIG_LICENSES = ("LICENSE", "lib/libcxx/LICENSE.TXT", "lib/libcxxabi/LICENSE.TXT", "lib/libunwind/LICENSE.TXT")


def cargo_inputs(metadata: dict) -> list[dict]:
    packages = {p["id"]: p for p in metadata["packages"]}
    (viva,) = [p["id"] for p in packages.values()
               if p["id"] in metadata["workspace_members"] and p["name"] == "viva"]
    nodes = {n["id"]: n["deps"] for n in metadata["resolve"]["nodes"]}
    seen = set()
    pending = [viva]
    while pending:
        current = pending.pop()
        if current in seen:
            continue
        seen.add(current)
        pending.extend(d["pkg"] for d in nodes[current]
                       if any(k["kind"] != "dev" for k in d["dep_kinds"]))
    return sorted((packages[i] for i in seen), key=lambda p: (p["name"], p["version"], p["source"] or ""))


def package_notices(package: dict) -> list[tuple[str, Path]]:
    directory = Path(package["manifest_path"]).parent
    if package["source"] is None:
        local = directory.resolve().relative_to(ROOT).as_posix()
        # Local wrapper declares Apache; the linked vendored Ghostty source
        # has its own MIT text, separately included in the Release.
        paths = {
            "crates/viva": ROOT / "LICENSE",
            "upstream/herdr": ROOT / "upstream/herdr/LICENSE",
            "upstream/herdr/crates/ghostty-vt": ROOT / "upstream/herdr/LICENSE",
            "upstream/herdr/vendor/portable-pty": ROOT / "upstream/herdr/vendor/portable-pty/LICENSE.md",
        }
        if local not in paths:
            raise ValueError(f"Unreviewed local Cargo input: {local}")
        return [(paths[local].relative_to(ROOT).as_posix(), paths[local])]
    files = {p for p in directory.rglob("*") if p.is_file() and NOTICE_FILE.search(p.name)}
    root_license = any(p.parent == directory and NOTICE_FILE.match(p.name)
                       and p.name.lower().startswith(("license", "licence", "copying", "unlicense"))
                       for p in files)
    if package["license_file"]:
        declared = directory / package["license_file"]
        declared.resolve().relative_to(directory.resolve())
        files.add(declared)
        root_license = True
    # A notice for bundled data is not a substitute for the crate's own terms.
    for path in files:
        path.resolve().relative_to(directory.resolve())
    notices = [(p.relative_to(directory).as_posix(), p) for p in sorted(files)]
    if root_license:
        return notices
    key = (package["name"], package["version"])
    if key not in WORKSPACE_NOTICES:
        raise ValueError(f"No license or reviewed workspace notice for {key[0]} {key[1]}")
    repository, revision, *local_notices = WORKSPACE_NOTICES[key]
    vcs = json.loads((directory / ".cargo_vcs_info.json").read_text(encoding="utf-8"))
    if vcs.get("git", {}).get("sha1") != revision or package["repository"] != f"https://github.com/{repository}":
        raise ValueError(f"Workspace notice provenance changed for {key[0]} {key[1]}")
    return notices + [
        (f"https://github.com/{repository}/tree/{revision} (licenses/{p})", ROOT / "licenses" / p)
        for p in local_notices
    ]


def build_payload(metadata: dict, target: str, rust_docs: Path, zig_root: Path) -> dict:
    texts = {}

    def store(source: str, path: Path) -> dict:
        data = path.read_bytes()
        if not data:
            raise ValueError(f"Empty notice: {source}")
        digest = hashlib.sha256(data).hexdigest()
        texts.setdefault(digest, data.decode("utf-8"))
        return {"source": source, "sha256": digest}

    packages = []
    for package in cargo_inputs(metadata):
        packages.append({
            **{k: package[k] for k in ("name", "version", "license", "source")},
            "notices": [store(source, path) for source, path in package_notices(package)],
        })
    rust_terms = sorted((rust_docs / "licenses").glob("*.txt"))
    if not rust_terms:
        raise ValueError("No Rust standard-library license texts found")
    rust_notices = [store("rust/COPYRIGHT-library.html", rust_docs / "COPYRIGHT-library.html")]
    rust_notices.extend(store(f"rust/licenses/{path.name}", path) for path in rust_terms)
    return {
        "schema_version": 1,
        "target": target,
        "scope": (
            "Normal/build dependency superset from locked, target-filtered workspace Cargo metadata; "
            "may include dependencies not selected by the Viva build. Selected Rust/Zig toolchain "
            "notices are included. Not a linked inventory, a choice among license alternatives, "
            "or clearance of source obligations."
        ),
        "cargo": packages,
        "rust_toolchain": rust_notices,
        "zig_toolchain": [store(f"zig/{name}", zig_root / name) for name in ZIG_LICENSES],
        "notice_texts_by_sha256": texts,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("metadata", type=Path)
    parser.add_argument("target")
    parser.add_argument("rust_docs", type=Path)
    parser.add_argument("zig_root", type=Path)
    args = parser.parse_args()
    try:
        payload = build_payload(json.loads(args.metadata.read_text(encoding="utf-8")),
                                args.target, args.rust_docs, args.zig_root)
    except (OSError, ValueError) as error:
        raise SystemExit(f"Cannot assemble dependency notices: {error}") from error
    path = args.directory / PAYLOAD
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, sort_keys=True, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
