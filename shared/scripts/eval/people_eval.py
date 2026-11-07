#!/usr/bin/env python3
"""People (face clustering) evaluation against hand labels.

Scans a folder of internal photo copies with one engine on a scratch catalog,
runs face clustering, matches the engine's faces to labelled faces by content
hash and box overlap, and reports:

  * detection recall: labelled identity faces the engine found;
  * pairwise precision/recall (wrong merges vs missed merges) and B-cubed F1,
    counting unassigned or undetected faces as singletons;
  * the same numbers for small faces, per event and per tune/held-out split.

The labels file and the photos stay outside the repository (see README.md).
Re-clustering experiments reuse one scan: the first run stores a snapshot of the
scanned catalog (face embeddings included); later runs copy it, clear People and
cluster again with the given environment overrides.

  people_eval.py --engine <FileIDEngine> --images <dir> --labels <labels.json> --work <dir>
  people_eval.py ... --name tight --env FILEID_FACE_PASS1_COS=0.70
"""
from __future__ import annotations

import argparse
from collections import Counter, defaultdict
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from eval_common import (Engine, database_path, diff_snapshots, iou, live_state_snapshot,  # noqa: E402
                         require_internal, sha256_file)

MATCH_IOU = 0.4
# A face is "small" when its shorter side is under this many pixels on a
# 1536 px long-edge decode, the macOS scan's working size and the size the
# shared quality gate (40 px) is measured on.
SMALL_PX = 40.0
SCAN_ENV_KEYS = ("FILEID_SCAN_MAX_PIXELS",)


def parse_env(pairs):
    env = {}
    for pair in pairs or []:
        key, _, value = pair.partition("=")
        if not key or not _:
            raise SystemExit(f"--env needs KEY=VALUE, got {pair!r}")
        env[key] = value
    return env


def engine_box(bbox: str, width: int, height: int):
    """Engine box -> normalized top-left [x, y, w, h] on the upright image.
    macOS stores Vision's normalized bottom-left "x,y,w,h"; the shared Rust
    engine stores JSON pixels with a top-left origin."""
    text = bbox.strip()
    if text.startswith("{"):
        obj = json.loads(text)
        sw = float(obj.get("sourceWidth") or width)
        sh = float(obj.get("sourceHeight") or height)
        if obj.get("orientation") not in (None, 1):
            raise ValueError("raw-orientation boxes are not supported by the evaluator yet")
        return [obj["x"] / sw, obj["y"] / sh, obj["w"] / sw, obj["h"] / sh]
    x, y, w, h = (float(v) for v in text.split(","))
    return [x, 1.0 - y - h, w, h]


def run_scan(binary, images, state, env, timeout):
    if state.exists():
        shutil.rmtree(state)
    state.mkdir(parents=True)
    engine = Engine(binary, state, env=env, log=state / "engine.log")
    try:
        engine.wait("ready", 180)
        started = time.monotonic()
        engine.send({"startScan": {"rootPath": str(images)}})
        scan = engine.wait("scanComplete", timeout)
        scan_seconds = time.monotonic() - started
        engine.send({"runFaceClustering": {}})
        cluster = engine.wait("faceClusteringComplete", timeout)
        return {"scan": scan, "scanSeconds": round(scan_seconds, 2), "cluster": cluster}
    finally:
        engine.close()


def checkpoint(database: Path):
    connection = sqlite3.connect(database)
    connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    connection.close()


