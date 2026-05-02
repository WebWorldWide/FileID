#!/usr/bin/env python3
# Convert OpenAI CLIP's text encoder + vocabulary into the format
# FileID's in-app CLIPTextEncoder + CLIPTokenizer expect.
#
# Run once after first install. Produces:
#   ~/Library/Application Support/FileID/Models/clip_text/
#       clip_text.mlpackage   (CoreML text encoder, ~150 MB)
#       vocab.json            (BPE vocabulary, ~1 MB)
#       merges.txt            (BPE merge rules, ~500 KB)
#
# After this completes, the FileID Library search bar will run
# semantic CLIP search instead of keyword search — type "sunset" and
# get every photo that visually looks like a sunset, even if no tag
# or caption matches.
#
# Requirements:
#   pip install torch open_clip_torch coremltools
#
# This downloads the MobileCLIP-S2 weights from open_clip's hub
# (~120 MB, one-time). Conversion then takes ~2 min on Apple Silicon.
import os
import json
import shutil
from pathlib import Path
import torch
import open_clip
import coremltools as ct

DEST_DIR = Path.home() / "Library" / "Application Support" / "FileID" / "Models" / "clip_text"
DEST_DIR.mkdir(parents=True, exist_ok=True)

print(f"Building CLIP text encoder → {DEST_DIR}")

# 1. Load MobileCLIP-S2 (matches what FileID's MobileCLIPService uses
#    on the image side, so the embeddings live in the same space).
model, _, _ = open_clip.create_model_and_transforms(
    "MobileCLIP-S2",
    pretrained="datacompdr"
)
tokenizer = open_clip.get_tokenizer("MobileCLIP-S2")

# 2. Trace just the text encoder portion.
class TextEncoderModule(torch.nn.Module):
    def __init__(self, clip_model):
        super().__init__()
        self.clip = clip_model
    def forward(self, input_ids):
        # encode_text returns L2-normalized embeddings on its own.
        return self.clip.encode_text(input_ids, normalize=True)

text_module = TextEncoderModule(model).eval()
example_ids = tokenizer(["a photo of a dog"])  # shape [1, 77]
example_ids = example_ids.to(torch.int32)

print("Tracing model …")
traced = torch.jit.trace(text_module, example_ids)

# 3. Convert to CoreML.
print("Converting to CoreML (.mlpackage) …")
ml = ct.convert(
    traced,
    inputs=[ct.TensorType(name="input_ids", shape=example_ids.shape, dtype=ct.int32)],
    outputs=[ct.TensorType(name="text_embeds", dtype=ct.float32)],
    convert_to="mlprogram",
    compute_units=ct.ComputeUnit.ALL,
    minimum_deployment_target=ct.target.macOS14
)
ml_path = DEST_DIR / "clip_text.mlpackage"
if ml_path.exists():
    shutil.rmtree(ml_path)
ml.save(str(ml_path))
print(f"  → {ml_path}")

# 4. Export the BPE vocabulary + merges in OpenAI's standard format.
#    open_clip's tokenizer wraps a SimpleTokenizer that exposes encoder
#    + bpe_ranks. We dump them as vocab.json + merges.txt.
simple = tokenizer.tokenizer  # type: ignore[attr-defined]
encoder = simple.encoder
bpe_ranks = simple.bpe_ranks

vocab_path = DEST_DIR / "vocab.json"
with vocab_path.open("w") as f:
    json.dump({k: int(v) for k, v in encoder.items()}, f)
print(f"  → {vocab_path}")

merges_path = DEST_DIR / "merges.txt"
sorted_pairs = sorted(bpe_ranks.items(), key=lambda kv: kv[1])
with merges_path.open("w") as f:
    f.write("# OpenAI CLIP BPE merges, ranked\n")
    for (a, b), _ in sorted_pairs:
        f.write(f"{a} {b}\n")
print(f"  → {merges_path}")

print("\n✅ Done. Restart FileID to pick up the new model.")
print("   Search bar will now run CLIP semantic search when the query is ≥ 3 chars.")
