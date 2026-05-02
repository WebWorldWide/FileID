#!/usr/bin/env bash
# Fetches the MobileCLIP-S2 image + text encoders from Apple's official
# HuggingFace mirror, plus the OpenAI CLIP BPE vocabulary, into the
# paths CLIPTextEncoder + MobileCLIPService expect. No python required —
# everything is a flat HTTP fetch.
#
# Run once. Idempotent: re-running re-downloads everything (small enough
# that incremental cache management isn't worth the code).
set -euo pipefail

MODELS="$HOME/Library/Application Support/FileID/Models"
HF="https://huggingface.co"

mkdir -p "$MODELS"

fetch() {
  local url="$1" dest="$2"
  mkdir -p "$(dirname "$dest")"
  echo "→ $(basename "$dest")"
  curl --fail --location --progress-bar "$url" -o "$dest"
}

# 1. MobileCLIP-S2 image encoder ─────────────────────────────────
IMG_PKG="$MODELS/mobileclip_image/mobileclip_s2_image.mlpackage"
fetch "$HF/apple/coreml-mobileclip/resolve/main/mobileclip_s2_image.mlpackage/Manifest.json" \
      "$IMG_PKG/Manifest.json"
fetch "$HF/apple/coreml-mobileclip/resolve/main/mobileclip_s2_image.mlpackage/Data/com.apple.CoreML/model.mlmodel" \
      "$IMG_PKG/Data/com.apple.CoreML/model.mlmodel"
fetch "$HF/apple/coreml-mobileclip/resolve/main/mobileclip_s2_image.mlpackage/Data/com.apple.CoreML/weights/weight.bin" \
      "$IMG_PKG/Data/com.apple.CoreML/weights/weight.bin"

# 2. MobileCLIP-S2 text encoder ──────────────────────────────────
# CLIPTextEncoder.swift looks for clip_text/clip_text.mlpackage —
# Apple's file is named mobileclip_s2_text.mlpackage. Land it under
# the expected directory + filename so the loader picks it up
# without any code changes.
TXT_PKG="$MODELS/clip_text/clip_text.mlpackage"
fetch "$HF/apple/coreml-mobileclip/resolve/main/mobileclip_s2_text.mlpackage/Manifest.json" \
      "$TXT_PKG/Manifest.json"
fetch "$HF/apple/coreml-mobileclip/resolve/main/mobileclip_s2_text.mlpackage/Data/com.apple.CoreML/model.mlmodel" \
      "$TXT_PKG/Data/com.apple.CoreML/model.mlmodel"
fetch "$HF/apple/coreml-mobileclip/resolve/main/mobileclip_s2_text.mlpackage/Data/com.apple.CoreML/weights/weight.bin" \
      "$TXT_PKG/Data/com.apple.CoreML/weights/weight.bin"

# 3. OpenAI CLIP BPE vocabulary ──────────────────────────────────
# MobileCLIP uses the same 49,408-token OpenAI BPE encoder. We pull
# from openai/clip-vit-base-patch32 — same vocab as every CLIP model.
fetch "$HF/openai/clip-vit-base-patch32/resolve/main/vocab.json" \
      "$MODELS/clip_text/vocab.json"
fetch "$HF/openai/clip-vit-base-patch32/resolve/main/merges.txt" \
      "$MODELS/clip_text/merges.txt"

echo
echo "Installed:"
echo "  $MODELS/mobileclip_image/mobileclip_s2_image.mlpackage"
echo "  $MODELS/clip_text/clip_text.mlpackage"
echo "  $MODELS/clip_text/vocab.json"
echo "  $MODELS/clip_text/merges.txt"
echo
echo "Restart FileID — semantic search activates automatically."
