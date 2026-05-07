#!/bin/bash
# FileID launcher — wipes SQLite + transient caches, rebuilds release,
# bundles into FileID.app/, opens. Preserves downloaded model weights.
#
# Layout produced:
#   FileID.app/
#     Contents/
#       MacOS/
#         FileID       ← SwiftUI app, located via Bundle.main
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
    if ! command -v cmake >/dev/null 2>&1; then
        echo "❌ cmake not found — required to build Deep Analyze GPU kernels."
        echo "   Install: brew install cmake"
        echo "   Then re-run ./run.sh."
        exit 1
    fi
    # `xcrun --find metal` only locates the shim binary — on Xcode 26 /
    # macOS Tahoe the actual Metal Toolchain is a separate downloadable
    # component, and the shim errors with "cannot execute tool 'metal'
    # due to missing Metal Toolchain" if it isn't installed. Actually
    # invoke `metal --version` so this fails fast with a clear message
    # instead of bombing out 6s into the cmake configure.
    if ! TOOLCHAINS=Metal DEVELOPER_DIR="$XCODE_DEV_DIR" xcrun metal --version >/dev/null 2>&1; then
        echo "❌ Metal Toolchain not installed — required to build Deep Analyze GPU kernels."
        echo "   The 'metal' shim exists, but the toolchain component is missing."
        echo "   Install: xcodebuild -downloadComponent MetalToolchain"
        echo "   (Several-hundred-MB download; may prompt for auth.)"
        echo "   Then re-run ./run.sh."
        exit 1
    fi
    LOG="$PROJECT_DIR/.build/cache/metallib-build.log"
    mkdir -p "$(dirname "$LOG")"
    echo "⚙️  Building mlx.metallib (one-time, 1–3 min on first run)…"
    echo "    Streaming output to $LOG"
    # tee through a pipeline; pipefail surfaces cmake's exit code instead of tee's.
    set -o pipefail
    BUILDDIR=$(mktemp -d)
    if ! TOOLCHAINS=Metal DEVELOPER_DIR="$XCODE_DEV_DIR" cmake \
        "$PROJECT_DIR/.build/checkouts/mlx-swift/Source/Cmlx/mlx" \
        -B "$BUILDDIR" \
        -DMLX_BUILD_METAL=ON -DMLX_BUILD_TESTS=OFF -DMLX_BUILD_EXAMPLES=OFF \
        -DMLX_BUILD_BENCHMARKS=OFF -DMLX_BUILD_PYTHON_BINDINGS=OFF \
        -DCMAKE_BUILD_TYPE=Release 2>&1 | tee "$LOG"; then
        echo "❌ cmake configure failed — full log at $LOG"
        exit 1
    fi
    if ! TOOLCHAINS=Metal DEVELOPER_DIR="$XCODE_DEV_DIR" cmake \
        --build "$BUILDDIR" --target mlx-metallib 2>&1 | tee -a "$LOG"; then
        echo "❌ cmake build failed — full log at $LOG"
        exit 1
    fi
    BUILT="$BUILDDIR/mlx/backend/metal/kernels/mlx.metallib"
    if [ -f "$BUILT" ]; then
        mkdir -p "$(dirname "$METALLIB_CACHE")"
        cp "$BUILT" "$METALLIB_CACHE"
        echo "✅ Built mlx.metallib ($(du -sh "$METALLIB_CACHE" | cut -f1))"
        rm -rf "$BUILDDIR"
    else
        echo "❌ metallib build failed; cmake + Metal Toolchain are present but the build step did not produce mlx.metallib."
        echo "   Build artifacts at $BUILDDIR (kept for inspection)."
        echo "   Re-run ./run.sh after fixing the build."
        exit 1
    fi
fi

echo "🛑 Quitting any running FileID processes..."
# Stop the running app + engine BEFORE we touch the DB. If we wipe the
# .sqlite while an engine still has it open, that engine's next write
# trips SQLITE_IOERR — the "disk I/O error - BEGIN IMMEDIATE T..." you
# see in the sidebar after a hot restart.
#
# `osascript` first (lets the app quit cleanly + flush logs); pkill
# afterwards as the safety net for unresponsive instances.
osascript -e 'tell application "FileID" to quit' >/dev/null 2>&1 || true
sleep 0.5
pkill -f "FileID.app/Contents/MacOS/FileID"        2>/dev/null || true
pkill -f "FileID.app/Contents/MacOS/FileIDEngine"  2>/dev/null || true
pkill -x "FileID"                                   2>/dev/null || true
pkill -x "FileIDEngine"                             2>/dev/null || true
sleep 0.5
# Final hammer for anything still alive after the polite kill.
pkill -9 -f "FileID.app/Contents/MacOS/"           2>/dev/null || true
pkill -9 -x "FileIDEngine"                          2>/dev/null || true

echo "🧹 Wiping SQLite + caches (preserving model weights)..."
APP_SUPPORT="$HOME/Library/Application Support"
rm -f  "$APP_SUPPORT/FileID/fileid.sqlite" \
       "$APP_SUPPORT/FileID/fileid.sqlite-wal" \
       "$APP_SUPPORT/FileID/fileid.sqlite-shm"
rm -rf "$APP_SUPPORT/FileID/checkpoints"
rm -rf "$APP_SUPPORT/FileID/logs"
rm -rf "$APP_SUPPORT/FileID/thumbs.cache"
rm -rf "$APP_SUPPORT/FileID/face_crops"

echo "🧹 Resetting app preferences (UserDefaults)..."
# Wipes EVERY FileID preference: pickedFolderBookmark, sidebar visibility,
# active tab, library kind filter, last-rename undo journal, person-tag
# history, AI toggles, AI Models picker. Models on disk are preserved.
defaults delete com.fileid.app 2>/dev/null || true
# `cfprefsd` caches preferences in memory — restart it so the next FileID
# launch reads the fresh empty defaults instead of the cached old ones.
killall cfprefsd 2>/dev/null || true

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
