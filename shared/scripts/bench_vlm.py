#!/usr/bin/env python3
"""Stage 7 acceptance runs for the MLX vision-language models, on either engine.

Each run starts the engine on a fresh disposable state folder with the model
read from a scratch copy (`--hf-root`, the folder that holds `models/<repo>`),
never from ~/Documents/huggingface.

    bench_vlm.py --engine <bin> --flavor swift|rust --scratch <dir> --hf-root <copy>
        --photos <folder> [--model qwen3_vl_4b] [--describe] [--idle 20] [--chat]
        [--cycles 50] [--json out.json]
    bench_vlm.py --compare swift.json rust.json

--describe scans the photos and runs Describe Files (caption, tags, name) on
all of them: load time, seconds per photo after the load, the engine's peak
physical footprint and every stored output. --idle sets the model idle time
and reports the footprint once the model has parked. --chat asks the same
question twice and reports the time to the first streamed words and whether
the second (prompt-cached) answer matches the first. --cycles starts the
engine, loads the model and shuts down N times, counting unclean exits.
--compare reports how many outputs two runs share.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
from bench_model_load import Engine, footprint_mb  # noqa: E402

REPOS = {
    "qwen3_vl_2b": "mlx-community/Qwen3-VL-2B-Instruct-4bit",
    "qwen3_vl_4b": "lmstudio-community/Qwen3-VL-4B-Instruct-MLX-4bit",
}


def load_average():
    return os.getloadavg()[0]


def engine_env(state, hf_root, idle_seconds=None):
    env = {key: value for key, value in os.environ.items() if key != "ORT_DYLIB_PATH"}
    database = state / "FileID/fileid.sqlite"
    database.parent.mkdir(parents=True, exist_ok=True)
    (state / "home").mkdir(exist_ok=True)
    env.update({"FILEID_DATABASE_PATH": str(database), "FILEID_DB": str(database), "XDG_DATA_HOME": str(state),
                "HOME": str(state / "home"), "CFFIXED_USER_HOME": str(state / "home"), "TZ": "UTC",
                "FILEID_HF_CACHE_ROOT": str(hf_root), "FILEID_MODELS_DIR": str(state / "models"),
                "FILEID_OCR_WARMUP": "0"})
    if idle_seconds is not None:
        env["FILEID_MODEL_IDLE_SECONDS"] = str(idle_seconds)
    return env


def scan(engine, root):
    engine.send({"startScan": {"rootPath": str(root)}})
    engine.wait(lambda n, v: n == "scanComplete", 900)


def describe(engine, model, database):
    sent = engine.send({"deepAnalyzeAll": {"modelKind": model, "skipExisting": False, "tagsOnly": False, "proposeRenames": True}})
    first = None
    while True:
        stamp, value = engine.wait(lambda n, v: n in ("deepAnalyzeProgress", "deepAnalyzeComplete", "error"), 3600)
        if engine.last_was("error"):
            print("engine error:", value, file=sys.stderr)
            continue
        if engine.last_was("deepAnalyzeProgress"):
            if first is None and value.get("currentPath") and not value.get("currentCaption"):
                first = stamp
            continue
        done = stamp
        break
    processed = value.get("processed", 0)
    with sqlite3.connect(database) as db:
        rows = db.execute(
            "SELECT path_text, vlm_description, vlm_proposed_name, "
            "(SELECT group_concat(tag, ', ') FROM (SELECT tag FROM tags WHERE file_id=files.id AND source='vlm' ORDER BY tag)) "
            "FROM files WHERE kind='image' ORDER BY path_text").fetchall()
    outputs = {Path(path).name: {"description": text, "name": name, "tags": tags} for path, text, name, tags in rows}
    return {
        "processed": processed, "failed": value.get("failed"), "cancelled": value.get("cancelled"),
        "load_s": (first - sent) if first else None,
        "analyze_s": (done - first) if first else None,
        "per_photo_s": (done - first) / processed if first and processed else None,
        "outputs": outputs,
    }


def chat(engine, model, text):
    request = str(uuid.uuid4())
    sent = engine.send({"chatRequest": {"request": {"requestID": request, "conversationID": str(uuid.uuid4()), "action": "send",
                                                    "text": text, "useModel": True, "modelKind": model}}})
    first = None
    while True:
        stamp, value = engine.wait(lambda n, v: n == "chatResponse" and v.get("requestID") == request, 900)
        if first is None and value.get("status") == "streaming":
            first = stamp - sent
        if value.get("status") in ("completed", "error", "cancelled"):
            return {"first_words_s": first, "total_s": stamp - sent, "status": value.get("status"), "message": value.get("message", "")}


def prewarm(engine, model):
    sent = engine.send({"prewarmModel": {"modelKind": model}})
    stamp, value = engine.wait(lambda n, v: (n == "modelDownloadProgress" and v.get("modelKind") == model and v.get("fraction", 0) >= 1.0
                                             and str(v.get("message", "")).endswith("ready.")) or n == "error", 900)
    if engine.last_was("error"):
        raise RuntimeError(f"prewarm failed: {value}")
    return stamp - sent


def idle_footprint(engine, seconds):
    time.sleep(seconds + 8)
    samples = []
    for _ in range(5):
        samples.append(footprint_mb(engine.process.pid)[0])
        time.sleep(1)
    return min(samples)


def cycles(binary, state, hf_root, model, count):
    unclean = []
    timings = []
    for index in range(count):
        engine = Engine(binary, engine_env(state, hf_root))
        try:
            engine.wait(lambda n, v: n == "ready", 120)
            timings.append(prewarm(engine, model))
            engine.send({"shutdown": {}})
            code = engine.process.wait(timeout=60)
        except Exception as error:  # noqa: BLE001 - every failure counts as unclean
            engine.process.kill()
            unclean.append({"cycle": index, "error": str(error)})
            continue
        if code != 0:
            unclean.append({"cycle": index, "exit": code})
    return {"cycles": count, "unclean": unclean, "median_load_s": sorted(timings)[len(timings) // 2] if timings else None}


def compare(left, right):
    a = json.loads(Path(left).read_text())["describe"]["outputs"]
    b = json.loads(Path(right).read_text())["describe"]["outputs"]
    shared = sorted(set(a) & set(b))
    for field in ("description", "tags", "name"):
        same = sum(1 for key in shared if a[key][field] == b[key][field])
        print(f"{field}: {same}/{len(shared)} identical")
    for key in shared:
        if a[key]["description"] != b[key]["description"]:
            print(f"- {key}\n  {a[key]['description']!r}\n  {b[key]['description']!r}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--engine", type=Path)
    parser.add_argument("--flavor", choices=["swift", "rust"], default="swift")
    parser.add_argument("--scratch", type=Path)
    parser.add_argument("--hf-root", type=Path)
    parser.add_argument("--photos", type=Path)
    parser.add_argument("--model", default="qwen3_vl_4b")
    parser.add_argument("--describe", action="store_true")
    parser.add_argument("--idle", type=int)
    parser.add_argument("--chat", action="store_true")
    parser.add_argument("--chat-text", default="photos of people outdoors")
    parser.add_argument("--cycles", type=int, default=0)
    parser.add_argument("--json", type=Path)
    parser.add_argument("--compare", nargs=2, metavar=("LEFT", "RIGHT"))
    args = parser.parse_args()
    if args.compare:
        compare(*args.compare)
        return
    hf_root = args.hf_root.resolve()
    if Path.home() / "Documents/huggingface" in (hf_root, *hf_root.parents):
        raise SystemExit("--hf-root must be a scratch copy, never ~/Documents/huggingface")
    if not (hf_root / "models" / REPOS.get(args.model, "") / "config.json").is_file():
        raise SystemExit(f"{args.model} is not under {hf_root}/models")
    state = args.scratch.resolve() / f"vlm-{args.flavor}-{int(time.time())}"
    state.mkdir(parents=True)
    result = {"flavor": args.flavor, "model": args.model, "load_before": load_average()}
    if args.describe or args.chat or args.idle is not None:
        engine = Engine(args.engine, engine_env(state, hf_root, args.idle))
        try:
            engine.wait(lambda n, v: n == "ready", 120)
            if args.photos:
                scan(engine, args.photos.resolve())
            if args.describe:
                result["describe"] = describe(engine, args.model, state / "FileID/fileid.sqlite")
                result["describe"]["peak_mb"] = footprint_mb(engine.process.pid)[1]
            if args.chat:
                result["chat"] = [chat(engine, args.model, args.chat_text) for _ in range(2)]
                result["chat_identical"] = result["chat"][0]["message"] == result["chat"][1]["message"]
            if args.idle is not None:
                result["idle_mb"] = idle_footprint(engine, args.idle)
            result["peak_mb"] = footprint_mb(engine.process.pid)[1]
        finally:
            engine.close()
    if args.cycles:
        result["cycles"] = cycles(args.engine, state, hf_root, args.model, args.cycles)
    result["load_after"] = load_average()
    summary = {key: value for key, value in result.items() if key != "describe"}
    if "describe" in result:
        summary["describe"] = {key: value for key, value in result["describe"].items() if key != "outputs"}
    print(json.dumps(summary, indent=2))
    if args.json:
        args.json.write_text(json.dumps(result, indent=2))
    shutil.rmtree(state / "FileID" / "thumbs.cache", ignore_errors=True)


if __name__ == "__main__":
    main()
