#!/usr/bin/env python3
"""Probe portable catalog indexing and reversible operations on disposable files."""
import argparse
from contextlib import ExitStack, closing
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import struct
import tempfile
import time
import uuid

from conformance import Engine

MODEL = 'clip-vitb32-openai-v1:126bacbb4038a0820ca6582bd81f58d7892070deab8317b390e37d7632a53471'
PNG = bytes.fromhex('89504e470d0a1a0a0000000d494844520000000200000002080600000072b60d240000001149444154789c63f87f86e13f0833c0180066120b29b97883690000000049454e44ae426082')


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def tool(engine, action, **fields):
    request_id = str(uuid.uuid4())
    engine.send({'toolRequest': {'request': {'requestID': request_id, 'action': action, **fields}}})
    return engine.wait('toolResponse', request_id)


def check_ok(response):
    assert response['status'] == 'ok', response
    return response


def prepare_visual(engine):
    request_id = str(uuid.uuid4())
    engine.send({'catalogRequest': {'request': {'requestID': request_id, 'action': 'search', 'fileID': 1, 'searchMode': 'semantic'}}})
    cold = engine.wait('catalogResponse', request_id)
    assert cold['status'] in ('ok', 'error'), cold
    deadline = time.monotonic() + 30
    while True:
        jobs = engine.request('jobs')['jobs']
        if any(job['kind'] == 'catalogIndex' and job['state'] == 'completed' for job in jobs):
            return
        assert time.monotonic() < deadline, jobs
        time.sleep(0.05)


def safe_parent(path):
    path = path.resolve(strict=True)
    forbidden = Path('/Volumes/Adlon')
    if path == forbidden or forbidden in path.parents:
        raise ValueError('Fixtures must never be written to Adlon')
    if not path.is_dir():
        raise ValueError('Choose an existing temporary fixture parent')
    return path


def run(binary, directory, exports):
    state = directory / 'state'
    library = directory / 'library'
    library.mkdir()
    photo = library / 'photo.png'
    photo.write_bytes(PNG)
    documents = [library / 'draft.txt', library / 'occupied.txt']
    for path in documents:
        path.write_bytes(b'original fixture document')
    originals = {path: digest(path) for path in [photo, *documents]}
    database_path = state / 'FileID' / 'fileid.sqlite'
    bootstrap = Engine(binary, state, swift=False)
    try:
        bootstrap.wait('ready')
    finally:
        bootstrap.close()
    with closing(sqlite3.connect(database_path)) as database, database:
        for file_id, path in enumerate([photo, *documents], start=1):
            database.execute('INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension,content_hash) VALUES(?,?,?, ?,?,0,?,?,?)',
                             (file_id, str(path), file_id, path.stat().st_size, path.stat().st_mtime,
                              'image' if file_id == 1 else 'doc', path.suffix[1:], bytes.fromhex(originals[path])))
        # These are explicit unit vectors for retrieval tests, not model inference results.
        for file_id, component in [(1, 0), (2, 0), (3, 1)]:
            vector = [0.0] * 512
            vector[component] = 1.0
            database.execute('INSERT INTO clip_embeddings(file_id,embedding,model) VALUES(?,?,?)',
                             (file_id, struct.pack('<512f', *vector), MODEL))
    engine = Engine(binary, state, swift=False)
    try:
        engine.wait('ready')
        prepare_visual(engine)
        hits = engine.request('search', fileID=1, searchMode='semantic')['hits']
        assert hits and hits[0]['fileID'] == 2 and all(hit['fileID'] != 1 for hit in hits), hits
        preview = check_ok(tool(engine, 'preview', fileIDs=[1], destination=str(exports),
                                recipe={'kind': 'photo', 'format': 'png', 'maxDimension': 16}))
        operation = preview['operationID']
        result = check_ok(tool(engine, 'execute', operationID=operation))
        output = Path(result['outputs'][0]['outputPath'])
        output_hash = digest(output)
        target = state / 'FileID' / 'ExportRecovery' / operation / ('0-' + output.name)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(output.read_bytes())
        collision = tool(engine, 'undo', operationID=operation)
        assert collision['status'] == 'error', collision
        assert output.is_file() and digest(target) == output_hash
        target.unlink()
        undone = check_ok(tool(engine, 'undo', operationID=operation))
        assert undone['outputs'][0]['state'] == 'undone', undone
        assert not output.exists() and digest(target) == output_hash
        assert digest(photo) == originals[photo]
        destination = library / 'Sorted'
        destination.mkdir()
        occupied = destination / documents[1].name
        occupied.write_bytes(b'unrelated occupant')
        moves = []
        for file_id, source in enumerate(documents, start=2):
            moves.append({'fileID': file_id, 'source': str(source), 'destination': str(destination / source.name),
                          'category': 'Sorted', 'confidence': 'review', 'sizeBytes': source.stat().st_size,
                          'contentHash': originals[source], 'modifiedAt': source.stat().st_mtime})
        engine.send({'applyRestructure': {'libraryRoot': str(library), 'moves': moves, 'useSymlinks': False}})
        applied = engine.wait('restructureApplyResult')
        assert applied['applied'] == 2 and applied['failed'] == 0, applied
        assert occupied.read_bytes() == b'unrelated occupant'
        assert not any(source.exists() for source in documents)
        engine.send({'undoRestructure': {'libraryRoot': str(library)}})
        restored = engine.wait('restructureApplyResult')
        assert restored['applied'] == 2 and restored['failed'] == 0, restored
        assert all(digest(path) == originals[path] for path in documents)
        assert occupied.read_bytes() == b'unrelated occupant'
    finally:
        engine.close()
    engine = Engine(binary, state, swift=False)
    try:
        engine.wait('ready')
        history = check_ok(tool(engine, 'history'))
        assert history['operationID'] == operation and history['outputs'][0]['state'] == 'undone', history
        with closing(sqlite3.connect(database_path)) as database:
            saved = database.execute('SELECT state,inverse_json FROM catalog_operations WHERE id=?', (operation,)).fetchone()
            assert saved[0] == 'undone' and json.loads(saved[1])[0]['output']['state'] == 'undone'
        prepare_visual(engine)
        assert engine.request('search', fileID=1, searchMode='semantic')['hits'][0]['fileID'] == 2
    finally:
        engine.close()
    assert all(digest(path) == value for path, value in originals.items())
    print(json.dumps({'catalogIndex': 'passed', 'recoveryCollision': 'passed', 'exportUndo': 'passed',
                      'restructureUndo': 'passed', 'restart': 'passed',
                      'crossDeviceRecovery': directory.stat().st_dev != exports.stat().st_dev}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--engine', type=Path, required=True)
    parser.add_argument('--fixture-parent', type=Path, default=Path(tempfile.gettempdir()))
    parser.add_argument('--export-parent', type=Path)
    args = parser.parse_args()
    binary = args.engine.resolve(strict=True)
    parent = safe_parent(args.fixture_parent)
    export_parent = safe_parent(args.export_parent or parent)
    with ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='FileIDParity-', dir=parent)))
        exports = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='FileIDExports-', dir=export_parent)))
        run(binary, directory, exports)


if __name__ == '__main__':
    main()
