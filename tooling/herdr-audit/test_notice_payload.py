import hashlib
import importlib.util
import json
import re
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('notice_payload', Path(__file__).with_name('notice_payload.py'))
payload = importlib.util.module_from_spec(spec)
spec.loader.exec_module(payload)


class NoticePayloadTests(unittest.TestCase):
    def fixture(self, folder):
        text = 'Copyright example\nPermission example\n'
        (folder / 'LICENSE').write_text(text)
        digest = hashlib.sha256(text.encode()).hexdigest()
        metadata = {'packages': [{'name': 'example', 'version': '1.0',
                                  'license': 'MIT OR Apache-2.0',
                                  'manifest_path': str(folder / 'Cargo.toml')}]}
        captured = {'source_tree': 'fixture-only', 'scope': 'unit fixture, not real package evidence',
                    'packages': [{'name': 'example', 'version': '1.0',
                                  'declared_license': 'MIT OR Apache-2.0',
                                  'notice_files': [{'file': 'LICENSE', 'sha256': digest}]}],
                    'workspace_notice_fallbacks': []}
        return metadata, captured

    def test_duplicates_are_deduplicated_without_selecting_license_alternative(self):
        with tempfile.TemporaryDirectory() as d:
            metadata, captured = self.fixture(Path(d))
            captured['packages'][0]['notice_files'] *= 2
            result = payload.build_payload(metadata, captured)
            self.assertEqual(len(result['notice_texts']), 1)
            self.assertEqual(len(result['packages'][0]['notice_refs']), 2)
            self.assertEqual(result['packages'][0]['declared_license'], 'MIT OR Apache-2.0')

    def test_changed_notice_is_rejected(self):
        with tempfile.TemporaryDirectory() as d:
            folder = Path(d)
            metadata, captured = self.fixture(folder)
            (folder / 'LICENSE').write_text('changed after original capture')
            with self.assertRaisesRegex(ValueError, 'differs from captured hash'):
                payload.build_payload(metadata, captured)

    def test_missing_or_changed_package_is_rejected(self):
        with tempfile.TemporaryDirectory() as d:
            metadata, captured = self.fixture(Path(d))
            metadata['packages'][0]['license'] = 'MIT'
            with self.assertRaisesRegex(ValueError, 'License declaration changed'):
                payload.build_payload(metadata, captured)
            metadata['packages'].clear()
            with self.assertRaisesRegex(ValueError, 'Missing or ambiguous package'):
                payload.build_payload(metadata, captured)

    def test_real_captured_payload_has_exact_text_for_all_selected_notices(self):
        folder = ROOT / 'research/herdr-190'
        captured = json.loads((folder / 'completion/cargo-target-notices.json').read_text())
        result = json.loads((folder / 'license-closure/cargo-notice-payload.json').read_text())
        self.assertEqual(result['source_tree'], captured['source_tree'])
        # These are advertised facts for this fixed capture, not live counters.
        # An intentional inventory refresh must also update its public claims.
        self.assertEqual(len(captured['packages']), 155)
        self.assertEqual(len(result['packages']), 155)
        self.assertEqual(len(result['notice_texts']), 111)
        texts = {t['sha256']: t['text'] for t in result['notice_texts']}
        self.assertEqual(len(texts), len(result['notice_texts']))
        for digest, text in texts.items():
            self.assertEqual(hashlib.sha256(text.encode('utf-8')).hexdigest(), digest)
        observed = {(p['name'], p['version']): p for p in result['packages']}
        self.assertEqual(len(observed), len(captured['packages']))
        for p in captured['packages']:
            actual = observed[(p['name'], p['version'])]
            self.assertEqual(actual['declared_license'], p['declared_license'])
            expected = {n['sha256'] for n in p['notice_files']}
            if not expected:
                expected = {f['sha256'] for f in captured['workspace_notice_fallbacks']
                            if p['name'] in f['packages']}
            self.assertEqual({n['sha256'] for n in actual['notice_refs']}, expected)
            self.assertTrue(expected)
            self.assertTrue(expected <= texts.keys())


