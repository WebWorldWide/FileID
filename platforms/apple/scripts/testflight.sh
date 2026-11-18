#!/bin/bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_DIR"

PACKAGE_ONLY=0
if [ "${1:-}" = "--package-only" ]; then
    PACKAGE_ONLY=1
elif [ "$#" -gt 0 ]; then
    echo "Usage: bash scripts/testflight.sh [--package-only]" >&2
    exit 2
fi

PROFILE="${FILEID_MAC_APP_STORE_PROFILE:-}"
APP_IDENTITY="${FILEID_APP_SIGNING_IDENTITY:-}"
INSTALLER_IDENTITY="${FILEID_INSTALLER_SIGNING_IDENTITY:-}"
API_KEY_ID="${APP_STORE_CONNECT_API_KEY_ID:-}"
API_ISSUER_ID="${APP_STORE_CONNECT_ISSUER_ID:-}"
API_KEY_FILE="${APP_STORE_CONNECT_API_KEY_FILE:-}"
APP_ID="com.fileid.app"
VERSION="$(tr -d '[:space:]' < ../windows/VERSION)"
BUILD_NUM="${FILEID_BUILD_NUMBER:-$(git rev-list --count HEAD)}"
STAGE_DIR="$(mktemp -d /tmp/fileid-testflight.XXXXXX)"
APP="$STAGE_DIR/FileID.app"
PROFILE_PLIST="$STAGE_DIR/profile.plist"
PACKAGE="$PROJECT_DIR/dist/FileID-${VERSION}-${BUILD_NUM}.pkg"
cleanup() { rm -rf "$STAGE_DIR"; }
trap cleanup EXIT INT TERM

if [ -z "$PROFILE" ] || [ ! -f "$PROFILE" ]; then
    echo "Set FILEID_MAC_APP_STORE_PROFILE to the macOS App Store provisioning profile for $APP_ID." >&2
    exit 1
fi
if [ -z "$APP_IDENTITY" ]; then
    APP_IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null \
        | sed -n '/Apple Distribution:/s/.*"\(.*\)"/\1/p' | head -1)"
fi
if [ -z "$APP_IDENTITY" ]; then
    echo "Install an Apple Distribution signing identity, or set FILEID_APP_SIGNING_IDENTITY." >&2
    exit 1
fi
if [ -z "$INSTALLER_IDENTITY" ]; then
    INSTALLER_IDENTITY="$(security find-identity -v 2>/dev/null \
        | sed -n '/Mac Installer Distribution:/s/.*"\(.*\)"/\1/p' | head -1)"
fi
if [ -z "$INSTALLER_IDENTITY" ]; then
    INSTALLER_IDENTITY="$(security find-identity -v 2>/dev/null \
        | sed -n '/3rd Party Mac Developer Installer:/s/.*"\(.*\)"/\1/p' | head -1)"
fi
if [ -z "$INSTALLER_IDENTITY" ]; then
    echo "Install a Mac Installer Distribution certificate, or set FILEID_INSTALLER_SIGNING_IDENTITY." >&2
    exit 1
fi
if [ "$PACKAGE_ONLY" -eq 0 ] && { [ -z "$API_KEY_ID" ] || [ -z "$API_ISSUER_ID" ] || [ ! -f "$API_KEY_FILE" ]; }; then
    echo "Set APP_STORE_CONNECT_API_KEY_ID, APP_STORE_CONNECT_ISSUER_ID, and APP_STORE_CONNECT_API_KEY_FILE to upload." >&2
    echo "Use --package-only to build a signed package without uploading." >&2
    exit 1
fi

security cms -D -i "$PROFILE" > "$PROFILE_PLIST"
PROFILE_APP_ID="$(
    /usr/libexec/PlistBuddy -c 'Print :Entitlements:application-identifier' "$PROFILE_PLIST" 2>/dev/null \
    || /usr/libexec/PlistBuddy -c 'Print :Entitlements:com.apple.application-identifier' "$PROFILE_PLIST"
)"
case "$PROFILE_APP_ID" in
    "$APP_ID"|*."$APP_ID") ;;
    *) echo "Provisioning profile application identifier does not match $APP_ID." >&2; exit 1 ;;
esac

echo "Building sandboxed macOS App Store binaries…"
swift build -c release -Xswiftc -DFILEID_APP_STORE --product FileID
bash scripts/ensure_mlx_metallib.sh
[ -s "$PROJECT_DIR/.build/cache/mlx.metallib" ] || {
    echo "The required MLX Metal library was not built; refusing to package." >&2
    exit 1
}
# The sandboxed variant bundles the same single engine as the Developer ID build:
# Contents/MacOS/fileidd, started by the app as a child that inherits its sandbox
# (Resources/FileIDEngineAppStore.entitlements). FILEID_REQUIRE_ORT=1 refuses a
# bundle without the pinned ONNX Runtime.
FILEID_APP_STORE=1 FILEID_REQUIRE_ORT=1 FILEID_BUILD_CONFIGURATION=release \
    bash scripts/assemble_app.sh "$APP" "$VERSION" "$BUILD_NUM"
[ -x "$APP/Contents/MacOS/fileidd" ] || { echo "fileidd is missing from the App Store bundle." >&2; exit 1; }
[ ! -e "$APP/Contents/MacOS/FileIDEngine" ] || { echo "A Swift engine binary is in the App Store bundle; only fileidd ships." >&2; exit 1; }
[ -s "$APP/Contents/MacOS/mlx.metallib" ] || {
    echo "The required MLX Metal library is missing from the app bundle." >&2
    exit 1
}
cp "$PROFILE" "$APP/Contents/embedded.provisionprofile"
chmod -R a+rX "$APP"

# Nested code first (inside-out, no --deep): the metallibs beside the app and
# beside libFileIDAppleKit, the bundled dylibs, then fileidd with the inherited
# sandbox entitlements.
for lib in "$APP/Contents/MacOS/"*.metallib "$APP/Contents/Frameworks/"*.metallib; do
    [ -f "$lib" ] || continue
    codesign --force --sign "$APP_IDENTITY" --timestamp "$lib"
done
for lib in "$APP/Contents/Frameworks/"*.dylib; do
    [ -f "$lib" ] || continue
    codesign --force --sign "$APP_IDENTITY" --timestamp --options runtime "$lib"
done
codesign --force --sign "$APP_IDENTITY" --timestamp --options runtime \
    --entitlements Resources/FileIDEngineAppStore.entitlements \
    "$APP/Contents/MacOS/fileidd"
codesign --force --sign "$APP_IDENTITY" --timestamp --options runtime \
    --entitlements Resources/FileIDAppStore.entitlements "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"

mkdir -p "$PROJECT_DIR/dist"
if [ -e "$PACKAGE" ]; then
    echo "Refusing to overwrite existing package $PACKAGE; choose a new FILEID_BUILD_NUMBER." >&2
    exit 1
fi
productbuild --component "$APP" /Applications --sign "$INSTALLER_IDENTITY" "$PACKAGE"
pkgutil --check-signature "$PACKAGE"
echo "Signed TestFlight package: $PACKAGE"

if [ "$PACKAGE_ONLY" -eq 1 ]; then
    exit 0
fi
xcrun altool --upload-package "$PACKAGE" \
    --api-key "$API_KEY_ID" --api-issuer "$API_ISSUER_ID" \
    --p8-file-path "$API_KEY_FILE"
