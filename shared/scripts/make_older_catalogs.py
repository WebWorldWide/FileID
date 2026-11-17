#!/usr/bin/env python3
"""Fixtures of catalogs the Swift engine wrote at earlier schema versions.

The Swift engine's migrations (platforms/apple/engine/.../Storage/Database.swift) are
append-only, so a library written by an older build is a catalog at an earlier version of
that list. This script copies the migrator verbatim out of Database.swift (and the generated
CatalogSchema.swift it reads), compiles it against the GRDB the Swift package already built,
runs it with GRDB's own `migrate(upTo:)` to each version asked for, puts a few
representative rows in (files, tags, text, faces, and the tables a version has), and writes
one fixture folder per version with conformance.py's capture code, so a case can start the
Rust engine on it (shared/test-corpus/conformance/swift-written.json).

    make_older_catalogs.py --swift-build <SwiftPM scratch path of a built platforms/apple> \\
        --out shared/test-corpus/swift-written --version 20 --version 21 --version 28 --version 35

The fixtures are named older-catalog-vNN. Needs swiftc and a finished `swift build` there; it
makes no network request and writes only below a temporary folder and --out. The rows are
inserted by SQL, not by a scan, because no Swift engine of those versions exists to run.
"""
from __future__ import annotations

import argparse
import hashlib
import re
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import conformance as c  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
STORAGE = ROOT / "platforms/apple/engine/Sources/FileIDEngine/Storage"

MAIN = """import Foundation
import GRDB

let arguments = CommandLine.arguments
var config = Configuration()
config.prepareDatabase { db in
    try db.execute(sql: "PRAGMA journal_mode = WAL")
    try db.execute(sql: "PRAGMA synchronous = NORMAL")
}
let pool = try DatabasePool(path: arguments[1], configuration: config)
let migrator = SwiftMigrations.migrator
try migrator.migrate(pool, upTo: arguments[2])
print(try pool.read { try migrator.appliedIdentifiers($0).count })
"""


def identifiers():
    return re.findall(r'registerMigration\("(v(\d+)_[a-z0-9_]+)"', (STORAGE / "Database.swift").read_text(encoding="utf-8"))


def build_tool(swift_build, folder):
    source = (STORAGE / "Database.swift").read_text(encoding="utf-8")
    start = source.index("    static var migrator: DatabaseMigrator {")
    end = source.index("        return m\n    }", start) + len("        return m\n    }")
    (folder / "Migrator.swift").write_text("import GRDB\n\nenum SwiftMigrations {\n" + source[start:end] + "\n}\n", encoding="utf-8")
    (folder / "main.swift").write_text(MAIN, encoding="utf-8")
    sources = [folder / "Migrator.swift", STORAGE / "CatalogSchema.swift", folder / "main.swift"]
    build = Path(swift_build)
    subprocess.run(["swiftc", "-Onone", "-I", str(build / "debug"), "-I", str(build / "checkouts/GRDB.swift/Sources/GRDBSQLite"),
                    *map(str, sources), str(build / "debug/GRDB.o"), "-lsqlite3", "-o", str(folder / "make_catalog")], check=True)
    return folder / "make_catalog"


def columns(connection, table):
    return [row[1] for row in connection.execute(f'PRAGMA table_info("{table}")')]


def insert(connection, table, row):
    """INSERT the keys of `row` that the table has at this version; a table that is not there yet is skipped."""
    have = columns(connection, table)
    if not have:
        return
    values = {key: value for key, value in row.items() if key in have}
    connection.execute(f'INSERT INTO "{table}"({",".join(values)}) VALUES({",".join("?" * len(values))})', list(values.values()))


