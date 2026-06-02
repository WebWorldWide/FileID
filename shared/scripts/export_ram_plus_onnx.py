#!/usr/bin/env python3
"""Export RAM++ (Recognize Anything Plus) to a single-pass ONNX tagger for FileID.

WHY: FileID's automatic image tagging must run on every Windows GPU/iGPU/NPU
(via ONNX Runtime's DirectML / OpenVINO / QNN execution providers) and must be
license-clean. RAM++ (ram_plus_swin_large_14m) is Apache-2.0, a purpose-built
open-set image tagger (Swin-Large @ 384px, 4585-tag English vocabulary with
*frozen* tag embeddings), and runs as a single forward pass — exactly the shape
NPU EPs accelerate well. There is no official ONNX, so we export it ourselves.

This is the WS2 "export spike" from the plan. It is meant to be RUN AND ITERATED
ON by a developer with the RAM++ weights + a Python/torch environment — not a
guaranteed-first-run black box. It:

  1. loads ram_plus (swin_l, image_size=384),
  2. wraps the tagging forward so it takes a normalized image tensor and returns
     the per-tag logits (frozen tag embeddings baked into the graph as constants),
  3. exports to ONNX at a FIXED 1x3x384x384 input (fixed shapes sidestep the
     Swin window-attention dynamic-`nW` export issue and maximize NPU operator
     coverage; if export still trips the Concat/ShapeInferenceError, apply the
     documented Swin fix — see SWIN_EXPORT_NOTE below),
  4. writes the index-aligned English tag list next to the .onnx, and
  5. validates the ONNX output against the torch model on a sample image.

ONNX CONTRACT (this is what models/ram_plus.rs depends on — keep them in sync):
  input  "image"  : float32 [1, 3, 384, 384]  (RGB, ImageNet-normalized, see below)
  output "logits" : float32 [1, 4585]          (pre-sigmoid; caller does sigmoid + threshold)
  sidecar "ram_plus_tags.txt" : 4585 lines, one tag per line, index-aligned with logits

PREPROCESS (must match ram_plus.rs exactly):
  resize the image to 384x384 (bilinear), scale to [0,1], then normalize with
  ImageNet mean/std — the same constants FileID already uses for MobileCLIP:
    mean = [0.485, 0.456, 0.406], std = [0.229, 0.224, 0.225]

USAGE:
  pip install torch torchvision onnx onnxruntime onnxconverter-common pillow numpy
  pip install git+https://github.com/xinyu1205/recognize-anything.git
  # download ram_plus_swin_large_14m.pth from
  #   https://huggingface.co/xinyu1205/recognize-anything-plus-model
  python export_ram_plus_onnx.py \
      --checkpoint ram_plus_swin_large_14m.pth \
      --out-dir ./ram_plus_onnx \
      --sample-image some_photo.jpg

OUTPUT (place these where ram_plus.rs looks, i.e. %LOCALAPPDATA%\FileID\Models\ram_plus\):
  ram_plus.onnx
  ram_plus_tags.txt
"""

import argparse
import os
import sys

import numpy as np

IMAGENET_MEAN = [0.485, 0.456, 0.406]
IMAGENET_STD = [0.229, 0.224, 0.225]
IMAGE_SIZE = 384
NUM_TAGS_EXPECTED = 4585  # ram_plus_tag_embedding_class_4585_des_51

# SWIN_EXPORT_NOTE: Swin's window attention computes the number of windows from
# a mask tensor's shape, which torch.onnx cannot shape-infer (Concat rank
# mismatch — microsoft/Swin-Transformer#89). At a FIXED 384x384 input the window
# grid is static, so the common workaround is unnecessary; if you still hit the
# error, patch the swin block to derive nW from (H//window, W//window) instead of
# mask.shape[0], or export with torch>=2.2 + opset 17 (dynamo=False), which traces
# the static grid cleanly.


