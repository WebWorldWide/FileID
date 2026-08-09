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
PRESERVE_MODELS="false"   # only honored when WIPE=true
WIPE_DB_ONLY="false"      # lightest scope: only SQLite files; mutually exclusive with WIPE
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

  ./build.sh                     Interactive wizard (asks plain-English
                                 questions instead of flag soup)
  ./build.sh -windows [flags]    Build and launch on Windows
  ./build.sh -mac     [flags]    Build and launch on macOS
  ./build.sh -linux   [flags]    Linux (Phase 5 — deferred)

Common flags (after the target):
  --no-wipe        Don't destructively clear prior install + user data
                   (default: wipe ON for fresh-install verification)
  --preserve-models Keep downloaded model weights when wiping (Windows only;
                   only meaningful alongside the default wipe)
  --wipe-db-only   Lightest wipe: delete only fileid.sqlite{,.wal,.shm}.
                   Preserves models, logs, settings, cache, Desktop staging.
                   Use to get a fresh scan without losing anything else.
                   Mutually exclusive with default wipe; implies --no-wipe.
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
  ./build.sh                                # interactive wizard
  ./build.sh -windows                       # full fresh-install build + run
  ./build.sh -windows --no-wipe --debug     # iterate without wiping models
  ./build.sh -windows --no-run --tests      # CI-style verification
  ./build.sh -windows --arm64 --no-run      # cross-compile for Snapdragon
  ./build.sh -mac                           # macOS dev launch
EOF
    exit 0
}

# ─── Interactive wizard ─────────────────────────────────────────────────────
# Fires when build.sh is run with no arguments. Walks the user through the
# same configuration the flags expose, but as plain-English questions.
# Defaults shown in [brackets]; press Enter to accept.

ask_yes_no() {
    # ask_yes_no <prompt> <default y|n>
    # echoes 'true' or 'false'.
    local prompt="$1"
    local default="$2"
    local hint
    if [ "$default" = "y" ]; then hint="[Y/n]"; else hint="[y/N]"; fi
    local reply
    while true; do
        read -r -p "$prompt $hint: " reply
        reply="${reply:-$default}"
        reply="$(printf '%s' "$reply" | tr '[:upper:]' '[:lower:]')"
        case "$reply" in
            y|yes) echo "true"; return ;;
            n|no)  echo "false"; return ;;
            *)     echo "Please answer y or n." >&2 ;;
        esac
    done
}

ask_choice() {
    # ask_choice <prompt> <default-index> <choice1> <choice2> ...
    local prompt="$1"; shift
    local default="$1"; shift
    local i=1
    echo "$prompt" >&2
    for choice in "$@"; do
        echo "  $i) $choice" >&2
        i=$((i + 1))
    done
    local n=$#
    local reply
    while true; do
        read -r -p "  > [$default]: " reply
        reply="${reply:-$default}"
        if [[ "$reply" =~ ^[0-9]+$ ]] && [ "$reply" -ge 1 ] && [ "$reply" -le "$n" ]; then
            echo "$reply"
            return
        fi
        echo "  Enter a number between 1 and $n." >&2
    done
}