def populate(ctx, connection):
    pictures = ctx.root / "Pictures"
    pictures.mkdir()
    documents = ctx.root / "Documents"
    documents.mkdir()
    entries = [
        (1, pictures / "Picture.png", c.png_bytes(16, 8), "image", "png", 1700000060),
        (2, documents / "Notes.txt", b"Quarterly notes from the generated library\n", "doc", "txt", 1700000120),
        (3, documents / "Report.pdf", c.pdf_bytes("Generated report"), "doc", "pdf", 1700000180),
    ]
    for file_id, path, data, kind, extension, mtime in entries:
        path.write_bytes(data)
        c.os.utime(path, (mtime, mtime))
        stat = path.stat()
        insert(connection, "files", {
            "id": file_id, "path_text": str(path), "path_hash": c.path_hash(str(path), ctx.os), "size_bytes": stat.st_size,
            "created_at": float(mtime), "modified_at": float(mtime), "scanned_at": 1700000300.0, "kind": kind,
            "extension": extension, "phash": 4671237767210991616 if kind == "image" else None,
            "has_faces": 1 if kind == "image" else 0, "has_text": 1 if kind == "doc" else 0,
            "camera_model": "Generated Camera" if kind == "image" else None,
            "content_hash": hashlib.sha256(data).digest(), "file_ref": stat.st_ino,
            "path_search": str(path), "text_stage_done": 1 if kind == "doc" else 0,
            "vlm_description": "A generated picture" if kind == "image" else None,
        })
    for file_id, tag, source, score in ((1, "outdoor", "clip", 0.91), (1, "generated", "user", None), (2, "notes", "auto", 0.5)):
        insert(connection, "tags", {"file_id": file_id, "tag": tag, "source": source, "score": score})
    insert(connection, "ocr_text", {"file_id": 2, "text": "Quarterly notes from the generated library"})
    insert(connection, "persons", {"id": 1, "name": "Alex", "first_name": "Alex", "file_count": 1, "created_at": 1700000200.0,
                                   "representative_face_id": 1, "is_unknown": 0})
    insert(connection, "face_prints", {"id": 1, "file_id": 1, "person_id": 1, "print_data": b"\x00\x01\x02\x03",
                                       "bbox": "0.1,0.1,0.4,0.4", "face_quality": 0.8})
    insert(connection, "scan_sessions", {"id": "generated-scan", "root_path": str(ctx.root), "started_at": 1700000290.0,
                                         "completed_at": 1700000300.0, "total_files": 3, "status": "completed"})
    insert(connection, "restructure_feedback", {"token": "picture", "folder": "Pictures", "weight": 2})
    insert(connection, "catalog_revisions", {"file_id": 2, "revision": "generated-revision", "processing_version": "generated", "updated_at": 1700000300.0})
    insert(connection, "catalog_chapters", {"id": "generated-chapter", "file_id": 3, "start_seconds": 0.0, "end_seconds": 1.0,
                                            "title": "Generated chapter", "summary": "", "source_revision": "generated",
                                            "model_version": "user", "confidence": 1.0, "user_edited": 1, "stale": 0})
    insert(connection, "library_roots", {"path": str(ctx.root), "display": "Generated library", "added_at": 1700000290.0,
                                         "last_scan_at": 1700000300.0, "last_status": "completed"})
    insert(connection, "take_quality", {"file_id": 1, "source_revision": "generated", "version": "generated",
                                        "signals_json": "{}", "computed_at": 1700000300.0})
    connection.commit()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--swift-build", type=Path, required=True, help="SwiftPM scratch path of a built platforms/apple (has debug/GRDB.o)")
    parser.add_argument("--out", type=Path, required=True, help="folder that gets one older-catalog-vNN folder per version")
    parser.add_argument("--version", type=int, action="append", required=True)
    parser.add_argument("--fixture-parent", type=Path, default=Path(tempfile.gettempdir()))
    args = parser.parse_args()
    by_version = {int(number): name for name, number in identifiers()}
    parent = c.safe_parent(args.fixture_parent)
    with tempfile.TemporaryDirectory(prefix="FileIDOlder-", dir=parent) as work:
        tool = build_tool(args.swift_build, Path(work))
        for version in args.version:
            with tempfile.TemporaryDirectory(prefix="FileIDConformance-", dir=parent) as temporary:
                ctx = c.Context({"name": f"older-catalog-v{version}"}, "swift", c.host_os(), Path(temporary))
                ctx.database.parent.mkdir(parents=True)
                subprocess.run([str(tool), str(ctx.database), by_version[version]], check=True, capture_output=True)
                connection = sqlite3.connect(ctx.database)
                try:
                    populate(ctx, connection)
                finally:
                    connection.close()
                target = args.out / f"older-catalog-v{version}"
                c.capture_fixture(ctx, {"name": f"older-catalog-v{version}"}, target, c.repo_copies())
                print(f"wrote {target} ({by_version[version]})")


if __name__ == "__main__":
    main()
