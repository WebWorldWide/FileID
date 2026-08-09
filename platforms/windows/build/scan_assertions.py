#!/usr/bin/env python3
"""FileID (Windows) post-scan assertions.

Reads the engine SQLite DB after an `iterate.ps1` run and verifies tag +
face correctness. Prints a full summary, then asserts the invariants that
matter for the commercial-clean model swap:

  * the scan produced files with a low failure rate,
  * RAM++ / CLIP actually emitted tags,
  * face embeddings are 128-d SFace blobs (512 bytes), NOT 512-d ArcFace
    (2048 bytes) — the decisive proof the YuNet+SFace path is live,
  * face clustering formed at least one person when faces exist.

Exit 0 = GREEN, 1 = RED (assertion failed), 2 = environment problem.

Usage: python scan_assertions.py [path-to-fileid.sqlite]
Tunable via env: ASSERT_MIN_FILES, ASSERT_MAX_FAIL_RATE, ASSERT_EXPECT_FACES.
"""
import os
import sqlite3
import sys

db = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
    os.environ.get("LOCALAPPDATA", ""), "FileID", "fileid.sqlite")

if not os.path.exists(db):
    print(f"ENV-FAIL: DB not found at {db}")
    sys.exit(2)

con = sqlite3.connect(db)  # RW so any residual -wal is applied; engine has exited
con.row_factory = sqlite3.Row


def q1(sql, *a):
    return con.execute(sql, a).fetchone()[0]


def qall(sql, *a):
    return con.execute(sql, a).fetchall()


files = q1("SELECT count(*) FROM files")
failed = q1("SELECT count(*) FROM files WHERE failed=1")
with_faces = q1("SELECT count(*) FROM files WHERE has_faces=1")
with_text = q1("SELECT count(*) FROM files WHERE has_text=1")
tagged = q1("SELECT count(DISTINCT file_id) FROM tags")
total_tags = q1("SELECT count(*) FROM tags")
faces = q1("SELECT count(*) FROM face_prints")
persons = q1("SELECT count(*) FROM persons")
named = q1("SELECT count(*) FROM persons WHERE name IS NOT NULL AND name<>''")

bar = "=" * 64
print(bar)
print(f"files            {files}")
print(f"  failed         {failed}")
print(f"  has_faces      {with_faces}")
print(f"  has_text       {with_text}")
print(f"tagged files     {tagged}")
print(f"total tags       {total_tags}")
print(f"face_prints      {faces}")
print(f"persons          {persons}  (named {named})")

print("\n-- files by kind --")
for r in qall("SELECT kind, count(*) c FROM files GROUP BY kind ORDER BY c DESC"):
    print(f"  {r['kind']:<12} {r['c']}")

print("\n-- tags by source --")
for r in qall("SELECT source, count(*) c FROM tags GROUP BY source ORDER BY c DESC"):
    print(f"  {r['source']:<12} {r['c']}")

print("\n-- top 40 tags (the accuracy eyeball) --")
for r in qall("SELECT tag, count(*) c FROM tags GROUP BY tag ORDER BY c DESC LIMIT 40"):
    print(f"  {r['tag']:<26} {r['c']}")

print("\n-- face embedding blob sizes --")
size_rows = qall("SELECT length(print_data) n, count(*) c FROM face_prints GROUP BY n ORDER BY c DESC")
for r in size_rows:
    print(f"  {r['n']} bytes  x{r['c']}   ({r['n'] // 4}-d float32)")

print("\n-- largest person clusters --")
for r in qall("SELECT p.id, p.file_count, count(f.id) faces FROM persons p "
              "LEFT JOIN face_prints f ON f.person_id=p.id GROUP BY p.id "
              "ORDER BY faces DESC LIMIT 8"):
    print(f"  person {r['id']:<4} files={r['file_count']:<4} faces={r['faces']}")

# ---------------------------------------------------------------- assertions
fails = []
warns = []
MIN_FILES = int(os.environ.get("ASSERT_MIN_FILES", "1"))
MAX_FAIL_RATE = float(os.environ.get("ASSERT_MAX_FAIL_RATE", "0.10"))
EXPECT_FACES = os.environ.get("ASSERT_EXPECT_FACES", "0") == "1"

if files < MIN_FILES:
    fails.append(f"files {files} < required {MIN_FILES}")
if files and failed / files > MAX_FAIL_RATE:
    fails.append(f"failure rate {failed}/{files} = {failed/files:.1%} > {MAX_FAIL_RATE:.0%}")
if total_tags == 0:
    fails.append("no tags produced (RAM++/CLIP tagging path broken)")
elif tagged < max(1, files // 4):
    warns.append(f"only {tagged}/{files} files tagged")

# Decisive SFace check: 128-d float32 == 512 bytes. ArcFace was 2048.
sizes = [r["n"] for r in size_rows]
if faces:
    if any(s != 512 for s in sizes):
        fails.append(f"face print_data sizes {sizes} include non-512 "
                     f"(SFace must be 128-d/512B; 2048B = stale ArcFace)")
    if persons < 1:
        fails.append("faces present but zero person clusters formed")
elif EXPECT_FACES:
    fails.append("expected faces on this corpus but face_prints is empty")
else:
    warns.append("no faces detected in this subset")

print("\n" + bar)
for w in warns:
    print("WARN:", w)
if fails:
    for f in fails:
        print("FAIL:", f)
    print("RESULT: RED")
    sys.exit(1)
print("RESULT: GREEN")
sys.exit(0)
