#!/usr/bin/env bash
# Install the ONNX Runtime dynamic library the FileID engine loads on macOS.
#
# WHY THIS EXISTS
#   The shared Rust engine is built with `ort`'s `load-dynamic` feature, so it
#   `dlopen`s `libonnxruntime.dylib` at run time. On macOS arm64 `ort`'s
#   `download-binaries` ships ONLY a STATIC `libonnxruntime.a` (verified: pyke's
#   `aarch64-apple-darwin` tarball contains no dylib), so a load-dynamic build
#   has no runtime library unless you install one. This script installs the
#   ONNX Runtime macOS arm64 dylib where the engine looks.
#
#   `brew install onnxruntime` is the simplest supported path (the engine probes
#   /opt/homebrew/lib). This script exists for self-hosted HuggingFace mirrors.
#
# VERSION
#   `ort 2.0.0-rc.10` targets ONNX Runtime 1.22.0 and hard-panics if the loaded
#   dylib's minor version is < 22, so we install 1.22.0.
#
# LICENSE / EGRESS
#   ONNX Runtime is MIT-licensed. To preserve FileID's network policy, this
#   script refuses to download unless FILEID_ORT_DYLIB_URL points at a
#   HuggingFace-hosted mirror. See shared/docs/RUNTIME.md.
#
# USAGE
#   shared/scripts/install_onnxruntime_macos.sh [--force]
#
# Override the destination with FILEID_RUNTIME_DIR. FILEID_ORT_DYLIB_URL must
# point to a HuggingFace-hosted mirror of the official archive. The arm64
# archive and extracted dylib hashes are pinned below; a different artifact is
# rejected unless both expected hashes are supplied explicitly.
set -euo pipefail

ORT_VERSION="${ORT_VERSION:-1.22.0}"
ARCH="$(uname -m)"
OS="$(uname -s)"

if [[ "$OS" != "Darwin" ]]; then
  echo "error: this script is for macOS only (saw $OS)." >&2
  echo "  On Windows/Linux the ONNX Runtime is provided by the platform." >&2
  exit 1
fi
case "$ARCH" in
  arm64)
    ARCHIVE="onnxruntime-osx-arm64-${ORT_VERSION}.tgz"
    DEFAULT_ARCHIVE_SHA256="cab6dcbd77e7ec775390e7b73a8939d45fec3379b017c7cb74f5b204c1a1cc07"
    DEFAULT_DYLIB_SHA256="2b885992d3d6fa4130d39ec84a80d7504ff52750027c547bb22c86165f19406a"
    ;;
  x86_64)
    ARCHIVE="onnxruntime-osx-x86_64-${ORT_VERSION}.tgz"
    DEFAULT_ARCHIVE_SHA256=""
    DEFAULT_DYLIB_SHA256=""
    ;;
  *)
    echo "error: unsupported macOS architecture: $ARCH" >&2
    exit 1
    ;;
esac

FORCE=0
case "$#" in
  0) ;;
  1)
    case "$1" in
      --force) FORCE=1 ;;
      --help|-h)
        echo "Usage: install_onnxruntime_macos.sh [--force]"
        exit 0
        ;;
      *) echo "error: unknown argument: $1" >&2; exit 2 ;;
    esac
    ;;
  *) echo "error: expected at most one argument (try --help)." >&2; exit 2 ;;
esac

# HuggingFace-hosted mirror. Ships lib/libonnxruntime.<ver>.dylib.
URL="${FILEID_ORT_DYLIB_URL:-}"
if [[ -z "$URL" ]]; then
  echo "error: FILEID_ORT_DYLIB_URL is required and must point at a HuggingFace-hosted ONNX Runtime .tgz." >&2
  echo "  Simplest install: brew install onnxruntime" >&2
  exit 1
fi
case "$URL" in
  https://huggingface.co/*|https://*.huggingface.co/*|https://hf.co/*|https://*.hf.co/*) ;;
  *)
    echo "error: runtime downloads must come from huggingface.co or hf.co: $URL" >&2
    exit 1
    ;;
esac

