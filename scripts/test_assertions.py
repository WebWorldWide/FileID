#!/usr/bin/env python3
"""
Validate FileID's database after a corpus scan + face clustering.

Read-only. Exits 0 on all-pass, non-zero on first failure.
Usage: python3 scripts/test_assertions.py
"""

from __future__ import annotations

import sqlite3
import sys
from dataclasses import dataclass
from pathlib import Path


GREEN = "\033[32m"
RED = "\033[31m"
YELLOW = "\033[33m"
BOLD = "\033[1m"
RESET = "\033[0m"


@dataclass
class Assertion:
    name: str
    passed: bool
    detail: str


def db_path() -> Path:
    return Path.home() / "Library" / "Application Support" / "FileID" / "fileid.sqlite"


def assertions(conn: sqlite3.Connection) -> list[Assertion]:
    out: list[Assertion] = []

    def query_int(sql: str) -> int:
        return int(conn.execute(sql).fetchone()[0])

    # ── Files indexed ─────────────────────────────────────────────
    n_files = query_int("SELECT COUNT(*) FROM files")
    out.append(Assertion(
        "files indexed >= 10",
        n_files >= 10,
        f"got {n_files} (corpus had ~15)"
    ))

    # ── No silent failures ────────────────────────────────────────
    n_failed = query_int("SELECT COUNT(*) FROM files WHERE failed = 1")
    out.append(Assertion(
        "zero scan failures",
        n_failed == 0,
        f"{n_failed} files marked failed"
    ))

    # ── Faces detected ────────────────────────────────────────────
    n_faces = query_int("SELECT COUNT(*) FROM face_prints")
    out.append(Assertion(
        "face_prints rows >= 8",
        n_faces >= 8,
        f"got {n_faces} (corpus has ~10 face photos)"
    ))

    # ── Quality filter not catastrophic ───────────────────────────
    n_excluded = query_int("SELECT COUNT(*) FROM face_prints WHERE excluded = 1")
    excluded_pct = (100.0 * n_excluded / n_faces) if n_faces else 0.0
    out.append(Assertion(
        "quality filter excludes < 80% of faces",
        excluded_pct < 80,
        f"{n_excluded}/{n_faces} excluded ({excluded_pct:.1f}%) — "
        "(was 97.5% in the bug we just fixed)"
    ))

    # ── ArcFace coverage ──────────────────────────────────────────
    n_clusterable = query_int(
        "SELECT COUNT(*) FROM face_prints WHERE excluded = 0"
    )
    n_arcface = query_int(
        "SELECT COUNT(*) FROM face_prints "
        "WHERE excluded = 0 AND LENGTH(arcface_embedding) > 0"
    )
    arcface_pct = (100.0 * n_arcface / n_clusterable) if n_clusterable else 0.0
    out.append(Assertion(
        "ArcFace coverage on clusterable faces >= 80%",
        arcface_pct >= 80,
        f"{n_arcface}/{n_clusterable} have ArcFace ({arcface_pct:.1f}%)"
    ))

    # ── Persons clustered ─────────────────────────────────────────
    n_persons = query_int("SELECT COUNT(*) FROM persons")
    # Corpus has 3 named figures (Einstein, Curie, Tesla). Clustering
    # might produce 3 (perfect) or up to ~6 (some fragmentation across
    # photo years/formats). > 8 means we're over-fragmenting; < 2 means
    # we collapsed everyone (the bug we just fixed).
    out.append(Assertion(
        "persons in 2..8 (3 ground-truth identities, allow 2-3x fragmentation)",
        2 <= n_persons <= 8,
        f"got {n_persons} (corpus ground truth: 3 identities)"
    ))

    # ── No singleton-only library (catches the 1-cluster bug) ─────
    n_multi_face_persons = query_int(
        "SELECT COUNT(*) FROM persons WHERE file_count >= 2"
    )
    out.append(Assertion(
        ">= 1 person has multiple photos (no all-singletons collapse)",
        n_multi_face_persons >= 1,
        f"{n_multi_face_persons} persons with >=2 photos"
    ))

    # ── No mega-cluster (catches the everyone-collapses-to-one bug) ─
    biggest_cluster = query_int(
        "SELECT COALESCE(MAX(file_count), 0) FROM persons"
    ) if n_persons > 0 else 0
    out.append(Assertion(
        "biggest cluster has < 80% of all face_prints (no mega-cluster)",
        biggest_cluster == 0 or biggest_cluster < 0.8 * n_clusterable,
        f"biggest cluster = {biggest_cluster} faces / "
        f"{n_clusterable} clusterable"
    ))

    # ── Duplicates detected ───────────────────────────────────────
    # Corpus has 2 known near-dups (einstein at 600px + 400px,
    # curie at 600px + 400px).
    n_dup_groups = query_int("""
        SELECT COUNT(*) FROM (
            SELECT phash FROM files
            WHERE phash IS NOT NULL AND phash != 0 AND failed = 0
            GROUP BY phash HAVING COUNT(*) > 1
        )
    """)
    out.append(Assertion(
        "phash duplicate groups >= 1 (corpus has near-dups)",
        n_dup_groups >= 1,
        f"got {n_dup_groups} duplicate group(s)"
    ))

    # ── FK integrity ──────────────────────────────────────────────
    orphan_face_prints = query_int("""
        SELECT COUNT(*) FROM face_prints fp
        WHERE NOT EXISTS (SELECT 1 FROM files WHERE files.id = fp.file_id)
    """)
    out.append(Assertion(
        "no orphan face_prints (file_id integrity)",
        orphan_face_prints == 0,
        f"{orphan_face_prints} orphan face_prints rows"
    ))

    orphan_tags = query_int("""
        SELECT COUNT(*) FROM tags t
        WHERE NOT EXISTS (SELECT 1 FROM files WHERE files.id = t.file_id)
    """)
    out.append(Assertion(
        "no orphan tags (file_id integrity)",
        orphan_tags == 0,
        f"{orphan_tags} orphan tag rows"
    ))

    return out


def main() -> int:
    p = db_path()
    if not p.exists():
        print(f"{RED}DB not found at {p}{RESET}")
        print(f"{YELLOW}Run a scan first (bash scripts/iterate.sh).{RESET}")
        return 2

    conn = sqlite3.connect(f"file:{p}?mode=ro", uri=True)
    try:
        results = assertions(conn)
    finally:
        conn.close()

    print(f"\n{BOLD}FileID assertion results{RESET}")
    print("─" * 70)
    failed_any = False
    for r in results:
        if r.passed:
            print(f"  {GREEN}✓ PASS{RESET}  {r.name}")
            print(f"           {r.detail}")
        else:
            failed_any = True
            print(f"  {RED}✗ FAIL{RESET}  {BOLD}{r.name}{RESET}")
            print(f"           {RED}{r.detail}{RESET}")
    print("─" * 70)
    n_pass = sum(1 for r in results if r.passed)
    n_total = len(results)
    if failed_any:
        print(f"  {RED}{BOLD}{n_pass}/{n_total} passed — {n_total - n_pass} failure(s){RESET}\n")
        return 1
    print(f"  {GREEN}{BOLD}{n_pass}/{n_total} passed{RESET}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
