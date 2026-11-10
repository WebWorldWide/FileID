#!/usr/bin/env python3
"""Timeline (significant moments) evaluation against hand-labelled intervals.

Two modes, both on scratch catalogs inside the evaluation folder:

  candidates  Model-free. Decodes each video's frame pool with the engine's own
              pool worker (--sample-video-pool), embeds the pool frames with the
              engine's CLIP through a photo scan, then applies the scene-pool
              rules (scene cuts by CLIP cosine, activity windows by luma change;
              a port of VideoSceneSelection.swift / video_scene_selection.rs)
              with the given thresholds. Fast enough to sweep.
  full        End to end. Scans the videos, loads an installed visual model (no
              download is ever started: the run aborts if one begins), runs the
              engine's "moments" timeline job and reads the scenes, activity
              windows and moment chapters it stored.

Reports interval recall at IoU >= 0.3, cut timestamp error in seconds, precision
of proposed moments, the share of proposals inside labelled quiet spans, and the
fast actions (<= 1.5 s) that were never examined or never proposed.

  timeline_eval.py candidates --engine <FileIDEngine>   # data in ~/FileIDDev/Eval/timeline
  timeline_eval.py candidates --engine <FileIDEngine> --videos <dir> --labels <labels.json> --work <dir>
  timeline_eval.py full --engine <FileIDEngine> --videos <dir> --labels <labels.json> --work <dir> --model qwen3_vl_2b
"""
from __future__ import annotations

import argparse
from collections import defaultdict
import json
import math
import os
from pathlib import Path
import shutil
import sqlite3
import statistics
import struct
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from eval_common import (EVAL_ROOT, Engine, database_path, diff_snapshots,  # noqa: E402
                         engine_environment, live_state_snapshot, prepare_home, require_internal,
                         sha256_file)

MODEL_REPOS = {"qwen3_vl_2b": "mlx-community/Qwen3-VL-2B-Instruct-4bit",
               "qwen3_vl_4b": "lmstudio-community/Qwen3-VL-4B-Instruct-MLX-4bit"}

IOU = 0.3


class Rules:
    """Scene-pool thresholds; defaults mirror VideoSceneSelection."""

    def __init__(self, **overrides):
        self.pool_interval = 0.5
        self.cut_cosine = 0.85
        self.drift_cosine = 0.70
        self.confirm_frames = 2
        self.min_scene_seconds = 2.0
        self.max_scene_seconds = 20.0
        self.activity_change = 0.02
        self.activity_pad_seconds = 0.5
        self.merge_gap_seconds = 1.0
        self.max_window_seconds = 2.5
        self.max_window_frames = 6
        for key, value in overrides.items():
            if not hasattr(self, key):
                raise SystemExit(f"unknown rule {key}")
            setattr(self, key, type(getattr(self, key))(value))

    def as_dict(self):
        return dict(vars(self))


def cosine(a, b):
    dot = sum(x * y for x, y in zip(a, b))
    na = math.sqrt(sum(x * x for x in a))
    nb = math.sqrt(sum(y * y for y in b))
    return dot / (na * nb) if na > 0 and nb > 0 else 0.0


def scenes(frames, duration, rules: Rules):
    """frames: [(seconds, embedding, change)] -> [(start, end)]."""
    if not frames:
        return []
    bounds = [0]
    total = list(frames[0][1])
    pending = 0
    for i in range(1, len(frames)):
        start = bounds[-1]
        emb = frames[i][1]
        similar = (len(emb) == len(total) and cosine(emb, total) >= rules.cut_cosine
                   and cosine(emb, frames[start][1]) >= rules.drift_cosine)
        length = frames[i][0] - frames[start][0]
        if length >= rules.max_scene_seconds:
            bounds.append(i)
            total = list(emb)
            pending = 0
            continue
        if not similar and length >= rules.min_scene_seconds:
            pending += 1
            if pending >= rules.confirm_frames:
                first = i - rules.confirm_frames + 1
                bounds.append(first)
                total = list(frames[first][1])
                for j in range(first + 1, i + 1):
                    total = [t + e for t, e in zip(total, frames[j][1])]
                pending = 0
                continue
        else:
            pending = 0
        total = [t + e for t, e in zip(total, emb)]
    bounds.append(len(frames))
    out = []
    for lower, upper in zip(bounds, bounds[1:]):
        end = frames[upper][0] if upper < len(frames) else duration
        out.append((0.0 if lower == 0 else frames[lower][0], end))
    return out


