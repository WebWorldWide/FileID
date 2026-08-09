#!/usr/bin/env bash
# FileID dev bootstrap — macOS + Linux (all distros).
#
# Installs the SCRIPTABLE toolchain (Rust, Python 3.11, a C/build toolchain) and
# builds the isolated RAM++ export venv (pinned, from requirements-ramplus.txt),
# then prints the GUI-gated steps it can't automate (full Xcode on macOS).
# Idempotent — skips anything already present.
#
#   macOS  : Homebrew → rustup, python@3.11, cmake/pkg-config; Xcode CLT via xcode-select.
#   Linux  : detects apt/dnf/pacman/zypper/apk → build tools + python; rustup via rustup.rs.
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
have()  { command -v "$1" >/dev/null 2>&1; }

OS="$(uname -s)"
info "FileID dev bootstrap. OS=$OS  repo=$ROOT"

# --- package-manager install helper (per platform) -------------------------
PY=python3
install_macos() {
  if ! have brew; then
    info "installing Homebrew ..."
    /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
    # add brew to PATH for this session (Apple Silicon vs Intel prefixes)
    [[ -x /opt/homebrew/bin/brew ]] && eval "$(/opt/homebrew/bin/brew shellenv)"
    [[ -x /usr/local/bin/brew ]] && eval "$(/usr/local/bin/brew shellenv)"
  fi
  info "brew install python@3.11 cmake pkg-config ..."
  brew install python@3.11 cmake pkg-config >/dev/null || true
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

# --- Rust (rustup) ----------------------------------------------------------
if have rustc; then ok "Rust already present ($(rustc --version))"
else
  info "installing Rust via rustup ..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env" 2>/dev/null || export PATH="$HOME/.cargo/bin:$PATH"
  ok "Rust $(rustc --version 2>/dev/null || echo installed)"
fi

# --- RAM++ export venv (pinned) --------------------------------------------
if [[ "$SKIP_VENV" -eq 0 ]]; then
  VENV="$ROOT/.venv-ramplus"
  REQ="$SCRIPTS/requirements-ramplus.txt"
  info "creating pinned RAM++ export venv at $VENV (python: $PY) ..."
  [[ -d "$VENV" ]] && { warn "removing stale $VENV (re-pinning deps)"; rm -rf "$VENV"; }
  "$PY" -m venv "$VENV"
  VPY="$VENV/bin/python"
  "$VPY" -m pip install --upgrade pip
  "$VPY" -m pip install -r "$REQ"
  # recognize-anything WITHOUT deps — requirements-ramplus.txt owns the versions
  # so it can't drag in conflicting latest timm/transformers.
  "$VPY" -m pip install --no-deps "git+https://github.com/xinyu1205/recognize-anything.git"
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
