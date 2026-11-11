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

  people_eval.py --engine <FileIDEngine>      # data in ~/FileIDDev/Eval/people
  people_eval.py --engine <FileIDEngine> --images <dir> --labels <labels.json> --work <dir>
  people_eval.py ... --name tight --env FILEID_FACE_PASS1_COS=0.70
  people_eval.py ... --incremental
  people_eval.py --engine <fileid-engine> --flavor rust --scan-flavor swift   # Rust clusters the Swift scan
  people_eval.py ... --scan-flavor swift --reembed   # ... after embedding every face itself

--split tune clusters only the photos of that half (the other half's faces are
removed from the scratch copy before clustering), so thresholds and suggestion
queues can be tuned on one half and checked on the other without people that
span both halves mixing the two.

--incremental replays a library that grows: the later half of each event's photos
(by image path) is held back, the rest is clustered, then the held-back faces come
back as new faces and clustering runs again. New faces are compared with the
existing people and queued for review (FILEID_FACE_REVIEW_AUTO_ASSIGN=1 assigns
confident ones instead). It reports whether each new face's top suggestion is its
labelled identity, per reason and split, and checks that the default run changed no
assignment.

Every run also scores the merge and split suggestion queues the run stored: a merge
suggestion is right when both people's labelled faces have the same majority identity;
a split suggestion is right when the group that stays and the group that would move
have different majority identities.
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
from eval_common import (EVAL_ROOT, Engine, database_path, diff_snapshots, iou,  # noqa: E402
                         live_state_snapshot, prepare_home, require_internal, sha256_file)

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


def run_scan(binary, images, state, env, timeout, home=None):
    if state.exists():
        shutil.rmtree(state)
    state.mkdir(parents=True)
    engine = Engine(binary, state, env=env, log=state / "engine.log", home=home)
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


PEOPLE_TABLES = ("persons", "face_verifications", "face_verdicts", "person_merge_links",
                 "face_review_candidates", "face_identity_constraints", "face_assignment_assessed",
                 "face_merge_suggestions", "face_split_suggestions", "face_queue_dismissals")


def tables_of(connection):
    return {row[0] for row in connection.execute("SELECT name FROM sqlite_master WHERE type='table'")}


def fresh_copy(snapshot, state, reembed=False):
    if state.exists():
        shutil.rmtree(state)
    database = database_path(state)
    database.parent.mkdir(parents=True)
    shutil.copy2(snapshot, database)
    connection = sqlite3.connect(database)
    tables = tables_of(connection)
    connection.execute("UPDATE face_prints SET person_id=NULL")
    if reembed:
        connection.execute("UPDATE face_prints SET arcface_embedding=NULL,embedding_model=NULL,"
                           "processing_version=NULL,source_revision=NULL")
        if "face_refresh_failures" in tables:
            connection.execute("DELETE FROM face_refresh_failures")
    for table in PEOPLE_TABLES:
        if table in tables:
            connection.execute(f"DELETE FROM {table}")
    return database, connection


def cluster_once(binary, state, env, timeout, home=None):
    engine = Engine(binary, state, env=env, log=state / "engine.log", home=home)
    try:
        engine.wait("ready", 180)
        started = time.monotonic()
        engine.send({"runFaceClustering": {}})
        cluster = engine.wait("faceClusteringComplete", timeout)
        return {"cluster": cluster, "clusterSeconds": round(time.monotonic() - started, 2)}
    finally:
        engine.close()


def recluster(binary, snapshot, state, env, timeout, home=None, drop_hashes=frozenset(), hash_cache=None,
              reembed=False):
    _, connection = fresh_copy(snapshot, state, reembed)
    if drop_hashes:
        hashes = file_hashes(connection, {} if hash_cache is None else hash_cache)
        dropped = [file_id for file_id, digest in hashes.items() if digest in drop_hashes]
        connection.executemany("DELETE FROM face_prints WHERE file_id=?", [(file_id,) for file_id in dropped])
    connection.commit()
    connection.close()
    return cluster_once(binary, state, env, timeout, home)


