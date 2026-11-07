#!/usr/bin/env bash
# Builds the llama.cpp runtime (llama-server + ggml backends) that Linux
# packages ship for Deep Analyze, speech and chat generation.
#
# Offline: the caller supplies the pinned source archive (Flatpak sources,
# build-appimage.sh, or CI fetch it). This script never downloads anything; it
# verifies the archive, the files the FileID patch touches, and the vendored
# license sources against pinned SHA-256 sums before building.
#
# Usage:
#   build-llama-runtime.sh --archive /abs/llama.cpp-b9254.tar.gz \
#     --output /abs/empty/dir [--vulkan] [--jobs N]
set -euo pipefail

readonly LLAMA_BUILD=9254
readonly LLAMA_COMMIT=e947228222147356bc7e64154d3439e142481632
readonly LLAMA_ARCHIVE_URL="https://codeload.github.com/ggml-org/llama.cpp/tar.gz/${LLAMA_COMMIT}"
readonly LLAMA_ARCHIVE_SHA256=b45827df371f9a418680f0137f58a3a27e0e577b4c5e99e54519eea28be67241

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PATCH_FILE="${SCRIPT_DIR}/patches/llama-b${LLAMA_BUILD}-no-outbound.patch"

fail() { echo "build-llama-runtime: $*" >&2; exit 1; }
usage() {
  echo "Usage: build-llama-runtime.sh --archive ABSOLUTE_TARBALL --output ABSOLUTE_EMPTY_DIR [--vulkan] [--jobs N]" >&2
  echo "Pinned source: ${LLAMA_ARCHIVE_URL} (sha256 ${LLAMA_ARCHIVE_SHA256})" >&2
  exit 2
}

archive=""
output=""
vulkan=OFF
jobs="$(nproc 2>/dev/null || echo 2)"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --archive) [ "$#" -ge 2 ] || usage; archive="$2"; shift 2 ;;
    --output) [ "$#" -ge 2 ] || usage; output="$2"; shift 2 ;;
    --vulkan) vulkan=ON; shift ;;
    --jobs) [ "$#" -ge 2 ] || usage; jobs="$2"; shift 2 ;;
    *) usage ;;
  esac
