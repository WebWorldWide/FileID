#!/usr/bin/env bash
#
# FileID — unified cross-platform build dispatcher.
#
# This is the single command users run regardless of OS. It dispatches
# to the per-platform build script under platforms/<platform>/.
#
# Usage:
#   ./build.sh -windows               # Wipe + Release + Desktop + Run on Windows
#   ./build.sh -windows --no-wipe     # Skip the destructive wipe
#   ./build.sh -windows --debug       # Debug build (faster iteration)
#   ./build.sh -windows --no-run      # Build only, don't launch
#   ./build.sh -mac                   # macOS dev launch (run.sh)
#   ./build.sh -linux                 # Phase 5 — not yet supported
#
# Defaults for -windows:
#   - Wipe: ON          (destructive: clears Desktop\FileID + %LOCALAPPDATA%\FileID)
#   - Release: ON       (self-contained publish, runnable on any Win11 box)
#   - Desktop: ON       (drops a folder at ~/Desktop/FileID/)
#   - Run: ON           (launches FileID.exe after build completes)
#
# Override any of these with --no-<flag> to disable.
#
# Exit code: 0 on success, 1 on any failure.

set -euo pipefail

# ─── Argument parsing ───────────────────────────────────────────────────────
TARGET=""
WIPE="true"
RELEASE="true"
DESKTOP="true"
RUN="true"
RUN_TESTS="false"
ARM64="false"
SIGN="false"
VLM_NATIVE="false"
FAST="false"

show_help() {
    cat <<'EOF'
FileID — unified build dispatcher

  ./build.sh -windows [flags]    Build and launch on Windows
  ./build.sh -mac     [flags]    Build and launch on macOS
  ./build.sh -linux   [flags]    Linux (Phase 5 — deferred)

Common flags (after the target):
  --no-wipe        Don't destructively clear prior install + user data
                   (default: wipe ON for fresh-install verification)
  --no-run         Don't launch the app after build
  --no-desktop     Don't stage to ~/Desktop/FileID/
  --debug          Debug build instead of Release (faster iteration)
  --tests          Run cargo + dotnet tests
  --arm64          Cross-compile for ARM64 (Snapdragon WoA) — Windows only
  --vlm-native     Build with native llama.cpp bindings — Windows only
  --fast           Iteration-friendly release: thin LTO + parallel codegen
                   (~40-60% faster Rust compile, small runtime delta).
                   Use during inner-loop iteration.
  --sign           Authenticode-sign every binary — Windows only,
                   needs FILEID_EV_THUMBPRINT env var
  --help           Show this message

Examples:
  ./build.sh -windows                       # full fresh-install build + run
  ./build.sh -windows --no-wipe --debug     # iterate without wiping models
  ./build.sh -windows --no-run --tests      # CI-style verification
  ./build.sh -windows --arm64 --no-run      # cross-compile for Snapdragon
  ./build.sh -mac                           # macOS dev launch
EOF
    exit 0
}

if [ $# -eq 0 ]; then show_help; fi

while [ $# -gt 0 ]; do
    case "$1" in
        -windows|--windows) TARGET="windows" ;;
        -mac|--mac|-macos|--macos) TARGET="mac" ;;
        -linux|--linux) TARGET="linux" ;;
        --no-wipe) WIPE="false" ;;
        --debug) RELEASE="false" ;;
        --no-desktop) DESKTOP="false" ;;
        --no-run) RUN="false" ;;
        --tests) RUN_TESTS="true" ;;
        --arm64) ARM64="true" ;;
        --vlm-native) VLM_NATIVE="true" ;;
        --fast) FAST="true" ;;
        --sign) SIGN="true" ;;
        --help|-h) show_help ;;
        *)
            echo "ERROR: unknown argument '$1'" >&2
            echo "       run './build.sh --help' for usage." >&2
            exit 1
            ;;
    esac
    shift
done

