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
# Bundles: fileid-linux + a self-contained FileIDEngine + the whole GTK4 /
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

# --- 1. Fetch integrity-pinned linuxdeploy + GTK plugin -----------------------
fetch() {  # fetch <url> <dest> <sha256>
  local url="$1" dest="$2" sha256="$3"
  if [ -e "${dest}" ] && ! printf '%s  %s\n' "${sha256}" "${dest}" |
      sha256sum --check --status; then
    echo "Discarding stale or invalid cached tool: ${dest}"
    rm -f "${dest}"
  fi
  if [ ! -x "${dest}" ]; then
    echo "Downloading ${url}"
    local tmp="${dest}.tmp.$$"
    curl --fail --show-error --location --retry 3 --retry-all-errors \
      "${url}" -o "${tmp}"
    printf '%s  %s\n' "${sha256}" "${tmp}" | sha256sum --check --status || {
      rm -f "${tmp}"
      echo "ERROR: SHA256 mismatch for ${url}" >&2
      exit 1
    }
    mv -f "${tmp}" "${dest}"
    chmod +x "${dest}"
  fi
  printf '%s  %s\n' "${sha256}" "${dest}" | sha256sum --check --status || {
    echo "ERROR: cached tool failed SHA256 verification: ${dest}" >&2
    exit 1
  }
}
LD="${TOOLS}/linuxdeploy-x86_64.AppImage"
LD_GTK="${TOOLS}/linuxdeploy-plugin-gtk.sh"
fetch \
  "https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage" \
  "${LD}" \
  "c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d"
fetch \
  "https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/7a3fbc31a9e5075073ff8790f26effbac5f84453/linuxdeploy-plugin-gtk.sh" \
  "${LD_GTK}" \
  "b0f4cbc684a0103a9651f0955b635eaea0096b3a66c0f5a2c2aa337960375171"

# --- 2. Build the workspace (release) -----------------------------------------
echo "Building fileid-linux + FileIDEngine (release)…"
( cd "${REPO_ROOT}/platforms/linux" \
    && cargo build --release --locked \
    && cargo build --release --locked -p fileid-engine --bin FileIDEngine )

TARGET="${REPO_ROOT}/platforms/linux/target/release"

# --- 3. Stage AppDir ----------------------------------------------------------
install -Dm755 "${TARGET}/fileid-linux" "${APPDIR}/usr/bin/fileid-linux"
install -Dm755 "${TARGET}/FileIDEngine" "${APPDIR}/usr/bin/FileIDEngine"

# Desktop + icon + metainfo (reuse platforms/linux/data/ — single source).
DATA="${REPO_ROOT}/platforms/linux/data"
install -Dm644 "${DATA}/${APP_ID}.desktop"      "${APPDIR}/usr/share/applications/${APP_ID}.desktop"
install -Dm644 "${DATA}/${APP_ID}.metainfo.xml" "${APPDIR}/usr/share/metainfo/${APP_ID}.metainfo.xml"
install -Dm644 "${DATA}/${APP_ID}.svg"          "${APPDIR}/usr/share/icons/hicolor/scalable/apps/${APP_ID}.svg"

# linuxdeploy's root-icon (.DirIcon) step rejects a scalable-only theme on
# some continuous builds — give it a 256px raster fallback when a
# rasterizer is available.
PNG_DIR="${APPDIR}/usr/share/icons/hicolor/256x256/apps"
if command -v rsvg-convert >/dev/null 2>&1; then
  mkdir -p "${PNG_DIR}"
  rsvg-convert -w 256 -h 256 "${DATA}/${APP_ID}.svg" -o "${PNG_DIR}/${APP_ID}.png"
elif command -v convert >/dev/null 2>&1; then
  mkdir -p "${PNG_DIR}"
  convert -background none -resize 256x256 "${DATA}/${APP_ID}.svg" "${PNG_DIR}/${APP_ID}.png"
fi

# --- 4. Bundle with linuxdeploy + GTK plugin ----------------------------------
# The GTK plugin pulls in GTK4, libadwaita, GdkPixbuf loaders, GLib schemas, the
# Adwaita icon theme + GIO modules and writes the AppRun + GTK env hooks.
export DEPLOY_GTK_VERSION=4
export OUTPUT="FileID-x86_64.AppImage"
export LDAI_OUTPUT="${OUTPUT}"

# Prefer handing linuxdeploy the raster: some continuous builds fail the
# desktop-root icon lookup when given only an SVG. The icon must come from
# OUTSIDE the AppDir (an in-AppDir path can be deduped into an empty icon
# list), and pre-placing the root icons short-circuits the flaky lookup.
ICON_ARG="${DATA}/${APP_ID}.svg"
if [ -f "${PNG_DIR}/${APP_ID}.png" ]; then
  ICON_ARG="${WORK}/${APP_ID}.png"
  cp "${PNG_DIR}/${APP_ID}.png" "${ICON_ARG}"
  cp "${PNG_DIR}/${APP_ID}.png" "${APPDIR}/${APP_ID}.png"
  cp "${PNG_DIR}/${APP_ID}.png" "${APPDIR}/.DirIcon"
fi
( cd "${WORK}" && "${LD}" --appimage-extract-and-run \
    --appdir "${APPDIR}" \
    --plugin gtk \
    --desktop-file "${APPDIR}/usr/share/applications/${APP_ID}.desktop" \
    --icon-file "${ICON_ARG}" \
    --output appimage )

(
  cd "${WORK}"
  sha256sum "${OUTPUT}" > "${OUTPUT}.sha256"
  sha256sum --check "${OUTPUT}.sha256"
)

echo "Done: ${WORK}/${OUTPUT}"
echo "SHA256: ${WORK}/${OUTPUT}.sha256"