def activity_windows(frames, duration, rules: Rules):
    if len(frames) < 2:
        return []
    spans = []
    for i in range(1, len(frames)):
        if frames[i][2] >= rules.activity_change:
            start = max(0.0, frames[i - 1][0] - rules.activity_pad_seconds)
            end = min(duration, frames[i][0] + rules.activity_pad_seconds)
            if spans and start - spans[-1][1] <= rules.merge_gap_seconds:
                spans[-1][1] = max(spans[-1][1], end)
            else:
                spans.append([start, end])
    windows = []
    seconds = [f[0] for f in frames]
    for span_start, span_end in spans:
        pieces = max(1, math.ceil((span_end - span_start) / rules.max_window_seconds))
        length = (span_end - span_start) / pieces
        for piece in range(pieces):
            start = span_start + piece * length
            end = span_end if piece == pieces - 1 else start + length
            times = [s for s in seconds if start <= s <= end]
            if len(times) < 2:
                nearest = sorted(seconds, key=lambda s: abs(s - start))[:2]
                times = sorted(set(times + nearest))
            if len(times) < 2:
                continue
            windows.append((min(start, times[0]), max(end, times[-1])))
    return windows


def luma_change(a, b):
    if len(a) != len(b) or not a:
        return 0.0
    return sum(abs(x - y) for x, y in zip(a, b)) / (len(a) * 255)


def iou(a, b):
    inter = max(0.0, min(a[1], b[1]) - max(a[0], b[0]))
    union = max(a[1], b[1]) - min(a[0], b[0])
    return inter / union if union > 0 else 0.0


def inside(proposal, label):
    length = proposal[1] - proposal[0]
    if length <= 0:
        return 0.0
    return max(0.0, min(proposal[1], label[1]) - max(proposal[0], label[0])) / length


def score_video(video, proposals):
    """proposals: {"moments": [(s,e)], "scenes": [(s,e)], "windows": [(s,e)]}"""
    positives = [i for i in video["intervals"] if i["kind"] != "quiet"]
    quiet = [i for i in video["intervals"] if i["kind"] == "quiet"]
    out = {}
    for name, spans in proposals.items():
        matched = []
        for label in positives:
            box = (label["start"], label["end"])
            best = max(spans, key=lambda p: iou(p, box), default=None)
            hit = best is not None and iou(best, box) >= IOU
            matched.append({"label": label, "hit": hit,
                            "startError": abs(best[0] - box[0]) if hit else None,
                            "endError": abs(best[1] - box[1]) if hit else None})
        precise = sum(1 for p in spans if any(iou(p, (l["start"], l["end"])) >= IOU for l in positives))
        within = sum(1 for p in spans if any(inside(p, (l["start"], l["end"])) >= 0.5 for l in positives))
        in_quiet = sum(1 for p in spans if any(inside(p, (q["start"], q["end"])) >= 0.5 for q in quiet))
        out[name] = {"matched": matched, "proposals": len(spans), "precise": precise, "within": within, "inQuiet": in_quiet}
    boundaries = sorted({s for s, _ in proposals.get("scenes", [])} - {0.0})
    out["cutErrors"] = [min((abs(c - b) for b in boundaries), default=None) for c in video.get("cuts", [])]
    examined = proposals.get("windows", [])
    out["fast"] = []
    for label in positives:
        if not label.get("fast"):
            continue
        box = (label["start"], label["end"])
        out["fast"].append({
            "label": label["label"], "video": video["id"], "start": label["start"],
            "examined": any(min(w[1], box[1]) - max(w[0], box[0]) > 0 for w in examined),
            "proposed": any(iou(p, box) >= IOU for p in proposals.get("moments", [])),
        })
    return out


