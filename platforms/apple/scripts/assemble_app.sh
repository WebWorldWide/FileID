#!/bin/bash
# Assemble FileID.app from built release products. Shared by run.sh,
# scripts/build_dmg.sh, and scripts/release.sh so the bundle layout and
# Info.plist live in exactly one place.
#
# Usage:
#   bash scripts/assemble_app.sh <output-bundle-path> [version] [build-number]
#
# Expects .build/release/{FileID,FileIDEngine} to exist (caller builds).
# Copies the cached mlx.metallib when present; warns when absent (Deep
# Analyze needs it at runtime).

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
APP_BUNDLE="${1:?usage: assemble_app.sh <bundle-path> [version] [build]}"
VERSION="${2:-1.0}"
BUILD_NUM="${3:-1}"

BUILD_DIR="$PROJECT_DIR/.build/release"
CONTENTS="$APP_BUNDLE/Contents"
METALLIB_CACHE="$PROJECT_DIR/.build/cache/mlx.metallib"

[ -x "$BUILD_DIR/FileID" ]       || { echo "❌ $BUILD_DIR/FileID missing — build first"; exit 1; }
[ -x "$BUILD_DIR/FileIDEngine" ] || { echo "❌ $BUILD_DIR/FileIDEngine missing — build first"; exit 1; }

rm -rf "$APP_BUNDLE"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"

cp "$BUILD_DIR/FileID"       "$CONTENTS/MacOS/FileID"
cp "$BUILD_DIR/FileIDEngine" "$CONTENTS/MacOS/FileIDEngine"
chmod +x "$CONTENTS/MacOS/FileID" "$CONTENTS/MacOS/FileIDEngine"

# MLX loads its GPU kernels from a metallib colocated with the engine
# binary; both names because MLX tries default.metallib then mlx.metallib.
if [ -f "$METALLIB_CACHE" ]; then
    cp "$METALLIB_CACHE" "$CONTENTS/MacOS/mlx.metallib"
    cp "$METALLIB_CACHE" "$CONTENTS/MacOS/default.metallib"
else
    echo "⚠️  $METALLIB_CACHE missing — Deep Analyze will fail at runtime."
    echo "   Run bash run.sh once on a Mac with Xcode + Metal Toolchain to build it."
fi

cat > "$CONTENTS/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>com.fileid.app</string>
    <key>CFBundleName</key><string>FileID</string>
    <key>CFBundleDisplayName</key><string>FileID</string>
    <key>CFBundleExecutable</key><string>FileID</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleIconFile</key><string>FileID</string>
    <key>CFBundleIconName</key><string>FileID</string>
    <key>CFBundleShortVersionString</key><string>${VERSION}</string>
    <key>CFBundleVersion</key><string>${BUILD_NUM}</string>
    <key>LSMinimumSystemVersion</key><string>15.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSDesktopFolderUsageDescription</key><string>FileID needs to read your folders to tag, dedupe, and reorganize files.</string>
    <key>NSDocumentsFolderUsageDescription</key><string>FileID needs to read your folders to tag, dedupe, and reorganize files.</string>
    <key>NSDownloadsFolderUsageDescription</key><string>FileID needs to read your folders to tag, dedupe, and reorganize files.</string>
</dict>
</plist>
PLIST

cp "$PROJECT_DIR/Resources/FileID.icns" "$CONTENTS/Resources/FileID.icns"
echo "✅ Assembled $APP_BUNDLE (v${VERSION}, build ${BUILD_NUM})"
