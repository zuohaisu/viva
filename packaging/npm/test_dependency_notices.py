#!/usr/bin/env python3
"""Validate release notice collection and generate offline pack fixtures."""

import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path

from release_dependency_notices import PAYLOAD, ROOT, ZIG_LICENSES, build_payload, package_notices

LICENSE = b"Fixture copyright (c) contributors\r\nFixture permission text\r\n"


def make_inputs(root):
    def package(name, license_bytes=None):
        directory = root / name
        directory.mkdir()
        if license_bytes is not None:
            (directory / "LICENSE-MIT").write_bytes(license_bytes)
            (directory / "COPYRIGHT").write_bytes(license_bytes)
        return {"name": name, "id": name, "version": "1.2.3", "license": "MIT OR Apache-2.0",
                "source": "registry+fixture", "manifest_path": str(directory / "Cargo.toml"),
                "repository": None, "license_file": None}

    viva = {**package("viva"), "source": None, "manifest_path": str(ROOT / "crates/viva/Cargo.toml")}
    runtime = package("fixture-runtime", LICENSE)
    build = package("fixture-build", LICENSE)
    dev = package("fixture-dev")  # A dev-only dependency need not carry a notice here.
    pkg = [viva, runtime, build, dev]
    deps = [{"pkg": name, "dep_kinds": [{"kind": kind}]} for name, kind in (
        (runtime["id"], None), (build["id"], "build"), (dev["id"], "dev"),
    )]
    rust = root / "rust"
    zig = root / "zig"
    for path in (rust / "COPYRIGHT-library.html", rust / "licenses/MIT.txt",
                 *(zig / name for name in ZIG_LICENSES)):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(LICENSE)
    metadata = {"packages": pkg, "workspace_members": [viva["id"]], "resolve": {"nodes": [
        {"id": p["id"], "deps": deps if p == viva else []} for p in pkg
    ]}}
    return metadata, rust, zig


def fixture(directory, target):
    with tempfile.TemporaryDirectory() as temp:
        metadata, rust, zig = make_inputs(Path(temp))
        payload = build_payload(metadata, target, rust, zig)
    path = directory / PAYLOAD
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload), encoding="utf-8")


class DependencyNoticeTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.metadata, self.rust, self.zig = make_inputs(Path(temp.name))

    def build(self):
        return build_payload(self.metadata, "x86_64-apple-darwin", self.rust, self.zig)

    def test_normal_and_build_notices_keep_hashes_terms_and_bytes(self):
        payload = self.build()
        self.assertEqual([p["name"] for p in payload["cargo"]], ["fixture-build", "fixture-runtime", "viva"])
        digest = hashlib.sha256(LICENSE).hexdigest()
        self.assertEqual(payload["notice_texts_by_sha256"][digest].encode(), LICENSE)
        self.assertEqual(len(payload["notice_texts_by_sha256"]), 2)
        self.assertEqual(payload["cargo"][0]["license"], "MIT OR Apache-2.0")
        self.assertEqual({r["sha256"] for r in payload["cargo"][0]["notices"]}, {digest})
        self.assertEqual(len(payload["zig_toolchain"]), len(ZIG_LICENSES))

    def test_missing_notice_fails_closed(self):
        for path in (self.rust.parent / "fixture-runtime").iterdir():
            path.unlink()
        with self.assertRaisesRegex(ValueError, "No license.*fixture-runtime"):
            self.build()

    def test_workspace_notice_is_bound_to_the_published_crate_revision(self):
        directory = self.rust.parent / "fixture-dev"
        package = {"source": "registry+fixture", "repository": "https://github.com/wez/wezterm",
                   "name": "vtparse", "version": "0.6.2", "license_file": None,
                   "manifest_path": str(directory / "Cargo.toml")}
        vcs = directory / ".cargo_vcs_info.json"
        vcs.write_text('{"git":{"sha1":"edeae72b5fc55c7fa4aa1d08bbefd08c5493f757"}}', encoding="utf-8")
        [(source, path)] = package_notices(package)
        self.assertIn("edeae72b5fc55c7fa4aa1d08bbefd08c5493f757", source)
        self.assertEqual(path, ROOT / "licenses/wezterm/LICENSE.md")
        vcs.write_text('{"git":{"sha1":"unexpected"}}', encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "provenance changed"):
            package_notices(package)

    def test_missing_toolchain_text_fails_closed(self):
        (self.zig / "LICENSE").unlink()
        with self.assertRaises(FileNotFoundError):
            self.build()


if __name__ == "__main__":
    if sys.argv[1:2] == ["fixture"]:
        fixture(Path(sys.argv[2]), sys.argv[3])
    else:
        unittest.main()
