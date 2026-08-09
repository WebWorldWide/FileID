#!/usr/bin/env python3
"""Run one isolated face-clustering candidate against a labelled catalog clone."""

from __future__ import annotations

import argparse
import json
import math
import os
import sqlite3
import sys
import time
import uuid
from pathlib import Path
from typing import Any

from real_data_validation import (
    EngineDriver,
    collect_face_metrics,
    inner_payload,
    isolated_environment,
    settle_command,
    utc_now,
    validate_face_invariants,
)


def parse_override(raw: str) -> tuple[str, str]:
    name, separator, value = raw.partition("=")
    name = name.strip().upper()
    value = value.strip()
    if not separator or not name.startswith("FILEID_FACE_") or not value:
        raise argparse.ArgumentTypeError(
            "tuning overrides must use FILEID_FACE_NAME=value"
        )
    return name, value


def clone_database(source: Path, destination: Path) -> None:
    source_uri = f"file:{source.as_posix()}?mode=ro"
    with sqlite3.connect(source_uri, uri=True) as input_db, sqlite3.connect(
        destination
    ) as output_db:
        input_db.execute("PRAGMA query_only=ON")
        input_db.backup(output_db)
        integrity = output_db.execute("PRAGMA integrity_check").fetchone()
        if integrity is None or integrity[0] != "ok":
            raise RuntimeError(f"cloned database failed integrity_check: {integrity}")


def load_oracle(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict) or value.get("version") != 1:
        raise ValueError("face oracle must be an object with version 1")
    same_groups = value.get("sameIdentityGroups")
    different_pairs = value.get("differentIdentityPairs")
    if not isinstance(same_groups, list) or not isinstance(different_pairs, list):
        raise ValueError("face oracle requires sameIdentityGroups and differentIdentityPairs")
    seen_labels: set[str] = set()
    for group in same_groups:
        if not isinstance(group, dict):
            raise ValueError("same-identity groups must be objects")
        label = group.get("label")
        face_ids = group.get("faceIDs")
        if (
            not isinstance(label, str)
            or not label.strip()
            or label in seen_labels
            or not isinstance(face_ids, list)
            or len(face_ids) < 2
            or any(isinstance(face_id, bool) or not isinstance(face_id, int) for face_id in face_ids)
            or len(set(face_ids)) != len(face_ids)
        ):
            raise ValueError(f"invalid same-identity group: {group!r}")
        seen_labels.add(label)
    seen_pairs: set[tuple[int, int]] = set()
    for pair in different_pairs:
        if not isinstance(pair, dict):
            raise ValueError("different-identity pairs must be objects")
        left = pair.get("leftFaceID")
        right = pair.get("rightFaceID")
        if (
            isinstance(left, bool)
            or not isinstance(left, int)
            or isinstance(right, bool)
            or not isinstance(right, int)
            or left == right
        ):
            raise ValueError(f"invalid different-identity pair: {pair!r}")
        key = tuple(sorted((left, right)))
        if key in seen_pairs:
            raise ValueError(f"duplicate different-identity pair: {key}")
        seen_pairs.add(key)
    return value


def evaluate_oracle(database: Path, oracle: dict[str, Any]) -> dict[str, Any]:
    requested_ids = {
        int(face_id)
        for group in oracle["sameIdentityGroups"]
        for face_id in group["faceIDs"]
    }
    requested_ids.update(
        int(pair[key])
        for pair in oracle["differentIdentityPairs"]
        for key in ("leftFaceID", "rightFaceID")
    )
    placeholders = ",".join("?" for _ in requested_ids)
    with sqlite3.connect(database) as connection:
        connection.row_factory = sqlite3.Row
        rows = connection.execute(
            "SELECT id,person_id,face_quality,excluded FROM face_prints "
            f"WHERE id IN ({placeholders}) ORDER BY id",
            sorted(requested_ids),
        ).fetchall()
    faces = {
        int(row["id"]): {
            "personID": int(row["person_id"]) if row["person_id"] is not None else None,
            "faceQuality": float(row["face_quality"] or 0.0),
            "excluded": bool(row["excluded"]),
        }
        for row in rows
    }
    missing = sorted(requested_ids - faces.keys())
    same_results = []
    for group in oracle["sameIdentityGroups"]:
        face_ids = [int(face_id) for face_id in group["faceIDs"]]
        owners = [faces.get(face_id, {}).get("personID") for face_id in face_ids]
        passed = not any(owner is None for owner in owners) and len(set(owners)) == 1
        same_results.append(
            {
                "label": group["label"],
                "faceIDs": face_ids,
                "personIDs": owners,
                "passed": passed,
            }
        )
    different_results = []
    for pair in oracle["differentIdentityPairs"]:
        left = int(pair["leftFaceID"])
        right = int(pair["rightFaceID"])
        left_owner = faces.get(left, {}).get("personID")
        right_owner = faces.get(right, {}).get("personID")
        passed = left_owner is None or right_owner is None or left_owner != right_owner
        different_results.append(
            {
                "label": pair.get("label"),
                "leftFaceID": left,
                "rightFaceID": right,
                "leftPersonID": left_owner,
                "rightPersonID": right_owner,
                "passed": passed,
            }
        )
    return {
        "faces": faces,
        "missingFaceIDs": missing,
        "sameIdentityGroups": same_results,
        "differentIdentityPairs": different_results,
        "checks": {
            "allFaceIDsExist": not missing,
            "sameIdentityGroupsPreserved": bool(same_results)
            and all(result["passed"] for result in same_results),
            "differentIdentityPairsSeparated": bool(different_results)
            and all(result["passed"] for result in different_results),
        },
    }


