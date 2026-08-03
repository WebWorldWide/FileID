#!/bin/bash
# FileID launcher — rebuilds, bundles into FileID.app/, and optionally opens.
# A fresh-state wipe remains the default for parity testing; pass --no-wipe for
# normal iteration or --wipe-db-only for a fresh library without resetting
# preferences/caches. Downloaded model weights are always preserved.
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

set -euo pipefail

WIPE_MODE="full"
CONFIGURATION="release"
RUN_APP=1
NO_WIPE_REQUESTED=0
DB_WIPE_REQUESTED=0
for arg in "$@"; do
    case "$arg" in
        --no-wipe) WIPE_MODE="none"; NO_WIPE_REQUESTED=1 ;;
        --wipe-db-only) WIPE_MODE="db"; DB_WIPE_REQUESTED=1 ;;
        --debug) CONFIGURATION="debug" ;;
        --no-run) RUN_APP=0 ;;
        --help|-h)
            cat <<'EOF'
Usage: ./run.sh [--no-wipe | --wipe-db-only] [--debug] [--no-run]

  --no-wipe       Preserve the library, caches, logs, and preferences.
  --wipe-db-only  Remove only fileid.sqlite{,-wal,-shm}.
  --debug         Build SwiftPM products in debug configuration.
  --no-run        Assemble FileID.app without opening it.
EOF
            exit 0
            ;;
        *) echo "Unknown flag: $arg (try --help)" >&2; exit 1 ;;
    esac
done
if [ "$NO_WIPE_REQUESTED" -eq 1 ] && [ "$DB_WIPE_REQUESTED" -eq 1 ]; then
    echo "Choose only one wipe mode: --no-wipe or --wipe-db-only." >&2
    exit 1
fi

PROJECT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$PROJECT_DIR"        # so `swift build` finds Package.swift no matter where this is invoked from
APP_NAME="FileID"
BUILD_DIR="$PROJECT_DIR/.build/$CONFIGURATION"
APP_BUNDLE="$PROJECT_DIR/$APP_NAME.app"
CONTENTS="$APP_BUNDLE/Contents"

XCODE_DEV_DIR="${DEVELOPER_DIR:-$(xcode-select -p 2>/dev/null || true)}"

echo "🔨 Building FileID + FileIDEngine ($CONFIGURATION)..."
if [ -x "$XCODE_DEV_DIR/usr/bin/xcodebuild" ]; then
    DEVELOPER_DIR="$XCODE_DEV_DIR" swift build -c "$CONFIGURATION" --product FileID
    DEVELOPER_DIR="$XCODE_DEV_DIR" swift build -c "$CONFIGURATION" --product FileIDEngine
else
    swift build -c "$CONFIGURATION" --product FileID
    swift build -c "$CONFIGURATION" --product FileIDEngine
fi

bash "$PROJECT_DIR/scripts/ensure_mlx_metallib.sh"

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

APP_SUPPORT="$HOME/Library/Application Support"
if [ "$WIPE_MODE" != "none" ]; then
    echo "🧹 Wiping SQLite state (preserving model weights)..."
    rm -f "$APP_SUPPORT/FileID/fileid.sqlite" \
          "$APP_SUPPORT/FileID/fileid.sqlite-wal" \
          "$APP_SUPPORT/FileID/fileid.sqlite-shm"
fi
if [ "$WIPE_MODE" = "full" ]; then
    echo "🧹 Wiping transient caches + resetting app preferences..."
    rm -rf "$APP_SUPPORT/FileID/checkpoints"
    rm -rf "$APP_SUPPORT/FileID/logs"
    rm -rf "$APP_SUPPORT/FileID/thumbs.cache"
    rm -rf "$APP_SUPPORT/FileID/face_crops"
    defaults delete com.fileid.app 2>/dev/null || true
    killall cfprefsd 2>/dev/null || true
elif [ "$WIPE_MODE" = "none" ]; then
    echo "ℹ️  Preserving library, caches, logs, and preferences (--no-wipe)."
fi

echo "📦 Assembling $APP_NAME.app bundle..."
FILEID_BUILD_CONFIGURATION="$CONFIGURATION" \
    bash "$PROJECT_DIR/scripts/assemble_app.sh" "$APP_BUNDLE"

# LaunchServices caches icons aggressively. Touching the bundle invalidates
# the cache so the new icon shows up immediately.
touch "$APP_BUNDLE"

echo "✅ Built: $APP_BUNDLE"
if [ "$RUN_APP" -eq 1 ]; then
    echo "🚀 Launching..."
    open "$APP_BUNDLE"
else
    echo "ℹ️  Launch skipped (--no-run)."
fi
