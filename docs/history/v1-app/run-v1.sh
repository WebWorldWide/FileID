#!/bin/bash
# FileID v1 (LEGACY) builder — fallback launcher for the original SwiftData
# app under Sources/. The default launcher is now `run.sh` (which builds v2).
# This is kept around so v1 can still be exercised side-by-side until the
# v2 cutover lands.
#
# Builds a release .app bundle and wipes SwiftData + transient caches before
# launching so a rebuild always starts from a known clean state. Downloaded
# model weights (Application Support/FileID/Models/ and
# ~/Documents/huggingface/models/) are preserved — they're multi-GB and
# re-downloading them every compile would be punishing.

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
    echo "   FileID needs Xcode (not just Command Line Tools) for the SwiftData macro plugin."
    exit 1
fi

echo "🔨 Building $APP_NAME (release)..."
DEVELOPER_DIR="$XCODE_DEV_DIR" swift build -c release

echo "🧹 Wiping SwiftData store + transient caches (preserving model weights)..."
APP_SUPPORT="$HOME/Library/Application Support"
rm -f  "$APP_SUPPORT/default.store" \
       "$APP_SUPPORT/default.store-wal" \
       "$APP_SUPPORT/default.store-shm"
rm -f  "$APP_SUPPORT/FileID/app_running.json"
rm -rf "$APP_SUPPORT/FileID/FacePrintCache"
rm -rf "$APP_SUPPORT/FileID/ScanCache"
rm -rf "$HOME/Library/Caches/com.adamnolle.FileID"
rm -rf "$HOME/Library/Logs/FileID"

echo "📦 Assembling .app bundle..."
rm -rf "$APP_BUNDLE"
mkdir -p "$CONTENTS/MacOS"
mkdir -p "$CONTENTS/Resources"

cp "$BUILD_DIR/$APP_NAME"  "$CONTENTS/MacOS/$APP_NAME"
cp "$PROJECT_DIR/Info.plist" "$CONTENTS/Info.plist"

if [ -d "$PROJECT_DIR/Resources" ]; then
    cp -r "$PROJECT_DIR/Resources/." "$CONTENTS/Resources/"
fi

chmod +x "$CONTENTS/MacOS/$APP_NAME"

echo "✅ Built: $APP_BUNDLE"
echo "🚀 Launching (fresh state)..."
open "$APP_BUNDLE"
