#!/usr/bin/env python3
"""Score scan tags against the labelled subjects of the private search set.

The search labels (``search/labels.json``) say, for 72 subjects, which of the
499 people-set photos show them. A photo counts as tagged with a subject when
one of its stored tags is in the subject's accepted-tag list below, so the
numbers are a lower bound on recall (RAM++ may say "gown" where we accept
"wedding dress") and an upper bound on precision (an accepted tag on a
non-labelled photo may still be right). Photos the labeller marked ambiguous
for a subject are left out of that subject's counts.

  tag_eval.py --engine <fileid-engine>                 # scan, then score
  tag_eval.py --engine <fileid-engine> --wide          # store every candidate >= 0.3 (for offline rule sweeps)
  tag_eval.py --db <fileid.sqlite>                     # score an existing catalog
  tag_eval.py --db <fileid.sqlite> --simulate          # re-select tags from a wide catalog with the engine rules

Every engine run gets an isolated state and home (see eval_common.py).
"""
from __future__ import annotations

import argparse
import collections
import json
import os
from pathlib import Path
import shutil
import sqlite3
import struct
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
import eval_common  # noqa: E402
from eval_common import EVAL_ROOT, Engine, database_path, diff_snapshots, live_state_snapshot, require_internal  # noqa: E402

# Subject -> tags that count as naming it (lowercase, RAM++ vocabulary words).
ACCEPT = {
    "wedding dress": ["wedding dress", "bride", "wedding gown", "gown", "bridal gown"],
    "church altar": ["altar", "church", "chapel", "cathedral"],
    "flower bouquet": ["bouquet", "flower bouquet", "flower arrangement", "bridal bouquet"],
    "christmas tree": ["christmas tree"],
    "wrapped presents": ["gift", "present", "gift bag", "gift box", "gift wrap", "wrapping paper", "christmas gift"],
    "birthday cake": ["birthday cake", "cake", "cupcake", "candle"],
    "balloons": ["balloon"],
    "baseball game": ["baseball", "baseball game", "baseball player", "baseball field", "softball", "baseball team", "ballpark", "baseball stadium"],
    "baseball bat": ["baseball bat", "bat"],
    "baseball cap": ["baseball cap", "cap"],
    "stadium": ["stadium", "baseball stadium", "ballpark", "bleacher", "grandstand", "football stadium"],
    "playground slide": ["slide", "playground slide"],
    "picnic table": ["picnic table"],
    "parked cars": ["car", "parking lot", "parking", "parked car", "vehicle", "suv", "minivan", "van", "truck", "pickup truck"],
    "pond": ["pond", "lake"],
    "ocean": ["ocean", "sea", "beach", "shore", "coast", "seashore", "wave"],
    "sand": ["sand", "beach", "sandy beach"],
    "pumpkin": ["pumpkin", "halloween pumpkin", "pumpkin patch", "jack-o'-lantern", "jack o lantern"],
    "hay bales": ["hay", "hay bale", "haystack", "straw", "bale"],
    "apple orchard": ["orchard", "apple tree", "apple", "apple orchard"],
    "eyeglasses": ["glasses", "eyeglasses", "spectacles", "eyeglass"],
    "sunglasses": ["sunglasses"],
    "necktie": ["tie", "necktie", "bow tie"],
    "baby": ["baby", "infant", "newborn"],
    "stroller": ["stroller", "baby carriage", "pram", "baby stroller"],
    "teddy bear": ["teddy", "teddy bear", "stuffed animal", "stuffed toy", "plush"],
    "couch": ["couch", "sofa", "loveseat"],
    "television": ["television", "tv", "flat screen", "television room"],
    "pizza": ["pizza"],
    "dinner table": ["dining table", "dinner table", "dining room", "dinner", "dining"],
    "cowboy hat": ["cowboy hat", "cowboy"],
    "american football": ["football", "american football", "football player", "football helmet", "football field"],
    "running race": ["race", "running", "marathon", "runner", "run", "jog", "track"],
    "classroom": ["classroom", "lecture", "student"],
    "clouds": ["cloud", "cloudy", "storm cloud", "cloudy sky", "overcast"],
    "sunset": ["sunset", "dusk", "sundown", "twilight"],
    "night": ["night", "night sky", "nighttime", "evening", "night view", "night light"],
    "autumn leaves": ["autumn", "fall", "autumn leaves", "foliage", "leaf", "autumn leaf", "fall foliage"],
    "brick wall": ["brick wall", "brick", "brick building"],
    "helmet": ["helmet", "football helmet", "bicycle helmet", "batting helmet", "hard hat", "safety helmet"],
    "backpack": ["backpack", "rucksack", "knapsack"],
    "hoodie": ["hoodie", "sweatshirt", "hood", "hooded sweatshirt"],
    "red dress": ["red dress"],
    "fireworks": ["firework", "fireworks", "firework display"],
    "american flag": ["american flag", "flag", "flag of the united states", "us flag", "stars and stripes"],
    "carousel": ["carousel", "merry-go-round", "merry go round"],
    "statue": ["statue", "sculpture", "monument", "bust"],
    "fountain": ["fountain", "water fountain"],
    "horse": ["horse", "pony", "foal", "mare", "stallion"],
    "snow": ["snow", "snowy", "winter", "snowfall", "snowman"],
    "dinosaur": ["dinosaur", "t-rex", "tyrannosaurus"],
    "swimming pool": ["swimming pool", "pool", "swimming"],
    "tent": ["tent", "camping", "campsite"],
    "stage performance": ["stage", "performance", "concert", "performer", "theater", "theatre"],
    "microphone": ["microphone", "mic"],
    "marching band": ["marching band", "band", "parade", "drum", "trumpet"],
    "dog": ["dog", "puppy", "pup", "hound", "retriever", "labrador"],
    "cat": ["cat", "kitten", "kitty"],
    "airplane": ["airplane", "plane", "aircraft", "airliner", "jet"],
    "skateboard": ["skateboard", "skateboarder", "skateboarding"],
    "bicycle": ["bicycle", "bike", "cyclist", "cycling"],
    "motorcycle": ["motorcycle", "motorbike", "motorcyclist"],
    "boat": ["boat", "ship", "sailboat", "kayak", "yacht", "rowboat", "fishing boat"],
    "mountains": ["mountain", "mountain range", "peak", "alp"],
    "laptop": ["laptop", "notebook computer", "computer"],
    "piano": ["piano", "keyboard", "pianist", "grand piano"],
    "guitar": ["guitar", "guitarist", "acoustic guitar", "electric guitar"],
    "umbrella": ["umbrella", "parasol"],
    "train": ["train", "locomotive", "railroad", "railway", "steam train", "train station"],
    "cow": ["cow", "cattle", "calf", "bull", "dairy cow"],
    "tractor": ["tractor"],
    "goat": ["goat", "kid goat", "billy goat"],
}

