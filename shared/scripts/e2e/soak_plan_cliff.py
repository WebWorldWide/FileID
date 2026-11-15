#!/usr/bin/env python3
"""Reproduce the Organize Folders plan-time cliff at 5,000 files (restructure_semantic.rs HNSW_MIN).

usage: python3 soak_plan_cliff.py --engine <binary> --work <scratch dir> [--files 5400] [--flavor rust]
Generates a library with soak_library.py plus near-identical one-line text files (what the soak adds each round), scans it, times planRestructure at the full size and again after
deleting enough files to leave fewer than 5,000 catalogued files."""
import argparse, os, shutil, subprocess, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import soak  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--engine", required=True)
ap.add_argument("--work", required=True)
ap.add_argument("--files", type=int, default=5400)
ap.add_argument("--flavor", default="rust")
ap.add_argument("--use-library", help="copy this library (for example the one a soak run left behind) instead of generating one")
a = ap.parse_args()
work = os.path.abspath(a.work)
lib = os.path.join(work, "lib")
shutil.rmtree(work, ignore_errors=True)
os.makedirs(work)
if a.use_library:
    subprocess.run(["cp", "-R", a.use_library, lib], check=True)
else:
    subprocess.run([sys.executable, os.path.join(os.path.dirname(os.path.abspath(__file__)), "soak_library.py"), lib, "--files", "3250"], check=True)
import random, uuid
dirs = sorted({d for d, _, fs in os.walk(lib) if fs and "Edge" not in d})
rng = random.Random(3)
for i in range(0 if a.use_library else max(0, a.files - 3000)):
    with open(os.path.join(rng.choice(dirs), f"new-r{i}-{uuid.uuid4().hex[:6]}.txt"), "w") as f:
        f.write(f"fresh file round {i} {uuid.uuid4()} harbor lantern\n")
e = soak.Engine(a.engine, a.flavor, os.path.join(work, "state"), os.path.join(work, "engine.log"), os.path.join(work, "Trash"), os.path.join(work, "models"), True, "cliff")


def visible():
    with soak.ro(e.db) as c:
        return c.execute("SELECT count(*) FROM files WHERE failed=0").fetchone()[0]


def plan():
    t = time.time()
    e.send("planRestructure", {"libraryRoot": lib})
    e.wait(lambda x: soak.kind(x) == "restructurePlan", 900)
    return time.time() - t


print(f"load {os.getloadavg()[0]:.1f}")
e.scan(lib)
print(f"{visible()} catalogued files: plan {plan():.1f}s")
victims = sorted(os.path.join(d, f) for d, _, fs in os.walk(lib) for f in fs if f.startswith("new-r"))
for p in victims[: max(0, visible() - 4800)]:
    os.remove(p)
e.scan(lib)
print(f"{visible()} catalogued files: plan {plan():.1f}s")
e.close()
