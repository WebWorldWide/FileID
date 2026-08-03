#!/bin/bash
# Assemble FileID.app from built release products. Shared by run.sh,
# scripts/build_dmg.sh, and scripts/release.sh so the bundle layout and
# Info.plist live in exactly one place.
#
# Usage:
#   bash scripts/assemble_app.sh <output-bundle-path> [version] [build-number]
#
# Expects .build/release/{FileID,FileIDEngine} and the cached mlx.metallib.

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
APP_BUNDLE="${1:?usage: assemble_app.sh <bundle-path> [version] [build]}"
VERSION_FILE="$PROJECT_DIR/../windows/VERSION"
[ -f "$VERSION_FILE" ] || { echo "❌ canonical version file missing: $VERSION_FILE"; exit 1; }
VERSION="${2:-$(tr -d '[:space:]' < "$VERSION_FILE")}"
BUILD_NUM="${3:-1}"

BUILD_CONFIGURATION="${FILEID_BUILD_CONFIGURATION:-release}"
case "$BUILD_CONFIGURATION" in
    release|debug) ;;
    *) echo "❌ unsupported FILEID_BUILD_CONFIGURATION: $BUILD_CONFIGURATION"; exit 1 ;;
esac
BUILD_DIR="$PROJECT_DIR/.build/$BUILD_CONFIGURATION"
CONTENTS="$APP_BUNDLE/Contents"
METALLIB_CACHE="$PROJECT_DIR/.build/cache/mlx.metallib"

[ -x "$BUILD_DIR/FileID" ]       || { echo "❌ $BUILD_DIR/FileID missing — build first"; exit 1; }
[ -x "$BUILD_DIR/FileIDEngine" ] || { echo "❌ $BUILD_DIR/FileIDEngine missing — build first"; exit 1; }
[ -s "$METALLIB_CACHE" ]         || { echo "❌ $METALLIB_CACHE missing — refusing to assemble an app without Deep Analyze"; exit 1; }

rm -rf "$APP_BUNDLE"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"

cp "$BUILD_DIR/FileID"       "$CONTENTS/MacOS/FileID"
cp "$BUILD_DIR/FileIDEngine" "$CONTENTS/MacOS/FileIDEngine"
chmod +x "$CONTENTS/MacOS/FileID" "$CONTENTS/MacOS/FileIDEngine"

/usr/bin/otool -l "$CONTENTS/MacOS/FileIDEngine" \
    | /usr/bin/grep -F '__info_plist' >/dev/null \
    || { echo "❌ FileIDEngine is missing its embedded background-agent metadata"; exit 1; }
/usr/bin/strings "$CONTENTS/MacOS/FileIDEngine" \
    | /usr/bin/grep -F 'com.fileid.app.engine' >/dev/null \
    || { echo "❌ FileIDEngine is missing its helper bundle identity"; exit 1; }
/usr/bin/strings "$CONTENTS/MacOS/FileIDEngine" \
    | /usr/bin/awk '/<key>LSUIElement<\/key>/ { if (getline > 0 && $0 ~ /<true\/>/) found=1 } END { exit found ? 0 : 1 }' \
    || { echo "❌ FileIDEngine is not marked as a background agent"; exit 1; }

cp "$METALLIB_CACHE" "$CONTENTS/MacOS/mlx.metallib"

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
    <key>NSSpeechRecognitionUsageDescription</key><string>FileID transcribes audio on-device to give voice memos and recordings descriptive names. Nothing leaves your Mac.</string>
</dict>
</plist>
PLIST

cp "$PROJECT_DIR/Resources/FileID.icns" "$CONTENTS/Resources/FileID.icns"
echo "✅ Assembled $APP_BUNDLE (v${VERSION}, build ${BUILD_NUM})"