NOISE = {"image", "photo", "photograph", "photography", "picture", "face", "catch", "stand", "sit", "lay",
         "pose", "wear"}
MAC_EXTRA_SUPPRESS = {"person", "people", "hair", "head", "hand", "play", "game", "make", "fill", "dark",
                      "blue", "red", "white", "black", "green", "brown", "yellow", "pink", "gray", "grey", "purple"}
PEOPLE_WORDS = {"man", "woman", "boy", "girl", "child", "couple"}
REDUNDANT_WITH = {"goggles": "sunglasses"}


def load_labels(path: Path) -> dict:
    return json.loads(path.read_text())


def run_scan(binary: Path, images: Path, state: Path, env: dict, timeout: float, home: Path) -> dict:
    if state.exists():
        shutil.rmtree(state)
    state.mkdir(parents=True)
    engine = Engine(binary, state, env=env, log=state / "engine.log", home=home)
    try:
        engine.wait("ready", 300)
        started = time.monotonic()
        engine.send({"startScan": {"rootPath": str(images)}})
        scan = engine.wait("scanComplete", timeout)
        return {"scan": scan, "scanSeconds": round(time.monotonic() - started, 2)}
    finally:
        engine.close()


def read_catalog(db: Path, images: Path):
    """relative photo path -> {"tags": [(tag, score)], "faces": n, "clip": [f32]}"""
    connection = sqlite3.connect(db)
    connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    photos = {}
    prefix = str(images.resolve()).rstrip("/") + "/"
    rows = connection.execute("SELECT id, path_text, has_faces FROM files WHERE kind='image'")
    by_id = {}
    for file_id, path_text, has_faces in rows:
        real = os.path.realpath(path_text)
        rel = real[len(prefix):] if real.startswith(prefix) else path_text
        entry = {"tags": [], "faces": 0, "has_faces": bool(has_faces), "clip": None}
        photos[rel] = entry
        by_id[file_id] = entry
    for file_id, tag, score in connection.execute("SELECT file_id, tag, score FROM tags WHERE source='auto'"):
        if file_id in by_id:
            by_id[file_id]["tags"].append((tag, score))
    for file_id, n in connection.execute("SELECT file_id, count(*) FROM face_prints GROUP BY file_id"):
        if file_id in by_id:
            by_id[file_id]["faces"] = n
    try:
        for file_id, blob in connection.execute("SELECT file_id, embedding FROM clip_embeddings"):
            if file_id in by_id and blob and len(blob) % 4 == 0:
                by_id[file_id]["clip"] = struct.unpack(f"<{len(blob) // 4}f", blob)
    except sqlite3.OperationalError:
        pass
    connection.close()
    for entry in photos.values():
        entry["tags"].sort(key=lambda t: -(t[1] or 0))
    return photos


