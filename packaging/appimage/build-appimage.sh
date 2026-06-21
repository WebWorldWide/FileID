#!/usr/bin/env bash
# Build a portable FileID-x86_64.AppImage (SECONDARY channel; Flatpak is primary).
#
# WHY an old glibc baseline: an AppImage is forward-compatible, not backward —
# a binary built against glibc 2.39 (Ubuntu 24.04) will NOT start on an older
# host. Build on the OLDEST glibc you want to support. ubuntu-20.04 (glibc 2.31)
# is the recommended baseline; that covers everything from ~2020 onward.
#
#   *** caveat: GTK 4.14 / libadwaita 1.5 dev packages are NOT in the
#   ubuntu-20.04 archive. To build on the old-glibc baseline you must install
#   GTK4/libadwaita from a backports PPA or build them — otherwise build on the
#   oldest distro that ships GTK 4.14 (Ubuntu 22.04 / glibc 2.35) and accept a
#   higher floor. This trade-off needs Linux-side iteration; see README.md. ***
#
# Bundles: fileid-linux + FileIDEngine + libonnxruntime.so + the whole GTK4 /
# libadwaita / glib runtime (via linuxdeploy-plugin-gtk).
#
# Usage (from repo root):  ./packaging/appimage/build-appimage.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="${REPO_ROOT}/packaging/appimage/build"
APPDIR="${WORK}/AppDir"
TOOLS="${WORK}/tools"
APP_ID="io.github.fileid.FileID"

mkdir -p "${TOOLS}"
rm -rf "${APPDIR}"
mkdir -p "${APPDIR}"

# --- 1. Fetch linuxdeploy + the GTK plugin (pinned to continuous releases) ----
fetch() {  # fetch <url> <dest>
  local url="$1" dest="$2"
  if [ ! -x "${dest}" ]; then
    echo "Downloading ${url}"
    curl -fsSL "${url}" -o "${dest}"
    chmod +x "${dest}"
  fi
}
LD="${TOOLS}/linuxdeploy-x86_64.AppImage"
LD_GTK="${TOOLS}/linuxdeploy-plugin-gtk.sh"
fetch "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage" "${LD}"
fetch "https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh" "${LD_GTK}"

# --- 2. Build the workspace (release) -----------------------------------------
echo "Building fileid-linux + FileIDEngine (release)…"
( cd "${REPO_ROOT}/platforms/linux" \
    && cargo build --release --locked \
    && cargo build --release --locked -p fileid-engine --bin FileIDEngine )

TARGET="${REPO_ROOT}/platforms/linux/target/release"

# --- 3. Stage AppDir ----------------------------------------------------------
install -Dm755 "${TARGET}/fileid-linux" "${APPDIR}/usr/bin/fileid-linux"
install -Dm755 "${TARGET}/FileIDEngine" "${APPDIR}/usr/bin/FileIDEngine"

# The dlopen'd ONNX Runtime (ort load-dynamic). Same sourcing caveat as Flatpak:
# `download-binaries` fetched it into target/ during the build — find + bundle
# it, and set ORT_DYLIB_PATH via the AppRun wrapper below.
ORT_SO="$(find "${REPO_ROOT}/platforms/linux/target" -name 'libonnxruntime.so*' -type f -print -quit || true)"
if [ -n "${ORT_SO}" ]; then
  install -Dm755 "${ORT_SO}" "${APPDIR}/usr/lib/libonnxruntime.so"
else
  echo "WARNING: libonnxruntime.so not found — ML inference will fail until it is bundled (Linux-side verification needed)."
fi

# Desktop + icon + metainfo (reuse platforms/linux/data/ — single source).
DATA="${REPO_ROOT}/platforms/linux/data"
install -Dm644 "${DATA}/${APP_ID}.desktop"      "${APPDIR}/usr/share/applications/${APP_ID}.desktop"
install -Dm644 "${DATA}/${APP_ID}.metainfo.xml" "${APPDIR}/usr/share/metainfo/${APP_ID}.metainfo.xml"
install -Dm644 "${DATA}/${APP_ID}.svg"          "${APPDIR}/usr/share/icons/hicolor/scalable/apps/${APP_ID}.svg"

# --- 4. AppRun hook: point load-dynamic ONNX Runtime at the bundled .so -------
# Files in AppDir/apprun-hooks/*.sh are sourced by linuxdeploy's generated
# AppRun (which the GTK plugin extends with its own GTK/GdkPixbuf env). The
# engine binary is found next to fileid-linux automatically (locate_engine_binary
# search order #2), so only ORT_DYLIB_PATH needs setting.
mkdir -p "${APPDIR}/apprun-hooks"
cat > "${APPDIR}/apprun-hooks/ort-dylib-path.sh" <<'HOOK'
export ORT_DYLIB_PATH="${APPDIR}/usr/lib/libonnxruntime.so"
HOOK

# --- 5. Bundle with linuxdeploy + GTK plugin ----------------------------------
# The GTK plugin pulls in GTK4, libadwaita, GdkPixbuf loaders, GLib schemas, the
# Adwaita icon theme + GIO modules and writes the AppRun + GTK env hooks.
export DEPLOY_GTK_VERSION=4
export OUTPUT="FileID-x86_64.AppImage"
export LDAI_OUTPUT="${OUTPUT}"

( cd "${WORK}" && "${LD}" --appimage-extract-and-run \
    --appdir "${APPDIR}" \
    --plugin gtk \
    --desktop-file "${APPDIR}/usr/share/applications/${APP_ID}.desktop" \
    --icon-file "${APPDIR}/usr/share/icons/hicolor/scalable/apps/${APP_ID}.svg" \
    --output appimage )

echo "Done: ${WORK}/${OUTPUT}"
