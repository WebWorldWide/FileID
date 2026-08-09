#!/usr/bin/env bash
# FileID Linux — dev build script.
#
# Builds:
#   1. The shared Rust engine (platforms/windows/src/engine/) → Linux binary
#   2. The GTK4 + libadwaita app (platforms/linux/src/app/)
#
# Stages into platforms/linux/dist/fileid/ with the engine binary placed
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
DIST_DIR="$PLATFORM_DIR/dist/fileid"

PROFILE="${PROFILE:-release}"
TARGET_DIR_FLAG=""

step()  { printf "\033[36m>> %s\033[0m\n" "$*"; }
ok()    { printf "  \033[32m[OK]\033[0m %s\n" "$*"; }
fail()  { printf "  \033[31m[X]\033[0m %s\n" "$*" >&2; exit 1; }

step "Building shared engine ($PROFILE)"
( cd "$ENGINE_DIR" && cargo build --$PROFILE ) || fail "engine build failed"
ENGINE_BIN="$ENGINE_DIR/target/$PROFILE/FileIDEngine"
[[ -x "$ENGINE_BIN" ]] || fail "engine binary not found at $ENGINE_BIN"
ok  "engine: $ENGINE_BIN"

step "Building GTK app ($PROFILE)"
( cd "$APP_DIR" && cargo build --$PROFILE ) || fail "app build failed"
APP_BIN="$APP_DIR/target/$PROFILE/fileid-linux"
[[ -x "$APP_BIN" ]] || fail "app binary not found at $APP_BIN"
ok  "app: $APP_BIN"

step "Staging into $DIST_DIR"
mkdir -p "$DIST_DIR"
cp -f "$APP_BIN"    "$DIST_DIR/fileid-linux"
cp -f "$ENGINE_BIN" "$DIST_DIR/FileIDEngine"
cp -f "$PLATFORM_DIR/data/io.github.fileid.FileID.desktop" "$DIST_DIR/"
ok  "staged"

step "Done."
echo "Run: $DIST_DIR/fileid-linux"
