#!/usr/bin/env bash
# FileID Linux — dev build script.
#
# Builds:
#   1. The shared Rust engine (platforms/windows/src/engine/) → Linux binary
#   2. The GTK4 + libadwaita app (platforms/linux/src/app/)
#
# By default stages into platforms/linux/dist/fileid/ with the engine binary placed
# next to the app binary so EngineClient::locate_engine_binary() finds it.
#
# Requires (Debian/Ubuntu): build-essential libgtk-4-dev libadwaita-1-dev
# Requires (Fedora):        gcc gtk4-devel libadwaita-devel
# Requires (Arch):          base-devel gtk4 libadwaita

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLATFORM_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$PLATFORM_DIR/../.." && pwd)"
ENGINE_DIR="$REPO_ROOT/platforms/windows/src/engine"
APP_DIR="$PLATFORM_DIR/src/app"
DIST_DIR="${FILEID_LINUX_DIST_DIR:-$PLATFORM_DIR/dist/fileid}"

PROFILE="${PROFILE:-release}"

step()  { printf "\033[36m>> %s\033[0m\n" "$*"; }
ok()    { printf "  \033[32m[OK]\033[0m %s\n" "$*"; }
fail()  { printf "  \033[31m[X]\033[0m %s\n" "$*" >&2; exit 1; }
case "$PROFILE" in
  release) BUILD_ARGS=(--release) ;;
  debug) BUILD_ARGS=() ;;
  *) fail "PROFILE must be release or debug" ;;
esac