def all_true(value: Any) -> bool:
    if isinstance(value, dict):
        return bool(value) and all(all_true(child) for child in value.values())
    if isinstance(value, list):
        return bool(value) and all(all_true(child) for child in value)
    return value is True


def run() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database", type=Path, required=True)
    parser.add_argument("--engine", type=Path, required=True)
    parser.add_argument("--models", type=Path, required=True)
    parser.add_argument("--ort-dylib-path", type=Path)
    parser.add_argument("--oracle", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--tuning", type=parse_override, action="append", default=[])
    parser.add_argument("--timeout-minutes", type=float, default=10.0)
    args = parser.parse_args()

    source = args.database.resolve(strict=True)
    engine = args.engine.resolve(strict=True)
    models = args.models.resolve(strict=True)
    oracle_path = args.oracle.resolve(strict=True)
    ort_dylib = (
        args.ort_dylib_path.resolve(strict=True)
        if args.ort_dylib_path is not None
        else None
    )
    output = args.output.resolve(strict=False)
    if output.exists():
        raise FileExistsError(f"output directory already exists: {output}")
    if not math.isfinite(args.timeout_minutes) or args.timeout_minutes <= 0:
        raise ValueError("--timeout-minutes must be positive")
    overrides = dict(args.tuning)
    if len(overrides) != len(args.tuning):
        raise ValueError("duplicate tuning override")
    oracle = load_oracle(oracle_path)

    output.mkdir(parents=True)
    state = output / "state"
    database = state / "FileID" / "fileid.sqlite"
    runtime_temp = state / "runtime-temp"
    database.parent.mkdir(parents=True)
    runtime_temp.mkdir(parents=True)
    clone_database(source, database)

    baseline = collect_face_metrics(source)
    baseline["checks"] = validate_face_invariants(baseline)
    allowed = set(overrides)
    environment, stripped = isolated_environment(
        allowed,
        state=state,
        db_path=database,
        models=models,
        runtime_temp=runtime_temp,
        ort_dylib_path=ort_dylib,
    )
    environment.update(overrides)

    summary: dict[str, Any] = {
        "startedAt": utc_now(),
        "sourceDatabase": str(source),
        "candidateDatabase": str(database),
        "engine": str(engine),
        "oracle": str(oracle_path),
        "tuning": overrides,
        "strippedInheritedEnvironment": stripped,
        "baseline": baseline,
    }
    driver = EngineDriver(engine, environment, output, engine.parent)
    exit_code: int | None = None
    started = time.monotonic()
    try:
        driver.start()
        ready = driver.wait_for("ready", after=0, timeout_seconds=90)
        command_id = f"cluster-{uuid.uuid4()}"
        driver.send(command_id, {"runFaceClustering": {}})
        complete = driver.wait_for(
            "faceClusteringComplete",
            command_id=command_id,
            timeout_seconds=args.timeout_minutes * 60,
            predicate=lambda value: isinstance(value, dict),
        )
        fence = settle_command(driver, command_id, {"faceClusteringComplete": 1})
        driver.send("shutdown", {"shutdown": {}})
        exit_code = driver.stop(30)
        metrics = collect_face_metrics(database)
        metrics["checks"] = validate_face_invariants(metrics)
        labels = evaluate_oracle(database, oracle)
        summary.update(
            {
                "ready": inner_payload(ready.value, "ready"),
                "event": inner_payload(complete.value, "faceClusteringComplete"),
                "commandFence": fence,
                "metrics": metrics,
                "labels": labels,
                "shutdown": {"exitCode": exit_code, "cleanExit": exit_code == 0},
            }
        )
        checks = {
            "cleanExit": exit_code == 0,
            "stdoutAllJSON": not driver.invalid_stdout,
            "stdoutReaderHealthy": driver.stdout_reader_error is None,
            "stderrReaderHealthy": driver.stderr_reader_error is None,
            "noStderr": not driver.stderr_lines,
            "metricInvariants": all_true(metrics["checks"]),
            "labelOracle": all_true(labels["checks"]),
        }
        summary["checks"] = checks
        summary["result"] = "GREEN" if all_true(checks) else "RED"
    except BaseException as error:
        summary["runError"] = f"{type(error).__name__}: {error}"
        if driver.process is not None and driver.process.poll() is None:
            try:
                driver.send("shutdown-error", {"shutdown": {}})
                exit_code = driver.stop(15)
            except BaseException:
                exit_code = driver.force_stop()
        else:
            driver.force_stop()
        summary["shutdown"] = {"exitCode": exit_code, "cleanExit": exit_code == 0}
        summary["result"] = "ERROR"
    finally:
        if driver.process is not None and driver.process.poll() is None:
            driver.force_stop()
        summary["wallSeconds"] = time.monotonic() - started
        summary["finishedAt"] = utc_now()
        (output / "summary.json").write_text(
            json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
    print(json.dumps({"result": summary["result"], "output": str(output)}))
    return 0 if summary["result"] == "GREEN" else 1


if __name__ == "__main__":
    sys.exit(run())
