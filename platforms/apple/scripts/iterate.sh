#!/bin/bash
# FileID — automated regression test.
#
# Builds engine + app, refreshes the test corpus, wipes the DB, drives
# the engine through a full scan + face clustering, then runs assertions.
#
# Exit codes:
#   0  — all assertions passed
#   1  — one or more assertions failed
#   2  — environment / build / engine-IPC problem
#
# For "run against my real library" use scripts/iterate_truenas.sh.

set -uo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_DIR"

GREEN="\033[32m"
RED="\033[31m"
YELLOW="\033[33m"
BOLD="\033[1m"
RESET="\033[0m"

step() { printf "${BOLD}▶ %s${RESET}\n" "$1"; }
ok()   { printf "  ${GREEN}✓${RESET} %s\n" "$1"; }
warn() { printf "  ${YELLOW}!${RESET} %s\n" "$1"; }
fail() { printf "  ${RED}✗${RESET} %s\n" "$1"; }

# ─── 1. Build ─────────────────────────────────────────────────────────
step "Building FileIDEngine"
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer \
    swift build --product FileIDEngine 2>&1 | tail -3
if [ ! -x "$PROJECT_DIR/.build/debug/FileIDEngine" ]; then
    fail "engine binary not found after build"
    exit 2
fi
ok "engine binary present"

# ─── 2. Compile bookmark helper ───────────────────────────────────────
step "Building make_bookmark helper"
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer \
    swiftc -O "$PROJECT_DIR/scripts/make_bookmark.swift" \
           -o "$PROJECT_DIR/scripts/make_bookmark" 2>&1
ok "make_bookmark ready"

# ─── 3. Build / refresh the corpus ────────────────────────────────────
step "Refreshing test corpus"
bash "$PROJECT_DIR/scripts/build_corpus.sh" | tail -10
CORPUS="$PROJECT_DIR/Tests/Corpus"
N_CORPUS=$(find "$CORPUS" -type f ! -name 'README.md' ! -name '.DS_Store' | wc -l | tr -d ' ')
if [ "$N_CORPUS" -lt 10 ]; then
    fail "corpus has only $N_CORPUS files (expected >= 10) — Wikipedia network issue?"
    exit 2
fi
ok "corpus has $N_CORPUS files"

# ─── 4. Wipe the DB ───────────────────────────────────────────────────
step "Wiping FileID DB"
APP_SUPPORT="$HOME/Library/Application Support/FileID"
rm -f "$APP_SUPPORT/fileid.sqlite" \
      "$APP_SUPPORT/fileid.sqlite-wal" \
      "$APP_SUPPORT/fileid.sqlite-shm"
rm -rf "$APP_SUPPORT/face_crops"
ok "DB wiped"

# ─── 5. Drive the engine ──────────────────────────────────────────────
step "Driving engine (scan + cluster)"

BOOKMARK_B64="$("$PROJECT_DIR/scripts/make_bookmark" "$CORPUS")"
if [ -z "$BOOKMARK_B64" ]; then
    fail "bookmark generation failed"
    exit 2
fi

PIPE_DIR=$(mktemp -d)
CMD_FIFO="$PIPE_DIR/cmds"
EVENT_LOG="$PIPE_DIR/events.jsonl"
mkfifo "$CMD_FIFO"

# Start engine with stdin from FIFO; engine writes events on stderr.
"$PROJECT_DIR/.build/debug/FileIDEngine" < "$CMD_FIFO" 2> "$EVENT_LOG" &
ENGINE_PID=$!
# Hold the FIFO open for writing so it doesn't EOF.
exec 3> "$CMD_FIFO"

# Always tear down the engine + FIFO on script exit.
cleanup() {
    exec 3>&- 2>/dev/null || true
    kill "$ENGINE_PID" 2>/dev/null || true
    wait "$ENGINE_PID" 2>/dev/null || true
    rm -rf "$PIPE_DIR" 2>/dev/null || true
}
trap cleanup EXIT

# ipc_send <payload-as-json>
ipc_send() {
    local payload_json="$1"
    local id
    id="$(uuidgen)"
    printf '{"id":"%s","payload":%s}\n' "$id" "$payload_json" >&3
}

# wait_for_event <event-key> <timeout-seconds>
wait_for_event() {
    local key="$1"
    local timeout="$2"
    local elapsed=0
    while [ $elapsed -lt "$timeout" ]; do
        if grep -q "\"$key\"" "$EVENT_LOG" 2>/dev/null; then
            return 0
        fi
        sleep 1
        elapsed=$((elapsed + 1))
    done
    return 1
}

if ! wait_for_event "ready" 15; then
    fail "engine never sent 'ready' event"
    tail -20 "$EVENT_LOG" 2>/dev/null
    exit 2
fi
ok "engine ready"

# IPCCommand.startScan(rootBookmark: Data, rootPathDisplay: String)
# Swift Codable synthesizes named keys when associated values are named.
ipc_send "{\"startScan\":{\"rootBookmark\":\"$BOOKMARK_B64\",\"rootPathDisplay\":\"$CORPUS\"}}"
ok "startScan sent"

if ! wait_for_event "scanComplete" 240; then
    fail "scan didn't complete within 240s"
    tail -30 "$EVENT_LOG"
    exit 2
fi
ok "scanComplete"

# Engine auto-enqueues face clustering after scan finishes — no need to
# send `runFaceClustering` manually. We just wait for the completion
# event.
if ! wait_for_event "faceClusteringComplete" 180; then
    fail "face clustering didn't complete within 180s"
    tail -30 "$EVENT_LOG"
    exit 2
fi
ok "faceClusteringComplete (auto-triggered)"

# Clean shutdown
ipc_send "\"shutdown\""
sleep 2
cleanup
trap - EXIT
ok "engine shut down"

# ─── 6. Run assertions ────────────────────────────────────────────────
step "Running assertions"
if [ -x "$PROJECT_DIR/.venv/bin/python" ]; then
    PYTHON="$PROJECT_DIR/.venv/bin/python"
else
    warn ".venv not found; falling back to system python3"
    PYTHON=python3
fi
"$PYTHON" "$PROJECT_DIR/scripts/test_assertions.py"
EXIT=$?

if [ "$EXIT" -eq 0 ]; then
    printf "\n${GREEN}${BOLD}🎉  iterate: GREEN${RESET}\n"
else
    printf "\n${RED}${BOLD}🛑  iterate: RED — fix the failing assertions above${RESET}\n"
fi
exit $EXIT