done
[ -n "$archive" ] && [ -n "$output" ] || usage
[[ "$archive" = /* && "$output" = /* ]] || fail "--archive and --output must be absolute"
[[ "$jobs" =~ ^[1-9][0-9]*$ ]] || fail "--jobs must be a positive integer"
[ "$(uname -s)" = Linux ] || fail "Linux only"
for tool in cmake ninja c++ patch sha256sum tar; do
  command -v "$tool" >/dev/null || fail "missing build tool: $tool"
done
[ -f "$archive" ] && [ ! -L "$archive" ] || fail "source archive is not a regular file: $archive"
if [ -e "$output" ] || [ -L "$output" ]; then
  [ -d "$output" ] && [ ! -L "$output" ] && [ -z "$(ls -A "$output")" ] || fail "--output must be a new or empty directory"
fi

printf '%s  %s\n' "$LLAMA_ARCHIVE_SHA256" "$archive" | sha256sum --check --status ||
  fail "source archive does not match the pinned llama.cpp b${LLAMA_BUILD} SHA-256"

work="$(mktemp -d "${TMPDIR:-/tmp}/fileid-llama.XXXXXX")"
trap 'rm -rf "$work"' EXIT
src="$work/src"
build="$work/build"
mkdir "$src"
tar -xzf "$archive" -C "$src" --strip-components=1 --no-same-owner --no-same-permissions

( cd "$src" && sha256sum --check --strict --quiet ) <<'EOF' || fail "upstream source does not match pinned file hashes"
94f29bbed6a22c35b992c5c6ebf0e7c92f13b836b90f36f461c9cf2f0f1d010d  LICENSE
4b45cbe16d7b71b89ae6127e26e0d90a029198ca5e958ad8e3d0b8bbed364d8b  vendor/cpp-httplib/LICENSE
c0d068392ea65358b798b8c165103560f06e9e3b38c4ab4e2d8810a7b931af86  licenses/LICENSE-jsonhpp
594c2fe35d49488b4382dbfaec8f98366defca819d916ac95becf3e75f4200b3  vendor/stb/stb_image.h
ac7af4de748b7e26b777f37e01cee313a308a7296a3eb080e2906b320cc55c89  vendor/miniaudio/miniaudio.h
0bf208a408ba2c7e63739d62a0a492a13f90b0113214776835c855629ef90043  vendor/sheredom/subprocess.h
166e8ce85e6293423a0ca772fb18078b7c9f80de9736462fcc46800e78459451  common/http.h
0f55b4a7f637bb47624c90b62a94006ff83170168666df8233bc3c6a5cf1ba07  tools/server/server-models.cpp
ecc8c9d994de0506f07bd1684d708f0d23f95a9dbbbf4a64a11e64418dea34f6  vendor/cpp-httplib/httplib.cpp
EOF

patch -d "$src" -p1 -F0 -N -t -i "$PATCH_FILE" >/dev/null || fail "no-outbound patch did not apply cleanly"
( cd "$src" && sha256sum --check --strict --quiet ) <<'EOF' || fail "patched source does not match pinned file hashes"
d5a6e71ebf436e1de38da0282e0fe9e400b20597fa17fab3a0a383812f9e3a38  common/http.h
65d6e0f93e8625b15e2e99e77e06335ec8bb0255105141ad2271e9f1d810dda4  tools/server/server-models.cpp
c386e956260264b4c42413763b74d3e5453388e6ecb6c7b74368bddffac04151  vendor/cpp-httplib/httplib.cpp
EOF

options=(
  -DCMAKE_BUILD_TYPE=Release
  -DBUILD_SHARED_LIBS=ON
  "-DCMAKE_INSTALL_RPATH=\$ORIGIN"
  -DCMAKE_BUILD_WITH_INSTALL_RPATH=ON
  -DGGML_NATIVE=OFF
  -DGGML_BACKEND_DL=ON
  -DGGML_CPU_ALL_VARIANTS=ON
  -DGGML_OPENMP=OFF
  -DGGML_CPU_KLEIDIAI=OFF
  -DGGML_RPC=OFF
  -DGGML_CUDA=OFF
  "-DGGML_VULKAN=${vulkan}"
  -DLLAMA_BUILD_NUMBER="${LLAMA_BUILD}"
  -DLLAMA_BUILD_COMMIT="${LLAMA_COMMIT}"
  -DLLAMA_BUILD_TESTS=OFF
  -DLLAMA_BUILD_EXAMPLES=OFF
  -DLLAMA_BUILD_TOOLS=ON
  -DLLAMA_BUILD_SERVER=ON
  -DLLAMA_BUILD_UI=OFF
  -DLLAMA_USE_PREBUILT_UI=OFF
  -DLLAMA_LLGUIDANCE=OFF
  -DLLAMA_OPENSSL=OFF
  -DLLAMA_BUILD_BORINGSSL=OFF
  -DLLAMA_BUILD_LIBRESSL=OFF
  -DFETCHCONTENT_FULLY_DISCONNECTED=ON
)
cmake -S "$src" -B "$build" -G Ninja "${options[@]}"

for option in "${options[@]}"; do
  [[ "$option" == -D*=* ]] || continue
  key="${option#-D}"; key="${key%%=*}"
  value="${option#*=}"
  grep -Eq "^${key}:[A-Z]+=${value//\$/\\\$}\$" "$build/CMakeCache.txt" ||
    fail "configured cache does not hold ${key}=${value}"
done
if grep -Eq 'ui-download\.cmake|download-models\.cmake|llguidance_ext|KleidiAI_Download|_deps/(boringssl|libressl)' "$build/build.ninja"; then
  fail "build graph contains a download step"
fi

mapfile -t backends < <(ninja -C "$build" -t targets all | sed -n -E 's/^(ggml-cpu-[A-Za-z0-9_.]+|ggml-vulkan): phony$/\1/p' | sort -u)
cpu_variants=0
for target in "${backends[@]}"; do [[ "$target" == ggml-cpu-* ]] && cpu_variants=$((cpu_variants + 1)); done
[ "$cpu_variants" -gt 0 ] || fail "no CPU backend variants were configured"
if [ "$vulkan" = ON ]; then
  printf '%s\n' "${backends[@]}" | grep -qx ggml-vulkan || fail "Vulkan backend target missing"
fi
cmake --build "$build" --parallel "$jobs" --target llama-server "${backends[@]}"

mkdir -p "$output"
cp -P "$build/bin/llama-server" "$output/"
for library in "$build"/bin/lib*.so*; do
  cp -P "$library" "$output/"
done
[ -x "$output/llama-server" ] || fail "llama-server was not built"
compgen -G "$output/libggml-cpu-*.so" >/dev/null || fail "CPU backend libraries were not built"
if [ "$vulkan" = ON ]; then
  [ -f "$output/libggml-vulkan.so" ] || fail "Vulkan backend library was not built"
fi

cp "$src/LICENSE" "$output/LICENSE"
{
  echo "FileID bundles llama.cpp b${LLAMA_BUILD} (commit ${LLAMA_COMMIT}) as llama-server and its ggml libraries."
  echo "Source: ${LLAMA_ARCHIVE_URL}"
  echo "Archive SHA-256: ${LLAMA_ARCHIVE_SHA256}"
  echo "Built by FileID's platforms/linux/build/build-llama-runtime.sh with backends: CPU$([ "$vulkan" = ON ] && echo ', Vulkan')."
  echo "FileID applies platforms/linux/build/patches/llama-b${LLAMA_BUILD}-no-outbound.patch, which disables"
  echo "outbound HTTP clients (model download, remote media and router proxy). The loopback inference server remains."
  echo
  echo "==== llama.cpp and ggml (MIT) ===="
  cat "$src/LICENSE"
  echo
  echo "==== cpp-httplib (MIT), vendor/cpp-httplib ===="
  cat "$src/vendor/cpp-httplib/LICENSE"
  echo
  echo "==== nlohmann/json (MIT), vendor/nlohmann ===="
  cat "$src/licenses/LICENSE-jsonhpp"
  echo
  echo "==== stb_image (MIT or public domain), vendor/stb ===="
  sed -n '/^This software is available under 2 licenses/,/^\*\//p' "$src/vendor/stb/stb_image.h"
  echo
  echo "==== miniaudio (public domain or MIT No Attribution), vendor/miniaudio ===="
  sed -n '/^This software is available as a choice of the following licenses/,/^\*\//p' "$src/vendor/miniaudio/miniaudio.h"
  echo
  echo "==== sheredom/subprocess.h (Unlicense), vendor/sheredom ===="
  sed -n '1,/unlicense\.org/p' "$src/vendor/sheredom/subprocess.h"
} > "$output/THIRD-PARTY-NOTICES.txt"

version="$("$output/llama-server" --version 2>&1)" || fail "llama-server --version failed"
[[ "$version" == *"version: ${LLAMA_BUILD} (${LLAMA_COMMIT})"* ]] || fail "unexpected llama-server version: $version"

(
  cd "$output"
  find . -maxdepth 1 -type f -printf '%P\n' | LC_ALL=C sort | xargs sha256sum > "$work/SHA256SUMS"
  mv "$work/SHA256SUMS" SHA256SUMS
  sha256sum --check --strict --quiet SHA256SUMS
)
echo "Staged llama.cpp b${LLAMA_BUILD} runtime (Vulkan ${vulkan}) in ${output}"
