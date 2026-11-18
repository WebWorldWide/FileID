#!/bin/bash
# Assemble FileID.app from built release products. Shared by run.sh,
# scripts/build_dmg.sh, and scripts/release.sh so the bundle layout and
# Info.plist live in exactly one place.
#
# Usage:
#   bash scripts/assemble_app.sh <output-bundle-path> [version] [build-number]
#
# Expects .build/release/FileID to exist (caller builds). Copies the cached
# mlx.metallib when present; warns when absent (Deep Analyze needs it at runtime).
#
# The one engine is the shared Rust engine: the script builds FileIDAppleKit and
# the Rust engine with --features apple-adapters and bundles them as
# Contents/MacOS/fileidd and Contents/Frameworks/libFileIDAppleKit.dylib. No
# Swift engine binary is bundled, for Developer ID and App Store builds alike.
# FILEID_RUST_ENGINE_BINARY skips the cargo build with an engine already built
# that way; FILEID_RUST_TARGET_DIR sets cargo's target folder;
# FILEID_SWIFT_BUILD_ARGS adds arguments (for example --skip-update) to the
# FileIDAppleKit build. FILEID_APP_STORE=1
# (testflight.sh) marks the sandboxed variant; the bundle layout is the same.
#
# The engine runs every model on tract, linked into fileidd: no ONNX Runtime
# library is bundled, and the script refuses a bundle that contains one.

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
[ -x "$BUILD_DIR/FileID" ] || { echo "❌ $BUILD_DIR/FileID missing — build first"; exit 1; }

# Run-path entries that point outside the bundle (the Xcode toolchain, a Metal
# toolchain) resolve nothing in a shipped app; drop every absolute one except
# /usr/lib and /System, and fail if one remains.
foreign_rpaths() {
    otool -l "$1" 2>/dev/null | awk '$1=="cmd"{c=$2} c=="LC_RPATH" && $1=="path"{print $2}' \
        | grep -E '^/' | grep -vE '^(/usr/lib|/System)(/|$)' || true
}
strip_foreign_rpaths() {
    local rpath
    while IFS= read -r rpath; do
        [ -n "$rpath" ] && install_name_tool -delete_rpath "$rpath" "$1"
    done < <(foreign_rpaths "$1")
    return 0
}
bundle_macho_files() {
    local file
    while IFS= read -r file; do
        file "$file" | grep -q 'Mach-O' && echo "$file"
    done < <(find "$CONTENTS/MacOS" "$CONTENTS/Frameworks" -type f 2>/dev/null)
    return 0
}

rm -rf "$APP_BUNDLE"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"

cp "$BUILD_DIR/FileID"       "$CONTENTS/MacOS/FileID"
chmod +x "$CONTENTS/MacOS/FileID"

shopt -s nullglob
for resource_bundle in "$BUILD_DIR/"*.bundle; do
    cp -R "$resource_bundle" "$CONTENTS/Resources/"
done
shopt -u nullglob

mkdir -p "$CONTENTS/Frameworks"
xcrun swift-stdlib-tool --copy --platform macosx \
    --scan-executable "$CONTENTS/MacOS/FileID" \
    --destination "$CONTENTS/Frameworks"
if ! otool -l "$CONTENTS/MacOS/FileID" | grep -F 'path @executable_path/../Frameworks' >/dev/null; then
    install_name_tool -add_rpath '@executable_path/../Frameworks' "$CONTENTS/MacOS/FileID"
fi
strip_foreign_rpaths "$CONTENTS/MacOS/FileID"

# MLX first loads a colocated mlx.metallib beside the engine binary (fileidd).
if [ -f "$METALLIB_CACHE" ]; then
    cp "$METALLIB_CACHE" "$CONTENTS/MacOS/mlx.metallib"
else
    echo "⚠️  $METALLIB_CACHE missing — Deep Analyze will fail at runtime."
    echo "   Run bash run.sh once on a Mac with Xcode + Metal Toolchain to build it."
fi

SWIFT_SCRATCH="$(dirname "$BUILD_DIR")"
APPLE_KIT="$BUILD_DIR/libFileIDAppleKit.dylib"
read -r -a SWIFT_EXTRA_ARGS <<< "${FILEID_SWIFT_BUILD_ARGS:-}"
# The App Store variant builds every module with the same flag (testflight.sh builds the app so).
[ "${FILEID_APP_STORE:-0}" = "1" ] && SWIFT_EXTRA_ARGS+=(-Xswiftc -DFILEID_APP_STORE)
swift build --package-path "$PROJECT_DIR" --scratch-path "$SWIFT_SCRATCH" \
    -c "$BUILD_CONFIGURATION" --product FileIDAppleKit ${SWIFT_EXTRA_ARGS[@]+"${SWIFT_EXTRA_ARGS[@]}"}
