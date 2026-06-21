#!/usr/bin/env bash
#
# FileID — build + install the cross-platform command-line tools.
#
# Builds three release binaries and copies them onto your PATH (~/.cargo/bin):
#
#   fileid        platforms/cli                  the CLI front-end
#   fileid-tui    platforms/tui                  the terminal UI (ratatui)
#   FileIDEngine  platforms/windows/src/engine   the scan / ML engine
#
# All three are required together: `fileid scan --models` and the TUI's "s"
# (scan) key both SPAWN the FileIDEngine binary, so it has to be discoverable on
# PATH next to `fileid` / `fileid-tui`.
#
# Idempotent + safe: re-run any time. Builds are incremental (warm target dirs
# are reused) and installs are atomic (temp file + mv) so re-running while a tool
# is in use can't corrupt it. Uses `cargo build --release` + `cp` rather than
# `cargo install` precisely to reuse each crate's existing target dir.
#
# Why cargo is invoked from the repo root with --manifest-path (not `cd` into
# each crate): the engine crate pins a Windows-only toolchain
# (rust-toolchain.toml: channel 1.90 + *-pc-windows-msvc targets) and ships a
# Windows-target .cargo/config.toml. Cargo discovers rust-toolchain.toml /
# .cargo/config.toml from the *current directory*, so invoking from the repo
# root sidesteps both and builds the engine natively with the host's default
# toolchain on macOS / Linux.

set -euo pipefail

# --- locate the repo --------------------------------------------------------
# This script lives in <repo>/scripts/, so the repo root is one level up.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# --- destination ------------------------------------------------------------
BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
mkdir -p "$BIN_DIR"

# --- crate manifests --------------------------------------------------------
ENGINE_DIR="platforms/windows/src/engine"   # [[bin]] name = FileIDEngine
CLI_DIR="platforms/cli"                      # [[bin]] name = fileid
TUI_DIR="platforms/tui"                      # [[bin]] name = fileid-tui

# --- build ------------------------------------------------------------------
# The engine binary is built with --no-default-features to drop `pdf-analyze`
# (pdfium): it matches how the CLI/TUI link the engine, avoids a libpdfium
# runtime dependency, and is irrelevant to the image scan / tag / face / embed
# pipeline. Deep-Analyze PDF rasterization is the only thing it turns off.
echo "==> [1/3] Building FileIDEngine (release)  —  $ENGINE_DIR"
cargo build --release --no-default-features --manifest-path "$ENGINE_DIR/Cargo.toml"

echo "==> [2/3] Building fileid CLI (release)    —  $CLI_DIR"
cargo build --release --manifest-path "$CLI_DIR/Cargo.toml"

echo "==> [3/3] Building fileid-tui (release)    —  $TUI_DIR"
cargo build --release --manifest-path "$TUI_DIR/Cargo.toml"

# --- install (atomic) -------------------------------------------------------
install_bin() {
    local src="$1" name="$2"
    if [ ! -x "$src" ]; then
        echo "ERROR: built binary missing or not executable: $src" >&2
        exit 1
    fi
    local tmp="$BIN_DIR/.${name}.tmp.$$"
    cp -f "$src" "$tmp"
    chmod +x "$tmp"
    mv -f "$tmp" "$BIN_DIR/$name"      # atomic replace, even if $name is running
    echo "    $name  ->  $BIN_DIR/$name"
}

echo "==> Installing to $BIN_DIR"
install_bin "$ENGINE_DIR/target/release/FileIDEngine" "FileIDEngine"
install_bin "$CLI_DIR/target/release/fileid"          "fileid"
install_bin "$TUI_DIR/target/release/fileid-tui"      "fileid-tui"

# --- next steps -------------------------------------------------------------
echo ""
echo "Done. Installed fileid, fileid-tui, and FileIDEngine into $BIN_DIR"
echo ""

case ":${PATH}:" in
    *":$BIN_DIR:"*)
        echo "  $BIN_DIR is already on your PATH — you're ready to go." ;;
    *)
        echo "  NOTE: $BIN_DIR is NOT on your PATH yet. Add it, e.g.:"
        echo "      echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> ~/.zshrc && source ~/.zshrc"
        echo "  (fileid / fileid-tui must find FileIDEngine on PATH to run a full scan.)" ;;
esac

cat <<'EOF'

  Scan a folder:
      fileid scan ~/Pictures --models      # full pipeline (tags, faces, CLIP, hashes)
      fileid scan ~/Pictures               # fast, model-free filename + text index

  Then explore the library it built:
      fileid people                        # face clusters
      fileid search <query>                # search the indexed library
      fileid-tui                           # terminal UI (press 's' to scan)

  --models needs the AI weights installed (the macOS desktop app's
  ~/Library/Application Support/FileID/Models is used automatically when present).
EOF
