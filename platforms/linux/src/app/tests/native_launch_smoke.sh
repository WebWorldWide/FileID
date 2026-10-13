#!/usr/bin/env bash
set -euo pipefail
: "${FILEID_ENGINE:?Set FILEID_ENGINE to an offline-built real engine}"
: "${FILEID_LINUX_APP:?Set FILEID_LINUX_APP to an offline-built GTK app}"
test -x "$FILEID_ENGINE"
test -x "$FILEID_LINUX_APP"
fixture=$(mktemp -d /tmp/fileid-native-launch-XXXXXX)
trap 'rm -rf "$fixture"' EXIT
set +e
XDG_DATA_HOME="$fixture/state" GSK_RENDERER=cairo timeout --kill-after=2s 4s xvfb-run -a "$FILEID_LINUX_APP" >"$fixture/launch.log" 2>&1
result=$?
set -e
cat "$fixture/launch.log"
if [[ $result != 124 ]]; then
    echo "Expected a live GTK window until the smoke timeout; got exit $result" >&2
    exit 1
fi
if grep -E 'panicked at|Gtk-CRITICAL|Adwaita-CRITICAL' "$fixture/launch.log"; then
    exit 1
fi
test -s "$fixture/state/FileID/fileid.sqlite"
echo 'PASS: real GTK entrypoint stayed alive with isolated engine state; no inference or corpus mutation requested.'