interactive_wizard() {
    cat <<'EOF'

────────────────────────────────────────────────────────
  FileID build wizard
  Press Enter to accept the default in [brackets].
────────────────────────────────────────────────────────

EOF

    local plat
    plat=$(ask_choice "Where are we building?" 1 \
        "Windows" \
        "macOS" \
        "Linux (Phase 5 — not yet supported)")
    case "$plat" in
        1) TARGET="windows" ;;
        2) TARGET="mac" ;;
        3) TARGET="linux" ;;
    esac
    echo ""

    # Linux isn't wired up yet — short-circuit before the rest of the
    # wizard so the user doesn't answer irrelevant questions just to be
    # told the platform is deferred.
    if [ "$TARGET" = "linux" ]; then return; fi

    local preset
    preset=$(ask_choice "What kind of build?" 2 \
        "Fresh install — wipe everything, release build, launch (slow, definitive)" \
        "Iterate — keep models + DB, debug build, launch (fast inner loop)" \
        "Tests only — no wipe, no launch, run cargo + dotnet tests" \
        "CI release — release build, sign, no launch (publish artifact)" \
        "Custom — answer each question individually")
    echo ""

    case "$preset" in
        1) # Fresh install
            WIPE="true"; RELEASE="true"; DESKTOP="true"; RUN="true"
            RUN_TESTS="false"; FAST="false"
            if [ "$TARGET" = "windows" ]; then
                local wipe_scope
                wipe_scope=$(ask_choice "How much should we wipe?" 3 \
                    "Build artifacts only (cargo target/, dotnet bin/obj/, dist/)" \
                    "DB only (lightest — preserves models, logs, settings, cache, Desktop staging)" \
                    "Build artifacts + library DB (preserves downloaded models — recommended)" \
                    "Everything — also delete downloaded models (multi-GB redownload)")
                case "$wipe_scope" in
                    1) WIPE="false" ;;
                    2) WIPE="false"; WIPE_DB_ONLY="true" ;;
                    3) WIPE="true"; PRESERVE_MODELS="true" ;;
                    4) WIPE="true"; PRESERVE_MODELS="false" ;;
                esac
                echo ""
            fi
            ;;
        2) # Iterate
            WIPE="false"; RELEASE="false"; DESKTOP="false"; RUN="true"
            RUN_TESTS="false"; FAST="false"
            ;;
        3) # Tests only
            WIPE="false"; RELEASE="false"; DESKTOP="false"; RUN="false"
            RUN_TESTS="true"; FAST="false"
            ;;
        4) # CI release
            WIPE="true"; PRESERVE_MODELS="false"
            RELEASE="true"; DESKTOP="false"; RUN="false"
            RUN_TESTS="false"; FAST="false"
            if [ "$TARGET" = "windows" ]; then SIGN="true"; fi
            ;;
        5) # Custom — walk every toggle
            WIPE=$(ask_yes_no "Wipe before building?" "y")
            if [ "$WIPE" = "true" ] && [ "$TARGET" = "windows" ]; then
                PRESERVE_MODELS=$(ask_yes_no "Preserve downloaded model weights when wiping?" "y")
            fi
            RELEASE=$(ask_yes_no "Release build (slower, but standalone)?" "y")
            DESKTOP=$(ask_yes_no "Stage to Desktop (~/Desktop/FileID*)?" "y")
            RUN=$(ask_yes_no "Launch the app after build finishes?" "y")
            RUN_TESTS=$(ask_yes_no "Run cargo + dotnet tests after build?" "n")
            if [ "$TARGET" = "windows" ]; then
                ARM64=$(ask_yes_no "Cross-compile for ARM64 (Snapdragon WoA)?" "n")
                VLM_NATIVE=$(ask_yes_no "Native llama.cpp bindings (cmake required)?" "n")
                FAST=$(ask_yes_no "Use --fast (thin LTO, faster Rust compile)?" "n")
                SIGN=$(ask_yes_no "Authenticode-sign all binaries?" "n")
            fi
            ;;
    esac

    # Recap as the equivalent flag invocation so the user can copy it for
    # next time and skip the wizard.
    local equiv="./build.sh -$TARGET"
    [ "$WIPE_DB_ONLY" = "true" ] && equiv="$equiv --wipe-db-only"
    [ "$WIPE" = "false" ] && [ "$WIPE_DB_ONLY" = "false" ] && equiv="$equiv --no-wipe"
    [ "$WIPE" = "true"  ] && [ "$PRESERVE_MODELS" = "true" ] && equiv="$equiv --preserve-models"
    [ "$RELEASE" = "false" ] && equiv="$equiv --debug"
    [ "$DESKTOP" = "false" ] && equiv="$equiv --no-desktop"
    [ "$RUN" = "false" ] && equiv="$equiv --no-run"
    [ "$RUN_TESTS" = "true" ] && equiv="$equiv --tests"
    [ "$ARM64" = "true" ] && equiv="$equiv --arm64"
    [ "$VLM_NATIVE" = "true" ] && equiv="$equiv --vlm-native"
    [ "$FAST" = "true" ] && equiv="$equiv --fast"
    [ "$SIGN" = "true" ] && equiv="$equiv --sign"

    echo ""
    echo "→ Equivalent for next time:  $equiv"
    echo ""
    local confirm
    confirm=$(ask_yes_no "Run this build now?" "y")
    if [ "$confirm" = "false" ]; then
        echo "Aborted." >&2
        exit 0
    fi
    echo ""
}

if [ $# -eq 0 ]; then interactive_wizard; fi

while [ $# -gt 0 ]; do
    case "$1" in
        -windows|--windows) TARGET="windows" ;;
        -mac|--mac|-macos|--macos) TARGET="mac" ;;
        -linux|--linux) TARGET="linux" ;;
        --no-wipe) WIPE="false" ;;
        --preserve-models) PRESERVE_MODELS="true" ;;
        --wipe-db-only) WIPE_DB_ONLY="true"; WIPE="false" ;;
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
        $WIPE && $PRESERVE_MODELS && ps_args+=("-PreserveModels")
        $WIPE_DB_ONLY && ps_args+=("-WipeDbOnly")
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