def aggregate(scored):
    report = {}
    names = sorted({k for s in scored for k in s if k not in ("cutErrors", "fast")})
    for name in names:
        rows = [m for s in scored for m in s[name]["matched"]]
        by_kind = defaultdict(list)
        for m in rows:
            by_kind[m["label"]["kind"]].append(m)
        proposals = sum(s[name]["proposals"] for s in scored)
        starts = [m["startError"] for m in rows if m["hit"]]
        report[name] = {
            "proposals": proposals,
            "recall": round(sum(m["hit"] for m in rows) / len(rows), 3) if rows else None,
            "recallByKind": {k: round(sum(m["hit"] for m in v) / len(v), 3) for k, v in by_kind.items()},
            "precisionIoU": round(sum(s[name]["precise"] for s in scored) / proposals, 3) if proposals else None,
            "precisionWithin": round(sum(s[name]["within"] for s in scored) / proposals, 3) if proposals else None,
            "inQuiet": round(sum(s[name]["inQuiet"] for s in scored) / proposals, 3) if proposals else None,
            "medianStartError": round(statistics.median(starts), 2) if starts else None,
        }
    cuts = [c for s in scored for c in s["cutErrors"] if c is not None]
    labelled_cuts = sum(len(s["cutErrors"]) for s in scored)
    report["cuts"] = {
        "labelled": labelled_cuts,
        "medianError": round(statistics.median(cuts), 2) if cuts else None,
        "within0_5": round(sum(1 for c in cuts if c <= 0.5) / labelled_cuts, 3) if labelled_cuts else None,
        "within1_0": round(sum(1 for c in cuts if c <= 1.0) / labelled_cuts, 3) if labelled_cuts else None,
    }
    fast = [f for s in scored for f in s["fast"]]
    report["fast"] = {"labelled": len(fast), "notExamined": [f"{f['video']}@{f['start']}" for f in fast if not f["examined"]],
                      "notProposed": [f"{f['video']}@{f['start']}" for f in fast if not f["proposed"]]}
    return report


def split_report(labels_doc, scored_by_id):
    out = {}
    groups = {"all": [v["id"] for v in labels_doc["videos"]]}
    for name, events in labels_doc.get("split", {}).items():
        groups[name] = [v["id"] for v in labels_doc["videos"] if v["event"] in events]
    for name, ids in groups.items():
        out[name] = aggregate([scored_by_id[i] for i in ids if i in scored_by_id])
    return out


def video_paths(videos_dir: Path, labels_doc):
    by_hash = {}
    for path in sorted(videos_dir.iterdir()):
        if path.is_file() and not path.name.startswith("."):
            by_hash[sha256_file(path)] = path
    out = {}
    for video in labels_doc["videos"]:
        if video["sha256"] not in by_hash:
            raise SystemExit(f"{video['id']}: no file with the labelled content hash in {videos_dir}")
        out[video["id"]] = by_hash[video["sha256"]]
    return out


def pool_for(binary, source, directory: Path, state: Path, home: Path):
    manifest = directory / "manifest.json"
    if manifest.exists():
        return json.loads(manifest.read_text())
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    result = subprocess.run([str(binary), "--sample-video-pool", str(source), "0.5", "448", str(directory)],
                            env=engine_environment(state, home=home), capture_output=True, timeout=1800)
    if result.returncode != 0:
        raise SystemExit(f"pool worker failed ({result.returncode}) for {source.name}")
    return json.loads(manifest.read_text())


def embed_pool_frames(binary, pools: Path, state: Path, timeout, home: Path):
    """CLIP vectors for every pool frame, from one photo scan of the pool folders."""
    database = database_path(state)
    if not database.exists():
        state.mkdir(parents=True, exist_ok=True)
        engine = Engine(binary, state, log=state / "engine.log", home=home)
        try:
            engine.wait("ready", 180)
            engine.send({"startScan": {"rootPath": str(pools)}})
            engine.wait("scanComplete", timeout)
        finally:
            engine.close()
    connection = sqlite3.connect(database)
    rows = connection.execute("SELECT f.path_text, c.embedding FROM clip_embeddings c JOIN files f ON f.id = c.file_id").fetchall()
    connection.close()
    vectors = {}
    for path, blob in rows:
        vectors[os.path.realpath(path)] = list(struct.unpack(f"{len(blob) // 4}f", blob))
    return vectors


def run_candidates(args, labels_doc, rules: Rules):
    work = require_internal(args.work, "work folder")
    paths = video_paths(require_internal(args.videos, "videos"), labels_doc)
    pools = work / "pools"
    state = work / "pool-state"
    home = prepare_home(args.home)
    manifests = {vid: pool_for(args.engine, path, pools / vid, state, home) for vid, path in paths.items()}
    vectors = embed_pool_frames(args.engine, pools, work / "pool-clip", args.timeout, home)
    scored = {}
    for video in labels_doc["videos"]:
        manifest = manifests[video["id"]]
        frames, previous = [], None
        for frame in manifest["frames"]:
            path = os.path.realpath(pools / video["id"] / frame["file"])
            embedding = vectors.get(path)
            if embedding is None:
                raise SystemExit(f"no CLIP vector for {path}; is the CLIP model installed?")
            change = luma_change(previous, frame["signature"]) if previous is not None else 0.0
            previous = frame["signature"]
            frames.append((frame["seconds"], embedding, change))
        duration = manifest["duration"]
        scored[video["id"]] = score_video(video, {
            "scenes": scenes(frames, duration, rules),
            "windows": activity_windows(frames, duration, rules),
        })
    return scored