def build_logits_wrapper(model):
    """Wrap RAM++ so forward(image) -> per-tag logits [B, num_class].

    This mirrors RAM++'s `generate_tag` up to (but not including) the sigmoid +
    thresholding. The attribute names (image_proj / visual_encoder /
    wordvec_proj / label_embed / tagging_head / fc) follow the recognize-anything
    `ram_plus` model. VERIFY against your installed package version — if an
    attribute moved, adjust here; the rest of the script is version-agnostic.
    """
    import torch
    import torch.nn as nn
    import torch.nn.functional as F

    class RamPlusLogits(nn.Module):
        def __init__(self, m):
            super().__init__()
            self.m = m
            # Bake RAM++'s delete_tag_index suppression in as a constant additive
            # mask (generate_tag zeroes those classes). Empty for stock ram_plus,
            # but honored so suppressed vocab can't surface as chips. -inf →
            # sigmoid 0 downstream in ram_plus.rs.
            mask = torch.zeros(int(m.num_class))
            delete = list(getattr(m, "delete_tag_index", []) or [])
            if delete:
                mask[delete] = float("-inf")
            self.register_buffer("tag_mask", mask)

        def forward(self, image):
            # Faithfully mirrors recognize-anything ram_plus.generate_tag up to
            # (but not including) the sigmoid: the per-class description
            # embeddings are reweighted by CLS-token similarity + softmax, then
            # run through the tagging cross-attention head. (The earlier version
            # skipped the reweight and emitted [B, num_class*51] — wrong shape,
            # tagger never loaded.) Frozen tag embeddings are graph constants, so
            # the exported model takes ONLY the image and emits [B, num_class].
            m = self.m
            image_embeds = m.image_proj(m.visual_encoder(image))
            image_atts = torch.ones(
                image_embeds.size()[:-1], dtype=torch.long, device=image.device
            )
            bs = image_embeds.shape[0]
            des_per_class = int(m.label_embed.shape[0] / m.num_class)  # 51

            image_cls = image_embeds[:, 0, :]
            image_cls = image_cls / image_cls.norm(dim=-1, keepdim=True)
            reweight = m.reweight_scale.exp()
            # CLS·description similarity → [bs, num_class, des] → softmax over the
            # 51 descriptions of each class.
            sim = (reweight * image_cls @ m.label_embed.t()).view(bs, -1, des_per_class)
            weight = F.softmax(sim, dim=2)  # [bs, num_class, des]
            reshaped = m.label_embed.view(int(m.num_class), des_per_class, -1)  # [num_class, des, 512]
            # Weighted sum over descriptions → one embedding per class.
            label_embed = (weight.unsqueeze(-1) * reshaped.unsqueeze(0)).sum(dim=2)  # [bs, num_class, 512]
            label_embed = F.relu(m.wordvec_proj(label_embed))

            tagging_embed = m.tagging_head(
                encoder_embeds=label_embed,
                encoder_hidden_states=image_embeds,
                encoder_attention_mask=image_atts,
                return_dict=False,
                mode="tagging",
            )
            logits = m.fc(tagging_embed[0]).squeeze(-1)  # [bs, num_class]
            return logits + self.tag_mask

    return RamPlusLogits(model).eval()


def load_model(checkpoint):
    import torch

    # transformers >= 4.0 moved these utility fns from modeling_utils to
    # pytorch_utils; recognize-anything's vendored bert.py still imports them from
    # the old location, so it ImportErrors on modern transformers (incl. the
    # transformers 5.x that Python 3.14 forces — there are no Python-3.14 wheels
    # for the old transformers RAM++ was built against). Re-export them into the
    # legacy namespace so the import resolves; the functions are unchanged.
    try:
        import transformers.modeling_utils as _mu
        import transformers.pytorch_utils as _pu

        for _n in (
            "apply_chunking_to_forward",
            "find_pruneable_heads_and_indices",
            "prune_linear_layer",
            "Conv1D",
        ):
            if not hasattr(_mu, _n) and hasattr(_pu, _n):
                setattr(_mu, _n, getattr(_pu, _n))
    except Exception:  # noqa: BLE001 — best-effort compat shim
        pass

    try:
        from ram.models import ram_plus
    except ImportError as e:
        # The `ram` package itself rarely fails to import — the usual cause is a
        # MISSING DEPENDENCY (recognize-anything doesn't declare timm /
        # transformers / fairscale, so pip doesn't pull them). Surface the real
        # error instead of a misleading "not installed".
        sys.exit(
            f"Could not import ram.models.ram_plus: {e!r}\n\n"
            "If 'ram' itself is missing:\n"
            "  pip install git+https://github.com/xinyu1205/recognize-anything.git\n"
            "If a DEPENDENCY is missing (most common):\n"
            "  pip install timm fairscale transformers\n"
            "On a `timm.models.layers` or transformers-API import error, pin known-good:\n"
            '  pip install "timm<1.0" "transformers==4.25.1"'
        )
    if not os.path.isfile(checkpoint):
        sys.exit(f"checkpoint not found: {checkpoint}")
    model = ram_plus(pretrained=checkpoint, image_size=IMAGE_SIZE, vit="swin_l")
    model.eval()
    tag_list = list(getattr(model, "tag_list", []))
    if len(tag_list) != NUM_TAGS_EXPECTED:
        print(
            f"WARNING: tag_list has {len(tag_list)} entries, expected "
            f"{NUM_TAGS_EXPECTED}. Proceeding, but confirm ram_plus.rs NUM_TAGS.",
            file=sys.stderr,
        )
    # tags can be bytes/np types — coerce to str.
    tag_list = [str(t).strip() for t in tag_list]
    return model, tag_list


