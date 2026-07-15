#!/usr/bin/env bash
# FileID dev bootstrap — macOS + Linux (all distros).
#
# Installs system-packaged build prerequisites and builds the isolated RAM++
# export venv. Security-sensitive bootstrap tools must already be installed;
# this script never downloads and executes remote installer code.
# then prints the GUI-gated steps it can't automate (full Xcode on macOS).
# Idempotent — skips anything already present.
#
#   macOS  : requires Homebrew; installs python@3.11, cmake/pkg-config; Xcode CLT via xcode-select.
#   Linux  : detects apt/dnf/pacman/zypper/apk → build tools + python; requires existing Rust.
#            (Linux is Phase 5 / engine-only today; the C# app + macOS app don't build here.)
#
# Usage:  bash shared/scripts/setup-dev.sh [--skip-export-venv]
set -euo pipefail

SKIP_VENV=0
[[ "${1:-}" == "--skip-export-venv" ]] && SKIP_VENV=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPTS="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
info()  { printf '\033[36m[setup]\033[0m %s\n' "$*"; }
ok()    { printf '\033[32m[ ok ]\033[0m %s\n' "$*"; }
warn()  { printf '\033[33m[warn]\033[0m %s\n' "$*"; }
die()   { printf '\033[31m[fail]\033[0m %s\n' "$*" >&2; exit 1; }
have()  { command -v "$1" >/dev/null 2>&1; }

OS="$(uname -s)"
info "FileID dev bootstrap. OS=$OS  repo=$ROOT"

# --- package-manager install helper (per platform) -------------------------
PY=python3
install_macos() {
  if ! have brew; then
    die "Homebrew is required but missing. Install it separately from a reviewed, trusted source, then rerun; FileID will not execute a mutable remote installer."
  fi
  info "brew install python@3.11 cmake pkg-config ..."
  brew install python@3.11 cmake pkg-config >/dev/null
  PY="$(brew --prefix)/bin/python3.11"; [[ -x "$PY" ]] || PY=python3.11
  # Xcode Command Line Tools (clang, headers) — full Xcode is App Store only.
  if ! xcode-select -p >/dev/null 2>&1; then
    info "requesting Xcode Command Line Tools (a GUI prompt may appear) ..."
    xcode-select --install || true
  fi
}

install_linux() {
  . /etc/os-release 2>/dev/null || true
  local id="${ID:-} ${ID_LIKE:-}"
  info "Linux distro: ${PRETTY_NAME:-unknown}"
  if   have apt-get; then sudo apt-get update -y && sudo apt-get install -y build-essential clang cmake pkg-config libssl-dev curl git python3 python3-venv python3-pip
  elif have dnf;     then sudo dnf install -y @"Development Tools" clang cmake pkgconf-pkg-config openssl-devel curl git python3 python3-virtualenv python3-pip
  elif have pacman;  then sudo pacman -Sy --needed --noconfirm base-devel clang cmake pkgconf openssl curl git python python-pip
  elif have zypper;  then sudo zypper install -y -t pattern devel_basis && sudo zypper install -y clang cmake pkg-config libopenssl-devel curl git python311 python311-venv python311-pip
  elif have apk;     then sudo apk add build-base clang cmake pkgconf openssl-dev curl git python3 py3-pip py3-virtualenv
  else warn "unrecognized distro — install manually: a C toolchain, cmake, pkg-config, openssl-dev, python3.11+venv, curl, git."
  fi
  for c in python3.11 python3; do have "$c" && { PY="$c"; break; }; done
}

case "$OS" in
  Darwin) install_macos ;;
  Linux)  install_linux ;;
  *) warn "unsupported OS '$OS' — this script targets macOS + Linux (use setup-dev.ps1 on Windows)."; exit 1 ;;
esac

# --- Rust ------------------------------------------------------------------
if have rustc && have cargo; then
  RUST_VERSION="$(rustc --version | awk '{print $2}')"
  if [[ ! "$RUST_VERSION" =~ ^([0-9]+)\.([0-9]+)(\.[0-9]+)? ]]; then
    die "Unable to parse rustc version: $RUST_VERSION"
  fi
  RUST_MAJOR="${BASH_REMATCH[1]}"
  RUST_MINOR="${BASH_REMATCH[2]}"
  if (( RUST_MAJOR < 1 || (RUST_MAJOR == 1 && RUST_MINOR < 90) )); then
    die "Rust 1.90 or newer is required by rust-toolchain.toml; found $RUST_VERSION. Install the pinned toolchain separately, then rerun."
  fi
  ok "Rust already present ($(rustc --version))"
else
  die "Rust and Cargo are required. Install the 1.90 toolchain pinned by rust-toolchain.toml separately from a reviewed, trusted source, then rerun; FileID will not pipe a remote installer into a shell."
fi

# --- RAM++ export venv (pinned) --------------------------------------------
if [[ "$SKIP_VENV" -eq 0 ]]; then
  VENV="$ROOT/.venv-ramplus"
  REQ="$SCRIPTS/requirements-ramplus.txt"
  info "creating pinned RAM++ export venv at $VENV (python: $PY) ..."
  [[ -d "$VENV" ]] && { warn "removing stale $VENV (re-pinning deps)"; rm -rf "$VENV"; }
  "$PY" -m venv "$VENV"
  VPY="$VENV/bin/python"
  "$VPY" -m pip install -r "$REQ"
  # Apache-2.0 recognize-anything at the reviewed 2025-02-18 commit. Install
  # WITHOUT deps because requirements-ramplus.txt owns every direct version.
  "$VPY" -m pip install --no-deps \
    "git+https://github.com/xinyu1205/recognize-anything.git@7cb804a8609e9f4b1a50b7f31436d2df40bb9481"
  info "verifying the ram_plus import resolves ..."
  "$VPY" -c "from ram.models import ram_plus; print('ram_plus import OK')"
  ok "RAM++ export venv ready"
fi

echo
ok "Toolchain ready. Next:"
if [[ "$OS" == "Darwin" ]]; then
  echo "  Engine:  cargo build --release --manifest-path platforms/windows/src/engine/Cargo.toml"
  echo "  macOS app: open platforms/apple in Xcode (full Xcode from the App Store is required for SwiftUI/MLX)."
else
  echo "  Engine:  cargo build --release --manifest-path platforms/windows/src/engine/Cargo.toml"
  echo "  (Linux is Phase 5 / engine-only today — no desktop app yet.)"
fi
echo "  RAM++:   source .venv-ramplus/bin/activate ; then run shared/scripts/export_ram_plus_onnx.py"