def score(photos: dict, labels: dict, split: str) -> dict:
    """Per-subject and pooled precision / recall of the stored tags."""
    all_photos = [p for p in labels["photos"] if p in photos]
    missing = [p for p in labels["photos"] if p not in photos]
    per_query = []
    tp = fp = fn = 0
    for query in labels["queries"]:
        if split != "all" and query["split"] != split:
            continue
        accept = set(ACCEPT.get(query["query"], [query["query"]]))
        matches = set(query["matches"])
        ambiguous = set(query.get("ambiguous", []))
        q_tp = q_fp = q_fn = 0
        for photo in all_photos:
            if photo in ambiguous:
                continue
            tagged = any(tag.lower() in accept for tag, _ in photos[photo]["tags"])
            relevant = photo in matches
            if tagged and relevant:
                q_tp += 1
            elif tagged:
                q_fp += 1
            elif relevant:
                q_fn += 1
        tp += q_tp
        fp += q_fp
        fn += q_fn
        per_query.append({"query": query["query"], "split": query["split"], "present": query["present"],
                          "tp": q_tp, "fp": q_fp, "fn": q_fn,
                          "precision": q_tp / (q_tp + q_fp) if q_tp + q_fp else None,
                          "recall": q_tp / (q_tp + q_fn) if q_tp + q_fn else None})
    precision = tp / (tp + fp) if tp + fp else 0.0
    recall = tp / (tp + fn) if tp + fn else 0.0
    f1 = 2 * precision * recall / (precision + recall) if precision + recall else 0.0
    counts = [len(photos[p]["tags"]) for p in all_photos]
    content = [len([t for t, _ in photos[p]["tags"] if not t.startswith("Year_")]) for p in all_photos]
    return {"split": split, "photos": len(all_photos), "missing": len(missing), "tp": tp, "fp": fp, "fn": fn,
            "precision": round(precision, 4), "recall": round(recall, 4), "f1": round(f1, 4),
            "tagsPerPhoto": round(sum(counts) / max(1, len(counts)), 2),
            "contentTagsPerPhoto": round(sum(content) / max(1, len(content)), 2),
            "perQuery": per_query}


