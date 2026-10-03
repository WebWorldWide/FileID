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
step "Privacy gate — checking telemetry markers"
python3 "$REPO_ROOT/shared/scripts/check_binary_privacy.py" "$DIST_DIR/fileid-linux" "$DIST_DIR/FileIDEngine" || fail "privacy gate failed"
ok  "privacy gate clean"

step "Done."
echo "Run: $DIST_DIR/fileid-linux"
