#!/usr/bin/env python3
"""Score text search strength (good matches vs weaker look-alikes) against hand labels.

Runs one engine over a copy of a scanned catalog (CLIP image vectors already stored), sends
every labelled query the way the Mac Library does (hybrid search with the query words and a
client CLIP text vector) and scores the files the engine calls good matches.

    search_eval.py --engine <FileIDEngine> --flavor swift|rust --catalog <snapshot.sqlite>
                   [--vector-engine <Swift FileIDEngine>] [--modes hybrid semantic]

The client vector comes from `embedTextQuery` on `--vector-engine` (default: the engine under
test), so two engines can be compared with identical client vectors; the engine under test
still embeds its own "a photo of …" prompt for the strength judgement when the CLIP text model
is installed in the isolated home.

`<eval root>/search/labels.json`:

    {"photos": ["<event>/<file>", ...],
     "queries": [{"query": "dog", "split": "tune|heldout", "present": true,
                  "matches": ["<event>/<file>", ...], "ambiguous": [...]}]}

Ambiguous photos count neither for nor against a query. Precision is macro over present
queries with at least one good match; recall is macro over present queries; "found" is the share
of present queries with a good match; "none" is the share of absent queries answered with no
good match. Writes a JSON report beside the run; never touches the live catalog.
"""
from __future__ import annotations

import argparse
import json
import re
import shutil
import statistics
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from eval_common import EVAL_ROOT, Engine, database_path, diff_snapshots, live_state_snapshot, prepare_home, require_internal  # noqa: E402

GOOD = re.compile(r"(\d+) good match")


def good_count(message: str | None) -> int:
    found = GOOD.search(message or "")
    return int(found.group(1)) if found else 0


def embed(engine: Engine, text: str) -> list[float]:
    query_id = f"q-{time.monotonic_ns()}"
    engine.send({"embedTextQuery": {"query": text, "queryID": query_id}})
    while True:
        reply = engine.wait("clipTextEmbedding", 60)
        if reply.get("queryID") == query_id:
            return reply.get("embedding") or []


def clip_model(catalog: Path) -> str:
    import sqlite3
    with sqlite3.connect(f"file:{catalog}?mode=ro", uri=True) as db:
        row = db.execute("SELECT model FROM clip_embeddings GROUP BY model ORDER BY COUNT(*) DESC LIMIT 1").fetchone()
    if not row:
        raise SystemExit("the catalog has no CLIP vectors; scan it with the CLIP image model first")
    return row[0]


def score(queries, results, split):
    present = [q for q in queries if q["present"] and q["split"] == split]
    absent = [q for q in queries if not q["present"] and q["split"] == split]
    precision, recall, found = [], [], 0
    for q in present:
        good = [path for path in results[q["query"]] if path not in set(q["ambiguous"])]
        hits = len(set(good) & set(q["matches"]))
        if good:
            precision.append(hits / len(good))
        recall.append(hits / len(q["matches"]))
        found += hits > 0
    none = sum(1 for q in absent if not [p for p in results[q["query"]] if p not in set(q["ambiguous"])])
    false_good = sum(len([p for p in results[q["query"]] if p not in set(q["ambiguous"])]) for q in absent)
    return {
        "present": len(present), "absent": len(absent),
        "precision": round(statistics.fmean(precision), 3) if precision else None,
        "recall": round(statistics.fmean(recall), 3) if recall else None,
        "found": round(found / len(present), 3) if present else None,
        "absentNone": f"{none} of {len(absent)}",
        "absentFalseGood": false_good,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--engine", type=Path, required=True)
    parser.add_argument("--flavor", choices=["swift", "rust"], required=True)
    parser.add_argument("--catalog", type=Path, required=True, help="scanned snapshot.sqlite (copied, never opened in place)")
    parser.add_argument("--vector-engine", type=Path, help="engine that computes the client vector (default --engine)")
    parser.add_argument("--labels", type=Path, default=EVAL_ROOT / "search/labels.json")
    parser.add_argument("--work", type=Path, default=EVAL_ROOT / "search/runs")
    parser.add_argument("--home", type=Path, default=EVAL_ROOT / "home")
    parser.add_argument("--name", default=None)
    parser.add_argument("--modes", nargs="+", default=["hybrid", "semantic"], choices=["hybrid", "semantic"])
    args = parser.parse_args()

    labels = json.loads(args.labels.read_text())
    queries = labels["queries"]
    before = live_state_snapshot()
    home = prepare_home(args.home)
    models = home / "Library/Application Support/FileID/Models"
    env = {"FILEID_MODELS_DIR": str(models)}
    name = args.name or args.flavor
    work = require_internal(args.work, "work folder") / name
    shutil.rmtree(work, ignore_errors=True)
    database = database_path(work)
    database.parent.mkdir(parents=True)
    shutil.copy2(require_internal(args.catalog, "catalog"), database)
    model = clip_model(database)

    vectors: dict[str, list[float]] = {}
    vector_engine = Engine(args.vector_engine or args.engine, work / "vectors", env=env, home=home)
    try:
        vector_engine.wait("ready", 60)
        for q in queries:
            vectors[q["query"]] = embed(vector_engine, q["query"])
    finally:
        vector_engine.close()
    if any(not vector for vector in vectors.values()):
        raise SystemExit("the vector engine returned no CLIP text vector; install clip_text in the isolated home")

    report = {"engine": str(args.engine), "flavor": args.flavor, "catalog": args.catalog.name, "model": model,
              "queries": len(queries), "modes": {}}
    engine = Engine(args.engine, work, env=env, home=home, log=work / "engine.log")
    try:
        engine.wait("ready", 60)
        for mode in args.modes:
            results, raw, waited = {}, {}, []
            for q in queries:
                fields = dict(query=q["query"], searchMode=mode, queryVector=vectors[q["query"]], embeddingModel=model,
                              limit=100, resultScope="files")
                for attempt in range(120):
                    started = time.perf_counter()
                    response = engine.catalog("search", **fields)
                    waited.append((time.perf_counter() - started) * 1000)
                    if response["status"] == "ok" and "Preparing" not in (response.get("message") or ""):
                        break
                    time.sleep(1)
                else:
                    raise SystemExit(f"search for {q['query']!r} never became ready: {response.get('message')}")
                hits = [hit["path"] for hit in response["hits"]]
                good = good_count(response.get("message"))
                photos = {}
                for path in hits:
                    for photo in labels["photos"]:
                        if path.endswith("/" + photo):
                            photos[path] = photo
                results[q["query"]] = [photos.get(path, path) for path in hits[:good]]
                raw[q["query"]] = {"message": response.get("message"), "hits": [photos.get(p, p) for p in hits]}
            report["modes"][mode] = {
                "tune": score(queries, results, "tune"),
                "heldout": score(queries, results, "heldout"),
                "searchMs": {"median": round(statistics.median(waited), 1), "max": round(max(waited), 1)},
                "results": raw,
            }
    finally:
        engine.close()
    after = live_state_snapshot()
    report["liveStateChanged"] = diff_snapshots(before, after)
    out = work / "report.json"
    out.write_text(json.dumps(report, indent=1))
    for mode, values in report["modes"].items():
        print(mode, "tune", values["tune"], "heldout", values["heldout"], "ms", values["searchMs"])
    print("report", out)
    if report["liveStateChanged"]:
        print("WARNING live state changed:", report["liveStateChanged"][:5])
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
