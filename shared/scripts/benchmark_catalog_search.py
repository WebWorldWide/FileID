#!/usr/bin/env python3
"""Measure warm IPC lexical search on deterministic synthetic catalog fixtures."""
import argparse
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import platform
import sqlite3
import statistics
import tempfile
import time

from check_catalog_roundtrip import Engine


QUERIES = ("Family", "Alex baseball", "Grandma present", "Roof estimate", "Bucket42 baseball")
FILE_SQL = "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension,vlm_description) VALUES(?,?,?,100,10,0,?,?,?)"
CHAPTER_SQL = "INSERT INTO catalog_chapters(id,file_id,start_seconds,end_seconds,title,summary,source_revision,model_version,confidence,user_edited) VALUES(?,?,12.5,20,'Gift Opening','Grandma opens a birthday present','fixture','fixture',1,1)"
PASSAGE_SQL = "INSERT INTO catalog_passages(id,file_id,page,text,source_revision,model_version,confidence) VALUES(?,?,1,'Roof repair estimate and invoice','fixture','fixture',1)"
SCOPE = "Warm IPC lexical search only; synthetic names, descriptions, chapters, and page evidence. No semantic/person models or application background analysis. No index restore, scan quality, or UI measurement."


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def file_rows(files):
    for i in range(1, files + 1):
        document = i % 10 == 0
        extension = "pdf" if document else "mov"
        yield (i, f"/internal/Family Library/Bucket{i % 1000} - File {i}.{extension}", i,
               "doc" if document else "video", extension,
               "Roof repair estimate and invoice" if document else "Alex plays baseball and opens birthday presents")


def fixture_tables(files):
    return (
        ("files", FILE_SQL, file_rows(files)),
        ("catalog_chapters", CHAPTER_SQL, ((f"chapter-{i}", i) for i in range(1, files + 1, 100))),
        ("catalog_passages", PASSAGE_SQL, ((f"passage-{i}", i) for i in range(10, files + 1, 100))),
    )