if [ -z "$TARGET" ]; then
    echo "ERROR: no target specified. Use -windows, -mac, or -linux." >&2
    echo "       run './build.sh --help' for usage." >&2
    exit 1
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ─── Dispatch ───────────────────────────────────────────────────────────────
case "$TARGET" in
    windows)
        # Build the PowerShell flag list. PowerShell -Switch flags are
        # passed as `-Wipe`, `-Release`, etc. — true means "include the
        # flag", false means "omit it".
        ps_args=()
        $WIPE       && ps_args+=("-Wipe")
        $RELEASE    && ps_args+=("-Release")
        $DESKTOP    && ps_args+=("-Desktop")
        $RUN        && ps_args+=("-Run")
        $RUN_TESTS  && ps_args+=("-RunTests")
        $ARM64      && ps_args+=("-Arm64")
        $VLM_NATIVE && ps_args+=("-VlmNative")
        $FAST       && ps_args+=("-Fast")
        $SIGN       && ps_args+=("-Sign")

        # Locate pwsh (PowerShell 7+) preferred over Windows PowerShell 5.1.
        if command -v pwsh > /dev/null 2>&1; then
            PS=pwsh
        elif command -v powershell > /dev/null 2>&1; then
            PS=powershell
        else
            echo "ERROR: neither pwsh nor powershell found in PATH." >&2
            echo "       install PowerShell 7+: https://aka.ms/install-powershell" >&2
            exit 1
        fi

        SCRIPT="$REPO_ROOT/platforms/windows/build/build-all.ps1"
        if [ ! -f "$SCRIPT" ]; then
            echo "ERROR: build script not found at $SCRIPT" >&2
            exit 1
        fi

        echo "→ $PS $SCRIPT ${ps_args[*]}"
        exec "$PS" -NoProfile -ExecutionPolicy Bypass -File "$SCRIPT" "${ps_args[@]}"
        ;;

    mac)
        SCRIPT="$REPO_ROOT/platforms/apple/run.sh"
        if [ ! -f "$SCRIPT" ]; then
            echo "ERROR: macOS build script not found at $SCRIPT" >&2
            exit 1
        fi
        # The macOS run.sh handles its own configuration; just dispatch.
        # If the user passed --tests, run the test suite first.
        if $RUN_TESTS; then
            ( cd "$REPO_ROOT/platforms/apple" && swift test )
        fi
        "$SCRIPT"

        # Mirror FileID.app to ~/Desktop for one-click access — matches
        # the Windows --desktop default. Wipe any prior copy + Finder's
        # numbered duplicates (FileID 2.app, …) so the new bundle keeps
        # its real name.
        if $DESKTOP; then
            APP_BUNDLE="$REPO_ROOT/platforms/apple/FileID.app"
            DESKTOP_APP="$HOME/Desktop/FileID.app"
            if [ -d "$APP_BUNDLE" ]; then
                find "$HOME/Desktop" -maxdepth 1 -name "FileID *.app" -exec rm -rf {} + 2>/dev/null || true
                rm -rf "$DESKTOP_APP"
                cp -R "$APP_BUNDLE" "$DESKTOP_APP"
                echo "✅ Mirrored to $DESKTOP_APP"
            else
                echo "⚠️  $APP_BUNDLE not found — skipping Desktop mirror." >&2
            fi
        fi
        ;;

    linux)
        cat <<'EOF' >&2
Linux is deferred to Phase 5 (see shared/docs/SHIP.md).

The Rust engine is cross-platform-clean and will build on Linux today
(`cargo build --release` from platforms/windows/src/engine/ — works on
any *nix). The blocker is the UI: WinUI 3 is Windows-only, so the
Linux app needs an Avalonia or GTK4-Rust port.

If you need the engine standalone for headless use, run:
    cd platforms/windows/src/engine && cargo build --release

For UI: cross that bridge when we get there.
EOF
        exit 1
        ;;
esac