def run_full(args, labels_doc):
    work = require_internal(args.work, "work folder")
    videos_dir = require_internal(args.videos, "videos")
    paths = video_paths(videos_dir, labels_doc)
    state = work / f"full-{args.name}"
    if state.exists():
        shutil.rmtree(state)
    state.mkdir(parents=True)
    # A bare engine binary has no speech-recognition usage description, so macOS
    # TCC aborts it on the first speech request; speech is not labelled here.
    env = {"FILEID_TIMELINE_SPEECH": "0", **dict(e.split("=", 1) for e in args.env or [])}
    home = prepare_home(args.home, [MODEL_REPOS[args.model]] if args.model in MODEL_REPOS else [])
    engine = Engine(args.engine, state, env=env, log=state / "engine.log", home=home)
    started = time.monotonic()
    try:
        engine.wait("ready", 180)
        engine.send({"startScan": {"rootPath": str(videos_dir)}})
        engine.wait("scanComplete", args.timeout)

        def refuse_download(payload):
            progress = payload.get("modelDownloadProgress")
            if isinstance(progress, dict):
                progress = progress.get("_0", progress)
                # A verified install reports "Downloading ... (100%)" once and fetches nothing.
                if "Downloading" in str(progress.get("message", "")) and progress.get("fraction", 0) < 1.0:
                    engine.close()
                    raise SystemExit("the engine started a model download; aborted (models must already be installed)")

        engine.send({"prewarmModel": {"modelKind": args.model}})
        while True:
            ready = engine.wait("modelDownloadProgress", 900, on_event=refuse_download)
            ready = ready.get("_0", ready)
            if ready.get("fraction", 0) >= 1.0 and "ready" in ready.get("message", ""):
                break
        connection = sqlite3.connect(database_path(state))
        ids = {os.path.realpath(p): i for i, p in connection.execute("SELECT id, path_text FROM files WHERE kind='video'")}
        connection.close()
        file_ids = [ids[os.path.realpath(paths[v["id"]])] for v in labels_doc["videos"]]
        response = engine.catalog("enqueueTimeline", fileIDs=file_ids, timelineMode="moments")
        if response.get("status") != "ok":
            raise SystemExit(f"enqueueTimeline failed: {response.get('message')}")
        job_id = (response.get("jobs") or [{}])[0].get("id")
        deadline = time.monotonic() + args.timeout
        while time.monotonic() < deadline:
            time.sleep(10)
            jobs = engine.catalog("jobs").get("jobs") or []
            job = next((j for j in jobs if j.get("id") == job_id), None) or (jobs[0] if jobs else {})
            if job.get("state") in ("completed", "failed", "cancelled", "paused"):
                break
        else:
            raise SystemExit("timeline job did not finish in time")
        if job.get("state") != "completed":
            raise SystemExit(f"timeline job ended {job.get('state')}: {job.get('error')}")
    finally:
        engine.close()
    elapsed = time.monotonic() - started
    connection = sqlite3.connect(database_path(state))
    path_by_id = {i: os.path.realpath(p) for i, p in connection.execute("SELECT id, path_text FROM files")}
    id_by_video = {v["id"]: next(i for i, p in path_by_id.items() if p == os.path.realpath(paths[v["id"]])) for v in labels_doc["videos"]}
    scored = {}
    for video in labels_doc["videos"]:
        fid = id_by_video[video["id"]]
        moments = connection.execute("SELECT start_seconds, end_seconds FROM catalog_chapters WHERE file_id=? AND id LIKE 'timeline-moment:%'", (fid,)).fetchall()
        scene_rows = connection.execute("SELECT start_seconds, end_seconds FROM catalog_passages WHERE file_id=? AND id LIKE 'scene:%'", (fid,)).fetchall()
        windows = connection.execute("SELECT start_seconds, end_seconds FROM catalog_coverage WHERE file_id=? AND id LIKE 'moment-window:%'", (fid,)).fetchall()
        drafts = connection.execute("SELECT start_seconds, end_seconds FROM catalog_chapters WHERE file_id=? AND id NOT LIKE 'timeline-moment:%'", (fid,)).fetchall()
        scored[video["id"]] = score_video(video, {"moments": moments, "scenes": scene_rows, "windows": windows, "drafts": drafts})
    connection.close()
    return scored, elapsed, video_timing(state)


