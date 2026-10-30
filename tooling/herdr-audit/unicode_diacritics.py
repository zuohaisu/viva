#!/usr/bin/env python3
"""Reconstruct protocol data from the licensed, fixed Unicode 6.0 dataset.

Reuse gzip/csv/Git, not a custom Unicode database or license classifier.
The selection describes the interoperable numeric table, not Kitty program code.
Input is the raw or gzip-compressed UnicodeData.txt with the fixed Git blob ID.
  python3 tooling/herdr-audit/unicode_diacritics.py /tmp/UnicodeData.txt.gz
No fetching, source mutation, build, runtime/session operation or legal clearance.
"""
import csv
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
DATA_BLOB = '8d7222b13789c43e5ed36ddf0d3eb7ebe8d72c7b'
SOURCE_BLOB = 'b62f8177cf901d201f242f4bf092bea59bf050c5'
SOURCE_SHA256 = '8e1bb778abad3cf7ae09434e4ed64748c1dbc69a7671c713e4e3ae5cf0590ed7'
# Normalization-sensitive characters excluded by the protocol's numeric table.
EXCLUDED = frozenset(int(n, 16) for n in
    '0300 0301 0302 0303 0304 0306 0307 0308 0309 030A 030B 030C 030F 0311 0313 0314 0342 0653 0654'.split())


def reconstruct(data):
    blob = hashlib.sha1(b'blob '+str(len(data)).encode()+b'\0'+data).hexdigest()
    if blob != DATA_BLOB:
        raise ValueError('Not the fixed Unicode 6.0 data blob')
    result = []
    for row in csv.reader(io.StringIO(data.decode('utf-8')), delimiter=';'):
        if len(row) != 15:
            raise ValueError('Unexpected UnicodeData row')
        cp = int(row[0], 16)
        if row[2:5] == ['Mn', '230', 'NSM'] and not row[5] and cp not in EXCLUDED:
            result.append(cp)
    return result


def inspect(path):
    packed = path.read_bytes()
    data = gzip.decompress(packed) if packed.startswith(b'\x1f\x8b') else packed
    values = reconstruct(data)
    source = subprocess.check_output(['git', 'cat-file', 'blob', SOURCE_BLOB], cwd=ROOT)
    if hashlib.sha256(source).hexdigest() != SOURCE_SHA256:
        raise ValueError('Source differs from the original snapshot')
    table = source.decode().split('const diacritics: []const u21 = &.{', 1)[1].split('};', 1)[0]
    actual = [int(v, 16) for v in re.findall(r'0x[0-9a-fA-F]+', table)]
    if values != actual or len(values) != 297:
        raise ValueError('Reconstructed ordered table differs from original source')
    return {'scope': 'Numeric-data reconstruction, not a whole-file license or legal conclusion',
            'data_git_blob': DATA_BLOB, 'data_sha256': hashlib.sha256(data).hexdigest(),
            'source_git_blob': SOURCE_BLOB, 'source_sha256': SOURCE_SHA256,
            'selection': 'Mn / canonical combining class 230 / NSM / no decomposition; exclude 19 normalization-sensitive codepoints',
            'ordered_count': len(values), 'all_ordered_values_equal': True,
            'codepoints': [f'{cp:04X}' for cp in values]}


if __name__ == '__main__':
    print(json.dumps(inspect(Path(sys.argv[1])), indent=2))