def fixture_manifest(files):
    digest = hashlib.sha256()
    counts = {}
    for table, sql, rows in fixture_tables(files):
        digest.update((sql + "\n").encode("utf-8"))
        counts[table] = 0
        for row in rows:
            digest.update((json.dumps(row, ensure_ascii=True, separators=(",", ":")) + "\n").encode("utf-8"))
            counts[table] += 1
    identity = dict(generator="fileid-lexical-fixture-v1", rows_sha256=digest.hexdigest(),
                    row_counts=counts, queries=list(QUERIES), query_schedule="round-robin from index 0 in each phase")
    identity["manifest_sha256"] = hashlib.sha256(
        json.dumps(identity, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()
    return identity


def seed_fixture(directory, files):
    database = directory / "FileID/fileid.sqlite"
    if not database.is_file():
        raise RuntimeError("Engine did not create the fixture database")
    with closing(sqlite3.connect(database)) as db, db:
        db.execute("PRAGMA journal_mode=WAL")
        for table, sql, rows in fixture_tables(files):
            if db.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0] != 0:
                raise RuntimeError(f"Refusing to seed nonempty fixture table: {table}")
            db.executemany(sql, rows)


def summary(samples):
    return dict(median_ms=round(statistics.median(samples), 2),
                p95_ms=round(sorted(samples)[int(0.95 * (len(samples) - 1))], 2),
                maximum_ms=round(max(samples), 2))


def search_sample(engine, query, index):
    started = time.perf_counter()
    response = engine.request("search", query=query)
    elapsed_ms = (time.perf_counter() - started) * 1000
    # Required CatalogResponse/CatalogHit fields only; Engine owns correlation, extra keys are ignored.
    root_types = {"requestID": str, "status": str, "hits": list, "chapters": list, "jobs": list}
    if (not isinstance(response, dict)
            or any(type(response.get(field)) is not expected for field, expected in root_types.items())
            or response["status"] != "ok"):
        raise RuntimeError(f"Malformed or unsuccessful search response for {query}")
    hits = response["hits"]
    hit_types = {"fileID": int, "path": str, "kind": str, "text": str}
    if not hits or any(not isinstance(hit, dict) or any(
            type(hit.get(field)) is not expected for field, expected in hit_types.items()) for hit in hits):
        raise RuntimeError(f"Malformed or empty hits for {query}; no benchmark result accepted")
    return dict(index=index, query=query, elapsed_ms=elapsed_ms, hit_count=len(hits))


def run_trial(args, directory, number):
    started = time.perf_counter()
    engine = Engine(args.engine.resolve(), directory, swift=args.runtime == "swift")
    try:
        engine.wait("ready")
        ready_ms = (time.perf_counter() - started) * 1000
        first_query = search_sample(engine, QUERIES[0], 0)
        remaining_priming_samples = [search_sample(engine, query, index)
                                     for index, query in enumerate(QUERIES[1:], start=1)]
        for index in range(args.warmups):
            search_sample(engine, QUERIES[index % len(QUERIES)], index)
        samples = [search_sample(engine, QUERIES[index % len(QUERIES)], index)
                   for index in range(args.requests)]
    finally:
        engine.close()
    per_query = []
    for query in QUERIES:
        durations = [sample["elapsed_ms"] for sample in samples if sample["query"] == query]
        per_query.append(dict(query=query, sample_count=len(durations), **summary(durations)))
    return dict(trial=number, process_ready_ms=ready_ms, first_query=first_query,
                priming_count=len(QUERIES), remaining_priming_samples=remaining_priming_samples,
                warmup_count=args.warmups, query_count=len(QUERIES) + args.warmups + args.requests,
                sample_count=len(samples), samples=samples,
                per_query=per_query, **summary([sample["elapsed_ms"] for sample in samples]))


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine", type=Path, required=True)
    parser.add_argument("--runtime", choices=["swift", "rust"], required=True)
    parser.add_argument("--files", type=int, default=100_000)
    parser.add_argument("--requests", type=int, default=100, help="Warm samples per trial (10..10000)")
    parser.add_argument("--trials", type=int, default=3, help="Fresh engine processes (1..20)")
    parser.add_argument("--warmups", type=int, default=10, help="Additional excluded queries per trial (0..1000)")
    parser.add_argument("--build-profile", default="unknown", help="Caller-supplied; not inferred from binary path")
    parser.add_argument("--source-revision", default="unknown", help="Caller-supplied engine source identity")
    parser.add_argument("--conditions", default="unknown", help="Caller-supplied test conditions; no automatic process inventory")
    parser.add_argument("--output", type=Path, help="Optional new JSON path; existing paths are never overwritten")
    args = parser.parse_args(argv)
    if not args.engine.is_file():
        parser.error("Provide an engine binary")
    for name, minimum, maximum in (("files", 100, 1_000_000), ("requests", 10, 10_000),
                                   ("trials", 1, 20), ("warmups", 0, 1000)):
        if not minimum <= getattr(args, name) <= maximum:
            parser.error(f"--{name} must be between {minimum} and {maximum}")
    if args.trials * (len(QUERIES) + args.warmups + args.requests) > 100_000:
        parser.error("Total query count must not exceed 100000")
    for name in ("build_profile", "source_revision", "conditions"):
        if not 1 <= len(getattr(args, name)) <= 1024:
            parser.error(f"--{name.replace('_', '-')} must contain 1..1024 characters")
    if args.output is not None:
        if os.path.lexists(args.output):
            parser.error("Output path already exists; refusing reuse")
        if not args.output.parent.is_dir():
            parser.error("Output parent directory must already exist")
    return args


def benchmark(args):
    binary_hash = sha256_file(args.engine)
    harness = Path(__file__).resolve()
    identity = dict(sha256=sha256_file(harness), engine_helper_sha256=sha256_file(harness.with_name("check_catalog_roundtrip.py")))
    manifest = fixture_manifest(args.files)
    root = Path(tempfile.gettempdir()).resolve()
    if str(root).casefold() == "/volumes/adlon" or str(root).casefold().startswith("/volumes/adlon/"):
        raise RuntimeError("Synthetic fixtures must not be placed on Adlon")
    with tempfile.TemporaryDirectory(prefix="FileIDCatalogBenchmark-", dir=root) as temporary:
        directory = Path(temporary)
        if any(directory.iterdir()):
            raise RuntimeError("Refusing fixture directory reuse")
        engine = Engine(args.engine.resolve(), directory, swift=args.runtime == "swift")
        try:
            engine.wait("ready")
        finally:
            engine.close()
        seed_fixture(directory, args.files)
        trials = [run_trial(args, directory, number) for number in range(1, args.trials + 1)]
        if sha256_file(args.engine) != binary_hash:
            raise RuntimeError("Engine binary changed during benchmark; rejecting result")
        samples = [sample["elapsed_ms"] for trial in trials for sample in trial["samples"]]
        result = dict(
            schema_version=1, runtime=args.runtime, files=args.files, requests=args.requests,
            **summary(samples), scope=SCOPE,
            trial_count=args.trials, warmups=args.warmups, sample_count=len(samples), trials=trials,
            priming_queries_per_trial=len(QUERIES),
            query_count=args.trials * (len(QUERIES) + args.warmups + args.requests),
            engine=dict(sha256=binary_hash, build_profile=args.build_profile,
                        source_revision=args.source_revision, metadata_source="caller supplied; unverified"),
            harness=identity, fixture=manifest,
            host=dict(os=platform.system()[:256], release=platform.release()[:256],
                      machine=platform.machine()[:256], processor=platform.processor()[:256],
                      logical_cpu_count=os.cpu_count(), python_version=platform.python_version()),
            conditions=dict(caller_supplied=args.conditions, filesystem_cache="warm/unknown; not controlled",
                            engine_process="fresh for each trial; migration process excluded",
                            database="same deterministic seeded database reused across trials",
                            timing="perf_counter; IPC request round trip; validation excluded",
                            priming="one excluded execution per query per fresh process; includes first_query",
                            warm_samples="first query, remaining priming queries, and additional warmups excluded",
                            percentiles="sorted samples at floor(0.95 * (n - 1)); aggregate pools warm samples"),
        )
        serialized = json.dumps(result, indent=2, allow_nan=False)
    return serialized


def main(argv=None):
    args = parse_args(argv)
    serialized = benchmark(args)
    if args.output is not None:
        with args.output.open("x", encoding="utf-8") as stream:
            stream.write(serialized + "\n")
    print(serialized)


if __name__ == "__main__":
    main()
