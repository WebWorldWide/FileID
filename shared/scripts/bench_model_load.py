#!/usr/bin/env python3
"""Measure how fast each installed model loads in a FileID engine.

For every model it starts the engine on a disposable state folder, sends the
command that first needs the model and times it. The first round uses model
files copied without the OS file cache (cold); later rounds restart the engine
on the same copies (warm, file cache populated). Prints one table row per
model and round with time to ready, time to the first result, the second
(model already loaded) result, the engine's peak physical footprint and how
many ONNX Runtime CoreML compilations the run left in the temporary folder
(approximate when other processes use ONNX Runtime at the same time).

Installed weights are only read: each run copies the files it needs into the
scratch folder (`--scratch`), and the engine's catalog, logs, caches and model
lookups are pointed there.

    bench_model_load.py --engine <FileIDEngine> [--flavor swift|rust] --scratch <dir>
        [--models clip_text,clip_image,sface,qwen3vl2b,qwen3vl4b] [--rounds 3]
        (scan_none and cluster_none time the same scan/clustering with no model,
        so clip_image minus scan_none and sface minus cluster_none isolate the model)
        [--photos <folder of JPEGs>] [--json out.json]
"""
from __future__ import annotations

import argparse
import ctypes
import fcntl
import json
import os
from pathlib import Path
import queue
import shutil
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import uuid

HOME = Path.home()
APP_MODELS = HOME / "Library/Application Support/FileID/Models"
HF_MODELS = HOME / "Documents/huggingface/models"
F_NOCACHE = 48
RUSAGE_INFO_V4 = 4

SWIFT_MODELS = {
    "clip_text": [("clip_text/clip_text.onnx", "clip_text/clip_text.onnx"), ("clip_text/vocab.json", "clip_text/vocab.json"),
                  ("clip_text/merges.txt", "clip_text/merges.txt")],
    "clip_image": [("mobileclip_image/clip_vitb32_image.onnx", "mobileclip_image/clip_vitb32_image.onnx")],
    "sface": [("face_recognition_sface_2021dec.onnx", "face_recognition_sface_2021dec.onnx")],
}
RUST_MODELS = {
    "clip_text": [("clip_text/clip_text.onnx", "clip_text/clip_text.onnx"), ("clip_text/vocab.json", "clip_text/vocab.json"),
                  ("clip_text/merges.txt", "clip_text/merges.txt")],
    "clip_image": [("mobileclip_image/clip_vitb32_image.onnx", "mobileclip/mobileclip_s2_image.onnx")],
    "sface": [("face_recognition_sface_2021dec.onnx", "sface/face_recognition_sface_2021dec.onnx")],
}
BASELINES = {"scan_none": "clip_image", "cluster_none": "sface"}
VLMS = {
    "qwen3vl2b": ("qwen3_vl_2b", "mlx-community/Qwen3-VL-2B-Instruct-4bit"),
    "qwen3vl4b": ("qwen3_vl_4b", "lmstudio-community/Qwen3-VL-4B-Instruct-MLX-4bit"),
}


class RUsage(ctypes.Structure):
    _fields_ = [("uuid", ctypes.c_uint8 * 16), ("values", ctypes.c_uint64 * 40)]


LIBPROC = ctypes.CDLL("/usr/lib/libproc.dylib") if sys.platform == "darwin" else None


def footprint_mb(pid):
    """(current, lifetime peak) physical footprint in MB, which counts Metal and
    IOSurface memory that RSS misses."""
    if LIBPROC is None:
        return None, None
    usage = RUsage()
    if LIBPROC.proc_pid_rusage(pid, RUSAGE_INFO_V4, ctypes.byref(usage)) != 0:
        return None, None
    return usage.values[7] / 1048576, usage.values[28] / 1048576


