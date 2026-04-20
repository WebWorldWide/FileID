#!/bin/bash
# FileID App Builder
# Builds a proper .app bundle from the Swift Package

set -e

DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
PROJECT_DIR="$(cd "$(dirname "$0")" && pwd)"
APP_NAME="FileID"
BUILD_DIR="$PROJECT_DIR/.build/debug"
APP_BUNDLE="$PROJECT_DIR/$APP_NAME.app"
CONTENTS="$APP_BUNDLE/Contents"

echo "🔨 Building $APP_NAME..."
DEVELOPER_DIR=$DEVELOPER_DIR swift build 2>&1

echo "📦 Assembling .app bundle..."
rm -rf "$APP_BUNDLE"
mkdir -p "$CONTENTS/MacOS"
mkdir -p "$CONTENTS/Resources"

# Copy binary
cp "$BUILD_DIR/$APP_NAME" "$CONTENTS/MacOS/$APP_NAME"

# Copy Info.plist
cp "$PROJECT_DIR/Info.plist" "$CONTENTS/Info.plist"

# Copy Resources
if [ -d "$PROJECT_DIR/Resources" ]; then
    cp -r "$PROJECT_DIR/Resources/." "$CONTENTS/Resources/"
fi

# Set executable permissions
chmod +x "$CONTENTS/MacOS/$APP_NAME"

echo "✅ Built: $APP_BUNDLE"
echo "🚀 Launching..."
open "$APP_BUNDLE"