def preprocess_pil(path):
    """Match ram_plus.rs preprocessing exactly (resize 384, ImageNet norm)."""
    from PIL import Image

    img = Image.open(path).convert("RGB").resize((IMAGE_SIZE, IMAGE_SIZE), Image.BILINEAR)
    arr = np.asarray(img, dtype=np.float32) / 255.0  # HWC, [0,1]
    arr = (arr - np.array(IMAGENET_MEAN, np.float32)) / np.array(IMAGENET_STD, np.float32)
    arr = np.transpose(arr, (2, 0, 1))[None, ...]  # NCHW
    return np.ascontiguousarray(arr, dtype=np.float32)


def main():
    # Windows consoles default to cp1252; force UTF-8 so the → / ✓ in our
    # progress prints don't crash with UnicodeEncodeError mid-export.
    try:
        sys.stdout.reconfigure(encoding="utf-8")
        sys.stderr.reconfigure(encoding="utf-8")
    except Exception:  # noqa: BLE001 — best-effort; pre-3.7 / redirected streams
        pass
    ap = argparse.ArgumentParser()
    ap.add_argument("--checkpoint", required=True, help="ram_plus_swin_large_14m.pth")
    ap.add_argument("--out-dir", default="./ram_plus_onnx")
    ap.add_argument("--sample-image", help="optional image for ONNX-vs-torch validation")
    ap.add_argument("--opset", type=int, default=17)
    ap.add_argument(
        "--image-size",
        type=int,
        default=IMAGE_SIZE,
        help="Square input resolution (default 384 = the RAM++ fine-tune size). "
        "A smaller size (e.g. 256) re-exports a lighter, faster model — compute "
        "drops ~with the pixel ratio — for throughput on weaker GPUs. The shipped "
        "checkpoint is fine-tuned at 384, so VALIDATE tag F1 vs the 384 model on a "
        "real corpus before shipping (recognize-anything interpolates the Swin "
        "relative-position bias for the new size; the engine's RAM++ INPUT_SIZE "
        "must match whatever you export).",
    )
    ap.add_argument(
        "--precision",
        choices=["fp16", "fp32"],
        default="fp16",
        help="fp16 (default) ~halves the ONNX so it fits a 4 GB GPU alongside "
        "faces+CLIP; I/O stays fp32 so ram_plus.rs is unchanged. fp32 for max fidelity.",
    )
    ap.add_argument(
        "--dynamic-batch",
        action=argparse.BooleanOptionalAction,
        default=True,
        help="Export with a dynamic batch axis (image[0]/logits[0]) so the engine "
        "can run N images per forward — the RAM++ throughput fix (HW-4). The Swin "
        "window grid is static at 384px, so only the batch dim is made dynamic. "
        "Use --no-dynamic-batch for the legacy fixed [1,3,384,384] export.",
    )
    args = ap.parse_args()
    # Resolution drives model construction (ram_plus(image_size=...)), the dummy
    # input, and validation — all read the module-global IMAGE_SIZE, so set it
    # from the CLI before anything downstream reads it.
    globals()["IMAGE_SIZE"] = args.image_size

    import torch

    os.makedirs(args.out_dir, exist_ok=True)
    onnx_path = os.path.join(args.out_dir, "ram_plus.onnx")
    tags_path = os.path.join(args.out_dir, "ram_plus_tags.txt")

    model, tag_list = load_model(args.checkpoint)
    wrapper = build_logits_wrapper(model)

    with open(tags_path, "w", encoding="utf-8") as f:
        f.write("\n".join(tag_list) + "\n")
    print(f"wrote {len(tag_list)} tags → {tags_path}")

    # Per-class sigmoid thresholds. RAM++ ships calibrated per-class cutoffs
    # (mean ~0.68) in `model.class_threshold`; dumping them lets ram_plus.rs
    # apply a per-class cutoff instead of a single global one (precision win).
    # Wrapped so a missing/oddly-shaped attribute on the installed package
    # version degrades to a constant-0.68 vector instead of failing the export.
    thr_path = os.path.join(args.out_dir, "ram_plus_thresholds.txt")
    try:
        ct = getattr(model, "class_threshold", None)
        if ct is None:
            raise AttributeError("model has no `class_threshold`")
        thr = [float(x) for x in ct.detach().cpu().flatten().tolist()]
        if len(thr) != len(tag_list):
            print(
                f"WARNING: class_threshold len {len(thr)} != {len(tag_list)} tags; "
                "normalizing (pad/truncate to 0.68).",
                file=sys.stderr,
            )
            thr = (thr + [0.68] * len(tag_list))[: len(tag_list)]
        n_default = sum(1 for t in thr if abs(t - 0.68) < 1e-6)
        print(
            f"per-class thresholds: min={min(thr):.3f} max={max(thr):.3f} "
            f"mean={sum(thr) / len(thr):.3f} ({n_default}/{len(thr)} still at default 0.68)"
        )
    except Exception as e:  # noqa: BLE001 — any failure must fall back, not abort
        print(
            f"WARNING: could not read per-class thresholds ({e!r}); writing a "
            "constant-0.68 vector (ram_plus.rs then behaves like the global cutoff).",
            file=sys.stderr,
        )
        thr = [0.68] * len(tag_list)
    with open(thr_path, "w", encoding="utf-8") as f:
        f.write("\n".join(f"{t:.6f}" for t in thr) + "\n")
    print(f"wrote {len(thr)} per-class thresholds → {thr_path}")

    dummy = torch.zeros(1, 3, IMAGE_SIZE, IMAGE_SIZE, dtype=torch.float32)
    # A dynamic batch axis lets a caller run N images per forward. NOTE (corrects
    # the original HW-4 hypothesis): batched RAM++ was MEASURED on an RTX 2060
    # (2026-06) and DISPROVEN — the GPU is compute+VRAM saturated at batch=1 (util
    # p50 87%, VRAM ~90% full), so batch=4 ran ~23% SLOWER, not faster. Keep the
    # dynamic axis (harmless; opt-in via the engine's batch coordinator for
    # high-SM/high-VRAM cards that do NOT saturate at batch=1), but the real
    # throughput levers are a lighter/lower-res model or a faster-kernel EP, not
    # batching. ONLY dim 0 is made dynamic; the Swin window grid stays static at
    # the chosen image size so the window-attention reshapes (the dynamic-`nW`
    # Concat issue in SWIN_EXPORT_NOTE) are untouched.
    dyn_axes = (
        {"image": {0: "batch"}, "logits": {0: "batch"}} if args.dynamic_batch else None
    )
    # torch >= 2.5 DEFAULTS torch.onnx.export to the dynamo exporter, which for
    # this model emits a Loop/SequenceInsert graph that (a) the SWIN_EXPORT_NOTE
    # warns against and (b) breaks fp16 conversion (Sequence vs Tensor elem-type
    # mismatch → the ONNX won't even load). Force the legacy TorchScript tracer
    # (dynamo=False): it unrolls the static 384² Swin window grid into a clean
    # constant graph that fp16-converts and loads. Passed only when the running
    # torch supports the kwarg (>=2.5) so the script still runs on torch 2.2-2.4.
    export_kwargs = dict(
        input_names=["image"],
        output_names=["logits"],
        opset_version=args.opset,
        # Constant-folding evaluates Swin-L's conv/matmul subgraphs on CPU at
        # export time — pathologically slow for this model (tens of minutes /
        # hours). Skip it: onnxruntime constant-folds at session load instead,
        # for the same runtime graph.
        do_constant_folding=False,
        dynamic_axes=dyn_axes,
    )
    import inspect as _inspect

    if "dynamo" in _inspect.signature(torch.onnx.export).parameters:
        export_kwargs["dynamo"] = False
    with torch.no_grad():
        torch.onnx.export(wrapper, dummy, onnx_path, **export_kwargs)
    print(f"exported → {onnx_path}  (dynamic_batch={args.dynamic_batch})")

    # fp16: convert graph weights to half precision (≈450 MB vs ≈800 MB) so the
    # tagger fits a 4 GB GPU next to faces+CLIP. keep_io_types=True leaves the
    # "image" input + "logits" output as fp32 → ram_plus.rs feeds/reads fp32
    # unchanged; only internal weights/compute are fp16. (fp16 diverges a little
    # more from torch — judge by top-k overlap below, not max|Δlogit|.)
    if args.precision == "fp16":
        import onnx
        from onnxconverter_common import float16

        if not args.sample_image:
            print(
                "WARNING: exporting fp16 WITHOUT --sample-image — no numerical check "
                "runs. Pass --sample-image to validate top-k agreement before shipping.",
                file=sys.stderr,
            )
        # Keep the numerically sensitive ops fp32: the reweight_scale.exp()
        # CLS-similarity path + softmax + layernorm + reductions overflow/lose
        # precision in fp16. The bulk (Conv/MatMul) still goes fp16 → ~half size.
        m16 = float16.convert_float_to_float16(
            onnx.load(onnx_path),
            keep_io_types=True,
            # "Cast" is blocked too: the tagging head has Cast ops at fp16/fp32
            # boundaries that the converter otherwise leaves emitting fp16 where a
            # blocked downstream op declares fp32 (load-time "Type (float16) does
            # not match expected (float)"). Keeping Casts as-is lets the converter
            # insert its own boundary casts consistently.
            op_block_list=["Softmax", "LayerNormalization", "ReduceMean", "ReduceSum", "Exp", "Div", "Cast"],
        )
        onnx.save(m16, onnx_path)
        print(f"converted to fp16 (fp32 I/O + sensitive ops) → {onnx_path}")

    # ALWAYS assert the ONNX output dim == tag count — the exact invariant
    # ram_plus.rs enforces at load. A cheap zero-input run catches an H1-class
    # head-shape bug even without --sample-image.
    import onnxruntime as _ort

    _sess = _ort.InferenceSession(onnx_path, providers=["CPUExecutionProvider"])
    _out = _sess.run(["logits"], {"image": np.zeros((1, 3, IMAGE_SIZE, IMAGE_SIZE), np.float32)})[0]
    if _out.shape[-1] != len(tag_list):
        sys.exit(
            f"FATAL: ONNX output dim {_out.shape[-1]} != {len(tag_list)} tags — the "
            f"tagging head is wrong (ram_plus.rs would bail at load). Fix the wrapper."
        )
    print(f"output dim OK: {_out.shape[-1]} == {len(tag_list)} tags")

    # Optional numerical validation: torch vs onnxruntime top-k agreement.
    if args.sample_image:
        import onnxruntime as ort

        x = preprocess_pil(args.sample_image)
        with torch.no_grad():
            torch_logits = wrapper(torch.from_numpy(x)).numpy().reshape(-1)
        sess = ort.InferenceSession(onnx_path, providers=["CPUExecutionProvider"])
        onnx_logits = sess.run(["logits"], {"image": x})[0].reshape(-1)

        max_abs = float(np.max(np.abs(torch_logits - onnx_logits)))
        k = 15
        t_top = set(np.argsort(-torch_logits)[:k].tolist())
        o_top = set(np.argsort(-onnx_logits)[:k].tolist())
        overlap = len(t_top & o_top)
        print(f"validation: max|Δlogit|={max_abs:.4g}  top{k}_overlap={overlap}/{k}")
        thr = 1.0 / (1.0 + np.exp(-onnx_logits))  # sigmoid
        top = np.argsort(-thr)[:k]
        print("top tags:", [f"{tag_list[i]}={thr[i]:.2f}" for i in top])
        if max_abs > 1e-2 or overlap < k - 2:
            print(
                "WARNING: ONNX and torch disagree more than expected — inspect the "
                "wrapper forward + opset before shipping this ONNX.",
                file=sys.stderr,
            )


if __name__ == "__main__":
    main()
