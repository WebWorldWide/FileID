#!/bin/bash

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
METALLIB_CACHE="${FILEID_METALLIB_CACHE:-$PROJECT_DIR/.build/cache/mlx.metallib}"
METALLIB_REVISION_FILE="${METALLIB_CACHE}.mlx-revision"

if ! command -v cmake >/dev/null 2>&1; then
    echo "❌ cmake not found — required to build Deep Analyze GPU kernels." >&2
    echo "   Install it with: brew install cmake" >&2
    exit 1
fi

XCODE_DEV_DIR="${DEVELOPER_DIR:-$(xcode-select -p 2>/dev/null || true)}"
if [ ! -d "$XCODE_DEV_DIR" ] || [ ! -x "$XCODE_DEV_DIR/usr/bin/xcodebuild" ]; then
    echo "❌ A full Xcode installation is required to build mlx.metallib." >&2
    echo "   Select it with: sudo xcode-select -s /Applications/Xcode.app/Contents/Developer" >&2
    exit 1
fi

if ! TOOLCHAINS=Metal DEVELOPER_DIR="$XCODE_DEV_DIR" xcrun metal --version >/dev/null 2>&1; then
    echo "❌ Metal Toolchain not installed — required to build Deep Analyze GPU kernels." >&2
    echo "   Install it with: xcodebuild -downloadComponent MetalToolchain" >&2
    exit 1
fi

MLX_SOURCE="${FILEID_MLX_SOURCE:-$PROJECT_DIR/.build/checkouts/mlx-swift/Source/Cmlx/mlx}"
if [ ! -d "$MLX_SOURCE" ]; then
    echo "❌ MLX sources missing at $MLX_SOURCE." >&2
    echo "   Run swift package resolve, then retry." >&2
    exit 1
fi
MLX_REVISION="$(git -C "$MLX_SOURCE" rev-parse HEAD)"
if [ -s "$METALLIB_CACHE" ] && [ -f "$METALLIB_REVISION_FILE" ] &&
   [ "$(cat "$METALLIB_REVISION_FILE")" = "$MLX_REVISION" ]; then
    echo "✅ mlx.metallib ready for $MLX_REVISION ($(du -sh "$METALLIB_CACHE" | cut -f1))"
    exit 0
fi

XCODE_MAJOR="$(DEVELOPER_DIR="$XCODE_DEV_DIR" xcodebuild -version | awk '/^Xcode / { split($2, parts, "."); print parts[1] }')"
LOG="$PROJECT_DIR/.build/cache/metallib-build.log"
BUILD_DIR="$(mktemp -d "${TMPDIR:-/tmp}/fileid-mlx-metallib.XXXXXX")"
CACHE_TMP=""
cleanup() {
    rm -rf "$BUILD_DIR"
    if [ -n "$CACHE_TMP" ]; then
        rm -f "$CACHE_TMP"
    fi
}
trap cleanup EXIT INT TERM

BUILD_SOURCE="$BUILD_DIR/mlx-source"
cp -R "$MLX_SOURCE" "$BUILD_SOURCE"
rm -rf "$BUILD_SOURCE/.git"
if [[ "$XCODE_MAJOR" =~ ^[0-9]+$ ]] && [ "$XCODE_MAJOR" -ge 27 ]; then
    python3 - "$BUILD_SOURCE/mlx/backend/metal/kernels" <<'PATCH_METAL'
from pathlib import Path
import stat
import sys

root = Path(sys.argv[1])

def patch(path, variants):
    source = path.read_text()
    for old, new in variants:
        if new in source:
            return
        if source.count(old) == 1:
            path.chmod(path.stat().st_mode | stat.S_IWUSR)
            path.write_text(source.replace(old, new))
            return
    raise SystemExit(f"Cannot patch pinned MLX source at {path}")

cmake = root / "CMakeLists.txt"
if "  set(METAL_FLAGS\n" not in cmake.read_text():
    patch(cmake, [
        ("set(METAL_FLAGS -Wall -Wextra -fno-fast-math -Wno-c++17-extensions)",
         "set(METAL_FLAGS -std=metal3.2 -Wall -Wextra -fno-fast-math -Wno-c++17-extensions)"),
    ])
patch(root / "fence.metal", [
    ("#ifndef __METAL_MEMORY_SCOPE_SYSTEM__", "#if __METAL_VERSION__ < 400"),
])
PATCH_METAL
fi

mkdir -p "$(dirname "$LOG")"
echo "⚙️  Building mlx.metallib (one-time, 1–3 min)…"
echo "   Streaming output to $LOG"

if ! TOOLCHAINS=Metal DEVELOPER_DIR="$XCODE_DEV_DIR" cmake \
    "$BUILD_SOURCE" \
    -B "$BUILD_DIR/build" \
    -DMLX_BUILD_METAL=ON \
    -DMLX_BUILD_TESTS=OFF \
    -DMLX_BUILD_EXAMPLES=OFF \
    -DMLX_BUILD_BENCHMARKS=OFF \
    -DMLX_BUILD_PYTHON_BINDINGS=OFF \
    -DCMAKE_BUILD_TYPE=Release 2>&1 | tee "$LOG"; then
    echo "❌ cmake configure failed — full log at $LOG" >&2
    exit 1
fi

if ! TOOLCHAINS=Metal DEVELOPER_DIR="$XCODE_DEV_DIR" cmake \
    --build "$BUILD_DIR/build" --target mlx-metallib 2>&1 | tee -a "$LOG"; then
    echo "❌ cmake build failed — full log at $LOG" >&2
    exit 1
fi

BUILT="$BUILD_DIR/build/mlx/backend/metal/kernels/mlx.metallib"
if [ ! -s "$BUILT" ]; then
    echo "❌ mlx.metallib build completed without producing a usable library." >&2
    exit 1
fi

mkdir -p "$(dirname "$METALLIB_CACHE")"
CACHE_TMP="$(mktemp "$(dirname "$METALLIB_CACHE")/mlx.metallib.XXXXXX")"
cp "$BUILT" "$CACHE_TMP"
mv "$CACHE_TMP" "$METALLIB_CACHE"
CACHE_TMP=""
printf '%s\n' "$MLX_REVISION" > "$METALLIB_REVISION_FILE"
echo "✅ Built mlx.metallib ($(du -sh "$METALLIB_CACHE" | cut -f1))"
