#!/usr/bin/env python3
"""
Convert InsightFace's ArcFace ONNX (Buffalo-L iResNet50 or Buffalo-S
MobileFace) to a CoreML .mlpackage that runs on the Apple Neural Engine.

Output is dropped into ~/Library/Application Support/FileID/Models/
where ArcFaceService looks for it on next engine launch.

Run once per Mac (or once per build machine and ship the .mlpackage).
Re-run only when InsightFace publishes new weights.

Requirements
------------
    python3 -m venv .venv && source .venv/bin/activate
    pip install onnx coremltools onnxruntime huggingface_hub numpy \
                pillow onnx2torch torch

Usage
-----
    python3 scripts/convert_arcface.py --variant iresnet50
    python3 scripts/convert_arcface.py --variant mobileface

Pipeline
--------
coremltools 9.0 dropped direct ONNX conversion. The supported path is:
    ONNX → torch.nn.Module via onnx2torch
         → traced TorchScript via torch.jit.trace
         → CoreML .mlpackage via coremltools.convert

The Buffalo-L recognition head uses 112x112 RGB input with
(pixel - 127.5) / 127.5 normalization. We bake that into the CoreML
graph via ImageType bias/scale so the Swift caller hands in a plain
pixel buffer with no preprocessing.

Output validation: a single random tensor is run through both ONNX and
CoreML; cosine similarity between the two embeddings should be > 0.99.
A lower number means the conversion changed semantics; bail.
"""

from __future__ import annotations

import argparse
import os
import shutil
import sys
from pathlib import Path

try:
    import numpy as np
    import onnx
    import onnxruntime as ort
    import coremltools as ct
    import torch
    from onnx2torch import convert as onnx2torch_convert
    from huggingface_hub import hf_hub_download
except ImportError as e:
    print(f"Missing dependency: {e.name}. Install with:")
    print("  pip install onnx coremltools onnxruntime huggingface_hub numpy \\")
    print("              pillow onnx2torch torch")
    sys.exit(1)

VARIANTS = {
    "iresnet50": {
        "repo": "immich-app/buffalo_l",
        "filename": "recognition/model.onnx",
        "out_name": "arcface_iresnet50.mlpackage",
        "input_size": 112,
    },
    "mobileface": {
        "repo": "immich-app/buffalo_s",
        "filename": "recognition/model.onnx",
        "out_name": "arcface_mobileface.mlpackage",
        "input_size": 112,
    },
}


def application_support_models() -> Path:
    base = Path.home() / "Library" / "Application Support" / "FileID" / "Models"
    base.mkdir(parents=True, exist_ok=True)
    return base


