#!/usr/bin/env python3
"""
Diagnose the "everyone clusters as one person" bug.

Checks two independent things:

1. **In-DB embeddings**: pulls a sample of ArcFace embeddings out of
   the live SQLite DB at ~/Library/Application Support/FileID/fileid.sqlite
   and computes pairwise cosine similarity. If they're all near 1.0,
   the model produced (effectively) constant output → the bug is in
   the model or its inputs. If they're varied, the bug is downstream
   (Chinese Whispers config, threshold, etc.).

2. **Model discrimination**: loads the converted .mlpackage and runs
   it against several actual face_crops/<id>.jpg files to confirm the
   model produces distinct embeddings for distinct inputs. If the DB
   embeddings are constant, this tells us whether the model itself is
   broken vs. whether the engine's pipeline mangles the inputs.

Read-only. Does not modify the DB or the .mlpackage.
"""

from __future__ import annotations

import argparse
import sqlite3
import struct
import sys
from pathlib import Path

try:
    import numpy as np
    import coremltools as ct
    from PIL import Image
except ImportError as e:
    print(f"Missing dependency: {e.name}. Activate the venv: source .venv/bin/activate")
    sys.exit(1)


def db_path() -> Path:
    return Path.home() / "Library" / "Application Support" / "FileID" / "fileid.sqlite"


def crops_dir() -> Path:
    return Path.home() / "Library" / "Application Support" / "FileID" / "face_crops"


def models_dir() -> Path:
    return Path.home() / "Library" / "Application Support" / "FileID" / "Models"


def decode_embedding(blob: bytes) -> np.ndarray:
    """Engine writes Float32 little-endian. Match that exactly."""
    n = len(blob) // 4
    return np.array(struct.unpack(f"<{n}f", blob), dtype=np.float32)


def cos(a: np.ndarray, b: np.ndarray) -> float:
    na = float(np.linalg.norm(a))
    nb = float(np.linalg.norm(b))
    if na == 0 or nb == 0:
        return 0.0
    return float(np.dot(a, b) / (na * nb))


# ─── Part 1: read embeddings out of the DB ───────────────────────────


