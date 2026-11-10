#!/usr/bin/env python3
"""People parity between the Swift and Rust engines on one Swift-scanned catalog.

Takes the stored Swift scan of people_eval.py (`<work>/swift-scan-default/snapshot.sqlite`),
gives it user corrections with the Swift engine (names, an Unknown person, a
different-people verdict, a merge), then clusters copies of that catalog with each
engine and compares every People table row for row (timestamps aside). It also
asks the Rust engine for the face refresh backlog, which must be empty, and times
`runFaceClustering` on both engines, back to back.

  people_parity.py --swift <FileIDEngine> --rust <fileid-engine> --work <people_eval work> --home <home>

Only aggregate counts are printed; the catalogs stay in the work folder.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import shutil
import sqlite3
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from eval_common import (Engine, database_path, diff_snapshots, live_state_snapshot,  # noqa: E402
                         prepare_home, require_internal)

TABLES = {
    "persons": "SELECT id,name,title,first_name,middle_name,last_name,suffix,COALESCE(is_unknown,0),"
               "representative_face_id,file_count,centroid,anchor_radius FROM persons ORDER BY id",
    "faces": "SELECT id,person_id FROM face_prints ORDER BY id",
    "verdicts": "SELECT person_a,person_b,same_person,face_a,face_b,file_a,bbox_a,file_b,bbox_b FROM face_verifications ORDER BY 1,2",
    "constraints": "SELECT * FROM face_identity_constraints ORDER BY 1,2,3",
    "merge_queue": "SELECT person_id,other_person_id,score,namespace FROM face_merge_suggestions ORDER BY 1,2",
    "split_queue": "SELECT person_id,score,keep_json,split_json,namespace FROM face_split_suggestions ORDER BY 1",
    "review": "SELECT face_id,person_id,score,reason,runner_up_person_id,runner_up_score,namespace FROM face_review_candidates ORDER BY 1",
    "assessed": "SELECT face_id,namespace,outcome FROM face_assignment_assessed ORDER BY 1",
}


def rows(database: Path) -> dict:
    connection = sqlite3.connect(database)
    out = {}
    for name, sql in TABLES.items():
        try:
            out[name] = connection.execute(sql).fetchall()
        except sqlite3.OperationalError:
            out[name] = None
    connection.close()
    return out


def copy_catalog(source: Path, state: Path) -> Path:
    if state.exists():
        shutil.rmtree(state)
    database = database_path(state)
    database.parent.mkdir(parents=True)
    shutil.copy2(source, database)
    return database


def checkpoint(database: Path):
    connection = sqlite3.connect(database)
    connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    connection.close()


def people(database: Path):
    connection = sqlite3.connect(database)
    found = connection.execute(
        "SELECT p.id, MIN(f.id) FROM persons p JOIN face_prints f ON f.person_id=p.id "
        "GROUP BY p.id ORDER BY COUNT(*) DESC, p.id").fetchall()
    connection.close()
    return found


def correct(binary: Path, state: Path, home: Path):
    """User corrections through the Swift engine, as the app would send them."""
    database = database_path(state)
    found = people(database)
    if len(found) < 6:
        raise SystemExit("the stored scan has too few people for the parity check")
    engine = Engine(binary, state, log=state / "engine.log", home=home)
    try:
        engine.wait("ready", 180)
        (a, _), (b, _), (c, face_c), (d, face_d), (e, _), (f, _) = found[:6]
        engine.send({"renamePerson": {"personID": a, "firstName": "Person", "lastName": "One"}})
        engine.wait("bulkActionResult", 60)
        engine.send({"renamePerson": {"personID": b, "firstName": "Person", "lastName": "Two"}})
        engine.wait("bulkActionResult", 60)
        engine.send({"markPersonsDifferent": {"sourcePersonID": c, "destinationPersonID": d,
                                              "sourceAnchorFaceID": face_c, "destinationAnchorFaceID": face_d}})
        engine.wait("bulkActionResult", 60)
        engine.send({"markPersonsAsUnknown": {"personIDs": [e]}})
        engine.wait("bulkActionResult", 60)
        engine.send({"mergeClusters": {"sourcePersonID": f, "destinationPersonID": a}})
        engine.wait("bulkActionResult", 60)
        engine.send({"runFaceClustering": {}})
        engine.wait("faceClusteringComplete", 600)
    finally:
        engine.close()
    checkpoint(database)


def cluster(binary: Path, state: Path, home: Path, refresh: bool = False):
    engine = Engine(binary, state, log=state / "engine.log", home=home)
    try:
        engine.wait("ready", 180)
        started = time.monotonic()
        engine.send({"runFaceClustering": {}})
        summary = engine.wait("faceClusteringComplete", 600)
        seconds = time.monotonic() - started
        backlog = None
        if refresh:
            engine.send({"listFaceReview": {"requestID": "parity-refresh", "queue": "refresh"}})
            backlog = engine.wait("faceReview", 60, "parity-refresh").get("refresh")
    finally:
        engine.close()
    checkpoint(database_path(state))
    return summary, seconds, backlog


def compare(left: dict, right: dict) -> dict:
    out = {}
    for name in TABLES:
        a, b = left[name], right[name]
        if a == b:
            out[name] = "identical" if a is not None else "absent"
        else:
            changed = len(set(a or []) ^ set(b or []))
            out[name] = f"differs ({len(a or [])} vs {len(b or [])} rows, {changed} rows differ)"
    return out


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--swift", type=Path, required=True)
    parser.add_argument("--rust", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True, help="people_eval.py work folder holding swift-scan-default")
    parser.add_argument("--home", type=Path, required=True)
    parser.add_argument("--reps", type=int, default=3)
    args = parser.parse_args()
    work = require_internal(args.work, "work folder")
    home = prepare_home(args.home)
    snapshot = work / "swift-scan-default/snapshot.sqlite"
    if not snapshot.exists():
        raise SystemExit(f"no stored Swift scan at {snapshot}; run people_eval.py with the Swift engine first")
    before = live_state_snapshot()

    prepared_state = work / "parity-prepared"
    copy_catalog(snapshot, prepared_state)
    correct(args.swift, prepared_state, home)
    prepared = database_path(prepared_state)
    reference = rows(prepared)

    report = {"reps": []}
    for rep in range(args.reps):
        for flavor, binary in (("swift", args.swift), ("rust", args.rust)):
            state = work / f"parity-{flavor}"
            copy_catalog(prepared, state)
            summary, seconds, backlog = cluster(binary, state, home, refresh=flavor == "rust")
            report["reps"].append({"flavor": flavor, "seconds": round(seconds, 3),
                                   "persons": summary.get("personCount"), "faces": summary.get("faceCount"),
                                   "backlog": backlog})
    swift_rows, rust_rows = rows(database_path(work / "parity-swift")), rows(database_path(work / "parity-rust"))
    report["swiftVsRust"] = compare(swift_rows, rust_rows)
    report["preparedVsRust"] = {name: compare(reference, rust_rows)[name] for name in ("persons", "verdicts", "constraints")}
    named = [row for row in reference["persons"] if any(row[1:7]) or row[7]]
    kept = {row[0]: row[1:8] for row in rust_rows["persons"]}
    report["namedIdentitiesKept"] = f"{sum(1 for row in named if kept.get(row[0]) == row[1:8])} of {len(named)}"
    changes = diff_snapshots(before, live_state_snapshot())
    report["liveStateChanges"] = changes
    (work / "report-parity.json").write_text(json.dumps(report, indent=1))
    print(json.dumps({k: v for k, v in report.items() if k != "reps"}, indent=1))
    for flavor in ("swift", "rust"):
        times = [r["seconds"] for r in report["reps"] if r["flavor"] == flavor]
        print(f"{flavor}: runFaceClustering {', '.join(f'{t:.3f}' for t in times)} s")
    print("rust refresh backlog:", report["reps"][-1]["backlog"])
    identical = all(value in ("identical", "absent") for value in report["swiftVsRust"].values())
    return 0 if identical and not changes else 1


if __name__ == "__main__":
    sys.exit(main())
