#!/usr/bin/env python3
"""A/B a re-exported RAM++ ONNX (e.g. 256px) against the shipped one (384px).

Answers the two questions that gate a lower-res ship:
  1. ACCURACY (EP-independent): per-image tag-set agreement (Jaccard + F1, with
     the 384 model as ground truth) over a real corpus sample. This is the
     decisive "does 256 hold tag quality?" check and runs identically on any EP.
  2. LATENCY (compute proxy): median ms/inference for each model on the SAME EP.
     CPU ratio is a first-order proxy for the GPU compute reduction (RAM++ is
     GPU-compute-bound, so a CPU compute-ratio tracks the GPU win direction).

Preprocess mirrors ram_plus.rs: resize to the model's own input H (read from the
session) with bilinear, /255, ImageNet normalize, NCHW float32. Thresholding
mirrors select_tags: sigmoid(logit) >= max(per_class_threshold, precision_floor).

Usage:
  python ram_ab.py --base <dir-with-384-onnx+tags+thresholds> \
                   --cand <dir-with-256-onnx+tags+thresholds> \
                   --corpus G:\\TrueNAS\\Users --n 150
"""
import argparse
import glob
import os
import random
import statistics
import time

import numpy as np
import onnxruntime as ort
from PIL import Image

IMAGENET_MEAN = np.array([0.485, 0.456, 0.406], dtype=np.float32)
IMAGENET_STD = np.array([0.229, 0.224, 0.225], dtype=np.float32)
PRECISION_FLOOR = 0.62  # FILEID_RAMPLUS_PRECISION_FLOOR default (ram_plus.rs)


def load_lines(path):
    with open(path, encoding="utf-8") as f:
        return [ln.rstrip("\n") for ln in f if ln.strip() != ""]


def sigmoid(x):
    return 1.0 / (1.0 + np.exp(-x))


class Model:
    def __init__(self, d, providers):
        onnx_path = os.path.join(d, "ram_plus.onnx")
        self.sess = ort.InferenceSession(onnx_path, providers=providers)
        self.ep = self.sess.get_providers()[0]
        self.iname = self.sess.get_inputs()[0].name
        shp = self.sess.get_inputs()[0].shape  # [1,3,H,W]
        self.size = int(shp[2]) if isinstance(shp[2], int) else 384
        self.tags = load_lines(os.path.join(d, "ram_plus_tags.txt"))
        tp = os.path.join(d, "ram_plus_thresholds.txt")
        if os.path.exists(tp):
            self.thr = np.array([float(x) for x in load_lines(tp)], dtype=np.float32)
        else:
            self.thr = np.full(len(self.tags), 0.68, dtype=np.float32)
        self.cut = np.maximum(self.thr, PRECISION_FLOOR)

    def prep(self, img):
        im = img.convert("RGB").resize((self.size, self.size), Image.BILINEAR)
        a = np.asarray(im, dtype=np.float32) / 255.0
        a = (a - IMAGENET_MEAN) / IMAGENET_STD
        return np.transpose(a, (2, 0, 1))[None, :, :, :].astype(np.float32)

    def run(self, x):
        return self.sess.run(None, {self.iname: x})[0].reshape(-1)

    def tagset(self, logits):
        p = sigmoid(logits)
        idx = np.nonzero(p >= self.cut)[0]
        return {self.tags[i] for i in idx}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", required=True, help="shipped 384 model dir")
    ap.add_argument("--cand", required=True, help="candidate (e.g. 256) model dir")
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--n", type=int, default=150)
    ap.add_argument("--seed", type=int, default=42)
    args = ap.parse_args()

    avail = ort.get_available_providers()
    prefer = [p for p in ("CUDAExecutionProvider", "DmlExecutionProvider", "CPUExecutionProvider") if p in avail]
    print(f"available EPs: {avail}\nusing: {prefer}")

    base = Model(args.base, prefer)
    cand = Model(args.cand, prefer)
    print(f"base  size={base.size} ep={base.ep} tags={len(base.tags)}")
    print(f"cand  size={cand.size} ep={cand.ep} tags={len(cand.tags)}")

    imgs = []
    for ext in ("jpg", "jpeg", "png"):
        imgs += glob.glob(os.path.join(args.corpus, "**", f"*.{ext}"), recursive=True)
    random.seed(args.seed)
    random.shuffle(imgs)
    imgs = imgs[: args.n]
    print(f"sampling {len(imgs)} images")

    # latency: warm + timed on a real preprocessed image
    probe = Image.open(imgs[0])
    for _ in range(3):
        base.run(base.prep(probe)); cand.run(cand.prep(probe))
    def med_ms(m):
        xs = []
        for _ in range(25):
            x = m.prep(probe); t = time.perf_counter(); m.run(x); xs.append((time.perf_counter() - t) * 1000)
        return statistics.median(xs)
    lat_b, lat_c = med_ms(base), med_ms(cand)

    # accuracy: tag-set agreement over the sample
    jac, f1, n_ok = [], [], 0
    extra_total = miss_total = 0
    for p in imgs:
        try:
            im = Image.open(p)
            sb = base.tagset(base.run(base.prep(im)))
            sc = cand.tagset(cand.run(cand.prep(im)))
        except Exception:
            continue
        n_ok += 1
        inter = len(sb & sc); union = len(sb | sc) or 1
        jac.append(inter / union)
        prec = inter / (len(sc) or 1); rec = inter / (len(sb) or 1)
        f1.append(0.0 if prec + rec == 0 else 2 * prec * rec / (prec + rec))
        miss_total += len(sb - sc)   # tags the 384 had that 256 dropped
        extra_total += len(sc - sb)  # tags 256 added that 384 didn't

    print("\n================ RAM++ A/B ================")
    print(f"images compared: {n_ok}")
    print(f"latency  base({base.size})={lat_b:.1f}ms  cand({cand.size})={lat_c:.1f}ms  "
          f"speedup={lat_b / lat_c:.2f}x  ({base.ep})")
    print(f"tag Jaccard mean={statistics.mean(jac):.3f}  median={statistics.median(jac):.3f}")
    print(f"tag F1      mean={statistics.mean(f1):.3f}  median={statistics.median(f1):.3f}")
    print(f"avg dropped tags/img (384 had, 256 missed): {miss_total / n_ok:.2f}")
    print(f"avg added tags/img   (256 added, 384 lacked): {extra_total / n_ok:.2f}")
    print(f"RESULT ab base={base.size} cand={cand.size} ep={base.ep} "
          f"speedup={lat_b / lat_c:.2f} jaccard={statistics.mean(jac):.3f} f1={statistics.mean(f1):.3f}")


if __name__ == "__main__":
    main()
