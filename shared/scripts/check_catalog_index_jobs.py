#!/usr/bin/env python3
"""Verify native index recovery and controls against an isolated actual engine."""
import argparse
import hashlib
import json
from pathlib import Path
import sqlite3
import tempfile
import time

from check_catalog_roundtrip import Engine


def verify(binary: Path) -> None:
    root = Path(__file__).resolve().parents[2]
    manifest = json.loads((root / "shared/models/manifest.json").read_text())
    hashes = {entry["id"]: entry["sha256"] for entry in manifest["artifacts"]}
    ids = ["clip_vitb32_image", "clip_vitb32_text", "clip_bpe_vocab", "clip_bpe_merges"]
    descriptor = "|".join(["clip-rgb-stretch224-bpe77-l2-v1"] + [f"{key}:{hashes[key]}" for key in ids])
    model = "clip-vitb32-openai-v1:" + hashlib.sha256(descriptor.encode()).hexdigest()
    job_id = "catalog-index-" + hashlib.sha256(model.encode()).hexdigest()
    with tempfile.TemporaryDirectory(prefix="fileid-index-jobs-") as temporary:
        directory = Path(temporary)
        engine = Engine(binary.resolve(), directory, swift=True)
        try:
            engine.wait("ready")
        finally:
            engine.close()
        with sqlite3.connect(directory / "FileID/fileid.sqlite") as database:
            database.execute(
                "INSERT INTO catalog_jobs(id,kind,file_ids_json,recipe_json,state,priority,progress,created_at,updated_at) VALUES(?,'catalogIndex','[]',?,'running',1,0.4,1,1)",
                (job_id, json.dumps({"model": model})),
            )
        engine = Engine(binary.resolve(), directory, swift=True)
        try:
            engine.wait("ready")
            jobs = engine.request("jobs")["jobs"]
            assert len(jobs) == 1 and jobs[0]["id"] == job_id
            assert jobs[0]["state"] == "paused" and jobs[0]["progress"] == 0.4
            cancelled = engine.request("cancelJob", jobID=job_id)["jobs"]
            assert cancelled[0]["state"] == "cancelled"
            engine.request("resumeJob", jobID=job_id)
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                jobs = engine.request("jobs")["jobs"]
                if jobs[0]["state"] == "completed":
                    break
                assert jobs[0]["state"] in ("queued", "running"), jobs
                time.sleep(0.05)
            else:
                raise RuntimeError("Native search index job did not finish")
            assert jobs[0]["progress"] == 1 and len(jobs) == 1
            request_id = "index-probe"
            engine.send({"catalogRequest": {"request": {
                "requestID": request_id, "action": "search", "searchMode": "semantic",
                "embeddingModel": model, "queryVector": [1.0] + [0.0] * 511,
            }}})
            response = engine.wait("catalogResponse", request_id)
            assert response["status"] == "ok" and response["hits"] == [], response
        finally:
            engine.close()
    print("Native index jobs recover, cancel and retry without a visual model; IPC remains responsive")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine", type=Path, required=True)
    verify(parser.parse_args().engine)