def diagnose_db(sample_size: int = 20) -> dict:
    p = db_path()
    if not p.exists():
        print(f"  DB not found at {p}")
        print(f"  Run a scan first.")
        return {"db_present": False}

    conn = sqlite3.connect(f"file:{p}?mode=ro", uri=True)
    try:
        total_faces = conn.execute(
            "SELECT COUNT(*) FROM face_prints WHERE excluded = 0"
        ).fetchone()[0]
        with_arcface = conn.execute(
            "SELECT COUNT(*) FROM face_prints "
            "WHERE excluded = 0 AND LENGTH(arcface_embedding) > 0"
        ).fetchone()[0]
        excluded = conn.execute(
            "SELECT COUNT(*) FROM face_prints WHERE excluded = 1"
        ).fetchone()[0]
        person_count = conn.execute(
            "SELECT COUNT(*) FROM persons"
        ).fetchone()[0]

        print(f"  Faces total (not excluded): {total_faces}")
        print(f"  Faces with ArcFace embedding: {with_arcface}")
        print(f"  Faces excluded by quality filter: {excluded}")
        print(f"  Persons in DB: {person_count}")

        if with_arcface == 0:
            print("\n  No ArcFace embeddings in the DB.")
            print("  → Either no scan has run with the model installed,")
            print("    or extractPendingPrints didn't run / failed.")
            return {"db_present": True, "with_arcface": 0,
                    "person_count": person_count}

        # Pull a stratified random sample.
        rows = conn.execute(
            f"""
            SELECT id, person_id, arcface_embedding FROM face_prints
            WHERE excluded = 0 AND LENGTH(arcface_embedding) > 0
            ORDER BY RANDOM()
            LIMIT {sample_size}
            """
        ).fetchall()

        embeddings = []
        for row_id, person_id, blob in rows:
            v = decode_embedding(blob)
            embeddings.append((row_id, person_id, v))

        if not embeddings:
            print("  Sample empty — odd.")
            return {"db_present": True, "with_arcface": with_arcface,
                    "person_count": person_count}

        dim = len(embeddings[0][2])
        norms = [float(np.linalg.norm(v)) for _, _, v in embeddings]
        print(f"\n  Sample of {len(embeddings)} embeddings:")
        print(f"  Embedding dim: {dim}  (should be 512)")
        print(f"  Norm range: {min(norms):.4f} … {max(norms):.4f}  (should be 1.0)")

        # Pairwise cosine across the sample.
        n = len(embeddings)
        sims = []
        for i in range(n):
            for j in range(i + 1, n):
                sims.append(cos(embeddings[i][2], embeddings[j][2]))
        sims = np.array(sims)
        print(f"\n  Pairwise cosine similarity across the sample:")
        print(f"    min:    {sims.min():.4f}")
        print(f"    median: {float(np.median(sims)):.4f}")
        print(f"    mean:   {sims.mean():.4f}")
        print(f"    max:    {sims.max():.4f}")
        print(f"    fraction > 0.99: {float((sims > 0.99).mean()):.2%}")
        print(f"    fraction > 0.90: {float((sims > 0.90).mean()):.2%}")
        print(f"    fraction > 0.80: {float((sims > 0.80).mean()):.2%}")
        print(f"    fraction > 0.40: {float((sims > 0.40).mean()):.2%}  (CW edge threshold)")

        # Diagnosis.
        if sims.min() > 0.99:
            print("\n  ┌──────────────────────────────────────────────────────────")
            print("  │  ⚠ DEGENERATE: every embedding is ~identical.")
            print("  │")
            print("  │  The model is producing constant (or near-constant) output")
            print("  │  for every face. With every pair at cosine ≈ 1.0, Chinese")
            print("  │  Whispers correctly merges everything into one cluster.")
            print("  │")
            print("  │  Next: run the model-side check below to see if it's the")
            print("  │  model itself or our preprocessing.")
            print("  └──────────────────────────────────────────────────────────")
            verdict = "degenerate"
        elif (sims > 0.40).mean() > 0.95:
            print("\n  ┌──────────────────────────────────────────────────────────")
            print("  │  ⚠ TOO PERMISSIVE: > 95% of pairs above the kNN edge")
            print("  │  threshold (0.40). The kNN graph is nearly fully connected,")
            print("  │  so CW collapses to one cluster.")
            print("  │")
            print("  │  Likely a normalization or preprocessing issue making faces")
            print("  │  look more similar than they should.")
            print("  └──────────────────────────────────────────────────────────")
            verdict = "too_permissive"
        else:
            print("\n  ┌──────────────────────────────────────────────────────────")
            print("  │  ✓ Embeddings look healthy. The bug is downstream — Chinese")
            print("  │  Whispers config, persistence, or the runClustering branch")
            print("  │  picking the wrong embedder.")
            print("  └──────────────────────────────────────────────────────────")
            verdict = "embeddings_ok"
        return {"db_present": True, "with_arcface": with_arcface,
                "person_count": person_count, "verdict": verdict,
                "sims": sims}
    finally:
        conn.close()


# ─── Part 2: test the .mlpackage with real face crops ────────────────