ENGINE_TARGET_DIR="${CARGO_TARGET_DIR:-$ENGINE_DIR/target}"
APP_TARGET_DIR="${CARGO_TARGET_DIR:-$PLATFORM_DIR/target}"
[[ "$ENGINE_TARGET_DIR" = /* ]] || ENGINE_TARGET_DIR="$ENGINE_DIR/$ENGINE_TARGET_DIR"
[[ "$APP_TARGET_DIR" = /* ]] || APP_TARGET_DIR="$APP_DIR/$APP_TARGET_DIR"

step "Building shared engine ($PROFILE)"
( cd "$ENGINE_DIR" && cargo build "${BUILD_ARGS[@]}" ) || fail "engine build failed"
ENGINE_BIN="$ENGINE_TARGET_DIR/$PROFILE/FileIDEngine"
[[ -x "$ENGINE_BIN" ]] || fail "engine binary not found at $ENGINE_BIN"
ok  "engine: $ENGINE_BIN"

step "Building GTK app ($PROFILE)"
( cd "$APP_DIR" && cargo build "${BUILD_ARGS[@]}" ) || fail "app build failed"
APP_BIN="$APP_TARGET_DIR/$PROFILE/fileid-linux"
[[ -x "$APP_BIN" ]] || fail "app binary not found at $APP_BIN"
ok  "app: $APP_BIN"

step "Staging into $DIST_DIR"
mkdir -p "$DIST_DIR"
cp -f "$APP_BIN"    "$DIST_DIR/fileid-linux"
cp -f "$ENGINE_BIN" "$DIST_DIR/FileIDEngine"
cp -f "$PLATFORM_DIR/data/io.github.fileid.FileID.desktop" "$DIST_DIR/"
FONT_SOURCE="$REPO_ROOT/platforms/apple/app/Sources/FileID/Resources/Fonts"
mkdir -p "$DIST_DIR/fonts"
cp -f "$FONT_SOURCE"/*.ttf "$FONT_SOURCE"/*-OFL.txt "$DIST_DIR/fonts/" || fail "font staging failed"
if [[ -n "${FILEID_LINUX_MEDIA_DIR:-}" ]]; then
  media_source="$FILEID_LINUX_MEDIA_DIR"
  [[ "$media_source" = /* && -d "$media_source" && ! -L "$media_source" ]] || fail "FILEID_LINUX_MEDIA_DIR must be an absolute regular directory"
  media_source="$(realpath -e "$media_source")"
  [[ -z "$(find "$media_source" -mindepth 1 -type d -print -quit)" ]] || fail "media runtime cannot contain subdirectories"
  while IFS= read -r -d '' link; do
    target="$(readlink "$link")"
    [[ "$target" != */* && -f "$media_source/$target" && "$(realpath -e "$link")" == "$media_source/"* ]] || fail "media runtime contains an external or broken link"
  done < <(find "$media_source" -mindepth 1 -type l -print0)
  while IFS= read -r -d '' entry; do
    case "$(basename "$entry")" in
      ffmpeg|ffprobe|SHA256SUMS|FFmpeg-LGPL-2.1.txt|OpenH264-BSD-2-Clause.txt|BUILD-AND-SOURCE.txt|ffmpeg-9.0.2.tar.xz|ffmpeg-9.0.2.tar.xz.asc|openh264-2.6.0.tar.gz|libavcodec.so*|libavdevice.so*|libavfilter.so*|libavformat.so*|libavutil.so*|libopenh264.so*|libswresample.so*|libswscale.so*) ;;
      *) fail "media runtime contains an unexpected file" ;;
    esac
  done < <(find "$media_source" -mindepth 1 -maxdepth 1 -print0)
  for required in ffmpeg ffprobe SHA256SUMS FFmpeg-LGPL-2.1.txt OpenH264-BSD-2-Clause.txt BUILD-AND-SOURCE.txt ffmpeg-9.0.2.tar.xz openh264-2.6.0.tar.gz; do
    [[ -f "$media_source/$required" ]] || fail "media runtime is missing $required"
  done
  while IFS= read -r line; do
    [[ "$line" =~ ^[0-9a-f]{64}\ \ (\./)?([A-Za-z0-9._-]+)$ ]] || fail "invalid media checksum entry"
    item="${BASH_REMATCH[2]}"
    case "$item" in ffmpeg|ffprobe|libavcodec.so*|libavdevice.so*|libavfilter.so*|libavformat.so*|libavutil.so*|libopenh264.so*|libswresample.so*|libswscale.so*|ffmpeg-9.0.2.tar.xz|openh264-2.6.0.tar.gz) ;; *) fail "unexpected media checksum entry" ;; esac
    [[ -f "$media_source/$item" ]] || fail "media checksum entry is missing its file"
  done < "$media_source/SHA256SUMS"
  for binary in "$media_source"/ffmpeg "$media_source"/ffprobe "$media_source"/lib*.so*; do
    [[ -f "$binary" ]] || continue
    grep -Fq "  $(basename "$binary")" "$media_source/SHA256SUMS" ||
      grep -Fq "  ./$(basename "$binary")" "$media_source/SHA256SUMS" || fail "media binary is not checksummed"
  done
  ( cd "$media_source" && sha256sum --check --status SHA256SUMS ) || fail "media runtime checksum mismatch"
  config="$(LD_LIBRARY_PATH="$media_source" "$media_source/ffmpeg" -version 2>/dev/null)" || fail "media runtime could not start"
  [[ "$config" == *"version 9.0.2"* && "$config" == *"--enable-libopenh264"* && "$config" == *"--disable-network"* && "$config" != *"--enable-gpl"* && "$config" != *"--enable-nonfree"* ]] || fail "media runtime is not the approved LGPL configuration"
  [[ ! -e "$DIST_DIR/media" && ! -L "$DIST_DIR/media" ]] || fail "media staging already exists; use a fresh FILEID_LINUX_DIST_DIR"
  mkdir "$DIST_DIR/media"
  cp -a "$media_source/." "$DIST_DIR/media/"
  ( cd "$DIST_DIR/media" && sha256sum --check --status SHA256SUMS ) || fail "staged media checksum mismatch"
  mapfile -d '' media_binaries < <(find "$DIST_DIR/media" -type f \( -name 'ffmpeg' -o -name 'ffprobe' -o -name '*.so*' \) -print0)
  python3 "$REPO_ROOT/shared/scripts/check_binary_privacy.py" "${media_binaries[@]}" || fail "media privacy gate failed"
fi
step "Privacy gate — checking telemetry markers"
python3 "$REPO_ROOT/shared/scripts/check_binary_privacy.py" "$DIST_DIR/fileid-linux" "$DIST_DIR/FileIDEngine" || fail "privacy gate failed"
ok  "privacy gate clean"

step "Done."
echo "Run: $DIST_DIR/fileid-linux"
