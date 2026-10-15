#!/usr/bin/env python3
import json
import re
from pathlib import Path

def require(condition, message):
    if not condition:
        raise ValueError(message)

def check(root):
    catalog = root / 'platforms/apple/shared/Sources/FileIDShared/Resources/model-catalog.json'
    document = json.loads(catalog.read_text())
    require(document['formatVersion'] == 1 and document['runtime'] == 'mlx-swift-lm 3.32.3', 'Invalid model catalog')
    entries = document['entries']
    require(len(entries) >= 200 and len({e['repo'] for e in entries}) == len(entries), 'Invalid model catalog')
    policy = {'apache-2.0', 'mit', 'bsd-2-clause', 'bsd-3-clause', 'isc'}
    swift = (root / 'platforms/apple/shared/Sources/FileIDShared/MLXModelCatalog.swift').read_text()
    architectures = {}
    for role in ('text', 'vision'):
        table = re.search(role + 'Architectures: Set<String> = \\[([^\\]]+)\\]', swift)
        require(table, 'Missing pinned runtime architecture table')
        architectures[role] = set(re.findall('"([a-z0-9_-]+)"', table[1]))
    for e in entries:
        require(re.fullmatch('[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', e['repo']), 'Invalid model catalog')
        require(e['role'] in architectures and e['architecture'] in architectures[e['role']], 'Invalid model catalog')
        require(e['license'] in policy and re.fullmatch('[0-9a-f]{40}', e['revision']), 'Invalid model catalog')
        require(1 <= len(e['bases']) <= 20, 'Invalid model catalog')
        for base in e['bases']:
            require(base['license'] in policy and re.fullmatch('[0-9a-f]{40}', base['revision']), 'Invalid model catalog')
            require('llama' not in base['repo'].lower() or base['repo'].startswith('HuggingFaceTB/'), 'Invalid model catalog')
        q = e['quantization']
        require(q['mode'] == 'affine' and q['bits'] in (2, 3, 4, 5, 6, 8) and (q['groupSize'] in (32, 64, 128)), 'Invalid model catalog')
        require(1 <= len(e['files']) <= 128, 'Invalid model catalog')
        require(len({f['path'] for f in e['files']}) == len(e['files']), 'Invalid model catalog')
        for f in e['files']:
            require(not Path(f['path']).is_absolute() and '..' not in Path(f['path']).parts, 'Invalid model catalog')
            require(f['path'].endswith('.safetensors') and re.fullmatch('[0-9a-f]{64}', f['sha256']), 'Invalid model catalog')
            require(0 < f['size'] < 1000000000000, 'Invalid model catalog')
        require(e['weightBytes'] == sum((f['size'] for f in e['files'])), 'Invalid model catalog')
    return entries
if __name__ == '__main__':
    entries = check(Path(__file__).resolve().parents[2])
    vision = sum((e['role'] == 'vision' for e in entries))
    print(f'Offline model catalog OK: {len(entries)} entries ({vision} vision, {len(entries) - vision} text).')
