#!/usr/bin/env python3
"""Scores eyes-closed calls against hand labels, per split.

    python3 -I shared/scripts/eval/eye_eval.py --labels <eye-labels.json> \
        --mac <swift FaceMeshTests.evalHarness output> [--rust <eye_state_eval output>]

Labels: {"split": {"tune": [events], "heldout": [events]}, "faces": [{"id", "event",
"image", "bbox": [x1, y1, x2, y2], "eyes": "open" | "closed" | "unsure"}]}. Methods:
Vision's eye-contour openness below 0.35, the Mac mesh on Vision's face (ratio below
0.175), the Mac mesh and the Rust mesh from the labelled box alone. A face a method
cannot judge counts as open, as Best Takes treats it. Prints aggregate numbers only.
"""
import argparse
import json
import statistics
from pathlib import Path

EYES_CLOSED_BELOW = 0.35
CLOSED_RATIO, OPEN_RATIO = 0.14, 0.24


def openness(ratio):
    return min(1.0, max(0.0, (ratio - CLOSED_RATIO) / (OPEN_RATIO - CLOSED_RATIO)))


def load_lines(path):
    return {r["id"]: r for r in (json.loads(l) for l in Path(path).read_text().splitlines() if l.strip())}


def score(faces, judge):
    tp = fp = fn = judged = 0
    for face in faces:
        value = judge(face["id"])
        judged += value is not None
        closed = value is not None and value < EYES_CLOSED_BELOW
        truth = face["eyes"] == "closed"
        tp += closed and truth
        fp += closed and not truth
        fn += truth and not closed
    p = tp / (tp + fp) if tp + fp else float("nan")
    r = tp / (tp + fn) if tp + fn else float("nan")
    return p, r, judged / len(faces) if faces else float("nan")


def best_threshold(faces, ratio):
    best = (0.0, None)
    for step in range(100, 260, 5):
        t = step / 1000
        tp = sum(1 for f in faces if (v := ratio(f["id"])) is not None and v < t and f["eyes"] == "closed")
        fp = sum(1 for f in faces if (v := ratio(f["id"])) is not None and v < t and f["eyes"] != "closed")
        fn = sum(1 for f in faces if f["eyes"] == "closed") - tp
        f1 = 2 * tp / (2 * tp + fp + fn) if tp else 0.0
        best = max(best, (f1, t), key=lambda b: b[0])
    return best


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--labels", required=True)
    parser.add_argument("--mac", required=True)
    parser.add_argument("--rust")
    args = parser.parse_args()
    labels = json.loads(Path(args.labels).read_text())
    mac = load_lines(args.mac)
    rust = load_lines(args.rust) if args.rust else {}
    get = lambda table, key: (lambda i: table.get(i, {}).get(key))
    ratio_judge = lambda f: (lambda i: None if f(i) is None else openness(f(i)))
    methods = {
        "Vision eye contours": get(mac, "visionOpen"),
        "Mac mesh, Vision face": ratio_judge(get(mac, "meshRatio")),
        "Mac mesh, labelled box": ratio_judge(get(mac, "boxRatio")),
    }
    if rust:
        methods["Rust mesh, labelled box"] = ratio_judge(get(rust, "ratio"))
    faces = [f for f in labels["faces"] if f["eyes"] in ("open", "closed")]
    counts = {e: sum(1 for f in labels["faces"] if f["eyes"] == e) for e in ("open", "closed", "unsure")}
    print(f"faces {len(labels['faces'])}: {counts}")
    for split, events in labels["split"].items():
        part = [f for f in faces if f["event"] in events]
        closed = sum(f["eyes"] == "closed" for f in part)
        print(f"\n{split}: {len(part)} judged faces ({closed} closed), {len(events)} events")
        for name, judge in methods.items():
            p, r, cover = score(part, judge)
            print(f"  {name:26s} precision {p:.2f}  recall {r:.2f}  judges {cover:.0%}")
        for name, key in (("Mac mesh, Vision face", "meshRatio"), ("Mac mesh, labelled box", "boxRatio")):
            f1, t = best_threshold(part, get(mac, key))
            print(f"  best F1 threshold ({name}): ratio < {t} (F1 {f1:.2f})")
    for key, label in (("meshMs", "Mac mesh, Vision face"), ("boxMs", "Mac mesh, labelled box")):
        times = sorted(r[key] for r in mac.values() if key in r)
        if times:
            print(f"\n{label}: median {statistics.median(times):.1f} ms, p90 {times[int(len(times) * 0.9)]:.1f} ms per face ({len(times)} faces)")
    if rust:
        times = sorted(r["ms"] for r in rust.values() if r.get("ms") is not None)
        print(f"Rust mesh, labelled box: median {statistics.median(times):.1f} ms, p90 {times[int(len(times) * 0.9)]:.1f} ms per face")
        pairs = [(m["boxRatio"], rust[i]["ratio"]) for i, m in mac.items() if i in rust and m.get("boxRatio") is not None and rust[i].get("ratio") is not None]
        diffs = sorted(abs(a - b) for a, b in pairs)
        agree = sum((a < 0.175) == (b < 0.175) for a, b in pairs)
        both_none = sum(1 for i, m in mac.items() if i in rust and (m.get("boxRatio") is None) != (rust[i].get("ratio") is None))
        if diffs:
            print(f"Mac vs Rust (labelled box): {len(pairs)} faces, |ratio diff| median {statistics.median(diffs):.4f}, p99 {diffs[int(len(diffs) * 0.99)]:.4f}, "
                  f"max {diffs[-1]:.4f}; same closed call on {agree}/{len(pairs)}; judged by only one engine: {both_none}")


if __name__ == "__main__":
    main()
