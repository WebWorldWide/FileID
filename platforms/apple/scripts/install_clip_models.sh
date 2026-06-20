#!/usr/bin/env bash
# Install CLIP ViT-B/32 models for FileID macOS (ONNX via CoreML EP).
# Mirrors the in-app CLIPModelInstaller fetch plan so the shell path and
# the UI path produce identical on-disk layouts.
#
# Run once. Idempotent: re-running re-downloads everything.
set -euo pipefail

MODELS="$HOME/Library/Application Support/FileID/Models"
XENOVA="https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main"
OPENAI="https://huggingface.co/openai/clip-vit-base-patch32/resolve/main"

mkdir -p "$MODELS"

fetch() {
  local url="$1" dest="$2"
  mkdir -p "$(dirname "$dest")"
  echo "→ $(basename "$dest")"
  curl --fail --location --progress-bar "$url" -o "$dest"
}

# 1. CLIP ViT-B/32 image encoder (ONNX — CoreML EP) ──────────────
fetch "$XENOVA/onnx/vision_model.onnx" \
      "$MODELS/mobileclip_image/clip_vitb32_image.onnx"

# 2. CLIP ViT-B/32 text encoder (ONNX) ───────────────────────────
fetch "$XENOVA/onnx/text_model.onnx" \
      "$MODELS/clip_text/clip_text.onnx"

# 3. OpenAI CLIP BPE vocabulary + merges ─────────────────────────
fetch "$OPENAI/vocab.json"   "$MODELS/clip_text/vocab.json"
fetch "$OPENAI/merges.txt"   "$MODELS/clip_text/merges.txt"

echo
echo "Installed:"
echo "  $MODELS/mobileclip_image/clip_vitb32_image.onnx  (CLIP image encoder)"
echo "  $MODELS/clip_text/clip_text.onnx                 (CLIP text encoder)"
echo "  $MODELS/clip_text/vocab.json"
echo "  $MODELS/clip_text/merges.txt"
echo
echo "Restart FileID — semantic search and restructure activate automatically."
