#!/bin/bash
# Build, sign, notarize, and package a distributable FileID DMG.
#
# Usage:
#   bash scripts/release.sh v1.0.0                  # full release (needs Developer ID + notary creds)
#   bash scripts/release.sh v1.0.0-rc1 --skip-notarize   # local dry run (ad-hoc fallback OK)
#
# Works without Xcode: CommandLineTools ship codesign, notarytool, stapler,
# and hdiutil. The only Xcode-dependent artifact is the cached mlx.metallib
# (built once by run.sh); this script requires the cache, never rebuilds it.
#
# One-time setup for real releases:
#   1. Apple Developer Program membership.
#   2. "Developer ID Application" certificate in the login keychain
#      (Keychain Access → Certificate Assistant → Request a Certificate…,
#      upload the CSR at developer.apple.com → Certificates).
#   3. xcrun notarytool store-credentials fileid-notary \
#        --apple-id <apple-id> --team-id <TEAMID>   # prompts for an
#        app-specific password from appleid.apple.com.
#
# Signing happens in /tmp because iCloud's FileProvider re-injects
# FinderInfo xattrs on bundles under Desktop/Documents the moment they
# change, which breaks the code seal.

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_DIR"

VERSION_ARG="${1:?usage: release.sh vX.Y.Z [--skip-notarize]}"
SKIP_NOTARIZE=0
[ "${2:-}" = "--skip-notarize" ] && SKIP_NOTARIZE=1