def print_report(report: dict, verbose: bool):
    print(f"[{report['split']}] photos={report['photos']} missing={report['missing']} "
          f"P={report['precision']:.3f} R={report['recall']:.3f} F1={report['f1']:.3f} "
          f"(tp={report['tp']} fp={report['fp']} fn={report['fn']}) "
          f"tags/photo={report['tagsPerPhoto']} content/photo={report['contentTagsPerPhoto']}")
    if verbose:
        for q in sorted(report["perQuery"], key=lambda q: (q["precision"] or 0) + (q["recall"] or 0)):
            p = "-" if q["precision"] is None else f"{q['precision']:.2f}"
            r = "-" if q["recall"] is None else f"{q['recall']:.2f}"
            print(f"  {q['split']:8} {q['query']:20} P={p:5} R={r:5}  tp={q['tp']:3} fp={q['fp']:3} fn={q['fn']:3}")


def tag_frequencies(photos: dict, top: int = 40):
    counter = collections.Counter()
    for entry in photos.values():
        for tag, _ in entry["tags"]:
            counter[tag] += 1
    return counter.most_common(top)


def load_thresholds(models: Path):
    tags = [l.strip() for l in (models / "ram_plus/ram_plus_tags.txt").read_text().splitlines() if l.strip()]
    thresholds = [float(l) for l in (models / "ram_plus/ram_plus_thresholds.txt").read_text().split()]
    assert len(tags) == len(thresholds), (len(tags), len(thresholds))
    return {t.lower(): th for t, th in zip(tags, thresholds)}


def load_label_matrix(path: Path, tags_path: Path):
    tags = [l.strip() for l in tags_path.read_text().splitlines() if l.strip()]
    data = path.read_bytes()
    dim = len(data) // 4 // len(tags)
    floats = struct.unpack(f"<{len(data) // 4}f", data)
    return {tags[i].lower(): floats[i * dim:(i + 1) * dim] for i in range(len(tags))}


def refine(hits):
    """Mac RamPlusService.refine: drop a head word when a two-word tag ends in it,
    drop redundant pairs, keep one people word."""
    names = {tag.lower() for tag, _ in hits}
    heads = {name.rsplit(" ", 1)[1] for name in names if " " in name}
    kept_people = False
    out = []
    for tag, s in hits:
        name = tag.lower()
        if " " not in name and name in heads:
            continue
        if REDUNDANT_WITH.get(name) in names:
            continue
        if name in PEOPLE_WORDS:
            if kept_people:
                continue
            kept_people = True
        out.append((tag, s))
    return out


