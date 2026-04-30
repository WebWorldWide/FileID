#!/bin/bash
# Build the ArcFace .mlpackage release assets that ArcFaceModelInstaller
# downloads on first launch. Runs convert_arcface.py for both variants
# and zips the output into /tmp/arcface_*.mlpackage.zip ready for
# `gh release upload v<X.Y.Z> /tmp/arcface_*.zip --clobber`.
#
# Requires Python 3.11 (coremltools doesn't ship 3.13/3.14 wheels yet).
# Re-run only when InsightFace publishes new weights or the conversion
# pipeline changes — outputs are deterministic for a given input.
#
# Usage:
#   bash scripts/build_arcface_assets.sh

set -e

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_DIR"

PYTHON="${PYTHON:-/opt/homebrew/opt/python@3.11/bin/python3.11}"
if [ ! -x "$PYTHON" ]; then
    echo "❌ Need Python 3.11. Install with: brew install python@3.11"
    exit 1
fi

VENV="$PROJECT_DIR/.venv"
if [ ! -d "$VENV" ]; then
    echo "🐍 Creating .venv with $PYTHON…"
    "$PYTHON" -m venv "$VENV"
fi

# shellcheck disable=SC1091
source "$VENV/bin/activate"
pip install --quiet --upgrade pip
pip install --quiet onnx coremltools onnxruntime huggingface_hub numpy pillow onnx2torch torch

MODELS_DIR="$HOME/Library/Application Support/FileID/Models"
mkdir -p "$MODELS_DIR"

for variant in iresnet50 mobileface; do
    echo "🔁 Converting $variant…"
    python3 scripts/convert_arcface.py --variant "$variant"
done

echo "📦 Zipping…"
cd "$MODELS_DIR"
for v in iresnet50 mobileface; do
    OUT="/tmp/arcface_${v}.mlpackage.zip"
    rm -f "$OUT"
    zip -qr "$OUT" "arcface_${v}.mlpackage"
    SIZE=$(du -sh "$OUT" | cut -f1)
    echo "  $OUT ($SIZE)"
done

cat <<'EOF'

✅ Done. Upload to the v0.1.0 release with:

   gh release upload v0.1.0 \
     /tmp/arcface_iresnet50.mlpackage.zip \
     /tmp/arcface_mobileface.mlpackage.zip \
     --clobber

EOF
