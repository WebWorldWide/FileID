#!/bin/bash
# Build a distributable FileID.dmg from FileID.app.
#
# Usage:
#   bash scripts/build_dmg.sh                     # package existing FileID.app/
#   bash scripts/build_dmg.sh --rebuild           # rebuild binaries first
#   bash scripts/build_dmg.sh --rebuild --clean   # also wipe local state
#                                                 # (~/Library/Application
#                                                 # Support/FileID, prefs)
#                                                 # AND ~/Documents/huggingface
#                                                 # before rebuilding — for
#                                                 # testing the welcome-sheet
#                                                 # download flow on a truly
#                                                 # fresh install.
#
# Outputs the final DMG to ~/Desktop/FileID.dmg AND keeps a copy at
# dist/FileID.dmg. Stale numbered duplicates in dist/ are wiped each
# run so you don't accumulate FileID 2.dmg, FileID 3.dmg, etc.
#
# Plain DMG — vanilla Finder window with FileID.app + an Applications
# symlink. Drag-to-install. Unsigned (ad-hoc only): default-Gatekeeper
# Macs need right-click → Open on first launch.

set -e

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_DIR"

REBUILD=0
CLEAN=0
for arg in "$@"; do
    case "$arg" in
        --rebuild) REBUILD=1 ;;
        --clean)   CLEAN=1 ;;
        --help|-h)
            sed -n '1,17p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "Unknown flag: $arg (try --help)"; exit 1 ;;
    esac
done

APP="FileID.app"
DIST_DIR="dist"
STAGE_DIR="$DIST_DIR/dmg-staging"
DMG_RW="$DIST_DIR/FileID-rw.dmg"
DMG_OUT="$DIST_DIR/FileID.dmg"
VOL_NAME="FileID"
XCODE_DEV_DIR="/Applications/Xcode.app/Contents/Developer"

cleanup() {
    if [ -d "/Volumes/$VOL_NAME" ]; then
        hdiutil detach "/Volumes/$VOL_NAME" -force -quiet 2>/dev/null || true
    fi
    rm -rf "$STAGE_DIR" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

if [ "$CLEAN" = "1" ]; then
    echo "🧨 --clean: wiping local FileID state + VLM weights…"
    bash "$PROJECT_DIR/scripts/wipe_local_state.sh" --purge-models
fi

if [ "$REBUILD" = "1" ]; then
    echo "🔨 Building release binaries…"
    if [ -d "$XCODE_DEV_DIR" ]; then
        DEVELOPER_DIR="$XCODE_DEV_DIR" swift build -c release --product FileID
        DEVELOPER_DIR="$XCODE_DEV_DIR" swift build -c release --product FileIDEngine
    else
        swift build -c release --product FileID
        swift build -c release --product FileIDEngine
    fi

    echo "📦 Assembling FileID.app bundle…"
    bash "$PROJECT_DIR/scripts/assemble_app.sh" "$PROJECT_DIR/$APP"

    # Strip DWARF — clang's __FILE__ / debug paths leak the maintainer's home.
    echo "🧹 Stripping debug symbols…"
    strip -S "$APP/Contents/MacOS/FileID"
    strip -S "$APP/Contents/MacOS/FileIDEngine"

    # MLX's vendored C++ bakes __FILE__ into __cstring; strip doesn't
    # touch it. Same-length byte rewrite keeps the binary valid.
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

    # strip + byte-rewrite invalidate the cdhash; macOS silently kills
    # tampered ad-hoc binaries (looks like a permanent "Starting…").
    # Sign in /tmp because iCloud's FileProvider re-injects FinderInfo
    # xattr on bundles in Desktop/Documents the moment they're stripped.
    echo "🔐 Re-signing bundle (ad-hoc)…"
    SIGN_TMP=$(mktemp -d /tmp/fileid-sign.XXXXXX)
    mv "$APP" "$SIGN_TMP/$APP"
    find "$SIGN_TMP/$APP" -exec xattr -c {} \; 2>/dev/null || true
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

if [ -d "/Volumes/$VOL_NAME" ]; then
    hdiutil detach "/Volumes/$VOL_NAME" -force -quiet 2>/dev/null || true
fi

# Stage: app + Applications symlink. Plain — no background image, no
# .DS_Store, no styled window. Finder picks defaults; reliable.
mkdir -p "$DIST_DIR"
# Wipe any stale numbered duplicates Finder leaves behind (FileID 2.dmg,
# FileID 3.dmg, etc.) plus the working files for this run.
rm -rf "$STAGE_DIR" "$DMG_RW" "$DMG_OUT"
find "$DIST_DIR" -maxdepth 1 -name "FileID *.dmg" -delete 2>/dev/null || true
find "$DIST_DIR" -maxdepth 1 -name "dmg-background*" -delete 2>/dev/null || true
mkdir -p "$STAGE_DIR"
cp -R "$APP" "$STAGE_DIR/"
ln -s /Applications "$STAGE_DIR/Applications"

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

echo "📦 Compressing…"
hdiutil convert "$DMG_RW" -format UDZO -imagekey zlib-level=9 -o "$DMG_OUT" >/dev/null
rm -f "$DMG_RW"

# Mirror to ~/Desktop for one-click access. Wipe any prior copy first
# so Finder doesn't number the new one (FileID 2.dmg, FileID 3.dmg…).
DESKTOP_OUT="$HOME/Desktop/FileID.dmg"
find "$HOME/Desktop" -maxdepth 1 -name "FileID *.dmg" -delete 2>/dev/null || true
rm -f "$DESKTOP_OUT"
cp "$DMG_OUT" "$DESKTOP_OUT"

SIZE=$(du -sh "$DMG_OUT" | cut -f1)
echo "✅ $DMG_OUT ($SIZE)"
echo "✅ $DESKTOP_OUT ($SIZE)"
