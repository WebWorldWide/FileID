#!/bin/bash
# Build a styled, distributable FileID.dmg from FileID.app.
#
# Usage:
#   bash scripts/build_dmg.sh            # uses the bundle currently at FileID.app/
#   bash scripts/build_dmg.sh --rebuild  # rebuilds release binaries first
#
# Output: dist/FileID.dmg
#
# Window layout: 600×400, icon view, no toolbar/sidebar, hazard-tape
# border + iridescent centre. Applications shortcut on the left,
# FileID.app on the right, leftward arrow drawn into the bg between
# them. Drag-to-install.
#
# DMG is unsigned. First-run on default-Gatekeeper Macs needs
# right-click → Open. Sign + notarize if you want frictionless
# distribution.

set -e

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_DIR"

APP="FileID.app"
DIST_DIR="dist"
STAGE_DIR="$DIST_DIR/dmg-staging"
DMG_RW="$DIST_DIR/FileID-rw.dmg"
DMG_OUT="$DIST_DIR/FileID.dmg"
BG_PNG="$DIST_DIR/dmg-background.png"
VOL_NAME="FileID"
XCODE_DEV_DIR="/Applications/Xcode.app/Contents/Developer"

# Best-effort cleanup on exit (handles ⌃C, AppleScript hangs, etc.)
cleanup() {
    if [ -d "/Volumes/$VOL_NAME" ]; then
        hdiutil detach "/Volumes/$VOL_NAME" -force -quiet 2>/dev/null || true
    fi
    rm -rf "$STAGE_DIR" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

if [ "${1:-}" = "--rebuild" ]; then
    echo "🔨 Building release binaries…"
    DEVELOPER_DIR="$XCODE_DEV_DIR" swift build -c release --product FileID
    DEVELOPER_DIR="$XCODE_DEV_DIR" swift build -c release --product FileIDEngine
    cp .build/release/FileID       "$APP/Contents/MacOS/FileID"
    cp .build/release/FileIDEngine "$APP/Contents/MacOS/FileIDEngine"
    chmod +x "$APP/Contents/MacOS/"{FileID,FileIDEngine}
fi

[ -d "$APP" ] || { echo "❌ $APP not found. Run with --rebuild or run.sh first."; exit 1; }
[ -x "$APP/Contents/MacOS/FileID" ] || { echo "❌ Missing $APP/Contents/MacOS/FileID"; exit 1; }
[ -x "$APP/Contents/MacOS/FileIDEngine" ] || { echo "❌ Missing $APP/Contents/MacOS/FileIDEngine"; exit 1; }

# Detach any prior mount before we start.
if [ -d "/Volumes/$VOL_NAME" ]; then
    hdiutil detach "/Volumes/$VOL_NAME" -force -quiet 2>/dev/null || true
fi

# 1. Render the background PNG.
mkdir -p "$DIST_DIR"
echo "🎨 Rendering DMG background…"
DEVELOPER_DIR="$XCODE_DEV_DIR" \
    swift scripts/make_dmg_background.swift "$BG_PNG" >/dev/null

# 2. Stage app + Applications symlink + hidden background.
rm -rf "$STAGE_DIR" "$DMG_RW" "$DMG_OUT"
mkdir -p "$STAGE_DIR/.background"
cp -R "$APP" "$STAGE_DIR/"
ln -s /Applications "$STAGE_DIR/Applications"
cp "$BG_PNG" "$STAGE_DIR/.background/background.png"

# 3. Build a writable DMG sized for the contents + headroom.
APP_KB=$(du -sk "$APP" | cut -f1)
HEADROOM_KB=$((APP_KB + 51200))
echo "💿 Creating writable image (~$((HEADROOM_KB / 1024)) MB)…"
hdiutil create \
    -volname "$VOL_NAME" \
    -srcfolder "$STAGE_DIR" \
    -fs HFS+ \
    -format UDRW \
    -size "${HEADROOM_KB}k" \
    -ov \
    "$DMG_RW" >/dev/null

# 4. Mount, run AppleScript to set window appearance, unmount.
echo "🖼  Styling window…"
hdiutil attach "$DMG_RW" -readwrite -noautoopen >/dev/null
MOUNT_POINT="/Volumes/$VOL_NAME"
SetFile -a V "$MOUNT_POINT/.background" 2>/dev/null || true

# AppleScript wrapped in a timeout so a frozen Finder can't hang the
# script. If styling fails, we proceed with a plain compressed DMG.
osascript <<EOF >/dev/null 2>&1 &
with timeout of 20 seconds
    tell application "Finder"
        tell disk "$VOL_NAME"
            open
            delay 1
            set current view of container window to icon view
            set toolbar visible of container window to false
            set statusbar visible of container window to false
            set the bounds of container window to {200, 120, 800, 520}
            set viewOptions to the icon view options of container window
            set arrangement of viewOptions to not arranged
            set icon size of viewOptions to 128
            set background picture of viewOptions to file ".background:background.png"
            set position of item "Applications" of container window to {160, 200}
            set position of item "$APP" of container window to {440, 200}
            update without registering applications
            delay 1
            close
        end tell
    end tell
end timeout
EOF
APPLESCRIPT_PID=$!

# Hard cap at 30s for the whole styling step.
WAITED=0
while kill -0 "$APPLESCRIPT_PID" 2>/dev/null; do
    sleep 1
    WAITED=$((WAITED + 1))
    if [ "$WAITED" -ge 30 ]; then
        echo "  ⚠️  AppleScript styling timed out — shipping plain DMG."
        kill -9 "$APPLESCRIPT_PID" 2>/dev/null || true
        # Also kill any lingering Finder instance that might be stuck.
        osascript -e 'tell application "Finder" to quit' >/dev/null 2>&1 || true
        break
    fi
done
wait "$APPLESCRIPT_PID" 2>/dev/null || true

# Settle Finder writes (.DS_Store) before unmount.
sync; sleep 2
hdiutil detach "$MOUNT_POINT" -quiet 2>/dev/null \
    || hdiutil detach "$MOUNT_POINT" -force -quiet 2>/dev/null \
    || true

# 5. Convert to compressed read-only DMG.
echo "📦 Compressing…"
hdiutil convert "$DMG_RW" -format UDZO -imagekey zlib-level=9 -o "$DMG_OUT" >/dev/null
rm -f "$DMG_RW"

SIZE=$(du -sh "$DMG_OUT" | cut -f1)
echo "✅ $DMG_OUT ($SIZE)"