[ -f "$APPLE_KIT" ] || { echo "❌ $APPLE_KIT missing after build"; exit 1; }
RUST_ENGINE="${FILEID_RUST_ENGINE_BINARY:-}"
if [ -z "$RUST_ENGINE" ]; then
    RUST_TARGET_DIR="${FILEID_RUST_TARGET_DIR:-$SWIFT_SCRATCH/rust-engine}"
    CARGO_PROFILE_ARGS=()
    [ "$BUILD_CONFIGURATION" = "release" ] && CARGO_PROFILE_ARGS=(--release)
    (cd "$PROJECT_DIR/../windows/src/engine" && \
        LIBRARY_PATH="$BUILD_DIR" CARGO_TARGET_DIR="$RUST_TARGET_DIR" \
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
cp "$RUST_ENGINE" "$CONTENTS/MacOS/fileidd"
chmod +x "$CONTENTS/MacOS/fileidd"
cp "$APPLE_KIT" "$CONTENTS/Frameworks/libFileIDAppleKit.dylib"
xcrun swift-stdlib-tool --copy --platform macosx \
    --scan-executable "$CONTENTS/Frameworks/libFileIDAppleKit.dylib" \
    --destination "$CONTENTS/Frameworks"
strip_foreign_rpaths "$CONTENTS/Frameworks/libFileIDAppleKit.dylib"
strip_foreign_rpaths "$CONTENTS/MacOS/fileidd"
# MLX looks for mlx.metallib beside the binary that contains it, which for
# fileidd is libFileIDAppleKit.dylib (Stage 7 VLM sessions).
if [ -f "$METALLIB_CACHE" ]; then
    cp "$METALLIB_CACHE" "$CONTENTS/Frameworks/mlx.metallib"
else
    echo "⚠️  $METALLIB_CACHE missing — fileidd's MLX models will fail at runtime."
fi
if ! otool -l "$CONTENTS/MacOS/fileidd" | grep -F 'path @executable_path/../Frameworks' >/dev/null; then
    install_name_tool -add_rpath '@executable_path/../Frameworks' "$CONTENTS/MacOS/fileidd"
fi
# Ad-hoc signatures keep dev bundles launchable after install_name_tool;
# release.sh re-signs everything with the Developer ID.
for code in "$CONTENTS/MacOS/mlx.metallib" "$CONTENTS/Frameworks/mlx.metallib" "$CONTENTS/Frameworks/libFileIDAppleKit.dylib" "$CONTENTS/MacOS/fileidd" "$CONTENTS/MacOS/FileID"; do
    [ -f "$code" ] && codesign --force --sign - "$code" >/dev/null
done
echo "🦀 Bundled the shared Rust engine as Contents/MacOS/fileidd"

while IFS= read -r macho; do
    leftover="$(foreign_rpaths "$macho")"
    if [ -n "$leftover" ]; then
        echo "❌ $macho keeps run-path entries outside the bundle: $leftover"; exit 1
    fi
done < <(bundle_macho_files)

# No ONNX Runtime ships: not as a bundled library, and not linked into the engine.
if [ -n "$(find "$APP_BUNDLE" -iname '*onnxruntime*' 2>/dev/null)" ]; then
    echo "❌ the bundle contains an ONNX Runtime library; the engine runs its models on tract"; exit 1
fi
if otool -L "$CONTENTS/MacOS/fileidd" | tail -n +2 | grep -iF 'onnxruntime' >/dev/null; then
    echo "❌ fileidd links an ONNX Runtime library"; exit 1
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
    <key>NSDesktopFolderUsageDescription</key><string>FileID reads this folder to tag and find your files, and moves or renames files only when you ask.</string>
    <key>NSDocumentsFolderUsageDescription</key><string>FileID reads this folder to tag and find your files, and moves or renames files only when you ask.</string>
    <key>NSDownloadsFolderUsageDescription</key><string>FileID reads this folder to tag and find your files, and moves or renames files only when you ask.</string>
    <key>NSRemovableVolumesUsageDescription</key><string>FileID reads this drive to tag and find your files, and moves or renames files only when you ask.</string>
    <key>NSNetworkVolumesUsageDescription</key><string>FileID reads this drive to tag and find your files, and moves or renames files only when you ask.</string>
    <key>NSSpeechRecognitionUsageDescription</key><string>FileID transcribes audio and video on-device to make recordings searchable. Nothing leaves your Mac.</string>
</dict>
</plist>
PLIST

cp "$PROJECT_DIR/Resources/FileID.icns" "$CONTENTS/Resources/FileID.icns"
# Apache-2.0 + third-party licence texts (Settings -> Advanced -> Acknowledgements opens them).
for notice in LICENSE THIRD_PARTY_NOTICES.txt; do
    [ -f "$PROJECT_DIR/../../$notice" ] || { echo "❌ $PROJECT_DIR/../../$notice missing"; exit 1; }
done
cp "$PROJECT_DIR/../../LICENSE" "$CONTENTS/Resources/LICENSE.txt"
cp "$PROJECT_DIR/../../THIRD_PARTY_NOTICES.txt" "$CONTENTS/Resources/THIRD_PARTY_NOTICES.txt"
# Required-reason API declarations and "no tracking, no collected data" (App Store privacy manifest).
cp "$PROJECT_DIR/Resources/PrivacyInfo.xcprivacy" "$CONTENTS/Resources/PrivacyInfo.xcprivacy"
plutil -lint "$CONTENTS/Resources/PrivacyInfo.xcprivacy" >/dev/null
echo "✅ Assembled $APP_BUNDLE (v${VERSION}, build ${BUILD_NUM})"