EXPECTED_SHA256="${EXPECTED_SHA256:-$DEFAULT_ARCHIVE_SHA256}"
EXPECTED_DYLIB_SHA256="${EXPECTED_DYLIB_SHA256:-$DEFAULT_DYLIB_SHA256}"
if [[ ! "$EXPECTED_SHA256" =~ ^[[:xdigit:]]{64}$ ]]; then
  echo "error: EXPECTED_SHA256 must be a pinned 64-character hex digest for $ARCHIVE." >&2
  exit 1
fi
if [[ ! "$EXPECTED_DYLIB_SHA256" =~ ^[[:xdigit:]]{64}$ ]]; then
  echo "error: EXPECTED_DYLIB_SHA256 must pin the extracted runtime dylib." >&2
  exit 1
fi

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

if [[ -f "$DEST" && "$FORCE" -ne 1 ]]; then
  INSTALLED_SHA="$(shasum -a 256 "$DEST" | awk '{print $1}')"
  if [[ "$(printf '%s' "$INSTALLED_SHA" | tr '[:upper:]' '[:lower:]')" == \
        "$(printf '%s' "$EXPECTED_DYLIB_SHA256" | tr '[:upper:]' '[:lower:]')" ]]; then
    echo "ONNX Runtime already installed and SHA256-verified: $DEST"
    exit 0
  fi
  echo "error: existing runtime failed SHA256 verification: $DEST" >&2
  echo "  expected $EXPECTED_DYLIB_SHA256" >&2
  echo "  got      $INSTALLED_SHA" >&2
  echo "  Re-run with --force to replace it." >&2
  exit 1
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "Downloading ONNX Runtime ${ORT_VERSION} (macOS ${ARCH}, MIT)…"
echo "  $URL"
curl --fail --location --proto '=https' --tlsv1.2 --retry 3 --retry-all-errors \
  -o "${TMP}/${ARCHIVE}" "$URL"

GOT_SHA="$(shasum -a 256 "${TMP}/${ARCHIVE}" | awk '{print $1}')"
if [[ "$(printf '%s' "$GOT_SHA" | tr '[:upper:]' '[:lower:]')" != \
      "$(printf '%s' "$EXPECTED_SHA256" | tr '[:upper:]' '[:lower:]')" ]]; then
  echo "error: SHA256 mismatch for ${ARCHIVE}" >&2
  echo "  expected $EXPECTED_SHA256" >&2
  echo "  got      $GOT_SHA" >&2
  exit 1
fi
echo "Archive SHA256 verified: $GOT_SHA"

echo "Extracting…"
tar -xzf "${TMP}/${ARCHIVE}" -C "$TMP"

# Find the versioned dylib (e.g. lib/libonnxruntime.1.22.0.dylib) and copy it
# (dereferencing any symlink) to the engine's expected libonnxruntime.dylib.
SRC="$(find "$TMP" -type f -path '*/lib/libonnxruntime*.dylib' -not -path '*.dSYM/*' | sort | sed -n '1p')"
if [[ -z "$SRC" ]]; then
  echo "error: no libonnxruntime*.dylib found inside ${ARCHIVE}." >&2
  exit 1
fi

GOT_DYLIB_SHA="$(shasum -a 256 "$SRC" | awk '{print $1}')"
if [[ "$(printf '%s' "$GOT_DYLIB_SHA" | tr '[:upper:]' '[:lower:]')" != \
      "$(printf '%s' "$EXPECTED_DYLIB_SHA256" | tr '[:upper:]' '[:lower:]')" ]]; then
  echo "error: SHA256 mismatch for extracted $(basename "$SRC")" >&2
  echo "  expected $EXPECTED_DYLIB_SHA256" >&2
  echo "  got      $GOT_DYLIB_SHA" >&2
  exit 1
fi
echo "Dylib SHA256 verified: $GOT_DYLIB_SHA"

mkdir -p "$RUNTIME_DIR"
STAGED="$RUNTIME_DIR/.libonnxruntime.dylib.tmp.$$"
cp -L "$SRC" "$STAGED"
chmod 0644 "$STAGED"
mv -f "$STAGED" "$DEST"

echo "✓ Installed: $DEST"
echo "  (from $(basename "$SRC"))"
echo "Now run a full AI scan, e.g.:  fileid scan ~/Pictures --models"
echo "Verify with:                   fileid runtime status"
