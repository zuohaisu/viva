import gzip
import hashlib
import importlib.util
import json
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
        # Intentional original-snapshot regression pin. A deliberate upstream/herdr
        # update must review and synchronize the tree/count constants below.
        hint = ("Snapshot mismatch: check for an inventory regression; for deliberate source updates, "
                "review provenance and synchronize snapshot constants.")
        self.assertEqual(result["source_tree"], "98b3f4ed3ae4c238f3267fb22c5920256571e308", hint)
        self.assertEqual(result["tracked_files_scanned"], 2945, hint)
        self.assertEqual(len(result["files"]), 2945, hint)
        self.assertEqual(len({r["path"] for r in result["files"]}), 2945, hint)
        self.assertEqual(len(result["zig_manifests"]), 22, hint)
        self.assertEqual(len(result["assets"]), 8, hint)

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

    def test_font_evidence_distinguishes_viva_commit_from_source_tree(self):
        evidence = json.loads((audit.ROOT / "research/herdr-190/font-evidence.json").read_text())
        self.assertNotIn("source_commit", evidence, "Use explicit Viva run provenance, not ambiguous source_commit")
        self.assertRegex(evidence["viva_commit"], r"^[0-9a-f]{40}$")
        self.assertEqual(evidence["source_tree"], self.result["source_tree"])
        records = {r["path"]: r for r in self.result["files"]}
        self.assertEqual(evidence["sha256"], records[evidence["path"]]["sha256"])

    def test_captured_native_notices_have_complete_hash_index(self):
        folder = audit.ROOT / "research/herdr-190/completion"
        native = json.loads((folder / "native-notices.json").read_text())
        cargo = json.loads((folder / "cargo-target-notices.json").read_text())
        notices = [n for p in native["packages"] for n in p["notices"]]
        notices += [{"path": n["notice_path"], "sha256": n["sha256"]}
                    for n in cargo["workspace_notice_fallbacks"]]
        for notice in notices:
            with self.subTest(path=notice["path"]):
                path = Path(notice["path"])
                self.assertFalse(path.is_absolute())
                self.assertNotIn("..", path.parts)
                self.assertEqual(hashlib.sha256((audit.ROOT / path).read_bytes()).hexdigest(),
                                 notice["sha256"])
        indexed = {n["path"] for n in notices if n["path"].startswith("licenses/herdr-native/")}
        present = {str(p.relative_to(audit.ROOT))
                   for p in (audit.ROOT / "licenses/herdr-native").rglob("*") if p.is_file()}
        self.assertEqual(indexed, present, "Every copied notice must have provenance/hash evidence")

    def test_completion_index_links_file_results_and_failed_diff_check(self):
        folder = audit.ROOT / "research/herdr-190/completion"
        baseline = json.loads((folder / "baseline.json").read_text())
        entries = json.loads((folder / "checks.json").read_text())["checks"]
        runtime = next(c for c in entries if c["log_name"] == "runtime-smoke.log")
        self.assertIn("runtime/result.json", runtime["evidence"])
        self.assertIn("runtime/server-status-after-stop.json", runtime["evidence"])
        for name in runtime["evidence"]:
            with self.subTest(path=name):
                self.assertFalse(Path(name).is_absolute())
                self.assertNotIn("..", Path(name).parts)
                self.assertTrue((folder / name).is_file())
                if name.endswith(".gz"):
                    self.assertTrue(gzip.decompress((folder / name).read_bytes()))
        observed = json.loads((folder / "runtime/server-status-after-stop.json").read_text())
        self.assertEqual(observed["args"], ["status", "server"])
        for key in ("returncode", "stdout", "stderr"):
            self.assertEqual(observed[key], baseline["cleanup"][key])
        self.assertIn("status: not running", observed["stdout"].splitlines())
        diff = next(c for c in entries if c["command"] == baseline["diff_check"]["command"])
        self.assertEqual(diff["exit_code"], baseline["diff_check"]["exit_code"])
        self.assertEqual(diff["stdout_and_stderr"], baseline["diff_check"]["output"])


if __name__ == "__main__":
    unittest.main()