def later_half_hashes(labels):
    images = defaultdict(dict)
    for face in labels:
        images[face["event"]][face["image"]] = face["sha256"]
    held = set()
    for by_image in images.values():
        ordered = sorted(by_image)
        held.update(by_image[name] for name in ordered[(len(ordered) + 1) // 2:])
    return held


def file_hashes(connection, hash_cache):
    out = {}
    for file_id, path in connection.execute("SELECT id, path_text FROM files"):
        real = os.path.realpath(path)
        if real not in hash_cache and os.path.exists(real):
            hash_cache[real] = sha256_file(Path(real))
        out[file_id] = hash_cache.get(real)
    return out


def assignments(database):
    connection = sqlite3.connect(database)
    rows = dict(connection.execute("SELECT id, person_id FROM face_prints"))
    connection.close()
    return rows


def co_members(assigned, faces):
    groups = defaultdict(set)
    for face in faces:
        if assigned.get(face) is not None:
            groups[assigned[face]].add(face)
    return {face: frozenset(groups[assigned[face]]) if assigned.get(face) is not None else None for face in faces}


def incremental_run(binary, snapshot, state, env, timeout, labels, hash_cache, home=None):
    """Cluster the earlier half of each event, then bring the later half back as new faces."""
    database, connection = fresh_copy(snapshot, state)
    held_hashes = later_half_hashes(labels)
    hashes = file_hashes(connection, hash_cache)
    held_files = sorted(file_id for file_id, digest in hashes.items() if digest in held_hashes)
    marks = ",".join("?" * len(held_files)) or "NULL"
    connection.execute("DROP TABLE IF EXISTS eval_held_faces")
    connection.execute(f"CREATE TABLE eval_held_faces AS SELECT * FROM face_prints WHERE file_id IN ({marks})", held_files)
    held_faces = [row[0] for row in connection.execute("SELECT id FROM eval_held_faces")]
    connection.execute(f"DELETE FROM face_prints WHERE file_id IN ({marks})", held_files)
    connection.commit()
    connection.close()
    first = cluster_once(binary, state, env, timeout, home)
    before = assignments(database)
    connection = sqlite3.connect(database)
    connection.execute("INSERT INTO face_prints SELECT * FROM eval_held_faces")
    connection.execute("UPDATE face_prints SET person_id=NULL WHERE id IN (SELECT id FROM eval_held_faces)")
    if "face_assignment_assessed" in tables_of(connection):
        connection.execute("DELETE FROM face_assignment_assessed WHERE face_id IN (SELECT id FROM eval_held_faces)")
    connection.execute("DROP TABLE eval_held_faces")
    connection.commit()
    connection.close()
    second = cluster_once(binary, state, env, timeout, home)
    after = assignments(database)
    earlier = list(before)
    then, now_ = co_members(before, earlier), co_members(after, earlier)
    connection = sqlite3.connect(database)
    candidates = {face: (person, reason, score) for face, person, reason, score in connection.execute(
        "SELECT face_id, person_id, reason, score FROM face_review_candidates")}
    connection.close()
    return {
        "first": first, "second": second, "heldFiles": len(held_files), "heldFaces": len(held_faces),
        "heldHashes": sorted(held_hashes),
        "earlierFacesWhoseGroupChanged": sum(1 for face in earlier if then[face] != now_[face]),
        "newFacesAssigned": sum(1 for face in held_faces if after.get(face) is not None),
        "candidates": candidates,
    }


def suggestion_report(labels, assigned, held_hashes, candidates):
    """Top suggestion correct? A person's identity is the majority label of its labelled earlier faces."""
    identity_faces = [f for f in labels if f["identity"].startswith("P")]
    votes = defaultdict(Counter)
    for face in identity_faces:
        det = assigned.get(face["id"])
        if face["sha256"] not in held_hashes and det and det["person"] is not None:
            votes[det["person"]][face["identity"]] += 1
    person_identity = {person: counts.most_common(1)[0][0] for person, counts in votes.items()}
    known = set(person_identity.values())
    new = [f for f in identity_faces if f["sha256"] in held_hashes]
    detected = [f for f in new if f["id"] in assigned]
    by_reason = defaultdict(lambda: {"queued": 0, "correct": 0})
    for face in detected:
        det = assigned[face["id"]]
        found = candidates.get(det["id"])
        if not found:
            continue
        person, reason, _ = found
        by_reason[reason]["queued"] += 1
        by_reason[reason]["correct"] += int(person_identity.get(person) == face["identity"])
    queued = sum(v["queued"] for v in by_reason.values())
    correct = sum(v["correct"] for v in by_reason.values())
    eligible = [f for f in detected if f["identity"] in known]
    strong = by_reason.get("suggested", {"queued": 0, "correct": 0})
    def after_accepting(face):
        det = assigned.get(face["id"])
        if det is None:
            return None
        if det["person"] is not None:
            return det["person"]
        found = candidates.get(det["id"])
        return found[0] if found and found[1] == "suggested" else None

    accepted = [(f["identity"], after_accepting(f)) for f in identity_faces]
    return {
        "newIdentityFaces": len(new),
        "newDetected": len(detected),
        "newWithKnownIdentity": len(eligible),
        "queued": queued,
        "correct": correct,
        "precision": round(correct / queued, 4) if queued else None,
        "recall": round(correct / len(eligible), 4) if eligible else None,
        "suggestedPrecision": round(strong["correct"] / strong["queued"], 4) if strong["queued"] else None,
        "byReason": {reason: dict(values) for reason, values in sorted(by_reason.items())},
        "acceptingSuggested": cluster_metrics(accepted),
    }


def incremental_reports(labels, assigned, split, held_hashes, candidates):
    out = {"all": suggestion_report(labels, assigned, held_hashes, candidates)}
    for name, events in (split or {}).items():
        out[name] = suggestion_report([f for f in labels if f["event"] in events], assigned, held_hashes, candidates)
    return out


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


def queue_report(labels, assigned, database: Path):
    """Score the stored merge and split suggestions against the labels."""
    connection = sqlite3.connect(database)
    tables = tables_of(connection)
    merges = list(connection.execute("SELECT person_id, other_person_id, score FROM face_merge_suggestions")) \
        if "face_merge_suggestions" in tables else []
    splits = list(connection.execute("SELECT person_id, keep_json, split_json, score FROM face_split_suggestions")) \
        if "face_split_suggestions" in tables else []
    connection.close()
    identity_of_face = {}
    votes = defaultdict(Counter)
    for face in labels:
        det = assigned.get(face["id"])
        if det is None or not face["identity"].startswith("P"):
            continue
        identity_of_face[det["id"]] = face["identity"]
        if det["person"] is not None:
            votes[det["person"]][face["identity"]] += 1
    majority = {person: counts.most_common(1)[0][0] for person, counts in votes.items()}

    def group_identity(faces):
        counts = Counter(identity_of_face[face] for face in faces if face in identity_of_face)
        return counts.most_common(1)[0][0] if counts else None

    merge_block = {"suggested": len(merges), "labelled": 0, "correct": 0}
    for keep, other, _ in merges:
        if keep in majority and other in majority:
            merge_block["labelled"] += 1
            merge_block["correct"] += int(majority[keep] == majority[other])
    split_block = {"suggested": len(splits), "labelled": 0, "correct": 0}
    for _, keep, moved, _ in splits:
        stay, go = group_identity(json.loads(keep)), group_identity(json.loads(moved))
        if stay and go:
            split_block["labelled"] += 1
            split_block["correct"] += int(stay != go)
    for block in (merge_block, split_block):
        block["precision"] = round(block["correct"] / block["labelled"], 4) if block["labelled"] else None
    return {"merges": merge_block, "splits": split_block}


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
    parser.add_argument("--scan-flavor", choices=("swift", "rust"),
                        help="cluster the stored scan of this flavor (default: --flavor); "
                             "e.g. --flavor rust --scan-flavor swift re-clusters a Swift scan with the Rust engine")
    parser.add_argument("--images", type=Path, default=EVAL_ROOT / "people/images")
    parser.add_argument("--labels", type=Path, default=EVAL_ROOT / "people/labels.json")
    parser.add_argument("--work", type=Path, default=EVAL_ROOT / "people/runs",
                        help="scratch folder for catalogs and reports")
    parser.add_argument("--home", type=Path, default=EVAL_ROOT / "home",
                        help="isolated engine home holding copies of the installed models")
    parser.add_argument("--name", default="baseline")
    parser.add_argument("--env", action="append", help="KEY=VALUE engine override (repeatable)")
    parser.add_argument("--rescan", action="store_true", help="scan again even if a snapshot exists")
    parser.add_argument("--timeout", type=float, default=3600)
    parser.add_argument("--incremental", action="store_true", help="hold back the later half of each event, then add it as new faces")
    parser.add_argument("--split", help="cluster and score only this half of the labels (e.g. tune)")
    parser.add_argument("--reembed", action="store_true",
                        help="clear the scan's face vectors first, so the clustering engine embeds every face itself")
    args = parser.parse_args()

    images = require_internal(args.images, "images")
    work = require_internal(args.work, "work folder")
    home = prepare_home(args.home)
    labels_doc = json.loads(args.labels.read_text())
    labels = labels_doc["faces"]
    drop_hashes = frozenset()
    if args.split:
        events = set(labels_doc.get("split", {}).get(args.split) or [])
        if not events:
            raise SystemExit(f"no split named {args.split!r} in {args.labels}")
        drop_hashes = frozenset(f["sha256"] for f in labels if f["event"] not in events)
        labels = [f for f in labels if f["event"] in events]
        labels_doc = {**labels_doc, "faces": labels, "split": {args.split: sorted(events)}}
    env = parse_env(args.env)
    scan_env = {k: v for k, v in env.items() if k in SCAN_ENV_KEYS}
    scan_key = hashlib.sha256(json.dumps(scan_env, sort_keys=True).encode()).hexdigest()[:8] if scan_env else "default"
    scan_flavor = args.scan_flavor or args.flavor
    scan_state = work / f"{scan_flavor}-scan-{scan_key}"
    snapshot = scan_state / "snapshot.sqlite"

    before = live_state_snapshot()
    meta = {"name": args.name, "flavor": args.flavor, "scanFlavor": scan_flavor, "env": env, "split": args.split,
            "reembed": args.reembed}
    if scan_flavor != args.flavor and (args.rescan or not snapshot.exists()):
        raise SystemExit(f"no stored {scan_flavor} scan in {scan_state}; run that flavor first")
    if args.rescan or not snapshot.exists():
        meta["scanRun"] = run_scan(args.engine, images, scan_state, env, args.timeout, home)
        checkpoint(database_path(scan_state))
        shutil.copy2(database_path(scan_state), snapshot)
    state = work / f"{args.flavor}-{args.name}"
    hash_cache = {}
    incremental = None
    if args.incremental and args.split:
        raise SystemExit("--split and --incremental cannot be combined yet")
    if args.incremental:
        incremental = incremental_run(args.engine, snapshot, state, env, args.timeout, labels, hash_cache, home)
        meta["clusterRun"] = {key: incremental[key] for key in ("first", "second", "heldFiles", "heldFaces",
                                                                 "earlierFacesWhoseGroupChanged", "newFacesAssigned")}
    else:
        meta["clusterRun"] = recluster(args.engine, snapshot, state, env, args.timeout, home, drop_hashes, hash_cache,
                                       args.reembed)
    changes = diff_snapshots(before, live_state_snapshot())
    if changes:
        print("WARNING: the live FileID state changed during the run:", *changes, sep="\n  ", file=sys.stderr)
    meta["liveStateChanges"] = changes

    detections = load_detections(database_path(state), images, hash_cache)
    assigned = match(labels, detections)
    report = split_reports(labels, assigned, labels_doc.get("split"))
    if incremental:
        report["incremental"] = incremental_reports(labels, assigned, labels_doc.get("split"),
                                                    set(incremental["heldHashes"]), incremental["candidates"])
    report["queues"] = queue_report(labels, assigned, database_path(state))
    report["meta"] = meta
    connection = sqlite3.connect(database_path(state))
    report["meta"]["persons"] = connection.execute("SELECT COUNT(*) FROM persons").fetchone()[0]
    report["meta"]["engineFaces"] = connection.execute("SELECT COUNT(*) FROM face_prints").fetchone()[0]
    connection.close()
    out = work / f"report-{args.flavor}-{args.name}.json"
    out.write_text(json.dumps(report, indent=1))
    for key in ("all", *labels_doc.get("split", {}).keys()):
        print(summary_line(key, report[key]))
        if incremental:
            block = report["incremental"][key]
            accepted = block["acceptingSuggested"]
            print(f"{'':8s} new faces {block['newDetected']}: queued {block['queued']} correct {block['correct']} "
                  f"P {block['precision']} R {block['recall']} suggestedP {block['suggestedPrecision']} "
                  f"| accepting suggested: pairP {accepted.get('pairPrecision')} pairR {accepted.get('pairRecall')}")
    if incremental:
        print(f"earlier faces whose group changed: {incremental['earlierFacesWhoseGroupChanged']}, "
              f"new faces assigned: {incremental['newFacesAssigned']}")
    for name, block in report["queues"].items():
        print(f"{name} queue: {block['suggested']} suggested, {block['labelled']} labelled, "
              f"{block['correct']} right, P {block['precision']}")
    print(f"persons {report['meta']['persons']}, engine faces {report['meta']['engineFaces']}; report {out}")
    return 1 if changes else 0


if __name__ == "__main__":
    sys.exit(main())
