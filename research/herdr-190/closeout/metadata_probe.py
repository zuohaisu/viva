#!/usr/bin/env python3
"""Read committed media properties, never infer permission from missing tags.

Existing inventory.py supplies Git paths/hashes, not codec/tag/pixel evidence.
Reuse Pillow and Mutagen for this gap (isolated tooling only, not app deps):
  PYTHONPATH=/tmp/viva-190-assets/tools python3.11 \
    research/herdr-190/closeout/metadata_probe.py > /tmp/media.json
No playback, network, source mutation, user data/session or license acceptance.
"""
from collections import Counter
import hashlib
import io
import json
from pathlib import Path
import subprocess
import zlib

ROOT = Path(__file__).resolve().parents[3]
TREE = '98b3f4ed3ae4c238f3267fb22c5920256571e308'
FIXTURES = 'upstream/herdr/vendor/libghostty-vt/src/terminal/kitty/testdata/'


def inspect():
    tree = subprocess.check_output(['git', 'rev-parse', 'HEAD:upstream/herdr'], cwd=ROOT, text=True).strip()
    if tree != TREE:
        raise ValueError('Original source changed; do not relabel new media as baseline')
    import PIL
    from PIL import Image
    import mutagen
    from mutagen.mp3 import MP3
    inventory = json.loads((ROOT/'research/herdr-190/inventory.json').read_text())
    assets = {v['path'] for v in inventory['assets']}
    facts = [v for v in inventory['files'] if v['path'] in assets or v['path'].startswith(FIXTURES)]
    records = []
    for fact in facts:
        path = fact['path']
        data = subprocess.check_output(['git', 'show', 'HEAD:'+path], cwd=ROOT)
        if hashlib.sha256(data).hexdigest() != fact['sha256']:
            raise ValueError('Media differs from fixed capture: '+path)
        item = {'path': path, 'sha256': fact['sha256'], 'size': len(data)}
        if data.startswith(b'\x89PNG\r\n\x1a\n'):
            with Image.open(io.BytesIO(data)) as image:
                pixels = image.convert('RGBA').tobytes()
                counts = Counter(pixels[i:i+4] for i in range(0, len(pixels), 4))
                item.update(format='PNG', dimensions=list(image.size),
                            metadata={k: str(v) for k, v in image.info.items()},
                            decoded_rgba_sha256=hashlib.sha256(pixels).hexdigest(),
                            unique_rgba_colors=len(counts))
                if len(counts) == 1:
                    item['uniform_rgba'] = list(next(iter(counts)))
        elif path.endswith('.mp3'):
            audio = MP3(io.BytesIO(data))
            item.update(format='MP3', length_seconds=audio.info.length,
                        sample_rate=audio.info.sample_rate, bitrate=audio.info.bitrate,
                        tags={k: str(v) for k, v in (audio.tags or {}).items()})
        elif path.endswith('.svg'):
            item.update(format='SVG', contains_metadata_element=b'<metadata' in data)
        else:
            raw = zlib.decompress(data) if 'zlib_deflate' in path else data
            dimensions = [128, 96] if '128x96' in path else [20, 15]
            if len(raw) != dimensions[0]*dimensions[1]*3:
                raise ValueError('Unexpected RGB fixture size')
            item.update(format='RGB', dimensions=dimensions,
                        decoded_rgb_sha256=hashlib.sha256(raw).hexdigest(),
                        unique_rgb_colors=len({raw[i:i+3] for i in range(0, len(raw), 3)}))
        records.append(item)
    return {'scope': 'Metadata/pixels, not authorship, copyrightability or permission classification. Empty tags do not nullify a repository license or prove absence of permission.',
            'source_tree': tree, 'pillow_version': PIL.__version__,
            'mutagen_version': mutagen.version_string, 'records': records}


if __name__ == '__main__':
    print(json.dumps(inspect(), indent=2, ensure_ascii=False))
