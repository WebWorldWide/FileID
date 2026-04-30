#!/bin/bash
# Iteration loop: kill, wipe DB, build, scan TrueNAS for N seconds, dump results.
# Usage: ./scripts/iterate.sh [seconds]   (default: 60)
set -e
SECONDS_ARG="${1:-60}"
PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_DIR"
XCODE_DEV_DIR="/Applications/Xcode.app/Contents/Developer"

pkill -9 FileIDv2 2>/dev/null || true
pkill -9 FileIDEngine 2>/dev/null || true
sleep 0.5

echo "🔨 Building engine + app..."
DEVELOPER_DIR="$XCODE_DEV_DIR" swift build --product FileIDEngine 2>&1 | tail -2
DEVELOPER_DIR="$XCODE_DEV_DIR" swift build --product FileIDv2     2>&1 | tail -2

# Wipe v2 SQLite + logs so each iteration starts clean.
rm -f "$HOME/Library/Application Support/FileID/fileid.sqlite"     \
      "$HOME/Library/Application Support/FileID/fileid.sqlite-wal" \
      "$HOME/Library/Application Support/FileID/fileid.sqlite-shm" \
      "$HOME/Library/Application Support/FileID/logs/scan.jsonl"   \
      "$HOME/Library/Application Support/FileID/logs/app.log"

# Snapshot current crash report directory contents so we can detect new crashes.
ls "$HOME/Library/Logs/DiagnosticReports/" 2>/dev/null | grep -i fileid | sort > /tmp/crashes_before.txt

echo "🚀 Running ${SECONDS_ARG}s scan against /Volumes/Adlon/TrueNAS..."
DEVELOPER_DIR="$XCODE_DEV_DIR" swift /Users/adamnolle/Desktop/FileID/scripts/perfharness.swift "$SECONDS_ARG" 2>&1 | tail -25

echo ""
echo "=== Per-stage timing (last 8 batches) ==="
tail -25 "$HOME/Library/Application Support/FileID/logs/scan.jsonl" 2>/dev/null \
  | jq -r 'select(.ev=="batch") | "files=\(.extra.files) wall=\(.extra.wallMs|round)ms load=\(.extra.loadP50Ms|round)/\(.extra.loadP95Ms|round) vision=\(.extra.visionP50Ms|round)/\(.extra.visionP95Ms|round) clip=\(.extra.clipP50Ms|round) ocr=\(.extra.ocrP50Ms|round) util=\((.extra.utilization*100)|round)%"' 2>/dev/null \
  | tail -8

echo ""
echo "=== Errors / warnings ==="
cat "$HOME/Library/Application Support/FileID/logs/scan.jsonl" 2>/dev/null \
  | jq -r 'select(.lvl=="warn" or .lvl=="error") | "\(.lvl) \(.ev) \(.error // "")"' 2>/dev/null \
  | sort | uniq -c | sort -rn | head -10

echo ""
echo "=== New crash reports? ==="
ls "$HOME/Library/Logs/DiagnosticReports/" 2>/dev/null | grep -i fileid | sort > /tmp/crashes_after.txt
diff /tmp/crashes_before.txt /tmp/crashes_after.txt | grep '^>' || echo "(no new crashes)"