def diagnose_model(variant: str = "iresnet50", n_crops: int = 6) -> dict:
    name = "arcface_iresnet50.mlpackage" if variant == "iresnet50" else "arcface_mobileface.mlpackage"
    mp = models_dir() / name
    if not mp.exists():
        print(f"  Model not found at {mp}")
        return {"model_present": False}

    print(f"  Loading {mp.name} …")
    model = ct.models.MLModel(str(mp))

    crops_p = crops_dir()
    if not crops_p.exists():
        print(f"  No face_crops/ directory yet — run a scan first.")
        return {"model_present": True, "crops_present": False}

    crops = sorted(crops_p.glob("*.jpg"))[:n_crops]
    if len(crops) < 2:
        print(f"  Need at least 2 crops in {crops_p}; have {len(crops)}.")
        return {"model_present": True, "crops_present": False}

    print(f"  Running model on {len(crops)} face crops …")
    embeddings = []
    for jpg in crops:
        img = Image.open(jpg).convert("RGB").resize((112, 112))
        out = model.predict({"input": img})
        # Output may be named various things; grab the first array-shaped value.
        vec = None
        for k, v in out.items():
            arr = np.asarray(v).flatten()
            if arr.size >= 64:
                vec = arr.astype(np.float32)
                break
        if vec is None:
            print(f"    {jpg.name}: no usable output")
            continue
        # L2-normalize the same way the engine does.
        nrm = float(np.linalg.norm(vec))
        if nrm > 0:
            vec = vec / nrm
        embeddings.append((jpg.name, vec))

    if len(embeddings) < 2:
        return {"model_present": True, "crops_present": True}

    print(f"\n  Pairwise cosine for {len(embeddings)} live face crops:")
    for i in range(len(embeddings)):
        for j in range(i + 1, len(embeddings)):
            c = cos(embeddings[i][1], embeddings[j][1])
            print(f"    {embeddings[i][0]:>16s}  vs  {embeddings[j][0]:>16s}  →  cosine = {c:+.4f}")

    sims = [cos(embeddings[i][1], embeddings[j][1])
            for i in range(len(embeddings))
            for j in range(i + 1, len(embeddings))]
    sims = np.array(sims)
    print(f"\n  Model-output cosine min: {sims.min():.4f}, max: {sims.max():.4f}")
    if sims.min() > 0.99:
        print("\n  ┌──────────────────────────────────────────────────────────")
        print("  │  ⚠ The model itself is producing identical output for")
        print("  │  different face crops. Conversion is broken.")
        print("  │")
        print("  │  Most likely: onnx2torch dropped BatchNorm running stats,")
        print("  │  or torch.jit.trace baked in the all-zeros example_input.")
        print("  │  Re-export with a real face image as the trace example,")
        print("  │  or use the `torch.fx`-based onnx2torch.")
        print("  └──────────────────────────────────────────────────────────")
    else:
        print("\n  ✓ Model produces distinct outputs for distinct inputs.")
        print("    Bug is in the engine's preprocessing / pipeline, not the model.")
    return {"model_present": True, "crops_present": True,
            "min_sim": float(sims.min()), "max_sim": float(sims.max())}


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--sample", type=int, default=20,
                   help="how many DB embeddings to sample (default 20)")
    p.add_argument("--crops", type=int, default=6,
                   help="how many face crops to feed the model (default 6)")
    p.add_argument("--variant", choices=["iresnet50", "mobileface"], default="iresnet50")
    args = p.parse_args()

    print("=" * 70)
    print(" Part 1 — embeddings as they live in the database")
    print("=" * 70)
    db_result = diagnose_db(sample_size=args.sample)

    print()
    print("=" * 70)
    print(f" Part 2 — does the {args.variant} .mlpackage discriminate live crops?")
    print("=" * 70)
    model_result = diagnose_model(variant=args.variant, n_crops=args.crops)

    print()
    print("=" * 70)
    print(" Summary")
    print("=" * 70)
    if db_result.get("verdict") == "degenerate":
        if model_result.get("min_sim", 1.0) > 0.99:
            print("  Model is broken. Conversion script needs to be re-run with a")
            print("  fix to onnx2torch trace input (use a real face, not zeros).")
        else:
            print("  Model is fine, but engine pipeline produces degenerate inputs.")
            print("  Check ArcFaceService.cgImageToPixelBuffer + the bias/scale in")
            print("  convert_arcface.py.")
    elif db_result.get("verdict") == "too_permissive":
        print("  Embeddings differ but the cosine threshold is too low for")
        print("  the actual distribution. Tune IdentityClustering pass1Cosine up.")
    elif db_result.get("verdict") == "embeddings_ok":
        print("  Embeddings are healthy. The 'one cluster' symptom is in")
        print("  IdentityClustering or the runClustering persistence path.")
    else:
        print("  Inconclusive — need a fresh scan with the model installed first.")


if __name__ == "__main__":
    main()
