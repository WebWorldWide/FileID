#!/usr/bin/env python3
"""Prove explicit People merges are atomic and preserve names in both engines."""
import argparse
from pathlib import Path
import sqlite3
import tempfile
from check_catalog_roundtrip import Engine


def snapshot(path):
    with sqlite3.connect(path) as db:
        return (
            db.execute("SELECT id,name,first_name,is_unknown,representative_face_id,file_count FROM persons ORDER BY id").fetchall(),
            db.execute("SELECT id,person_id FROM face_prints ORDER BY id").fetchall(),
        )


def verify(binary, swift, mode):
    root = Path(tempfile.gettempdir()).resolve()
    if str(root).casefold().startswith(("/volumes/adlon", "/srv/data")) or any(part.casefold() == "adlon" for part in root.parts):
        raise RuntimeError("Merge fixtures must remain internal")
    with tempfile.TemporaryDirectory(prefix="FileIDPeopleMerge-", dir=root) as temporary:
        directory = Path(temporary)
        engine = Engine(binary.resolve(), directory, swift=swift)
        try:
            engine.wait("ready")
        finally:
            engine.close()
        path = directory / "FileID/fileid.sqlite"
        with sqlite3.connect(path) as db:
            db.execute("INSERT INTO persons(id,created_at,is_unknown) VALUES(1,123,1)")
            db.execute("INSERT INTO persons(id,first_name,created_at) VALUES(2,'Alex',123)")
            for face in (1, 2):
                db.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(?,?,?,1,0,'image','jpg')", (face, str(directory / f"{face}.jpg"), face))
                db.execute("INSERT INTO face_prints(id,file_id,person_id,print_data,bbox) VALUES(?,?,?,X'00','[0,0,1,1]')", (face, face, face))
            if mode == "named":
                db.execute("UPDATE persons SET first_name='Grandma',is_unknown=0 WHERE id=1")
            if mode == "empty":
                db.execute("UPDATE persons SET first_name='' WHERE id=1")
            if mode == "rollback":
                db.execute("CREATE TRIGGER fixture_fail BEFORE DELETE ON persons WHEN OLD.id=2 BEGIN SELECT RAISE(ABORT,'fixture failure'); END")
        before = snapshot(path)
        engine = Engine(binary.resolve(), directory, swift=swift)
        try:
            engine.wait("ready")
            target = 99 if mode == "missing" else 1
            engine.send({"mergeClusters": {"sourcePersonID": 99 if mode == "missingSource" else 2, "destinationPersonID": target}})
            result = engine.wait("bulkActionResult")
            if mode in ("rollback", "missing", "missingSource"):
                assert result["failed"] == 1, (mode, result)
                assert snapshot(path) == before, (mode, "partial merge committed")
            else:
                assert result["succeeded"] == 1 and result["failed"] == 0, result
                people, faces = snapshot(path)
                assert people == [(1, None, "Grandma" if mode == "named" else "Alex", 0, 1, 2)], people
                assert faces == [(1, 1), (2, 1)], faces
        finally:
            engine.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--swift-engine", type=Path, required=True)
    parser.add_argument("--rust-engine", type=Path, required=True)
    parser.add_argument("--mode", choices=("all", "transfer", "rollback", "missing", "missingSource", "named", "empty"), default="all")
    parser.add_argument("--platform", choices=("both", "swift", "rust"), default="both")
    args = parser.parse_args()
    for binary, swift in ((args.swift_engine, True), (args.rust_engine, False)):
        if args.platform != "both" and swift != (args.platform == "swift"):
            continue
        for mode in (("transfer", "rollback", "missing", "missingSource", "named", "empty") if args.mode == "all" else (args.mode,)):
            verify(binary, swift, mode)
    print("People merge name transfer, missing selection and forced-failure rollback passed")


if __name__ == "__main__":
    main()
