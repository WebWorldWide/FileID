#!/usr/bin/env bash
# Install the ONNX Runtime dynamic library the FileID engine loads on macOS.
#
# WHY THIS EXISTS
#   The shared Rust engine is built with `ort`'s `load-dynamic` feature, so it
#   `dlopen`s `libonnxruntime.dylib` at run time. On macOS arm64 `ort`'s
#   `download-binaries` ships ONLY a STATIC `libonnxruntime.a` (verified: pyke's
#   `aarch64-apple-darwin` tarball contains no dylib), so a load-dynamic build
#   has no runtime library unless you install one. This script installs the
#   official, MIT-licensed ONNX Runtime macOS arm64 dylib where the engine looks.
#
#   Equivalent to `fileid runtime install`; provided for users not using the CLI.
#   `brew install onnxruntime` also works (the engine probes /opt/homebrew/lib).
#
# VERSION
#   `ort 2.0.0-rc.10` targets ONNX Runtime 1.22.0 and hard-panics if the loaded
#   dylib's minor version is < 22, so we install 1.22.0.
#
# LICENSE / EGRESS
#   ONNX Runtime is MIT-licensed (microsoft/onnxruntime). The download is from
#   github.com (Microsoft's official release) — a user-initiated, one-time fetch.
#   See shared/docs/RUNTIME.md and shared/docs/DECISIONS.md.
#
# USAGE
#   shared/scripts/install_onnxruntime_macos.sh [--force]
#
# Override the destination with FILEID_RUNTIME_DIR; override the version with
# ORT_VERSION; pin the archive hash with EXPECTED_SHA256 (recommended — see
# RUNTIME.md). With EXPECTED_SHA256 unset the script prints the hash it computed
# so you can pin it.
set -euo pipefail

ORT_VERSION="${ORT_VERSION:-1.22.0}"
ARCH="$(uname -m)"
OS="$(uname -s)"

if [[ "$OS" != "Darwin" ]]; then
  echo "error: this script is for macOS only (saw $OS)." >&2
  echo "  On Windows/Linux the ONNX Runtime is provided by the platform." >&2
  exit 1
fi
if [[ "$ARCH" != "arm64" ]]; then
  echo "warning: this script targets Apple-silicon (arm64); you are on $ARCH." >&2
  echo "  Adjust ARCHIVE below for x86_64 (onnxruntime-osx-x86_64-...) if needed." >&2
fi

# Official, MIT-licensed ONNX Runtime release. Ships lib/libonnxruntime.<ver>.dylib.
ARCHIVE="onnxruntime-osx-arm64-${ORT_VERSION}.tgz"
URL="https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/${ARCHIVE}"

# Pin the archive SHA256 for integrity. TODO(runtime-dylib): paste the value the
# script prints on first run (or from RUNTIME.md) to enforce verification.
EXPECTED_SHA256="${EXPECTED_SHA256:-}"

# Where the engine looks: <state-root>/runtime, with state-root =
# $XDG_DATA_HOME/FileID or ~/.local/share/FileID (matches paths::runtime_dir()).
default_state_root() {
  if [[ -n "${XDG_DATA_HOME:-}" ]]; then
    echo "${XDG_DATA_HOME}/FileID"
  else
    echo "${HOME}/.local/share/FileID"
  fi
}
RUNTIME_DIR="${FILEID_RUNTIME_DIR:-$(default_state_root)/runtime}"
DEST="${RUNTIME_DIR}/libonnxruntime.dylib"

FORCE=0
[[ "${1:-}" == "--force" ]] && FORCE=1

if [[ -f "$DEST" && "$FORCE" -ne 1 ]]; then
  echo "ONNX Runtime already installed: $DEST"
  echo "  Pass --force to reinstall."
  exit 0
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "Downloading ONNX Runtime ${ORT_VERSION} (macOS arm64, MIT)…"
echo "  $URL"
curl -fL --proto '=https' --tlsv1.2 -o "${TMP}/${ARCHIVE}" "$URL"

GOT_SHA="$(shasum -a 256 "${TMP}/${ARCHIVE}" | awk '{print $1}')"
if [[ -n "$EXPECTED_SHA256" ]]; then
  if [[ "$GOT_SHA" != "$EXPECTED_SHA256" ]]; then
    echo "error: SHA256 mismatch for ${ARCHIVE}" >&2
    echo "  expected $EXPECTED_SHA256" >&2
    echo "  got      $GOT_SHA" >&2
    exit 1
  fi
  echo "SHA256 verified: $GOT_SHA"
else
  echo "warning: EXPECTED_SHA256 is unset — proceeding WITHOUT verification." >&2
  echo "  Pin this value (in the script or via EXPECTED_SHA256) to enforce it:" >&2
  echo "  $GOT_SHA" >&2
fi

echo "Extracting…"
tar -xzf "${TMP}/${ARCHIVE}" -C "$TMP"

# Find the versioned dylib (e.g. lib/libonnxruntime.1.22.0.dylib) and copy it
# (dereferencing any symlink) to the engine's expected libonnxruntime.dylib.
SRC="$(find "$TMP" -type f -name 'libonnxruntime*.dylib' | head -n 1)"
if [[ -z "$SRC" ]]; then
  echo "error: no libonnxruntime*.dylib found inside ${ARCHIVE}." >&2
  exit 1
fi

mkdir -p "$RUNTIME_DIR"
cp -L "$SRC" "$DEST"
chmod 0644 "$DEST"

echo "✓ Installed: $DEST"
echo "  (from $(basename "$SRC"))"
echo "Now run a full AI scan, e.g.:  fileid scan ~/Pictures --models"
echo "Verify with:                   fileid runtime status"
