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
DIST_DIR="$PLATFORM_DIR/dist/fileid"

PROFILE="${PROFILE:-release}"
case "$PROFILE" in
    release) PROFILE_ARGS=(--release); PROFILE_DIR="release" ;;
    debug)   PROFILE_ARGS=();          PROFILE_DIR="debug" ;;
    *)       PROFILE_ARGS=(--profile "$PROFILE"); PROFILE_DIR="$PROFILE" ;;
esac

step()  { printf "\033[36m>> %s\033[0m\n" "$*"; }
ok()    { printf "  \033[32m[OK]\033[0m %s\n" "$*"; }
fail()  { printf "  \033[31m[X]\033[0m %s\n" "$*" >&2; exit 1; }

step "Building GTK app + shared engine ($PROFILE)"
( cd "$PLATFORM_DIR" && \
    cargo build --locked "${PROFILE_ARGS[@]}" -p fileid-linux --bin fileid-linux && \
    cargo build --locked "${PROFILE_ARGS[@]}" -p fileid-engine --bin FileIDEngine \
) || fail "Linux app/engine build failed"

ENGINE_BIN="$PLATFORM_DIR/target/$PROFILE_DIR/FileIDEngine"
APP_BIN="$PLATFORM_DIR/target/$PROFILE_DIR/fileid-linux"
[[ -x "$ENGINE_BIN" ]] || fail "engine binary not found at $ENGINE_BIN"
[[ -x "$APP_BIN" ]] || fail "app binary not found at $APP_BIN"
ok  "engine: $ENGINE_BIN"
ok  "app: $APP_BIN"

step "Staging into $DIST_DIR"
rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"
install -m 0755 "$APP_BIN"    "$DIST_DIR/fileid-linux"
install -m 0755 "$ENGINE_BIN" "$DIST_DIR/FileIDEngine"
install -m 0644 "$PLATFORM_DIR/data/io.github.fileid.FileID.desktop" "$DIST_DIR/"
install -m 0644 "$PLATFORM_DIR/data/io.github.fileid.FileID.metainfo.xml" "$DIST_DIR/"
install -m 0644 "$PLATFORM_DIR/data/io.github.fileid.FileID.svg" "$DIST_DIR/"
install -m 0644 "$REPO_ROOT/LICENSE" "$DIST_DIR/LICENSE"
python3 "$REPO_ROOT/shared/scripts/check_binary_privacy.py" \
    "$DIST_DIR/fileid-linux" \
    "$DIST_DIR/FileIDEngine" || fail "binary privacy gate failed"
ok  "staged"

step "Done."
echo "Run: $DIST_DIR/fileid-linux"
