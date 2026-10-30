import gzip
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[2]
FOLDER = ROOT / 'research/herdr-190/closeout'
spec = importlib.util.spec_from_file_location('unicode_diacritics', Path(__file__).with_name('unicode_diacritics.py'))
unicode_data = importlib.util.module_from_spec(spec)
spec.loader.exec_module(unicode_data)


def git_blob(data):
    return hashlib.sha1(b'blob '+str(len(data)).encode()+b'\0'+data).hexdigest()


class CloseoutTests(unittest.TestCase):
    def test_official_unicode6_reconstructs_all_297_original_values(self):
        index = json.loads((FOLDER/'notice-index.json').read_text())['data']
        path = ROOT/index['path']
        packed = path.read_bytes()
        raw = gzip.decompress(packed)
        self.assertEqual(hashlib.sha256(packed).hexdigest(), index['gzip_sha256'])
        self.assertEqual(hashlib.sha256(raw).hexdigest(), index['raw_sha256'])
        self.assertEqual(git_blob(raw), index['raw_git_blob'])
        result = unicode_data.inspect(path)  # real immutable Git source, no network
        captured = json.loads((FOLDER/'diacritics-reconstruction.json').read_text())
        self.assertEqual(result, captured)
        self.assertEqual(result['ordered_count'], 297)
        self.assertTrue(result['all_ordered_values_equal'])
        with self.assertRaisesRegex(ValueError, 'Not the fixed Unicode'):
            unicode_data.reconstruct(raw+b'\n')

    def test_supplemental_notices_have_exact_source_blob_and_text_hashes(self):
        notices = json.loads((FOLDER/'notice-index.json').read_text())['notices']
        indexed = set()
        for n in notices:
            path = Path(n['path'])
            self.assertFalse(path.is_absolute())
            self.assertNotIn('..', path.parts)
            data = (ROOT/path).read_bytes()
            self.assertEqual(hashlib.sha256(data).hexdigest(), n['sha256'])
            self.assertEqual(git_blob(data), n['git_blob'])
            indexed.add(str(path))
        present = {str(p.relative_to(ROOT)) for p in (ROOT/'licenses/herdr-supplemental').rglob('*') if p.is_file()}
        self.assertEqual(indexed, present)
        self.assertEqual(len(indexed), 6)

    def test_media_capture_is_bound_to_original_twelve_inputs(self):
        report = json.loads((FOLDER/'media-properties.json').read_text())
        inventory = json.loads((ROOT/'research/herdr-190/inventory.json').read_text())
        self.assertEqual(report['source_tree'], inventory['source_tree'])
        fixture_prefix = 'upstream/herdr/vendor/libghostty-vt/src/terminal/kitty/testdata/'
        assets = {a['path'] for a in inventory['assets']}
        expected = {r['path']: r['sha256'] for r in inventory['files']
                    if r['path'] in assets or r['path'].startswith(fixture_prefix)}
        rows = {r['path']: r for r in report['records']}
        self.assertEqual(set(rows), set(expected))
        self.assertEqual(len(rows), 12)
        for path, row in rows.items():
            raw = subprocess.check_output(['git', 'show', 'HEAD:'+path], cwd=ROOT)
            self.assertEqual(hashlib.sha256(raw).hexdigest(), row['sha256'])
            self.assertEqual(row['sha256'], expected[path])
        # A numeric pixel property, not a copyrightability/ownership assertion.
        flat = rows[fixture_prefix+'image-png-none-50x76-2147483647-raw.data']
        self.assertEqual(flat['dimensions'], [50, 76])
        self.assertEqual(flat['uniform_rgba'], [43, 47, 55, 255])
        self.assertEqual(hashlib.sha256(bytes(flat['uniform_rgba'])*50*76).hexdigest(), flat['decoded_rgba_sha256'])


if __name__ == '__main__':
    unittest.main()
