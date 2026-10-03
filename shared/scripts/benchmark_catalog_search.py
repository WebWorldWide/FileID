#!/usr/bin/env python3
"""Measure warm IPC lexical search on synthetic internal-drive catalog fixtures."""
import argparse
import json
from pathlib import Path
import sqlite3
import statistics
import tempfile
import time

from check_catalog_roundtrip import Engine


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine", type=Path, required=True)
    parser.add_argument("--runtime", choices=["swift", "rust"], required=True)
    parser.add_argument("--files", type=int, default=100_000)
    parser.add_argument("--requests", type=int, default=100)
    args = parser.parse_args()
    if not args.engine.is_file() or args.files < 100 or args.requests < 10:
        parser.error("Provide an engine binary, at least 100 files, and at least 10 requests")
    root = Path(tempfile.gettempdir()).resolve()
    if str(root).casefold() == "/volumes/adlon" or str(root).casefold().startswith("/volumes/adlon/"):
        raise RuntimeError("Synthetic fixtures must not be placed on Adlon")
    with tempfile.TemporaryDirectory(prefix="FileIDCatalogBenchmark-", dir=root) as temporary:
        directory = Path(temporary)
        engine = Engine(args.engine.resolve(), directory, swift=args.runtime == "swift")
        try:
            engine.wait("ready")
        finally:
            engine.close()
        with sqlite3.connect(directory / "FileID/fileid.sqlite") as db:
            db.execute("PRAGMA journal_mode=WAL")
            db.executemany("INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension,vlm_description) VALUES(?,?,?,100,10,0,?,?,?)", (
                (i, f"/internal/Family Library/Bucket{i % 1000} - File {i}.{'pdf' if i % 10 == 0 else 'mov'}", i, "doc" if i % 10 == 0 else "video", "pdf" if i % 10 == 0 else "mov", "Roof repair estimate and invoice" if i % 10 == 0 else "Alex plays baseball and opens birthday presents")
                for i in range(1, args.files + 1)
            ))
            db.executemany("INSERT INTO catalog_chapters(id,file_id,start_seconds,end_seconds,title,summary,source_revision,model_version,confidence,user_edited) VALUES(?,?,12.5,20,'Gift Opening','Grandma opens a birthday present','fixture','fixture',1,1)", (
                (f"chapter-{i}", i) for i in range(1, args.files + 1, 100)
            ))
            db.executemany("INSERT INTO catalog_passages(id,file_id,page,text,source_revision,model_version,confidence) VALUES(?,?,1,'Roof repair estimate and invoice','fixture','fixture',1)", (
                (f"passage-{i}", i) for i in range(10, args.files + 1, 100)
            ))
        queries = ["Family", "Alex baseball", "Grandma present", "Roof estimate", "Bucket42 baseball"]
        engine = Engine(args.engine.resolve(), directory, swift=args.runtime == "swift")
        try:
            engine.wait("ready")
            for query in queries:
                assert engine.request("search", query=query)["hits"], f"No hits for {query}"
            samples = []
            for index in range(args.requests):
                started = time.perf_counter()
                response = engine.request("search", query=queries[index % len(queries)])
                samples.append((time.perf_counter() - started) * 1000)
                assert response["hits"]
        finally:
            engine.close()
    result = dict(
        runtime=args.runtime, files=args.files, requests=args.requests,
        median_ms=round(statistics.median(samples), 2),
        p95_ms=round(sorted(samples)[int(0.95 * (len(samples) - 1))], 2),
        maximum_ms=round(max(samples), 2),
        scope="Warm IPC lexical search only; synthetic names, descriptions, chapters, and page evidence. No semantic/person models or application background analysis.",
    )
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