def convert(variant: str, out_dir: Path) -> Path:
    cfg = VARIANTS[variant]
    print(f"[1/4] Downloading {cfg['repo']}/{cfg['filename']} from HuggingFace...")
    onnx_path = hf_hub_download(repo_id=cfg["repo"], filename=cfg["filename"])
    print(f"      ONNX at: {onnx_path}")

    print(f"[2/4] Loading ONNX graph + converting to PyTorch...")
    onnx_model = onnx.load(onnx_path)
    g_input = onnx_model.graph.input[0]
    shape = [d.dim_value for d in g_input.type.tensor_type.shape.dim]
    print(f"      ONNX input '{g_input.name}' shape: {shape}")

    in_size = cfg["input_size"]
    fixed_shape = [1, 3, in_size, in_size]

    # ONNX → torch.nn.Module. Buffalo-L's first dim is dynamic (0); we
    # trace at the fixed [1, 3, 112, 112] shape since that's what we run
    # at inference time anyway.
    torch_module = onnx2torch_convert(onnx_model)
    torch_module.eval()
    example_input = torch.zeros(fixed_shape, dtype=torch.float32)
    with torch.no_grad():
        traced = torch.jit.trace(torch_module, example_input, strict=False)

    print(f"[3/4] Converting traced TorchScript to CoreML .mlpackage...")
    # InsightFace ArcFace pre-processes pixels as (x - 127.5) / 127.5.
    # CoreML's ImageType bakes that into the graph: scale + bias applied
    # per channel before the model proper.
    #   normalized = (pixel * scale) + bias
    #   want: (pixel - 127.5) / 127.5  =  pixel * (1/127.5) + (-1.0)
    scale = 1.0 / 127.5
    bias = [-1.0, -1.0, -1.0]

    image_input = ct.ImageType(
        name="input",
        shape=fixed_shape,
        scale=scale,
        bias=bias,
        color_layout=ct.colorlayout.RGB,
    )

    mlmodel = ct.convert(
        traced,
        inputs=[image_input],
        compute_units=ct.ComputeUnit.ALL,    # ANE when shapes allow
        convert_to="mlprogram",              # required for .mlpackage + ANE
        minimum_deployment_target=ct.target.macOS14,
    )

    # Author the output name 'output' for a stable feature key in
    # ArcFaceService's firstMultiArray() lookup. ArcFace ONNX usually
    # already names this output 'output' or 'embedding'.
    out_path = out_dir / cfg["out_name"]
    if out_path.exists():
        print(f"      Removing existing {out_path}")
        shutil.rmtree(out_path)
    mlmodel.save(str(out_path))
    print(f"      Saved: {out_path}")

    print(f"[4/4] Verifying ONNX vs CoreML embeddings agree...")
    rng = np.random.default_rng(seed=42)
    test_image = rng.random((1, 3, in_size, in_size), dtype=np.float32)
    # ONNX path: feed raw pre-normalized tensor (the ONNX itself doesn't
    # bake normalization in; that's only on the CoreML side).
    test_image_norm = (test_image * 255.0 - 127.5) / 127.5
    ort_session = ort.InferenceSession(onnx_path, providers=["CPUExecutionProvider"])
    onnx_out = ort_session.run(None, {g_input.name: test_image_norm})[0].squeeze()

    # CoreML path: feed a PIL image at [0..255]; scale+bias inside graph
    # produces the same normalized tensor the ONNX got.
    from PIL import Image
    pil = Image.fromarray((test_image[0].transpose(1, 2, 0) * 255).astype(np.uint8), "RGB")
    coreml_out_dict = mlmodel.predict({"input": pil})
    coreml_out = list(coreml_out_dict.values())[0].squeeze()

    cos = float(np.dot(onnx_out, coreml_out) / (np.linalg.norm(onnx_out) * np.linalg.norm(coreml_out)))
    print(f"      Cosine similarity ONNX vs CoreML (single tensor): {cos:.6f}")
    if cos < 0.99:
        print(f"      ERROR: cosine < 0.99 — conversion changed semantics. Investigate.")
        sys.exit(2)

    # Diversity check — the previous bug we caught: the ONNX↔CoreML
    # match looked perfect, but the model was producing constant output
    # for every input (so every face cosine = 1.0, clustering collapsed
    # to one). Run several distinct random tensors through CoreML and
    # confirm pairwise cosines vary. Healthy: min < 0.95. Degenerate:
    # min > 0.99.
    print(f"      Running diversity check (5 distinct inputs)...")
    distinct_outs = []
    for s in range(5):
        rng2 = np.random.default_rng(seed=100 + s)
        rand = rng2.random((in_size, in_size, 3), dtype=np.float32)
        pil2 = Image.fromarray((rand * 255).astype(np.uint8), "RGB")
        out2 = list(mlmodel.predict({g_input.name: pil2}).values())[0].squeeze()
        nrm = float(np.linalg.norm(out2))
        if nrm > 0:
            out2 = out2 / nrm
        distinct_outs.append(out2)
    pair_sims = []
    for i in range(len(distinct_outs)):
        for j in range(i + 1, len(distinct_outs)):
            pair_sims.append(float(np.dot(distinct_outs[i], distinct_outs[j])))
    mn, mx = min(pair_sims), max(pair_sims)
    print(f"      Pairwise cosine across 5 distinct inputs: min {mn:+.4f}  max {mx:+.4f}")
    if mn > 0.95:
        print(f"      ERROR: model is degenerate — produces near-identical output")
        print(f"      regardless of input. Conversion or graph is broken.")
        sys.exit(3)
    print(f"      OK.\n")
    print(f"Done. Restart FileID to pick up the new model.")
    return out_path


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--variant", choices=list(VARIANTS.keys()), required=True,
                   help="Which ArcFace variant to convert.")
    p.add_argument("--out-dir", type=Path, default=None,
                   help="Output dir (default: ~/Library/Application Support/FileID/Models/)")
    args = p.parse_args()
    out_dir = args.out_dir or application_support_models()
    out_dir.mkdir(parents=True, exist_ok=True)
    convert(args.variant, out_dir)


if __name__ == "__main__":
    main()
