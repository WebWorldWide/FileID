#!/usr/bin/env python3
"""Tag-quality report for RAM++ tuning.

Reads the FileID SQLite DB and prints, beyond scan_assertions.py's top-40 eyeball:
  - global tag frequency + mean score (so junk like "catch x40" is measurable)
  - the lowest-confidence accepted tags (the cut candidates for the suppress list
    or a higher precision floor)
  - optional per-file tag dump

Iterate: edit ram_plus_suppress.txt / set FILEID_RAMPLUS_PRECISION_FLOOR, rescan
with `iterate.ps1 -SkipBuild`, then diff this report run-to-run.

Usage:
  python tag_report.py [--db PATH] [--source auto] [--top 40] [--low 25] [--per-file]
"""
import argparse
import os
import sqlite3
import sys
from collections import defaultdict


def default_db():
    base = os.environ.get("LOCALAPPDATA") or os.path.expanduser("~")
    return os.path.join(base, "FileID", "fileid.sqlite")


def main():
    ap = argparse.ArgumentParser(description="RAM++ tag-quality report")
    ap.add_argument("--db", default=default_db(), help="path to fileid.sqlite")
    ap.add_argument("--source", default="auto",
                    help="tag source filter (e.g. auto); 'all' for no filter")
    ap.add_argument("--top", type=int, default=40, help="top-N by frequency")
    ap.add_argument("--low", type=int, default=25,
                    help="N lowest-confidence accepted tags to list")
    ap.add_argument("--per-file", action="store_true", help="dump tags per file")
    args = ap.parse_args()

    if not os.path.exists(args.db):
        print(f"DB not found: {args.db}", file=sys.stderr)
        sys.exit(2)

    con = sqlite3.connect(args.db)
    con.row_factory = sqlite3.Row

    if args.source == "all":
        rows = con.execute("SELECT file_id, tag, source, score FROM tags").fetchall()
    else:
        rows = con.execute(
            "SELECT file_id, tag, source, score FROM tags WHERE source = ?",
            (args.source,),
        ).fetchall()

    if not rows:
        print(f"No tags for source={args.source!r} in {args.db}", file=sys.stderr)
        sys.exit(1)

    freq = defaultdict(int)
    ssum = defaultdict(float)
    for r in rows:
        t = r["tag"]
        freq[t] += 1
        ssum[t] += (r["score"] or 0.0)

    nfiles = con.execute("SELECT COUNT(*) FROM files").fetchone()[0]
    ntagged = len({r["file_id"] for r in rows})

    print(f"== tag report ==  db={args.db}")
    print(f"files={nfiles}  tagged_files={ntagged}  tags={len(rows)}  "
          f"unique={len(freq)}  source={args.source}")
    if ntagged:
        print(f"avg tags/tagged-file = {len(rows) / ntagged:.2f}")

    print(f"\n-- top {args.top} by frequency (count | mean_score | tag) --")
    for t in sorted(freq, key=lambda k: (-freq[k], k))[: args.top]:
        print(f"{freq[t]:5d} | {ssum[t] / freq[t]:.3f} | {t}")

    print(f"\n-- {args.low} lowest-confidence accepted tags (cut candidates) --")
    for r in sorted(rows, key=lambda r: (r["score"] or 0.0))[: args.low]:
        print(f"{(r['score'] or 0.0):.3f} | {r['tag']}")

    if args.per_file:
        print("\n-- per-file --")
        byf = defaultdict(list)
        for r in rows:
            byf[r["file_id"]].append((r["tag"], r["score"] or 0.0))
        pathmap = {x["id"]: x["path_text"]
                   for x in con.execute("SELECT id, path_text FROM files")}
        for fid, tags in sorted(byf.items()):
            tags.sort(key=lambda x: -x[1])
            name = os.path.basename(pathmap.get(fid, str(fid)))
            joined = ", ".join(f"{t}({s:.2f})" for t, s in tags)
            print(f"{name}: {joined}")


if __name__ == "__main__":
    main()
