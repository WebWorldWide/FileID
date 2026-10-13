#!/usr/bin/env bash
set -euo pipefail

fixture='tabs::deep_analyze::tests::deep_owned_failure_survives_cancel_request_and_transport_terminals'
binary="${1-}"
if [[ $# != 1 || "$binary" != /* || ! -f "$binary" || ! -x "$binary" || -L "$binary" ]]; then
    echo "Usage: bash $0 /absolute/path/to/already-built-fileid-linux-test-binary" >&2
    echo 'Requires an existing Linux GTK display. No build, engine acquisition or display launcher is performed.' >&2
    exit 2
fi
umask 077
root=$(mktemp -d /tmp/fileid-deep-status-fixture.XXXXXXXX)
identity=''
cleanup() {
    result=$?
    if [[ $result != 0 ]]; then
        echo "Fixture preparation/execution failed; generated evidence retained at: $root" >&2
        return
    fi
    if [[ "$root" == /tmp/fileid-deep-status-fixture.* && -d "$root" && ! -L "$root" \
        && -n "$identity" && "$(stat -c '%d:%i' -- "$root")" == "$identity" \
        && -f "$root/.fileid-deep-status-fixture" && ! -L "$root/.fileid-deep-status-fixture" \
        && "$(cat "$root/.fileid-deep-status-fixture")" == 'fileid-deep-status-fixture-v1' ]]; then
        rm -rf -- "$root"
    else
        echo "Cleanup ownership could not be verified; generated state retained at: $root" >&2
        exit 1
    fi
}
trap cleanup EXIT
identity=$(stat -c '%d:%i' -- "$root")
mkdir -- "$root/xdg" "$root/xdg/FileID" "$root/config" "$root/cache" "$root/state" "$root/home" "$root/models"
set -o noclobber
printf 'fileid-deep-status-fixture-v1\n' > "$root/.fileid-deep-status-fixture"
printf '{}\n' > "$root/xdg/FileID/app-settings.json"
settings="$root/xdg/FileID/app-settings.json"
if [[ -L "$settings" || ! -f "$settings" || "$(cat "$settings")" != '{}' ]]; then
    echo 'Empty generated app-settings precondition failed; no test binary was invoked.' >&2
    exit 1
fi

# The process receives isolation before any GTK or FileID initialization.
env -u FILEID_ENGINE -u FILEID_LLAMA_SERVER -u FILEID_LLAMA_CLI -u FILEID_WHISPER_CLI \
    -u FILEID_FFMPEG -u FILEID_FFPROBE -u LOCALAPPDATA \
    FILEID_DEEP_STATUS_FIXTURE_ROOT="$root" XDG_DATA_HOME="$root/xdg" \
    XDG_CONFIG_HOME="$root/config" XDG_CACHE_HOME="$root/cache" XDG_STATE_HOME="$root/state" \
    HOME="$root/home" FILEID_MODELS_DIR="$root/models" GSETTINGS_BACKEND=memory \
    "$binary" --exact "$fixture" --ignored --test-threads=1 --nocapture \
    2>&1 | tee "$root/fixture-output.log"
if ! grep -Fq "test $fixture ... " "$root/fixture-output.log" \
    || ! grep -Eq '^test result: ok\. 1 passed; 0 failed; 0 ignored;' "$root/fixture-output.log"; then
    echo 'The supplied binary did not report the exact fixture as passed; no zero-test pass is accepted.' >&2
    exit 1
fi
