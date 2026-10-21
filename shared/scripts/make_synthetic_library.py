#!/usr/bin/env python3
"""Build a synthetic FileID library database for Chat latency measurements.

The engine creates and migrates the database, then this script fills it with
deterministic rows: files of every kind spread over six years, scan tags,
descriptions, text in images, document text, named people with faces, video
passages and stored-hash duplicates. No real files are created; paths point
under /synthetic and never exist.

    make_synthetic_library.py --engine <FileIDEngine> --out <dir> [--files 100000]
"""
from __future__ import annotations

import argparse
import hashlib
import random
import sqlite3
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from conformance import Engine  # noqa: E402

WORDS = """dog cat beach sunset mountain snow forest river lake city street car bicycle train airplane boat
birthday cake candle gift party balloon wedding dress bride groom church garden flower tree grass sky cloud
rain umbrella kitchen food pizza pasta salad coffee tea breakfast dinner table chair sofa window door house
building bridge tower museum statue painting book desk laptop phone screen keyboard guitar piano drum concert
stage crowd soccer football basketball baseball tennis golf swimming pool ocean wave sand shell rock cliff
desert camel horse cow sheep chicken duck bird eagle owl butterfly bee insect fish shark whale dolphin turtle
baby child kid toddler teenager adult grandmother grandfather family friend couple portrait selfie group smile
laugh hug kiss dance graduation diploma school classroom teacher student homework receipt invoice contract
letter envelope stamp map passport ticket hotel tent campfire hiking trail lantern night star moon fireworks
christmas halloween pumpkin costume easter egg thanksgiving turkey snowman sled ski snowboard skate park
playground swing slide sandbox picnic blanket basket fruit apple banana orange grape strawberry watermelon""".split()
COMPOUND = ["blue_sky", "green_grass", "sandy_beach", "birthday_party", "night_sky", "city_street", "living_room",
            "dining_table", "christmas_tree", "swimming_pool", "soccer_ball", "wedding_cake", "mountain_lake"]
FIRST = ["Kyle", "Emma", "Liam", "Olivia", "Noah", "Ava", "Mason", "Sophia", "Lucas", "Mia", "Ethan", "Grace",
         "Henry", "Chloe", "Jack", "Ella", "Owen", "Lily", "Leo", "Zoe"]
LAST = ["Nolle", "Smith", "Garcia", "Chen", "Patel", "Brown", "Lopez", "Kim"]
KINDS = [("image", 0.60, ["jpg", "heic", "png"]), ("video", 0.15, ["mov", "mp4"]), ("doc", 0.12, ["txt", "docx", "md"]),
         ("pdf", 0.08, ["pdf"]), ("audio", 0.03, ["m4a", "mp3"]), ("other", 0.02, ["zip", "dmg"])]
START = 1577836800  # 2020-01-01 UTC
SPAN = 6 * 365 * 86400


def create_database(engine: Path, out: Path) -> Path:
    out.mkdir(parents=True, exist_ok=True)
    process = Engine(engine, out)
    try:
        process.wait("ready", timeout=120)
    finally:
        process.close()
    return out / "FileID/fileid.sqlite"


def sentence(rng: random.Random, count: int) -> str:
    return " ".join(rng.choice(WORDS) for _ in range(count))