if ! [[ "$VERSION_ARG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.]+)?$ ]]; then
    echo "❌ Version must look like v1.0.0 or v1.0.0-rc1 (got: $VERSION_ARG)"
    exit 1
fi
VERSION="${VERSION_ARG#v}"
BUILD_NUM="$(git -C "$PROJECT_DIR" rev-list --count HEAD 2>/dev/null || echo 1)"

APP="FileID.app"
DIST_DIR="$PROJECT_DIR/dist"
DMG_OUT="$DIST_DIR/FileID-${VERSION_ARG}.dmg"
VOL_NAME="FileID"
NOTARY_PROFILE="${FILEID_NOTARY_PROFILE:-fileid-notary}"
APP_ENTITLEMENTS="$PROJECT_DIR/Resources/FileID.entitlements"
ENGINE_ENTITLEMENTS="$PROJECT_DIR/Resources/FileIDEngine.entitlements"
METALLIB_CACHE="$PROJECT_DIR/.build/cache/mlx.metallib"

cleanup() {
    if [ -d "/Volumes/$VOL_NAME" ]; then
        hdiutil detach "/Volumes/$VOL_NAME" -force -quiet 2>/dev/null || true
    fi
    rm -rf "$DIST_DIR/release-staging" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# ── Preconditions ────────────────────────────────────────────────────────────
if [ ! -f "$METALLIB_CACHE" ]; then
    echo "❌ $METALLIB_CACHE missing — Deep Analyze would be broken in the shipped app."
    echo "   Build it once with: bash run.sh (needs Xcode + Metal Toolchain), then re-run."
    exit 1
fi

IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null \
    | grep "Developer ID Application" | head -1 \
    | sed -E 's/^[^"]*"([^"]+)".*$/\1/' || true)"
if [ -z "$IDENTITY" ]; then
    if [ "$SKIP_NOTARIZE" = "1" ]; then
        echo "⚠️  No 'Developer ID Application' identity — falling back to AD-HOC signing."
        echo "   The DMG will NOT pass Gatekeeper on other Macs (dry-run only)."
        IDENTITY="-"
    else
        echo "❌ No 'Developer ID Application' identity in the keychain."
        echo "   See the one-time setup steps at the top of this script,"
        echo "   or dry-run with: bash scripts/release.sh $VERSION_ARG --skip-notarize"
        exit 1
    fi
fi

TIMESTAMP_FLAG="--timestamp"
[ "$IDENTITY" = "-" ] && TIMESTAMP_FLAG="--timestamp=none"

# ── Build + assemble ─────────────────────────────────────────────────────────
echo "🔨 Building release binaries (v${VERSION}, build ${BUILD_NUM})…"
XCODE_DEV_DIR="/Applications/Xcode.app/Contents/Developer"
if [ -d "$XCODE_DEV_DIR" ]; then
    DEVELOPER_DIR="$XCODE_DEV_DIR" swift build -c release --product FileID
    DEVELOPER_DIR="$XCODE_DEV_DIR" swift build -c release --product FileIDEngine
else
    swift build -c release --product FileID
    swift build -c release --product FileIDEngine
fi

bash "$PROJECT_DIR/scripts/assemble_app.sh" "$PROJECT_DIR/$APP" "$VERSION" "$BUILD_NUM"

# ── Strip + scrub (must precede signing — both invalidate the cdhash) ────────
echo "🧹 Stripping debug symbols…"
strip -S "$APP/Contents/MacOS/FileID"
strip -S "$APP/Contents/MacOS/FileIDEngine"

# MLX's vendored C++ bakes __FILE__ into __cstring; strip doesn't touch it.
# Same-length byte rewrite keeps the binary valid.
echo "🧹 Rewriting embedded source paths…"
python3 - "$APP/Contents/MacOS/FileID" "$APP/Contents/MacOS/FileIDEngine" <<'PY'
import sys, os
real = os.path.expanduser("~").encode()
generic = b"/Users/developer"
if len(real) > len(generic):
    pad = generic + b"_" * (len(real) - len(generic))
else:
    pad = generic[:len(real)]
for path in sys.argv[1:]:
    with open(path, "rb") as f:
        data = f.read()
    new = data.replace(real, pad)
    if new != data:
        with open(path, "wb") as f:
            f.write(new)
PY

# ── Sign (inside-out, hardened runtime, no --deep) ───────────────────────────
echo "🔐 Signing with: ${IDENTITY}"
SIGN_TMP=$(mktemp -d /tmp/fileid-release.XXXXXX)
mv "$APP" "$SIGN_TMP/$APP"
find "$SIGN_TMP/$APP" -exec xattr -c {} \; 2>/dev/null || true

# Nested code first (inside-out, no --deep): the metallibs live in
# Contents/MacOS (MLX loads them colocated with the engine binary), so
# codesign treats them as nested code that must carry its own signature
# before the bundle seal. They sign as Format=generic.
for lib in "$SIGN_TMP/$APP/Contents/MacOS/"*.metallib; do
    [ -f "$lib" ] || continue
    codesign --force $TIMESTAMP_FLAG --sign "$IDENTITY" "$lib"
done
# Then the executables, each with its own entitlements; the bundle
# signature then covers the main executable + seals resources. The engine
# MUST carry the same identity as the app — EngineClient's integrity gate
# refuses to spawn an engine whose Team ID differs from the app's.
codesign --force --options runtime $TIMESTAMP_FLAG \
    --entitlements "$ENGINE_ENTITLEMENTS" --sign "$IDENTITY" \
    "$SIGN_TMP/$APP/Contents/MacOS/FileIDEngine"
codesign --force --options runtime $TIMESTAMP_FLAG \
    --entitlements "$APP_ENTITLEMENTS" --sign "$IDENTITY" \
    "$SIGN_TMP/$APP/Contents/MacOS/FileID"
codesign --force --options runtime $TIMESTAMP_FLAG \
    --entitlements "$APP_ENTITLEMENTS" --sign "$IDENTITY" \
    "$SIGN_TMP/$APP"

echo "🔍 Verifying signatures…"
codesign --verify --deep --strict --verbose=2 "$SIGN_TMP/$APP"
APP_TEAM=$(codesign -dv "$SIGN_TMP/$APP" 2>&1 | grep "^TeamIdentifier" || echo "TeamIdentifier=adhoc")
ENGINE_TEAM=$(codesign -dv "$SIGN_TMP/$APP/Contents/MacOS/FileIDEngine" 2>&1 | grep "^TeamIdentifier" || echo "TeamIdentifier=adhoc")
echo "   app:    $APP_TEAM"
echo "   engine: $ENGINE_TEAM"
if [ "$APP_TEAM" != "$ENGINE_TEAM" ]; then
    echo "❌ Team ID mismatch between app and engine — the in-app integrity gate will refuse to spawn the engine."
    exit 1
fi

mv "$SIGN_TMP/$APP" "$PROJECT_DIR/$APP"
rm -rf "$SIGN_TMP"

# ── Notarize the app ─────────────────────────────────────────────────────────
if [ "$SKIP_NOTARIZE" = "0" ]; then
    echo "📤 Notarizing app (profile: $NOTARY_PROFILE)…"
    NOTARIZE_ZIP="$DIST_DIR/FileID-notarize.zip"
    mkdir -p "$DIST_DIR"
    rm -f "$NOTARIZE_ZIP"
    ditto -c -k --keepParent "$APP" "$NOTARIZE_ZIP"
    if ! xcrun notarytool submit "$NOTARIZE_ZIP" \
        --keychain-profile "$NOTARY_PROFILE" --wait; then
        echo "❌ Notarization failed. Inspect with:"
        echo "   xcrun notarytool log <submission-id> --keychain-profile $NOTARY_PROFILE"
        exit 1
    fi
    rm -f "$NOTARIZE_ZIP"
    echo "📎 Stapling app…"
    xcrun stapler staple "$APP"
else
    echo "⏭️  --skip-notarize: skipping app notarization."
fi

# ── DMG ──────────────────────────────────────────────────────────────────────
echo "💿 Building DMG…"
STAGE_DIR="$DIST_DIR/release-staging"
mkdir -p "$DIST_DIR"
rm -rf "$STAGE_DIR" "$DMG_OUT" "$DIST_DIR/FileID-rw.dmg"
mkdir -p "$STAGE_DIR"
cp -R "$APP" "$STAGE_DIR/"
ln -s /Applications "$STAGE_DIR/Applications"

APP_KB=$(du -sk "$APP" | cut -f1)
HEADROOM_KB=$((APP_KB + 51200))
hdiutil create -volname "$VOL_NAME" -srcfolder "$STAGE_DIR" -fs HFS+ \
    -format UDRW -size "${HEADROOM_KB}k" -ov "$DIST_DIR/FileID-rw.dmg" >/dev/null
hdiutil convert "$DIST_DIR/FileID-rw.dmg" -format UDZO -imagekey zlib-level=9 \
    -o "$DMG_OUT" >/dev/null
rm -f "$DIST_DIR/FileID-rw.dmg"

if [ "$IDENTITY" != "-" ]; then
    echo "🔐 Signing DMG…"
    codesign --force $TIMESTAMP_FLAG --sign "$IDENTITY" "$DMG_OUT"
fi

if [ "$SKIP_NOTARIZE" = "0" ]; then
    echo "📤 Notarizing DMG…"
    if ! xcrun notarytool submit "$DMG_OUT" \
        --keychain-profile "$NOTARY_PROFILE" --wait; then
        echo "❌ DMG notarization failed."
        exit 1
    fi
    echo "📎 Stapling DMG…"
    xcrun stapler staple "$DMG_OUT"
fi

# ── Final gate ───────────────────────────────────────────────────────────────
echo
echo "── Release summary ─────────────────────────────────────────────"
shasum -a 256 "$DMG_OUT"
if [ "$SKIP_NOTARIZE" = "0" ]; then
    echo "🔍 Gatekeeper assessment:"
    if spctl --assess --type open --context context:primary-signature -v "$DMG_OUT"; then
        echo "✅ DMG passes Gatekeeper (Notarized Developer ID)."
    else
        echo "❌ spctl assessment failed — do NOT ship this DMG."
        exit 1
    fi
else
    echo "ℹ️  Dry run (--skip-notarize): Gatekeeper assessment skipped."
    echo "   Launch test: open '$PROJECT_DIR/$APP' — verify the engine spawns"
    echo "   (both sides ad-hoc passes the integrity gate) and Deep Analyze runs"
    echo "   under the hardened runtime."
fi
echo "✅ $DMG_OUT"
