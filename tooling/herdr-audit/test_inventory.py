import hashlib
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("inventory", Path(__file__).with_name("inventory.py"))
audit = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(audit)


class InventoryTests(unittest.TestCase):
    def test_file_level_exceptions_are_discovered_not_relicensed(self):
        for header in [
            b"distributed under GPLv3", b"GNU Lesser General Public License",
            b"// SPDX-License-Identifier: HPND", b"// BSD-style license",
            b"https://www.unicode.org/terms_of_use.html",
        ]:
            with self.subTest(header=header):
                fact = audit.inspect_file("vendor/example", b"\n" + header)
                self.assertEqual(fact["signals"][0]["line"], 2)
                self.assertNotIn("license", fact)

    def test_opaque_asset_is_hashed_without_claiming_permission(self):
        fact = audit.inspect_file("assets/sound.mp3", b"\xff\x00\x01")
        self.assertEqual(len(fact["sha256"]), 64)
        self.assertNotIn("signals", fact)

    def test_metadata_keeps_unknown_license_and_omits_absolute_paths(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "LICENSE-MIT").write_text("example")
            (root / "NOTICE.md").write_text("example")
            (root / "src").mkdir()
            metadata = {"packages": [{
                "name": "example", "version": "1.0.0", "source": None,
                "manifest_path": str(root / "Cargo.toml"),
                "license": None, "license_file": "LICENSE-MIT",
            }]}
            row = audit.cargo_inventory(metadata)[0]
            self.assertIsNone(row["declared_license"])
            self.assertEqual(row["license_files_present"], ["LICENSE-MIT", "NOTICE.md"])
            self.assertNotIn(temp, str(row))


class CommittedTreeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Exercise real Git traversal/batch framing without Cargo or network.
        cls.result = audit.inventory({"packages": []})

    def test_fixed_source_tree_counts_and_complete_file_records(self):
        result = self.result
        self.assertEqual(result["source_tree"], "98b3f4ed3ae4c238f3267fb22c5920256571e308")
        self.assertEqual(result["tracked_files_scanned"], 2945)
        self.assertEqual(len(result["files"]), 2945)
        self.assertEqual(len({r["path"] for r in result["files"]}), 2945)
        self.assertEqual(len(result["zig_manifests"]), 22)
        self.assertEqual(len(result["assets"]), 8)

    def test_binary_and_dotfile_hashes_match_independent_git_show(self):
        records = {r["path"]: r for r in self.result["files"]}
        for path, kind in [
            ("vendor/libghostty-vt/pkg/freetype/res/FiraCode-Regular.ttf", "binary"),
            ("vendor/libghostty-vt/src/terminal/kitty/testdata/dog.png", "binary"),
            ("vendor/libghostty-vt/src/shell-integration/zsh/.zshenv", "text"),
            ("Cargo.toml", "text"),
            ("CLAUDE.md", "symlink"),
        ]:
            full_path = audit.PREFIX + path
            with self.subTest(path=full_path):
                blob = subprocess.check_output(["git", "show", "HEAD:" + full_path], cwd=audit.ROOT)
                self.assertEqual(records[full_path]["sha256"], hashlib.sha256(blob).hexdigest())
                self.assertEqual(records[full_path]["kind"], kind)


if __name__ == "__main__":
    unittest.main()