def recluster(binary, snapshot, state, env, timeout):
    if state.exists():
        shutil.rmtree(state)
    database = database_path(state)
    database.parent.mkdir(parents=True)
    shutil.copy2(snapshot, database)
    connection = sqlite3.connect(database)
    tables = {row[0] for row in connection.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    connection.execute("UPDATE face_prints SET person_id=NULL")
    for table in ("persons", "face_verifications", "face_verdicts", "person_merge_links"):
        if table in tables:
            connection.execute(f"DELETE FROM {table}")
    connection.commit()
    connection.close()
    engine = Engine(binary, state, env=env, log=state / "engine.log")
    try:
        engine.wait("ready", 180)
        started = time.monotonic()
        engine.send({"runFaceClustering": {}})
        cluster = engine.wait("faceClusteringComplete", timeout)
        return {"cluster": cluster, "clusterSeconds": round(time.monotonic() - started, 2)}
    finally:
        engine.close()


def load_detections(database: Path, images: Path, hash_cache: dict):
    connection = sqlite3.connect(database)
    columns = {row[1] for row in connection.execute("PRAGMA table_info(face_prints)")}
    quality = "p.face_quality" if "face_quality" in columns else "NULL"
    excluded = "p.excluded" if "excluded" in columns else "0"
    rows = connection.execute(
        f"SELECT p.id, f.path_text, p.bbox, p.person_id, {quality}, {excluded} "
        "FROM face_prints p JOIN files f ON f.id = p.file_id").fetchall()
    connection.close()
    by_hash = defaultdict(list)
    for face_id, path, bbox, person, q, ex in rows:
        real = os.path.realpath(path)
        digest = hash_cache.get(real)
        if digest is None:
            digest = hash_cache[real] = sha256_file(Path(real))
        by_hash[digest].append({"id": face_id, "bbox": bbox, "person": person, "quality": q, "excluded": ex})
    return by_hash


def match(labels, detections):
    """Greedy one-to-one matching by IoU within each photo."""
    by_hash = defaultdict(list)
    for face in labels:
        by_hash[face["sha256"]].append(face)
    assigned = {}
    for digest, faces in by_hash.items():
        found = detections.get(digest, [])
        pairs = []
        for i, face in enumerate(faces):
            for j, det in enumerate(found):
                try:
                    box = engine_box(det["bbox"], face["width"], face["height"])
                except (ValueError, KeyError):
                    continue
                overlap = iou(face["bbox"], box)
                if overlap >= MATCH_IOU:
                    pairs.append((overlap, i, j))
        used_l, used_d = set(), set()
        for overlap, i, j in sorted(pairs, reverse=True):
            if i in used_l or j in used_d:
                continue
            used_l.add(i)
            used_d.add(j)
            assigned[faces[i]["id"]] = found[j]
    return assigned


def small(face) -> bool:
    scale = 1536.0 / max(face["width"], face["height"])
    return min(face["bbox"][2] * face["width"], face["bbox"][3] * face["height"]) * scale < SMALL_PX


def cluster_metrics(items):
    """items: [(true identity, predicted cluster or None)]. Unassigned faces are
    singletons. Returns pairwise precision/recall and B-cubed P/R/F1."""
    n = len(items)
    if n == 0:
        return {"faces": 0}
    predicted = [p if p is not None else ("single", i) for i, (_, p) in enumerate(items)]
    truth = [t for t, _ in items]
    same_true = same_pred = both = 0
    by_pred = defaultdict(list)
    by_true = defaultdict(list)
    for i in range(n):
        by_pred[predicted[i]].append(i)
        by_true[truth[i]].append(i)
    pair_counts = Counter((truth[i], predicted[i]) for i in range(n))
    both = sum(c * (c - 1) // 2 for c in pair_counts.values())
    same_pred = sum(len(v) * (len(v) - 1) // 2 for v in by_pred.values())
    same_true = sum(len(v) * (len(v) - 1) // 2 for v in by_true.values())
    bp = sum(pair_counts[(truth[i], predicted[i])] / len(by_pred[predicted[i]]) for i in range(n)) / n
    br = sum(pair_counts[(truth[i], predicted[i])] / len(by_true[truth[i]]) for i in range(n)) / n
    impure = sum(1 for members in by_pred.values() if len({truth[i] for i in members}) > 1)
    return {
        "faces": n,
        "pairPrecision": round(both / same_pred, 4) if same_pred else None,
        "pairRecall": round(both / same_true, 4) if same_true else None,
        "wrongMergePairs": same_pred - both,
        "missedMergePairs": same_true - both,
        "bcubedPrecision": round(bp, 4),
        "bcubedRecall": round(br, 4),
        "bcubedF1": round(2 * bp * br / (bp + br), 4) if bp + br else 0.0,
        "mixedClusters": impure,
        "assigned": sum(1 for _, p in items if p is not None),
    }


def evaluate(labels, assigned):
    identity = [f for f in labels if f["identity"].startswith("P")]

    def block(faces):
        detected = [f for f in faces if f["id"] in assigned]
        items_all = [(f["identity"], assigned[f["id"]]["person"] if f["id"] in assigned else None) for f in faces]
        items_det = [(f["identity"], assigned[f["id"]]["person"]) for f in detected]
        return {
            "labelled": len(faces),
            "detected": len(detected),
            "detectionRecall": round(len(detected) / len(faces), 4) if faces else None,
            "all": cluster_metrics(items_all),
            "detectedOnly": cluster_metrics(items_det),
        }

    report = {"overall": block(identity)}
    report["small"] = block([f for f in identity if small(f)])
    report["notSmall"] = block([f for f in identity if not small(f)])
    for event in sorted({f["event"] for f in identity}):
        report.setdefault("events", {})[event] = block([f for f in identity if f["event"] == event])
    unsure = [f for f in labels if f["identity"] == "unsure"]
    nonface = [f for f in labels if f["identity"] == "nonface"]
    report["unsure"] = {"labelled": len(unsure), "detected": sum(1 for f in unsure if f["id"] in assigned),
                        "assigned": sum(1 for f in unsure if f["id"] in assigned and assigned[f["id"]]["person"] is not None)}
    report["nonface"] = {"labelled": len(nonface), "detected": sum(1 for f in nonface if f["id"] in assigned),
                         "assigned": sum(1 for f in nonface if f["id"] in assigned and assigned[f["id"]]["person"] is not None)}
    return report


def split_reports(labels, assigned, split):
    out = {"all": evaluate(labels, assigned)}
    for name, events in (split or {}).items():
        out[name] = evaluate([f for f in labels if f["event"] in events], assigned)
    return out


def summary_line(name, block):
    a = block["overall"]["all"]
    s = block["small"]
    return (f"{name:8s} faces {block['overall']['labelled']:3d} det {block['overall']['detectionRecall']:.3f} "
            f"pairP {a['pairPrecision'] or 0:.3f} pairR {a['pairRecall'] or 0:.3f} B3F1 {a['bcubedF1']:.3f} "
            f"mixed {a['mixedClusters']} | small n={s['labelled']} det {s['detectionRecall'] or 0:.3f} "
            f"B3F1 {s['all'].get('bcubedF1', 0):.3f}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--engine", required=True, type=Path, help="FileIDEngine or fileid-engine binary")
    parser.add_argument("--flavor", choices=("swift", "rust"), default="swift")
    parser.add_argument("--images", required=True, type=Path)
    parser.add_argument("--labels", required=True, type=Path)
    parser.add_argument("--work", required=True, type=Path, help="scratch folder for catalogs and reports")
    parser.add_argument("--name", default="baseline")
    parser.add_argument("--env", action="append", help="KEY=VALUE engine override (repeatable)")
    parser.add_argument("--rescan", action="store_true", help="scan again even if a snapshot exists")
    parser.add_argument("--timeout", type=float, default=3600)
    args = parser.parse_args()

    images = require_internal(args.images, "images")
    work = require_internal(args.work, "work folder")
    labels_doc = json.loads(args.labels.read_text())
    labels = labels_doc["faces"]
    env = parse_env(args.env)
    scan_env = {k: v for k, v in env.items() if k in SCAN_ENV_KEYS}
    scan_key = hashlib.sha256(json.dumps(scan_env, sort_keys=True).encode()).hexdigest()[:8] if scan_env else "default"
    scan_state = work / f"{args.flavor}-scan-{scan_key}"
    snapshot = scan_state / "snapshot.sqlite"

    before = live_state_snapshot()
    meta = {"name": args.name, "flavor": args.flavor, "env": env}
    if args.rescan or not snapshot.exists():
        meta["scanRun"] = run_scan(args.engine, images, scan_state, env, args.timeout)
        checkpoint(database_path(scan_state))
        shutil.copy2(database_path(scan_state), snapshot)
    state = work / f"{args.flavor}-{args.name}"
    meta["clusterRun"] = recluster(args.engine, snapshot, state, env, args.timeout)
    changes = diff_snapshots(before, live_state_snapshot())
    if changes:
        print("WARNING: the live FileID state changed during the run:", *changes, sep="\n  ", file=sys.stderr)
    meta["liveStateChanges"] = changes

    detections = load_detections(database_path(state), images, {})
    assigned = match(labels, detections)
    report = split_reports(labels, assigned, labels_doc.get("split"))
    report["meta"] = meta
    connection = sqlite3.connect(database_path(state))
    report["meta"]["persons"] = connection.execute("SELECT COUNT(*) FROM persons").fetchone()[0]
    report["meta"]["engineFaces"] = connection.execute("SELECT COUNT(*) FROM face_prints").fetchone()[0]
    connection.close()
    out = work / f"report-{args.flavor}-{args.name}.json"
    out.write_text(json.dumps(report, indent=1))
    for key in ("all", *labels_doc.get("split", {}).keys()):
        print(summary_line(key, report[key]))
    print(f"persons {report['meta']['persons']}, engine faces {report['meta']['engineFaces']}; report {out}")
    return 1 if changes else 0


if __name__ == "__main__":
    sys.exit(main())
