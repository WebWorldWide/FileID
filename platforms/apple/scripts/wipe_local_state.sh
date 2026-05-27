#!/bin/bash
# Wipe every trace of FileID from this Mac so the next launch behaves
# like a brand-new install. Verbose by design — prints every path it
# inspects, removes, or skips so you can verify what changed.
#
# Always wipes:
#   - /Applications/FileID.app                 (drag-installed bundle)
#   - ~/Applications/FileID.app                (per-user Applications)
#   - ~/Library/Application Support/FileID/    (DB, WAL/SHM, on-device
#                                              models, logs)
#   - ~/Library/Caches/com.fileid.app/         (URLSession ephemeral
#                                              cache + image thumbs)
#   - ~/Library/HTTPStorages/com.fileid.app/   (cookies, HSTS)
#   - ~/Library/Logs/FileID/                   (any rotated logs)
#   - ~/Library/Saved Application State/com.fileid.app.savedState/
#   - com.fileid.app UserDefaults              (folder bookmark, prefs)
#
# With --purge-models also wipes:
#   - ~/Documents/huggingface/                 (multi-GB MLX VLM
#                                              weights — Qwen, Gemma,
#                                              PaliGemma)
#   - ~/.cache/huggingface/                    (system HF cache,
#                                              touched by some libs)
#   - ~/Library/Caches/huggingface/            (alternative HF cache)

set -e

PURGE_MODELS=0
for arg in "$@"; do
    case "$arg" in
        --purge-models|-p) PURGE_MODELS=1 ;;
        --help|-h)
            sed -n '1,28p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "Unknown flag: $arg (try --help)"; exit 1 ;;
    esac
done

remove_path() {
    local label="$1"
    local target="$2"
    if [ -e "$target" ] || [ -L "$target" ]; then
        local size
        size=$(du -sh "$target" 2>/dev/null | cut -f1)
        echo "  ✓ $label  ($size)  $target"
        rm -rf "$target"
    else
        echo "  · $label  (absent)  $target"
    fi
}

echo "▶ Stopping running FileID processes…"
osascript -e 'tell application "FileID" to quit' >/dev/null 2>&1 || true
sleep 0.5
pkill -f "FileID.app/Contents/MacOS/FileID"        2>/dev/null || true
pkill -f "FileID.app/Contents/MacOS/FileIDEngine"  2>/dev/null || true
pkill -x "FileID"                                   2>/dev/null || true
pkill -x "FileIDEngine"                             2>/dev/null || true
sleep 0.5
pkill -9 -f "FileID.app/Contents/MacOS/" 2>/dev/null || true
pkill -9 -x "FileIDEngine"               2>/dev/null || true
echo "  ✓ processes signalled"

echo "▶ Removing installed bundles…"
remove_path "installed app   "  "/Applications/FileID.app"
remove_path "user app        "  "$HOME/Applications/FileID.app"

echo "▶ Removing per-user state…"
remove_path "Application Supp"  "$HOME/Library/Application Support/FileID"
remove_path "Caches           " "$HOME/Library/Caches/com.fileid.app"
remove_path "HTTPStorages     " "$HOME/Library/HTTPStorages/com.fileid.app"
remove_path "Logs             " "$HOME/Library/Logs/FileID"
remove_path "Saved State      " "$HOME/Library/Saved Application State/com.fileid.app.savedState"

echo "▶ Clearing UserDefaults (com.fileid.app)…"
defaults delete com.fileid.app >/dev/null 2>&1 && echo "  ✓ defaults cleared" \
    || echo "  · defaults already empty"
killall cfprefsd >/dev/null 2>&1 || true

if [ "$PURGE_MODELS" = "1" ]; then
    echo "▶ Purging downloaded model weights…"
    remove_path "Documents/hf    "  "$HOME/Documents/huggingface"
    remove_path ".cache/hf       "  "$HOME/.cache/huggingface"
    remove_path "Caches/hf       "  "$HOME/Library/Caches/huggingface"
fi

echo
if [ "$PURGE_MODELS" = "1" ]; then
    echo "✅ FileID + all model weights wiped. Next launch redownloads everything via the welcome sheet."
else
    echo "✅ FileID state wiped. Model weights preserved (use --purge-models to nuke them too)."
fi
