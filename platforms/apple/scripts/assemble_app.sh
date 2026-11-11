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
#
# Shared Rust engine (dogfood; the app starts it only when engine.backend is
# rust): FILEID_WITH_RUST_ENGINE=1 builds FileIDAppleKit and the Rust engine
# with --features apple-adapters and bundles them as Contents/MacOS/fileidd and
# Contents/Frameworks/libFileIDAppleKit.dylib. FILEID_RUST_ENGINE_BINARY skips
# the cargo build with an engine already built that way; FILEID_RUST_TARGET_DIR
# sets cargo's target folder. FILEID_ORT_DYLIB names the official MIT
# libonnxruntime.dylib (>= 1.22, self-contained) to bundle in Frameworks.

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
BUILD_DIR="${FILEID_BUILD_DIR:-$PROJECT_DIR/.build/$BUILD_CONFIGURATION}"
CONTENTS="$APP_BUNDLE/Contents"
METALLIB_CACHE="${FILEID_METALLIB_CACHE:-$PROJECT_DIR/.build/cache/mlx.metallib}"

[ -x "$BUILD_DIR/FileID" ]       || { echo "❌ $BUILD_DIR/FileID missing — build first"; exit 1; }
[ -x "$BUILD_DIR/FileIDEngine" ] || { echo "❌ $BUILD_DIR/FileIDEngine missing — build first"; exit 1; }

if [ ! -f "$BUILD_DIR/FileID_FileIDShared.bundle/model-catalog.json" ] && [ ! -f "$BUILD_DIR/FileID_FileIDShared.bundle/Contents/Resources/model-catalog.json" ]; then
    echo "❌ Shared model catalog resources missing — rebuild FileID first."
    exit 1
fi

rm -rf "$APP_BUNDLE"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"

cp "$BUILD_DIR/FileID"       "$CONTENTS/MacOS/FileID"
cp "$BUILD_DIR/FileIDEngine" "$CONTENTS/MacOS/FileIDEngine"
chmod +x "$CONTENTS/MacOS/FileID" "$CONTENTS/MacOS/FileIDEngine"


shopt -s nullglob
for resource_bundle in "$BUILD_DIR/"*.bundle; do
    cp -R "$resource_bundle" "$CONTENTS/Resources/"
done
shopt -u nullglob

mkdir -p "$CONTENTS/Frameworks"
xcrun swift-stdlib-tool --copy --platform macosx \
    --scan-executable "$CONTENTS/MacOS/FileID" \
    --scan-executable "$CONTENTS/MacOS/FileIDEngine" \
    --destination "$CONTENTS/Frameworks"
for executable in "$CONTENTS/MacOS/FileID" "$CONTENTS/MacOS/FileIDEngine"; do
    if ! otool -l "$executable" | grep -F 'path @executable_path/../Frameworks' >/dev/null; then
        install_name_tool -add_rpath '@executable_path/../Frameworks' "$executable"
    fi
done



# MLX first loads a colocated mlx.metallib beside the engine binary.
if [ -f "$METALLIB_CACHE" ]; then
    cp "$METALLIB_CACHE" "$CONTENTS/MacOS/mlx.metallib"
else
    echo "⚠️  $METALLIB_CACHE missing — Deep Analyze will fail at runtime."
    echo "   Run bash run.sh once on a Mac with Xcode + Metal Toolchain to build it."
fi

