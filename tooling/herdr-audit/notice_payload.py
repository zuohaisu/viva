#!/usr/bin/env python3
"""Export captured Cargo root notices as full text, not legal clearance.

Reuse official Cargo metadata and #190's hash inventory. No fetching, builds,
license-expression choice, session operations or package publication. This is
conservative material for B's actual package review, NOT a linked-artifact SBOM.
Run from the repo root with fresh nested-workspace `cargo metadata --locked`:
  python3 tooling/herdr-audit/notice_payload.py /tmp/metadata.json > /tmp/payload.json
"""
import hashlib
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
INVENTORY = ROOT / 'research/herdr-190/completion/cargo-target-notices.json'


def checked_text(path, expected):
    data = path.read_bytes()
    if hashlib.sha256(data).hexdigest() != expected:
        raise ValueError(f'Notice content differs from captured hash: {path.name}')
    # Strict decoding: never silently replace or normalize license bytes.
    return data.decode('utf-8')


def build_payload(metadata, captured, root=ROOT):
    texts, packages = {}, []
    for item in captured['packages']:
        matches = [p for p in metadata['packages']
                   if (p['name'], p['version']) == (item['name'], item['version'])]
        if len(matches) != 1:
            raise ValueError(f'Missing or ambiguous package: {item["name"]}')
        package = matches[0]
        if package['license'] != item['declared_license']:
            raise ValueError(f'License declaration changed: {item["name"]}')
        notices = []
        for notice in item['notice_files']:
            name = notice['file']
            if Path(name).name != name or name in ('', '.', '..'):
                raise ValueError('Expected a captured root notice filename')
            path = Path(package['manifest_path']).parent / name
            notices.append((path, notice['sha256'], name, 'captured Cargo root notice'))
        if not notices:
            for fallback in captured['workspace_notice_fallbacks']:
                if item['name'] not in fallback['packages']:
                    continue
                path = Path(fallback['notice_path'])
                if path.is_absolute() or '..' in path.parts:
                    raise ValueError('Expected a repository-relative fallback')
                notices.append((root / path, fallback['sha256'], str(path), fallback['scope']))
        if not notices:
            raise ValueError(f'No captured root notice or fallback: {item["name"]}')
        refs = []
        for path, digest, name, scope in notices:
            text = checked_text(path, digest)
            if digest in texts and texts[digest]['text'] != text:
                raise ValueError('Inconsistent duplicate digest')
            texts.setdefault(digest, {'sha256': digest, 'text': text})
            refs.append({'sha256': digest, 'source_file': name, 'capture_scope': scope})
        packages.append({'name': item['name'], 'version': item['version'],
                         'declared_license': item['declared_license'], 'notice_refs': refs})
    return {
        'schema_version': 1,
        'source_tree': captured['source_tree'],
        'scope': captured['scope'],
        'transport_policy': 'All captured root notices/fallbacks; no SPDX alternative chosen. Texts are UTF-8 byte-preserving and deduplicated by SHA-256.',
        'limitations': [
            'Not a license classifier, complete nested-file review, final SBOM or legal approval.',
            'A workspace licensing explanation is not automatically a complete permission text. In particular objc2/block2/objc2-encode/dispatch2 scope remains unresolved; preserve its explanation and obtain qualified review.',
            'Native code, language runtimes, embedded assets and platform-specific package contents require separate materials. B must export and inspect applicable texts inside each actual archive, not merely link to this JSON.',
        ],
        'packages': packages,
        'notice_texts': [texts[k] for k in sorted(texts)],
    }


if __name__ == '__main__':
    metadata = json.loads(Path(sys.argv[1]).read_text())
    captured = json.loads(INVENTORY.read_text())
    tree = subprocess.check_output(['git', 'rev-parse', 'HEAD:upstream/herdr'], cwd=ROOT, text=True).strip()
    if tree != captured['source_tree']:
        raise ValueError('Source snapshot changed; refresh the captured inventory first')
    print(json.dumps(build_payload(metadata, captured), indent=2, ensure_ascii=False))