def simulate(photos: dict, thresholds: dict, rules: dict, matrix: dict | None):
    """Re-select tags from a wide catalog the way the engine would."""
    floor = rules.get("floor", 0.62)
    cap = rules.get("max_tags", 8)
    thr_delta = rules.get("thr_delta", 0.0)
    thr_cap = rules.get("thr_cap", 1.0)
    blend = rules.get("clip_blend")  # {"a": slope, "center": float | "image"}
    import math
    logit = lambda p: math.log(max(p, 1e-6) / max(1 - p, 1e-6))
    sigmoid = lambda z: 1 / (1 + math.exp(-z))
    suppress = set(NOISE)
    if rules.get("mac_suppress"):
        suppress |= MAC_EXTRA_SUPPRESS
    out = {}
    for photo, entry in photos.items():
        hits = []
        candidates = [(tag, s) for tag, s in entry["tags"] if s is not None and not tag.startswith("Year_")]
        center = None
        if blend and matrix is not None and entry["clip"] is not None:
            cos_all = [sum(a * b for a, b in zip(matrix[t.lower()], entry["clip"])) for t, _ in candidates if t.lower() in matrix]
            if blend["center"] == "image":
                cos_all.sort()
                center = cos_all[len(cos_all) // 2] if cos_all else None
            else:
                center = blend["center"]
        for tag, s in candidates:
            name = tag.lower()
            if name in suppress:
                continue
            if center is not None and name in matrix:
                cos = sum(a * b for a, b in zip(matrix[name], entry["clip"]))
                s = sigmoid(logit(s) + blend["a"] * (cos - center))
            cut = max(min(thresholds.get(name, 0.68), thr_cap) - thr_delta, floor)
            if s < cut:
                continue
            hits.append((tag, s))
        hits.sort(key=lambda t: -t[1])
        if rules.get("no_face_people") and entry["faces"] == 0:
            hits = [(t, s) for t, s in hits if t.lower() not in PEOPLE_WORDS]
        if rules.get("refine"):
            hits = refine(hits)
        if matrix is not None and entry["clip"] is not None and rules.get("clip_veto") is not None:
            veto = rules["clip_veto"]
            kept = []
            for tag, s in hits:
                row = matrix.get(tag.lower())
                cos = sum(a * b for a, b in zip(row, entry["clip"])) if row else None
                if cos is not None and cos < veto["cosine"] and s < veto["below"]:
                    continue
                kept.append((tag, s))
            hits = kept
        hits = hits[:cap]
        out[photo] = {"tags": hits, "faces": entry["faces"], "has_faces": entry["has_faces"], "clip": entry["clip"]}
    return out


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--engine", type=Path, help="fileid-engine binary (Rust)")
    parser.add_argument("--db", type=Path, help="score an existing catalog instead of scanning")
    parser.add_argument("--images", type=Path, default=EVAL_ROOT / "people/images")
    parser.add_argument("--labels", type=Path, default=EVAL_ROOT / "search/labels.json")
    parser.add_argument("--work", type=Path, default=EVAL_ROOT / "tags/runs")
    parser.add_argument("--home", type=Path, default=EVAL_ROOT / "home")
    parser.add_argument("--name", default="run")
    parser.add_argument("--timeout", type=float, default=7200)
    parser.add_argument("--wide", action="store_true", help="store every RAM++ candidate >= 0.3")
    parser.add_argument("--env", action="append", default=[], help="KEY=VALUE for the engine")
    parser.add_argument("--simulate", action="store_true", help="re-select tags from a wide catalog")
    parser.add_argument("--rules", default="{}", help="JSON rules for --simulate")
    parser.add_argument("--matrix", type=Path, help="label CLIP matrix (.f32) for clip_veto rules")
    parser.add_argument("--split", choices=("tune", "heldout", "all"), default="all")
    parser.add_argument("--verbose", action="store_true")
    parser.add_argument("--dump", type=Path, help="write per-photo tags as JSON")
    args = parser.parse_args()

    labels = load_labels(args.labels)
    images = require_internal(args.images, "images folder")
    env = dict(kv.split("=", 1) for kv in args.env)
    if args.wide:
        env.update({"FILEID_RAMPLUS_THRESHOLD": "0.3", "FILEID_RAMPLUS_PRECISION_FLOOR": "0.3",
                    "FILEID_RAMPLUS_MAX_TAGS": "64", "FILEID_TAG_CAP": "64"})
    if args.db:
        db = args.db
    else:
        if not args.engine:
            parser.error("--engine or --db is required")
        state = require_internal(args.work / args.name, "work folder")
        before = live_state_snapshot()
        result = run_scan(args.engine, images, state, env, args.timeout, args.home)
        changed = diff_snapshots(before, live_state_snapshot())
        if changed:
            print("WARNING: live state changed:", changed)
        print(f"scan: {result['scanSeconds']} s, {json.dumps(result['scan'])[:300]}")
        db = database_path(state)
    photos = read_catalog(db, images)
    if args.simulate:
        models = args.home / "models"
        matrix = load_label_matrix(args.matrix, models / "ram_plus/ram_plus_tags.txt") if args.matrix else None
        photos = simulate(photos, load_thresholds(models), json.loads(args.rules), matrix)
    for split in (("tune", "heldout", "all") if args.split == "all" else (args.split,)):
        print_report(score(photos, labels, split), args.verbose and split == "all")
    print("top tags:", ", ".join(f"{t}×{n}" for t, n in tag_frequencies(photos, 30)))
    if args.dump:
        args.dump.write_text(json.dumps({p: {"tags": e["tags"], "faces": e["faces"]} for p, e in photos.items()},
                                        indent=0))


if __name__ == "__main__":
    main()
