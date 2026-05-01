#!/bin/bash
# Wipes the per-user FileID state on this Mac so the next launch
# behaves like a brand-new install. Use it before testing a release
# DMG to verify the empty-state UI.
#
# Removes (always):
#   - ~/Library/Application Support/FileID/   (SQLite, WAL/SHM, Models, logs)
#   - com.fileid.app UserDefaults             (folder bookmark, prefs)
#
# Removes (with --purge-models):
#   - ~/Documents/huggingface/                (multi-GB MLX VLM weights —
#                                              forces the welcome-sheet's
#                                              VLM download path to fire
#                                              on the next launch)
#
# Run.sh does the same thing as part of its rebuild loop; this is
# the standalone wipe for when you don't want to rebuild.

set -e

PURGE_MODELS=0
for arg in "$@"; do
    case "$arg" in
        --purge-models|-p)
            PURGE_MODELS=1
            ;;
        --help|-h)
            cat <<'EOF'
Usage: bash scripts/wipe_local_state.sh [--purge-models]

  --purge-models   Also rm -rf ~/Documents/huggingface/. Multi-GB.
                   Use when validating the welcome-sheet onboarding
                   flow on a truly fresh DMG install.
EOF
            exit 0
            ;;
        *)
            echo "Unknown flag: $arg (try --help)"
            exit 1
            ;;
    esac
done

osascript -e 'tell application "FileID" to quit' >/dev/null 2>&1 || true
sleep 0.5
pkill -f "FileID.app/Contents/MacOS/FileID"        2>/dev/null || true
pkill -f "FileID.app/Contents/MacOS/FileIDEngine"  2>/dev/null || true
pkill -x "FileID"                                   2>/dev/null || true
pkill -x "FileIDEngine"                             2>/dev/null || true
sleep 0.5
pkill -9 -f "FileID.app/Contents/MacOS/"           2>/dev/null || true
pkill -9 -x "FileIDEngine"                          2>/dev/null || true

rm -rf "$HOME/Library/Application Support/FileID"
defaults delete com.fileid.app >/dev/null 2>&1 || true
killall cfprefsd >/dev/null 2>&1 || true

if [ "$PURGE_MODELS" = "1" ]; then
    if [ -d "$HOME/Documents/huggingface" ]; then
        SIZE=$(du -sh "$HOME/Documents/huggingface" 2>/dev/null | cut -f1)
        echo "🧨 Purging ~/Documents/huggingface ($SIZE)…"
        rm -rf "$HOME/Documents/huggingface"
    fi
fi

if [ "$PURGE_MODELS" = "1" ]; then
    echo "✅ Local FileID state + VLM cache wiped. Next launch redownloads everything via the welcome sheet."
else
    echo "✅ Local FileID state wiped. ~/Documents/huggingface preserved (use --purge-models to nuke it)."
fi
