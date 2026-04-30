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

    echo "📦 Assembling FileID.app bundle…"
    rm -rf "$APP"
    mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
    cp .build/release/FileID       "$APP/Contents/MacOS/FileID"
    cp .build/release/FileIDEngine "$APP/Contents/MacOS/FileIDEngine"
    chmod +x "$APP/Contents/MacOS/"{FileID,FileIDEngine}

    METALLIB_CACHE="$PROJECT_DIR/.build/cache/mlx.metallib"
    if [ -f "$METALLIB_CACHE" ]; then
        cp "$METALLIB_CACHE" "$APP/Contents/MacOS/mlx.metallib"
        cp "$METALLIB_CACHE" "$APP/Contents/MacOS/default.metallib"
    else
        echo "⚠️  $METALLIB_CACHE missing — Deep Analyze will fail at runtime."
        echo "   Run bash run.sh once to build it, then re-run this script."
    fi

    cat > "$APP/Contents/Info.plist" <<'PLIST'
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
    <key>CFBundleShortVersionString</key><string>1.0</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>LSMinimumSystemVersion</key><string>15.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSDesktopFolderUsageDescription</key><string>FileID needs to read your folders to tag, dedupe, and reorganize files.</string>
    <key>NSDocumentsFolderUsageDescription</key><string>FileID needs to read your folders to tag, dedupe, and reorganize files.</string>
    <key>NSDownloadsFolderUsageDescription</key><string>FileID needs to read your folders to tag, dedupe, and reorganize files.</string>
</dict>
</plist>
PLIST
    cp "$PROJECT_DIR/Resources/FileID.icns" "$APP/Contents/Resources/FileID.icns"

    # Strip DWARF debug info. clang embeds absolute compile-time
    # source paths there, which would otherwise leak the maintainer's
    # home directory into public artifacts.
    echo "🧹 Stripping debug symbols…"
    strip -S "$APP/Contents/MacOS/FileID"
    strip -S "$APP/Contents/MacOS/FileIDEngine"

    # Sanitize leftover __cstring paths from MLX's vendored C++ —
    # `__FILE__` macros bake the source path into the binary, which
    # `strip` doesn't touch. Replace each occurrence with a same-
    # length placeholder so the binary stays valid.
    echo "🧹 Rewriting embedded source paths…"
    python3 - <<'PY' "$APP/Contents/MacOS/FileID" "$APP/Contents/MacOS/FileIDEngine"
import sys, os
real = os.path.expanduser("~").encode()
generic = b"/Users/developer"
if len(real) != len(generic):
    pad = generic + b"_" * (len(real) - len(generic)) if len(real) > len(generic) else generic[:len(real)]
else:
    pad = generic
for path in sys.argv[1:]:
    with open(path, "rb") as f:
        data = f.read()
    new = data.replace(real, pad)
    if new != data:
        with open(path, "wb") as f:
            f.write(new)
PY

    # Re-sign. `swift build -c release` ad-hoc-signs the binaries;
    # both `strip -S` and the byte-rewrite above invalidate that
    # cdhash. macOS silently refuses to spawn a tampered ad-hoc
    # binary, which presents in the UI as a permanent "Starting…"
    # state because the engine never reaches its IPC handshake.
    echo "🔐 Re-signing bundle (ad-hoc)…"
    # iCloud's FileProvider re-injects com.apple.FinderInfo on
    # bundles inside synced folders (Desktop / Documents) the
    # instant we strip them, which causes codesign to refuse.
    # Move the bundle to /tmp (non-iCloud), sign there, move back.
    SIGN_TMP=$(mktemp -d /tmp/fileid-sign.XXXXXX)
    mv "$APP" "$SIGN_TMP/$APP"
    find "$SIGN_TMP/$APP" -exec xattr -c {} \; 2>/dev/null || true
    # `--deep` recurses into Contents/MacOS/ and signs every Mach-O
    # plus the bundle wrapper. Ad-hoc identity ("-") leaves both
    # binaries with no Team ID, which the runtime integrity check
    # in EngineClient treats as the dev/unsigned path (matches).
    codesign --force --sign - --deep --timestamp=none "$SIGN_TMP/$APP" 2>&1 | sed 's/^/    /'
    if ! codesign --verify --deep --strict "$SIGN_TMP/$APP"; then
        echo "❌ codesign verify failed — refusing to package."
        rm -rf "$SIGN_TMP"
        exit 1
    fi
    mv "$SIGN_TMP/$APP" "$APP"
    rm -rf "$SIGN_TMP"
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
