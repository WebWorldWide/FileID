#!/usr/bin/env python3
"""Measure Chat time-to-first-evidence over IPC on a synthetic library.

Copies the library built by make_synthetic_library.py, starts the engine on the
copy, sends each query as a new conversation without a model and times the
gap between writing the command and the first chatResponse that carries hits
or finishes. Prints p50/p95 per query family and overall.

    benchmark_chat.py --engine <FileIDEngine> --library <state dir> [--repeat 5]
"""
from __future__ import annotations

import argparse
import json
import shutil
import statistics
import sys
import tempfile
import time
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from conformance import Engine  # noqa: E402

QUERIES = {
    "keyword": ["dogs on grass", "birthday cake party", "sunset beach", "receipt invoice", "snowman sled",
                "blue sky", "guitar concert stage"],
    "people": ["photos of Kyle Nolle", "videos of Emma"],
    "count": ["how many videos from September 19?", "how many photos from 2023?", "how many files are in Downloads?",
              "how many photos of Kyle Nolle are there?"],
    "size": ["largest videos", "how much space do duplicates use?", "how much space do videos use?",
             "biggest files in Downloads"],
    "dates": ["photos from March 2024", "videos from last year", "documents since June 2022"],
    "status": ["how many files are not described yet?", "find duplicates"],
}


def percentile(values, fraction):
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, round(fraction * (len(ordered) - 1))))
    return ordered[index]


def first_evidence(engine, text):
    request = {"requestID": str(uuid.uuid4()), "conversationID": str(uuid.uuid4()), "action": "send",
               "text": text, "useModel": False}
    line = json.dumps({"id": request["requestID"], "payload": {"chatRequest": {"request": request}}}) + "\n"
    started = time.perf_counter()
    engine.process.stdin.write(line)
    engine.process.stdin.flush()
    deadline = time.monotonic() + 60
    first = None
    while True:
        payload = engine.next_event(deadline)
        if payload is None:
            raise TimeoutError(text)
        response = payload.get("chatResponse")
        if not response:
            continue
        response = response.get("_0", response)
        if response.get("requestID") != request["requestID"]:
            continue
        if first is None and (response.get("hits") or response["status"] in ("completed", "error")):
            first = (time.perf_counter() - started) * 1000
        if response["status"] == "error":
            raise RuntimeError(f"{text}: {response['message']}")
        if response["status"] in ("completed", "cancelled"):
            return first, (time.perf_counter() - started) * 1000, len(response.get("hits", [])), response["message"]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--engine", type=Path, required=True)
    parser.add_argument("--library", type=Path, required=True, help="state directory holding FileID/fileid.sqlite")
    parser.add_argument("--repeat", type=int, default=5)
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="FileIDChatBench-") as temporary:
        state = Path(temporary)
        shutil.copytree(args.library / "FileID", state / "FileID", ignore=shutil.ignore_patterns("logs", "*.visual-index*"))
        engine = Engine(args.engine.resolve(strict=True), state, env={"TZ": "UTC", "FILEID_MODELS_DIR": str(state / "Models"),
                                                                      "CFFIXED_USER_HOME": str(state / "home")})
        try:
            engine.wait("ready", timeout=120)
            for family in QUERIES.values():
                for text in family:
                    first_evidence(engine, text)
            results = {}
            for _ in range(args.repeat):
                for family, texts in QUERIES.items():
                    for text in texts:
                        first, total, hits, message = first_evidence(engine, text)
                        results.setdefault(family, []).append(first)
                        if args.verbose:
                            print(f"{first:8.1f} ms {total:8.1f} ms {hits:4d} hits  {text} → {message[:90]}")
        finally:
            engine.close()
    every = [value for values in results.values() for value in values]
    print(f"{'family':10} {'n':>4} {'p50 ms':>8} {'p95 ms':>8} {'max ms':>8}")
    for family, values in [*results.items(), ("all", every)]:
        print(f"{family:10} {len(values):4d} {percentile(values, 0.5):8.1f} {percentile(values, 0.95):8.1f} {max(values):8.1f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