def video_timing(state: Path):
    """Per-video timeline job time and visual-model sequence calls from the engine's JSON log."""
    per_video, sequences = [], []
    log = state / "FileID/logs/scan.jsonl"
    if not log.exists():
        return None
    for line in log.read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        extra = event.get("extra") or {}
        if event.get("ev") == "timeline_scene_timing":
            per_video.append(extra.get("totalMs", 0) / 1000)
        elif event.get("ev") == "deep_sequence_timing":
            sequences.append(extra.get("totalMs", 0) / 1000)
    if not per_video:
        return None
    return {"videos": len(per_video), "meanVideoSeconds": round(statistics.mean(per_video), 1),
            "medianVideoSeconds": round(statistics.median(per_video), 1), "totalVideoSeconds": round(sum(per_video), 1),
            "sequenceCalls": len(sequences), "sequenceSeconds": round(sum(sequences), 1)}


def summary(name, block):
    parts = [f"{name:8s}"]
    for key in ("moments", "scenes", "windows"):
        if key in block:
            b = block[key]
            parts.append(f"{key} n={b['proposals']} R={b['recall']} P@IoU={b['precisionIoU']} Pin={b['precisionWithin']} quiet={b['inQuiet']}")
    c = block["cuts"]
    parts.append(f"cuts med={c['medianError']}s <=1s {c['within1_0']}")
    f = block["fast"]
    parts.append(f"fast {f['labelled']} unexamined {len(f['notExamined'])} unproposed {len(f['notProposed'])}")
    return " | ".join(parts)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("mode", choices=("candidates", "full"))
    parser.add_argument("--engine", required=True, type=Path)
    parser.add_argument("--videos", type=Path, default=EVAL_ROOT / "timeline/videos")
    parser.add_argument("--labels", type=Path, default=EVAL_ROOT / "timeline/labels.json")
    parser.add_argument("--work", type=Path, default=EVAL_ROOT / "timeline")
    parser.add_argument("--home", type=Path, default=EVAL_ROOT / "home",
                        help="isolated engine home holding copies of the installed models")
    parser.add_argument("--name", default="baseline")
    parser.add_argument("--model", default="qwen3_vl_2b", help="installed visual model kind (full mode)")
    parser.add_argument("--rule", action="append", help="candidates mode: rule=value override, e.g. activity_change=0.03")
    parser.add_argument("--env", action="append", help="full mode: KEY=VALUE engine override")
    parser.add_argument("--timeout", type=float, default=7200)
    args = parser.parse_args()
    labels_doc = json.loads(args.labels.read_text())
    before = live_state_snapshot()
    meta = {"mode": args.mode, "name": args.name}
    if args.mode == "candidates":
        rules = Rules(**dict(r.split("=", 1) for r in args.rule or []))
        meta["rules"] = rules.as_dict()
        scored = run_candidates(args, labels_doc, rules)
    else:
        scored, elapsed, timing = run_full(args, labels_doc)
        meta["seconds"] = round(elapsed, 1)
        meta["timing"] = timing
        meta["model"] = args.model
    changes = diff_snapshots(before, live_state_snapshot())
    meta["liveStateChanges"] = changes
    report = split_report(labels_doc, scored)
    report["meta"] = meta
    report["perVideo"] = {vid: {k: (v if k in ("cutErrors", "fast") else {kk: vv for kk, vv in v.items() if kk != "matched"})
                                for k, v in s.items()} for vid, s in scored.items()}
    out = require_internal(args.work, "work folder") / f"timeline-{args.mode}-{args.name}.json"
    out.write_text(json.dumps(report, indent=1))
    for key in ("all", *labels_doc.get("split", {}).keys()):
        print(summary(key, report[key]))
    if meta.get("timing"):
        print("timing", meta["timing"])
    if changes:
        print("WARNING: the live FileID state changed:", *changes, sep="\n  ", file=sys.stderr)
        return 1
    print(f"report {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