class SourceNoticeTests(unittest.TestCase):
    def setUp(self):
        self.folder = ROOT / 'research/herdr-190/license-closure'
        self.evidence = json.loads((self.folder / 'source-notices.json').read_text())

    def git_blob(self, data):
        return hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()

    def test_x11_original_notice_and_data_equivalence(self):
        record = self.evidence['x11']
        data = (ROOT / record['notice_path']).read_bytes()
        self.assertEqual(hashlib.sha256(data).hexdigest(), record['notice_sha256'])
        self.assertEqual(self.git_blob(data), record['notice_git_blob'])
        upstream = record['upstream_rgb_text'].encode()
        self.assertEqual(self.git_blob(upstream), record['data_git_blob'])
        local = (ROOT / 'upstream/herdr/vendor/libghostty-vt/src/terminal/res/rgb.txt').read_bytes()
        tokens = lambda b: [line.split() for line in b.decode().splitlines() if line.strip()]
        self.assertEqual(tokens(local), tokens(upstream))
        self.assertNotEqual(local, upstream, 'Whitespace differences must not be presented as byte identity')

    def test_theme_tree_and_all_supplemental_text_hashes(self):
        theme = self.evidence['themes']
        index = theme['original_blob_index']
        self.assertEqual(len(index), 607)
        tree = b''.join(b'100644 ' + name.encode() + b'\0' + bytes.fromhex(index[name])
                        for name in sorted(index, key=lambda n: n.encode()))
        digest = hashlib.sha1(b'tree ' + str(len(tree)).encode() + b'\0' + tree).hexdigest()
        self.assertEqual(digest, theme['ghostty_tree'])
        runtime = json.loads((self.folder / 'runtime-notices.json').read_text())
        records = [theme['collection_notice'], theme['credits']]
        records += runtime['rust_notices'] + runtime['zig_notices']
        for record in records:
            self.assertEqual(hashlib.sha256(record['text'].encode()).hexdigest(), record['sha256'])
        self.assertIn('individual', theme['collection_notice']['text'])
        indexed = {self.evidence['x11']['notice_path']}
        indexed.update(self.evidence['fontconfig_casefold']['notice_paths'])
        for record in self.evidence['additional_source_notices']['notices']:
            data = (ROOT / record['path']).read_bytes()
            self.assertEqual(hashlib.sha256(data).hexdigest(), record['sha256'])
            self.assertEqual(self.git_blob(data), record['git_blob'])
            indexed.add(record['path'])
        present = {str(p.relative_to(ROOT)) for p in (ROOT / 'licenses/herdr-data').rglob('*') if p.is_file()}
        self.assertEqual(indexed, present, 'Each copied supplemental notice needs provenance/hash')

    def test_glyph_packet_is_bound_to_source_and_full_notices(self):
        record = json.loads((self.folder / 'glyph-provenance.json').read_text())
        source = (ROOT / record['source']).read_bytes()
        self.assertEqual(hashlib.sha256(source).hexdigest(), record['source_sha256'])
        import base64
        payloads = re.findall(rb'^\s*"([A-Za-z0-9+/=]+)",', source, re.M)
        self.assertEqual(len(payloads), 5)
        self.assertEqual(len(record['glyphs']), 5)
        for raw, item in zip(payloads, record['glyphs']):
            self.assertEqual(hashlib.sha256(base64.b64decode(raw)).hexdigest(), item['original_payload_sha256'])
            self.assertTrue(item['outline_equal'])
            self.assertTrue(item['canonical_bytes_equal'])
        packet = json.loads((self.folder / 'glyph-notices.json').read_text())
        self.assertTrue(packet['not_blanket_mit'])
        for item in packet['notices']:
            data = item['text'].encode()
            self.assertEqual(hashlib.sha256(data).hexdigest(), item['sha256'])
            self.assertEqual(self.git_blob(data), item['git_blob'])
        spec = importlib.util.spec_from_file_location('glyph_audit', Path(__file__).with_name('glyph_provenance.py'))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as d:
            wrong = Path(d) / 'wrong.ttf'
            wrong.write_bytes(b'not the real font')
            with self.assertRaisesRegex(ValueError, 'Not the pinned font blob'):
                module.inspect(wrong)  # rejected before any optional fontTools import

    def test_glyph_source_is_pinned_independently_of_head(self):
        spec = importlib.util.spec_from_file_location('pinned_glyph', Path(__file__).with_name('glyph_provenance.py'))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        record = json.loads((self.folder / 'glyph-provenance.json').read_text())
        self.assertEqual(module.SOURCE_SHA256, record['source_sha256'])
        source = module.load_source()  # real committed Git object, no font SDK
        self.assertEqual(self.git_blob(source), module.SOURCE_BLOB)
        self.assertEqual(hashlib.sha256(source).hexdigest(), record['source_sha256'])
        with patch.object(module.subprocess, 'check_output', return_value=source+b'\n// drift\n') as read:
            with self.assertRaisesRegex(ValueError, 'Source SHA-256 differs'):
                module.load_source()
        read.assert_called_once_with(['git', 'cat-file', 'blob', module.SOURCE_BLOB], cwd=ROOT)

    def test_unicode_generated_representation_matches_fixed_17_data(self):
        record = self.evidence['fontconfig_casefold']
        raw = record['candidate_text']
        self.assertEqual(hashlib.sha256(raw.encode()).hexdigest(), record['candidate_sha256'])
        expected = {}
        for line in raw.splitlines():
            if not line or line.startswith('#'):
                continue
            cp, kind, mapping, *_ = line.split(';')
            if kind.strip() in ('C', 'F'):
                expected[int(cp, 16)] = tuple(int(c, 16) for c in mapping.split())
        header = (ROOT / record['header']).read_text()
        chars = bytes(int(n, 16) for n in re.findall(r'0x[0-9a-f]+', header.split('static const FcChar8')[1]))
        rows = re.findall(r'\{ (0x[0-9a-f]+), FC_CASE_FOLD_(\w+),\s*(0x[0-9a-f]+),\s*(-?\d+) \}', header)
        self.assertEqual(len(rows), 306)
        self.assertEqual(len(chars), 471)
        seen = set()
        for cp, kind, count, offset in rows:
            cp, count, offset = int(cp, 16), int(count, 16), int(offset)
            self.assertIn(kind, ('FULL', 'RANGE', 'EVEN_ODD'))
            for i in (range(1) if kind == 'FULL' else range(0, count, 2 if kind == 'EVEN_ODD' else 1)):
                code = cp + i
                self.assertNotIn(code, seen)
                seen.add(code)
                target = expected[code]
                if kind == 'FULL':
                    self.assertEqual(tuple(map(ord, chars[offset:offset+count].decode())), target)
                else:
                    self.assertEqual(len(target), 1)
                    # Compare the generated signed-short representation, not
                    # the runtime's arithmetic; do not silently change either.
                    self.assertEqual((target[0]-code+32768) % 65536-32768, offset)
        self.assertEqual(seen, expected.keys())
        self.assertEqual(len(seen), 1585)
        for path, digest in record['notice_paths'].items():
            self.assertEqual(hashlib.sha256((ROOT / path).read_bytes()).hexdigest(), digest)


if __name__ == '__main__':
    unittest.main()
