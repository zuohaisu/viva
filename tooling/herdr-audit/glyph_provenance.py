#!/usr/bin/env python3
"""Compare the actual five embedded glyphs with their fixed upstream font.

Uses the proven fontTools reader/compiler, not a custom font implementation.
Install fontTools in an isolated tooling directory, never product dependencies.
Supply ryanoasis/nerd-fonts@fa7b859994228a9c8759f99c55a8d31ee92a1b5e:
  patched-fonts/JetBrainsMono/Ligatures/Regular/JetBrainsMonoNerdFont-Regular.ttf
  python3 tooling/herdr-audit/glyph_provenance.py /tmp/font.ttf > /tmp/result.json
Source is the pinned original Git blob plus SHA-256, never the current HEAD.
The object must exist locally; missing objects fail rather than retargeting.
No network, builds, source mutation, runtime/session operations or clearance.
"""
import base64
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
SOURCE = 'upstream/herdr/vendor/libghostty-vt/src/font/glyf_rasterize_png_test.zig'
FONT_BLOB = '235a07aef9028e2953076efc9038f1b1182f8b2b'
# Fixed original source object, independent of the checkout's current HEAD.
# SHA-256 also binds this object to the committed d12f807 provenance capture.
SOURCE_BLOB = '881c0ed98aee538b81cab366607d83506a9b963f'
SOURCE_SHA256 = 'd4b7d9a337dd7d74fd858ad39ac33a46241367848ef95de75ac1698440d9e1f4'
CODEPOINTS = [0xe0a0, 0xf07b, 0xf015, 0xf004, 0xe7a8]


def load_source():
    source = subprocess.check_output(['git', 'cat-file', 'blob', SOURCE_BLOB], cwd=ROOT)
    if hashlib.sha256(source).hexdigest() != SOURCE_SHA256:
        raise ValueError('Source SHA-256 differs from the pinned provenance capture')
    return source


def inspect(font_path):
    data = font_path.read_bytes()
    blob = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
    if blob != FONT_BLOB:
        raise ValueError('Not the pinned font blob; do not substitute a different font/version')
    source = load_source()
    import fontTools
    from fontTools.ttLib import TTFont
    from fontTools.ttLib.tables._g_l_y_f import Glyph
    payloads = re.findall(rb'^\s*"([A-Za-z0-9+/=]+)",', source, re.M)
    if len(payloads) != len(CODEPOINTS):
        raise ValueError('Unexpected embedded glyph set')
    records = []
    with TTFont(font_path) as font:
        table, cmap = font['glyf'], font.getBestCmap()
        for cp, encoded in zip(CODEPOINTS, payloads):
            raw = base64.b64decode(encoded, validate=True)
            embedded = Glyph(raw)
            embedded.expand(table)
            name = cmap[cp]
            actual = table[name]
            left, le, lf = embedded.getCoordinates(table)
            right, re_, rf = actual.getCoordinates(table)
            equal_outline = left == right and le == re_ and [n & 1 for n in lf] == [n & 1 for n in rf]
            source_compiled, font_compiled = embedded.compile(table), actual.compile(table)
            if not equal_outline or source_compiled != font_compiled:
                raise ValueError(f'Embedded glyph does not match pinned font: {hex(cp)}')
            records.append({'codepoint': hex(cp), 'font_glyph': name,
                            'original_payload_sha256': hashlib.sha256(raw).hexdigest(),
                            'canonical_glyph_sha256': hashlib.sha256(font_compiled).hexdigest(),
                            'coordinate_count': len(left), 'contour_count': actual.numberOfContours,
                            'outline_equal': True, 'canonical_bytes_equal': True})
        version = sorted({n.toUnicode() for n in font['name'].names if n.nameID == 5})
    return {'role': 'Developer actual-font comparison, not independent QA or legal clearance',
            'source': SOURCE, 'source_git_blob': SOURCE_BLOB,
            'source_sha256': hashlib.sha256(source).hexdigest(),
            'font_source': 'ryanoasis/nerd-fonts@fa7b859994228a9c8759f99c55a8d31ee92a1b5e:patched-fonts/JetBrainsMono/Ligatures/Regular/JetBrainsMonoNerdFont-Regular.ttf',
            'font_git_blob': blob, 'font_sha256': hashlib.sha256(data).hexdigest(),
            'fonttools_version': fontTools.__version__, 'font_version': version,
            'comparison_scope': 'Equal decoded coordinates/endpoints/on-curve flags AND equal fontTools canonical glyph serialization. Not a claim of whole-font or original raw storage-byte identity; no hint removal or geometry normalization used.',
            'glyphs': records}


if __name__ == '__main__':
    print(json.dumps(inspect(Path(sys.argv[1])), indent=2, ensure_ascii=False))