def fill(database: Path, files: int, seed: int) -> None:
    rng = random.Random(seed)
    connection = sqlite3.connect(database)
    connection.execute("PRAGMA journal_mode=WAL")
    connection.execute("PRAGMA synchronous=OFF")
    if connection.execute("SELECT COUNT(*) FROM files").fetchone()[0]:
        raise SystemExit("The database already has files")
    cumulative, kinds = 0.0, []
    for kind, share, extensions in KINDS:
        cumulative += share
        kinds.append((cumulative, kind, extensions))
    rows, tags, ocr, docs, faces, passages = [], [], [], [], [], []
    hashes: list[tuple[bytes, int]] = []
    folders = {"image": ["Photos/{y}/{m:02d}", "Downloads", "Desktop", "Photos/Trips/{y}"],
               "video": ["Videos/{y}", "Photos/{y}/{m:02d}", "Downloads"],
               "doc": ["Documents/Work", "Documents/Home", "Downloads", "Desktop"],
               "pdf": ["Documents/Receipts/{y}", "Documents/Taxes", "Downloads"],
               "audio": ["Music", "Voice Memos"], "other": ["Downloads", "Archives"]}
    for file_id in range(1, files + 1):
        roll = rng.random()
        kind, extensions = next((k, e) for c, k, e in kinds if roll <= c or c == kinds[-1][0])
        created = START + rng.randrange(SPAN)
        year, month = 2020 + (created - START) // (365 * 86400), 1 + rng.randrange(12)
        folder = rng.choice(folders[kind]).format(y=min(year, 2025), m=month)
        extension = rng.choice(extensions)
        name = f"{rng.choice(WORDS)}_{file_id:06d}.{extension}"
        size = {"image": rng.randint(800_000, 9_000_000), "video": rng.randint(20_000_000, 4_000_000_000),
                "doc": rng.randint(4_000, 2_000_000), "pdf": rng.randint(20_000, 8_000_000),
                "audio": rng.randint(500_000, 40_000_000), "other": rng.randint(1_000_000, 900_000_000)}[kind]
        digest = hashlib.sha256(f"{seed}:{file_id}".encode()).digest()
        if hashes and rng.random() < 0.04:
            digest, size = rng.choice(hashes)
        elif len(hashes) < 20_000:
            hashes.append((digest, size))
        description = sentence(rng, rng.randint(8, 18)) if kind in ("image", "video") and rng.random() < 0.25 else None
        rows.append((file_id, f"/synthetic/Library/{folder}/{name}", file_id, size, float(created), float(created + 60),
                     float(created + 120), kind, extension, digest, description,
                     rng.random() if kind == "image" else None))
        if kind in ("image", "video"):
            for tag in set(rng.sample(WORDS, rng.randint(5, 9)) + ([rng.choice(COMPOUND)] if rng.random() < 0.3 else [])):
                tags.append((file_id, tag, "auto", round(rng.uniform(0.3, 0.99), 3)))
            if rng.random() < 0.1:
                ocr.append((file_id, sentence(rng, rng.randint(4, 30)), int(rng.random() < 0.2)))
        if kind in ("doc", "pdf"):
            docs.append((file_id, sentence(rng, rng.randint(30, 120))))
        if kind == "image" and rng.random() < 0.2:
            for _ in range(rng.randint(1, 3)):
                faces.append((file_id, 1 + rng.randrange(len(FIRST) * 3)))
        if kind == "video" and rng.random() < 0.15:
            for index in range(rng.randint(1, 4)):
                start = index * 30.0
                passages.append((f"p{file_id}-{index}", file_id, start, start + 10, sentence(rng, 12)))
    connection.executemany(
        "INSERT INTO files(id,path_text,path_hash,size_bytes,created_at,modified_at,scanned_at,kind,extension,"
        "content_hash,vlm_description,aesthetic) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)", rows)
    connection.executemany("INSERT OR IGNORE INTO tags(file_id,tag,source,score) VALUES(?,?,?,?)", tags)
    connection.executemany("INSERT INTO ocr_text(file_id,text,handwritten) VALUES(?,?,?)", ocr)
    connection.executemany("INSERT INTO doc_text(file_id,text) VALUES(?,?)", docs)
    people = []
    for person in range(1, len(FIRST) * 3 + 1):
        first, last = FIRST[(person - 1) % len(FIRST)], LAST[(person - 1) // len(FIRST) % len(LAST)]
        named = person <= len(FIRST) * 2
        people.append((person, f"{first} {last}" if named else None, first if named else None,
                       last if named else None, 0.0, 0 if named else 1))
    connection.executemany(
        "INSERT INTO persons(id,name,first_name,last_name,created_at,is_unknown) VALUES(?,?,?,?,?,?)", people)
    connection.executemany("INSERT INTO face_prints(file_id,person_id,print_data,bbox) VALUES(?,?,X'00','[0,0,1,1]')",
                           faces)
    connection.execute("UPDATE persons SET file_count=(SELECT COUNT(DISTINCT file_id) FROM face_prints "
                       "WHERE person_id=persons.id)")
    connection.executemany(
        "INSERT INTO catalog_passages(id,file_id,start_seconds,end_seconds,text,source_revision,model_version,"
        "confidence) VALUES(?,?,?,?,?,'synthetic','synthetic',1)", passages)
    connection.execute("INSERT INTO scan_sessions(id,root_path,started_at,completed_at,status) "
                       "VALUES('synthetic','/synthetic/Library',0,1,'completed')")
    connection.commit()
    connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    connection.close()
    print(f"{files} files, {len(tags)} tags, {len(ocr)} OCR, {len(docs)} documents, {len(faces)} faces, "
          f"{len(passages)} passages")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--engine", type=Path, required=True)
    parser.add_argument("--out", type=Path, default=None, help="state directory (default: a new temporary one)")
    parser.add_argument("--files", type=int, default=100_000)
    parser.add_argument("--seed", type=int, default=19)
    args = parser.parse_args()
    out = args.out or Path(tempfile.mkdtemp(prefix="FileIDSynthetic-"))
    database = create_database(args.engine.resolve(strict=True), out)
    fill(database, args.files, args.seed)
    print(out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
