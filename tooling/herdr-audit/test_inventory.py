import importlib.util
from pathlib import Path
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


if __name__ == "__main__":
    unittest.main()