def uncached_copy(source, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    with open(source, "rb") as reader, open(destination, "wb") as writer:
        fcntl.fcntl(reader.fileno(), F_NOCACHE, 1)
        fcntl.fcntl(writer.fileno(), F_NOCACHE, 1)
        while chunk := reader.read(8 << 20):
            writer.write(chunk)


def copy_tree(source, destination):
    for path in sorted(source.rglob("*")):
        if path.is_file() and not path.is_symlink():
            uncached_copy(path, destination / path.relative_to(source))


class Engine:
    def __init__(self, binary, env):
        self.started = time.perf_counter()
        self.process = subprocess.Popen([str(binary)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        env=env, text=True, encoding="utf-8", bufsize=1)
        self.events = queue.Queue()
        for stream in (self.process.stdout, self.process.stderr):
            threading.Thread(target=self._read, args=(stream,), daemon=True).start()

    def _read(self, stream):
        for line in stream:
            try:
                event = json.loads(line)
            except (json.JSONDecodeError, TypeError):
                continue
            if isinstance(event, dict) and isinstance(event.get("payload"), dict):
                self.events.put((time.perf_counter(), event["payload"]))

    def send(self, payload):
        self.process.stdin.write(json.dumps({"id": str(uuid.uuid4()), "payload": payload}) + "\n")
        self.process.stdin.flush()
        return time.perf_counter()

    def wait(self, match, timeout):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                stamp, payload = self.events.get(timeout=0.25)
            except queue.Empty:
                if self.process.poll() is not None:
                    raise RuntimeError(f"engine exited with {self.process.returncode}")
                continue
            for name, value in payload.items():
                value = value.get("_0", value) if isinstance(value, dict) else value
                if name == "error" and isinstance(value, dict) and value.get("kind") in {"db_open_failed", "db_newer_than_engine"}:
                    raise RuntimeError(value)
                if match(name, value):
                    self.last = name
                    return stamp, value
        raise TimeoutError("engine did not answer in time")

    def last_was(self, name):
        return getattr(self, "last", None) == name

    def close(self):
        if self.process.poll() is None:
            try:
                self.send({"shutdown": {}})
                self.process.wait(timeout=15)
            except (BrokenPipeError, OSError, subprocess.TimeoutExpired):
                self.process.kill()


def engine_env(flavor, state):
    env = dict(os.environ)
    database = state / "FileID/fileid.sqlite"
    database.parent.mkdir(parents=True, exist_ok=True)
    (state / "home").mkdir(exist_ok=True)
    env.update({"FILEID_DATABASE_PATH": str(database), "FILEID_DB": str(database), "XDG_DATA_HOME": str(state),
                "LOCALAPPDATA": str(state), "CFFIXED_USER_HOME": str(state / "home"), "TZ": "UTC",
                "FILEID_HF_CACHE_ROOT": str(state / "hf"), "FILEID_MODELS_DIR": str(state / "models"),
                "FILEID_OCR_WARMUP": "0"})
    if flavor == "rust":
        env["HOME"] = str(state / "home")
    return env


def install(flavor, model, state):
    if model in BASELINES:
        return
    if model in VLMS:
        repo = VLMS[model][1]
        copy_tree(HF_MODELS / repo, state / "hf/models" / repo)
        return
    table = SWIFT_MODELS if flavor == "swift" else RUST_MODELS
    root = state / ("home/Library/Application Support/FileID/Models" if flavor == "swift" else "models")
    for source, target in table[model]:
        uncached_copy(APP_MODELS / source, root / target)


def is_ready(name, _value):
    return name == "ready"


def text_query(engine, text):
    query = str(uuid.uuid4())
    sent = engine.send({"embedTextQuery": {"query": text, "queryID": query}})
    stamp, value = engine.wait(lambda n, v: n == "clipTextEmbedding" and v.get("queryID") == query, 300)
    if len(value.get("embedding") or []) != 512:
        raise RuntimeError("CLIP text returned no embedding")
    return stamp - sent


def scan(engine, root):
    sent = engine.send({"startScan": {"rootPath": str(root)}})
    first, _ = engine.wait(lambda n, v: n in ("fileDone", "batchSummary", "scanComplete"), 600)
    done = first
    if not engine.last_was("scanComplete"):
        done, _ = engine.wait(lambda n, v: n == "scanComplete", 600)
    return first - sent, done - sent


def cluster(engine):
    sent = engine.send({"runFaceClustering": {}})
    done, value = engine.wait(lambda n, v: n == "faceClusteringComplete", 600)
    return done - sent, value


def prewarm(engine, kind):
    sent = engine.send({"prewarmModel": {"modelKind": kind}})
    done, value = engine.wait(lambda n, v: (n == "modelDownloadProgress" and v.get("modelKind") == kind and v.get("fraction", 0) >= 1.0
                                            and str(v.get("message", "")).endswith("ready."))
                              or n == "error", 900)
    if engine.last_was("error"):
        raise RuntimeError(f"prewarm failed: {value}")
    return done - sent


def chat(engine, kind, text):
    request = str(uuid.uuid4())
    sent = engine.send({"chatRequest": {"request": {"requestID": request, "conversationID": str(uuid.uuid4()), "action": "send",
                                                    "text": text, "useModel": True, "modelKind": kind}}})
    first = None
    while True:
        stamp, value = engine.wait(lambda n, v: n == "chatResponse" and v.get("requestID") == request, 900)
        if first is None and value.get("status") == "streaming":
            first = stamp - sent
        if value.get("status") in ("completed", "error", "cancelled"):
            return first, stamp - sent, value.get("status"), value.get("message", "")


def seed_faces(binary, flavor, state, photos):
    """Scan the face photos once without models so later runs only time clustering."""
    engine = Engine(binary, engine_env(flavor, state))
    try:
        engine.wait(is_ready, 120)
        scan(engine, photos)
    finally:
        engine.close()
    with sqlite3.connect(state / "FileID/fileid.sqlite") as db:
        faces = db.execute("SELECT COUNT(*) FROM face_prints").fetchone()[0]
    return faces


def leaked_runtime_files():
    """CoreML compilations ONNX Runtime leaves in the per-user temporary folder
    (Foundation ignores TMPDIR there, so a run cannot be confined)."""
    return sum(1 for name in os.listdir(tempfile.gettempdir()) if name.startswith("onnxruntime-"))


def run_once(binary, flavor, model, state, photos, round_index):
    env = engine_env(flavor, state)
    leaked_before = leaked_runtime_files()
    engine = Engine(binary, env)
    row = {"model": model, "round": round_index}
    try:
        ready_at, _ = engine.wait(is_ready, 120)
        row["ready"] = ready_at - engine.started
        row["base_mb"] = footprint_mb(engine.process.pid)[0]
        if model == "clip_text":
            row["first"] = text_query(engine, "a dog running on the beach")
            row["second"] = text_query(engine, "birthday cake with candles")
        elif model in ("clip_image", "scan_none"):
            row["first"], row["total"] = scan(engine, photos / f"round{round_index}")
        elif model in ("sface", "cluster_none"):
            row["first"], value = cluster(engine)
            row["note"] = f"{value.get('faceCount', '?')} faces, {value.get('personCount', '?')} people"
        elif model in VLMS:
            kind = VLMS[model][0]
            row["first"] = prewarm(engine, kind)
            ttft, total, status, message = chat(engine, kind, "grass photos")
            row["second"], row["total"] = ttft, total
            row["note"] = f"chat {status}: {message[:60]!r}"
        current, peak = footprint_mb(engine.process.pid)
        row["peak_mb"] = peak
    finally:
        engine.close()
    row["tmp_left"] = leaked_runtime_files() - leaked_before
    return row


def prepare_photos(source, scratch, count):
    jpegs = sorted(p for p in source.rglob("*") if p.suffix.lower() in (".jpg", ".jpeg"))[:count]
    if not jpegs:
        raise SystemExit(f"no JPEGs under {source}")
    return jpegs


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--engine", required=True, type=Path)
    parser.add_argument("--scratch", required=True, type=Path)
    parser.add_argument("--models", default="clip_text,clip_image,sface,qwen3vl2b,qwen3vl4b")
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--photos", type=Path, help="folder of JPEGs; portraits give SFace faces to embed")
    parser.add_argument("--flavor", choices=["swift", "rust"], default="swift",
                        help="engine type: the macOS Swift FileIDEngine or the shared Rust engine (both binaries share a name)")
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()
    flavor = args.flavor
    scratch = args.scratch.resolve()
    if any(str(scratch).lower().startswith(p) for p in ("/volumes/adlon", str(APP_MODELS.parent).lower())):
        raise SystemExit("scratch must not be inside the live FileID folder")
    scratch.mkdir(parents=True, exist_ok=True)
    rows = []
    for model in [m for m in args.models.split(",") if m]:
        if model in VLMS and flavor == "rust":
            print(f"skip {model}: the shared engine runs GGUF models, not MLX", file=sys.stderr)
            continue
        table = SWIFT_MODELS if flavor == "swift" else RUST_MODELS
        if model not in VLMS and model not in BASELINES and not all((APP_MODELS / s).is_file() for s, _ in table.get(model, [("missing", "")])):
            print(f"skip {model}: not installed", file=sys.stderr)
            continue
        if model in VLMS and not (HF_MODELS / VLMS[model][1] / "config.json").is_file():
            print(f"skip {model}: not installed", file=sys.stderr)
            continue
        state = scratch / f"{flavor}-{model}"
        shutil.rmtree(state, ignore_errors=True)
        state.mkdir(parents=True)
        role = "sface" if model in VLMS else BASELINES.get(model, model)
        photos = state / "photos"
        if args.photos:
            for index in range(args.rounds):
                for n, jpeg in enumerate(prepare_photos(args.photos, scratch, 4 if role == "sface" else 1)):
                    target = photos / (f"round{index}" if role == "clip_image" else "faces") / f"{n}{jpeg.suffix}"
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(jpeg, target)
        if role == "clip_image" and not args.photos:
            print(f"skip {model}: needs --photos", file=sys.stderr)
            continue
        if role == "sface":
            if not args.photos:
                print(f"skip {model}: needs --photos", file=sys.stderr)
                continue
            seed_faces(args.engine, flavor, state, photos / "faces")
            with sqlite3.connect(state / "FileID/fileid.sqlite") as source, sqlite3.connect(state / "seed.sqlite") as seed:
                source.backup(seed)
        install(flavor, model, state)
        for index in range(args.rounds):
            if role == "sface":
                for suffix in ("-wal", "-shm"):
                    (state / f"FileID/fileid.sqlite{suffix}").unlink(missing_ok=True)
                shutil.copyfile(state / "seed.sqlite", state / "FileID/fileid.sqlite")
            row = run_once(args.engine, flavor, model, state, photos, index)
            row["cache"] = "cold" if index == 0 else "warm"
            rows.append(row)
            print(json.dumps(row), file=sys.stderr)
        shutil.rmtree(state / "hf", ignore_errors=True)
    print(f"\n{flavor} engine {args.engine}")
    print(f"{'model':<11} {'cache':<5} {'ready s':>8} {'first s':>8} {'next s':>8} {'total s':>8} {'base MB':>8} {'peak MB':>8} {'tmp':>5}  note")
    for row in rows:
        def cell(key):
            value = row.get(key)
            return f"{value:8.2f}" if isinstance(value, float) else f"{'-':>8}"
        print(f"{row['model']:<11} {row['cache']:<5} {cell('ready')} {cell('first')} {cell('second')} {cell('total')} "
              f"{row['base_mb'] or 0:8.0f} {row['peak_mb'] or 0:8.0f} {row['tmp_left']:5d}  {row.get('note', '')}")
    by_model = {}
    for row in rows:
        by_model.setdefault((row["model"], row["cache"]), []).append(row["first"])
    print("\nmedian first-result seconds: " + ", ".join(f"{m} {c} {statistics.median(v):.2f}" for (m, c), v in by_model.items()))
    if args.json:
        args.json.write_text(json.dumps(rows, indent=1))


if __name__ == "__main__":
    main()
