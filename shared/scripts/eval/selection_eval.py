#!/usr/bin/env python3
"""Score Chat selection checks ("which of these photos show …") against hand labels.

Scans `<eval>/selection/photos` into a disposable catalog, then for every question in
`<eval>/selection/labels.json` sends a chat request scoped to all the photos with the
local model on, and compares the photos the model answered "yes" for with the labels.
Prints precision and recall over the questions that have matches, and how many
no-match questions answered none. Works with either engine (`--flavor`).

    selection_eval.py --engine <bin> --flavor swift|rust --scratch <dir> --hf-root <model copy>
        [--model qwen3_vl_2b] [--json out.json]

labels.json: {"questions": [{"id": "Q01", "text": "...", "expected": ["s06", ...]}]}, where
the expected ids are photo file stems. The model is read from a scratch copy only.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import sqlite3
import sys
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from bench_model_load import Engine  # noqa: E402
from bench_vlm import engine_env, scan  # noqa: E402
from eval_common import EVAL_ROOT, LIVE_HF_MODELS  # noqa: E402


def ask(engine, model, text, ids):
    request = str(uuid.uuid4())
    sent = engine.send({"chatRequest": {"request": {"requestID": request, "conversationID": str(uuid.uuid4()), "action": "send",
                                                    "text": text, "useModel": True, "modelKind": model, "scopeFileIDs": ids}}})
    while True:
        stamp, value = engine.wait(lambda n, v: n == "chatResponse" and v.get("requestID") == request, 1800)
        if value.get("status") in ("completed", "error", "cancelled"):
            return value, stamp - sent


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--engine", required=True, type=Path)
    parser.add_argument("--flavor", choices=["swift", "rust"], default="swift")
    parser.add_argument("--scratch", required=True, type=Path)
    parser.add_argument("--hf-root", required=True, type=Path)
    parser.add_argument("--model", default="qwen3_vl_2b")
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()
    live = LIVE_HF_MODELS.parent.resolve()
    if args.hf_root.resolve() == live or live in args.hf_root.resolve().parents or args.hf_root.resolve() in live.parents:
        parser.error("--hf-root must be a scratch copy of the model, not the live folder")
    root = EVAL_ROOT / "selection"
    labels = json.loads((root / "labels.json").read_text())["questions"]
    state = args.scratch.resolve() / f"selection-{args.flavor}-{int(time.time())}"
    state.mkdir(parents=True)
    engine = Engine(args.engine, engine_env(state, args.hf_root.resolve()))
    rows = []
    try:
        engine.wait(lambda n, v: n == "ready", 120)
        scan(engine, (root / "photos").resolve())
        with sqlite3.connect(state / "FileID/fileid.sqlite") as db:
            ids = {Path(path).stem: file_id for file_id, path in db.execute("SELECT id, path_text FROM files WHERE kind='image'")}
        by_id = {file_id: stem for stem, file_id in ids.items()}
        for question in labels:
            value, seconds = ask(engine, args.model, question["text"], sorted(ids.values()))
            yes = sorted(by_id[hit["fileID"]] for hit in value.get("hits", []) if hit.get("match") == "yes" and hit.get("fileID") in by_id)
            unsure = sorted(by_id[hit["fileID"]] for hit in value.get("hits", []) if hit.get("match") == "unsure" and hit.get("fileID") in by_id)
            rows.append({"id": question["id"], "expected": question["expected"], "yes": yes, "unsure": unsure,
                         "status": value.get("status"), "seconds": round(seconds, 1)})
            print(f"{question['id']} {len(yes)} yes ({len(set(yes) & set(question['expected']))} right of {len(question['expected'])}), "
                  f"{len(unsure)} unsure, {seconds:.1f} s", flush=True)
    finally:
        engine.close()
    positive = [row for row in rows if row["expected"]]
    said = sum(len(row["yes"]) for row in positive)
    right = sum(len(set(row["yes"]) & set(row["expected"])) for row in positive)
    wanted = sum(len(row["expected"]) for row in positive)
    empty = [row for row in rows if not row["expected"]]
    summary = {
        "flavor": args.flavor, "model": args.model, "load": os.getloadavg()[0],
        "precision": round(right / said, 3) if said else None,
        "recall": round(right / wanted, 3) if wanted else None,
        "none_answered": f"{sum(1 for row in empty if not row['yes'])} of {len(empty)}",
        "seconds": round(sum(row["seconds"] for row in rows), 1),
    }
    print(json.dumps(summary, indent=2))
    if args.json:
        args.json.write_text(json.dumps({"summary": summary, "rows": rows}, indent=2))


if __name__ == "__main__":
    main()