if [ "${FILEID_WITH_RUST_ENGINE:-0}" = "1" ]; then
    SWIFT_SCRATCH="$(dirname "$BUILD_DIR")"
    APPLE_KIT="$BUILD_DIR/libFileIDAppleKit.dylib"
    swift build --package-path "$PROJECT_DIR" --scratch-path "$SWIFT_SCRATCH" \
        -c "$BUILD_CONFIGURATION" --product FileIDAppleKit
    [ -f "$APPLE_KIT" ] || { echo "❌ $APPLE_KIT missing after build"; exit 1; }
    RUST_ENGINE="${FILEID_RUST_ENGINE_BINARY:-}"
    if [ -z "$RUST_ENGINE" ]; then
        RUST_TARGET_DIR="${FILEID_RUST_TARGET_DIR:-$SWIFT_SCRATCH/rust-engine}"
        CARGO_PROFILE_ARGS=()
        [ "$BUILD_CONFIGURATION" = "release" ] && CARGO_PROFILE_ARGS=(--release)
        (cd "$PROJECT_DIR/../windows/src/engine" && \
            LIBRARY_PATH="$BUILD_DIR" CARGO_TARGET_DIR="$RUST_TARGET_DIR" LIBONNXRUNTIME_NO_PKG_CONFIG=1 \
            cargo build --locked "${CARGO_PROFILE_ARGS[@]}" --features apple-adapters)
        RUST_ENGINE="$RUST_TARGET_DIR/$BUILD_CONFIGURATION/FileIDEngine"
    fi
    [ -x "$RUST_ENGINE" ] || { echo "❌ Rust engine missing: $RUST_ENGINE"; exit 1; }
    otool -L "$RUST_ENGINE" | grep -F '@rpath/libFileIDAppleKit.dylib' >/dev/null || {
        echo "❌ $RUST_ENGINE was not built with --features apple-adapters"; exit 1; }
    # The test-trash-root feature (conformance and crash-harness builds) moves
    # trashed files into a scratch folder instead of the Trash; never ship it.
    if grep -aqF 'FILEID_TEST_TRASH_ROOT' "$RUST_ENGINE"; then
        echo "❌ $RUST_ENGINE is a test build (test-trash-root feature)"; exit 1
    fi
    # ort-sys links a pkg-config ONNX Runtime (e.g. Homebrew's) when it finds
    # one; the engine must load the bundled runtime at run time instead.
    if otool -L "$RUST_ENGINE" | tail -n +2 | grep -F 'libonnxruntime' >/dev/null; then
        echo "❌ $RUST_ENGINE links an ONNX Runtime at build time; rebuild with LIBONNXRUNTIME_NO_PKG_CONFIG=1"; exit 1
    fi
    cp "$RUST_ENGINE" "$CONTENTS/MacOS/fileidd"
    chmod +x "$CONTENTS/MacOS/fileidd"
    cp "$APPLE_KIT" "$CONTENTS/Frameworks/libFileIDAppleKit.dylib"
    if ! otool -l "$CONTENTS/MacOS/fileidd" | grep -F 'path @executable_path/../Frameworks' >/dev/null; then
        install_name_tool -add_rpath '@executable_path/../Frameworks' "$CONTENTS/MacOS/fileidd"
    fi
    if [ -n "${FILEID_ORT_DYLIB:-}" ]; then
        # Bundle only a self-contained runtime (system libraries only), never
        # a Homebrew build that links other Homebrew libraries.
        FOREIGN="$(otool -L "$FILEID_ORT_DYLIB" | tail -n +2 | awk '{print $1}' \
            | grep -vE '^(/usr/lib/|/System/Library/|@rpath/libonnxruntime)' || true)"
        if [ -n "$FOREIGN" ]; then
            echo "❌ $FILEID_ORT_DYLIB links non-system libraries:"; echo "$FOREIGN"; exit 1
        fi
        # Builds with Microsoft's telemetry client keep a device id and an event
        # queue under ~/Library/Application Support/Microsoft/DeveloperTools
        # (Homebrew's 1.31.1 does, even with telemetry turned off in the engine).
        if strings -a "$FILEID_ORT_DYLIB" | grep -E 'telemetry flush|DeviceId:' >/dev/null; then
            echo "❌ $FILEID_ORT_DYLIB carries a telemetry client; bundle a build without it"; exit 1
        fi
        cp "$FILEID_ORT_DYLIB" "$CONTENTS/Frameworks/libonnxruntime.dylib"
    else
        echo "⚠️  FILEID_ORT_DYLIB not set — the Rust engine will run without ONNX models."
    fi
    # Ad-hoc signatures keep dev bundles launchable after install_name_tool;
    # release.sh re-signs everything with the Developer ID.
    for code in "$CONTENTS/Frameworks/libFileIDAppleKit.dylib" "$CONTENTS/Frameworks/libonnxruntime.dylib" "$CONTENTS/MacOS/fileidd"; do
        [ -f "$code" ] && codesign --force --sign - "$code" >/dev/null
    done
    echo "🦀 Bundled the Rust engine as Contents/MacOS/fileidd (dogfood: defaults write com.fileid.app engine.backend rust)"
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
    <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
    <key>LSMinimumSystemVersion</key><string>15.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSDesktopFolderUsageDescription</key><string>FileID needs to read your folders to tag, dedupe, and reorganize files.</string>
    <key>NSDocumentsFolderUsageDescription</key><string>FileID needs to read your folders to tag, dedupe, and reorganize files.</string>
    <key>NSDownloadsFolderUsageDescription</key><string>FileID needs to read your folders to tag, dedupe, and reorganize files.</string>
    <key>NSSpeechRecognitionUsageDescription</key><string>FileID transcribes audio and video on-device to make recordings searchable. Nothing leaves your Mac.</string>
</dict>
</plist>
PLIST

cp "$PROJECT_DIR/Resources/FileID.icns" "$CONTENTS/Resources/FileID.icns"
# Required-reason API declarations and "no tracking, no collected data" (App Store privacy manifest).
cp "$PROJECT_DIR/Resources/PrivacyInfo.xcprivacy" "$CONTENTS/Resources/PrivacyInfo.xcprivacy"
plutil -lint "$CONTENTS/Resources/PrivacyInfo.xcprivacy" >/dev/null
echo "✅ Assembled $APP_BUNDLE (v${VERSION}, build ${BUILD_NUM})"
