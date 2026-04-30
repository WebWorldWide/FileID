#!/bin/bash
# Wipes the per-user FileID state on this Mac so the next launch
# behaves like a brand-new install. Use it before testing a release
# DMG to verify the empty-state UI.
#
# Removes:
#   - ~/Library/Application Support/FileID/   (SQLite, WAL/SHM, Models, logs)
#   - com.fileid.app UserDefaults             (folder bookmark, prefs)
#
# Preserves:
#   - ~/Documents/huggingface/   (multi-GB MLX VLM weights)
#
# Run.sh does the same thing as part of its rebuild loop; this is
# the standalone wipe for when you don't want to rebuild.

set -e

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

echo "✅ Local FileID state wiped. Next launch will start fresh."
