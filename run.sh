#!/bin/bash
# FileID v2 builder — DEFAULT LAUNCHER (replaces the old v1 run script;
# the v1 launcher is preserved as `run-v1.sh` for fallback).
#
# Assembles FileIDv2.app with the engine binary embedded inside
# Contents/MacOS/. Fresh-state on every run: wipes the v2 SQLite DB +
# transient caches; preserves model weights (multi-GB).
#
# Layout produced:
#   FileIDv2.app/
#     Contents/
#       MacOS/
#         FileIDv2     ← SwiftUI app, located via Bundle.main
#         FileIDEngine ← spawned as child by EngineClient.locateEngineBinary()
#       Resources/
#         FileID.icns
#       Info.plist     ← CFBundleIconFile = "FileID"

set -e

PROJECT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$PROJECT_DIR"        # so `swift build` finds Package.swift no matter where this is invoked from
APP_NAME="FileID"
BUILD_DIR="$PROJECT_DIR/.build/release"
APP_BUNDLE="$PROJECT_DIR/$APP_NAME.app"
CONTENTS="$APP_BUNDLE/Contents"

XCODE_DEV_DIR="/Applications/Xcode.app/Contents/Developer"
if [ ! -d "$XCODE_DEV_DIR" ]; then
    echo "❌ Xcode not found at $XCODE_DEV_DIR"
    exit 1
fi

echo "🔨 Building FileID + FileIDEngine (release)..."
DEVELOPER_DIR="$XCODE_DEV_DIR" swift build -c release --product FileID
DEVELOPER_DIR="$XCODE_DEV_DIR" swift build -c release --product FileIDEngine

# MLX requires a precompiled mlx.metallib for GPU kernels. SwiftPM doesn't
# build it (it's a cmake-driven step inside the mlx-c subproject), so we
# build it on demand here and stash it under a tools dir for fast reuse on
# subsequent runs. ~96 MB, takes ~30 s on first build.
#
# Requires:
#   - cmake (`brew install cmake`)
#   - Xcode's Metal Toolchain (`xcodebuild -downloadComponent MetalToolchain`)
#     plus `TOOLCHAINS=Metal` env var to expose `metal` to xcrun.
METALLIB_CACHE="$PROJECT_DIR/.build/cache/mlx.metallib"
if [ ! -f "$METALLIB_CACHE" ]; then
    echo "⚙️  Building mlx.metallib (one-time, ~30 s)..."
    if ! command -v cmake >/dev/null 2>&1; then
        echo "❌ cmake not found. Install via 'brew install cmake' to enable Deep Analyze."
        echo "   (Skipping metallib build — Deep Analyze will fail until cmake is installed.)"
    elif ! TOOLCHAINS=Metal DEVELOPER_DIR="$XCODE_DEV_DIR" xcrun --find metal >/dev/null 2>&1; then
        echo "❌ Metal Toolchain not found. Install via:"
        echo "   xcodebuild -downloadComponent MetalToolchain"
        echo "   (Skipping metallib build — Deep Analyze will fail until installed.)"
    else
        BUILDDIR=$(mktemp -d)
        TOOLCHAINS=Metal DEVELOPER_DIR="$XCODE_DEV_DIR" cmake \
            "$PROJECT_DIR/.build/checkouts/mlx-swift/Source/Cmlx/mlx" \
            -B "$BUILDDIR" \
            -DMLX_BUILD_METAL=ON -DMLX_BUILD_TESTS=OFF -DMLX_BUILD_EXAMPLES=OFF \
            -DMLX_BUILD_BENCHMARKS=OFF -DMLX_BUILD_PYTHON_BINDINGS=OFF \
            -DCMAKE_BUILD_TYPE=Release > /dev/null 2>&1
        TOOLCHAINS=Metal DEVELOPER_DIR="$XCODE_DEV_DIR" cmake --build "$BUILDDIR" --target mlx-metallib > /dev/null 2>&1
        BUILT="$BUILDDIR/mlx/backend/metal/kernels/mlx.metallib"
        if [ -f "$BUILT" ]; then
            mkdir -p "$(dirname "$METALLIB_CACHE")"
            cp "$BUILT" "$METALLIB_CACHE"
            echo "✅ Built mlx.metallib ($(du -sh "$METALLIB_CACHE" | cut -f1))"
        else
            echo "❌ metallib build failed; Deep Analyze will not work this run."
        fi
        rm -rf "$BUILDDIR"
    fi
fi

echo "🧹 Wiping SQLite + caches (preserving model weights)..."
APP_SUPPORT="$HOME/Library/Application Support"
rm -f  "$APP_SUPPORT/FileID/fileid.sqlite" \
       "$APP_SUPPORT/FileID/fileid.sqlite-wal" \
       "$APP_SUPPORT/FileID/fileid.sqlite-shm"
rm -rf "$APP_SUPPORT/FileID/checkpoints"
rm -rf "$APP_SUPPORT/FileID/logs"
rm -rf "$APP_SUPPORT/FileID/thumbs.cache"
rm -rf "$APP_SUPPORT/FileID/face_crops"

echo "📦 Assembling $APP_NAME.app bundle..."
rm -rf "$APP_BUNDLE"
mkdir -p "$CONTENTS/MacOS"
mkdir -p "$CONTENTS/Resources"

cp "$BUILD_DIR/FileID"       "$CONTENTS/MacOS/FileID"
cp "$BUILD_DIR/FileIDEngine" "$CONTENTS/MacOS/FileIDEngine"
chmod +x "$CONTENTS/MacOS/FileID" "$CONTENTS/MacOS/FileIDEngine"

# MLX needs the metallib colocated with the engine binary. Both names
# because MLX's load order tries `default.metallib` first then `mlx.metallib`.
if [ -f "$METALLIB_CACHE" ]; then
    cp "$METALLIB_CACHE" "$CONTENTS/MacOS/mlx.metallib"
    cp "$METALLIB_CACHE" "$CONTENTS/MacOS/default.metallib"
fi

cat > "$CONTENTS/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>com.fileid.app</string>
    <key>CFBundleName</key>
    <string>FileID</string>
    <key>CFBundleDisplayName</key>
    <string>FileID</string>
    <key>CFBundleExecutable</key>
    <string>FileID</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleIconFile</key>
    <string>FileID</string>
    <key>CFBundleIconName</key>
    <string>FileID</string>
    <key>CFBundleShortVersionString</key>
    <string>1.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>LSMinimumSystemVersion</key>
    <string>15.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSDesktopFolderUsageDescription</key>
    <string>FileID needs to read your folders to tag, dedupe, and reorganize files.</string>
    <key>NSDocumentsFolderUsageDescription</key>
    <string>FileID needs to read your folders to tag, dedupe, and reorganize files.</string>
    <key>NSDownloadsFolderUsageDescription</key>
    <string>FileID needs to read your folders to tag, dedupe, and reorganize files.</string>
</dict>
</plist>
PLIST

# Icon — uses the same compiled .icns as v1 so they share the brand asset.
cp "$PROJECT_DIR/Resources/FileID.icns" "$CONTENTS/Resources/FileID.icns"

# LaunchServices caches icons aggressively. Touching the bundle invalidates
# the cache so the new icon shows up immediately.
touch "$APP_BUNDLE"

echo "✅ Built: $APP_BUNDLE"
echo "🚀 Launching (fresh state)..."
open "$APP_BUNDLE"
