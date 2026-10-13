#!/usr/bin/env bash
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
scratch="${1:?Usage: build-inference-runtimes.sh ABSOLUTE_ISOLATED_SCRATCH}"
[[ "$scratch" = /* ]] || { echo 'Scratch must be absolute' >&2; exit 1; }
mkdir -p "$scratch"
scratch="$(realpath "$scratch")"
case "$scratch/" in "$repo/"*) echo 'Build artifacts must remain outside the source tree' >&2; exit 1;; esac
for tool in cmake ninja g++ python3 patch sha256sum; do command -v "$tool" >/dev/null; done
if [[ ! -f /usr/include/vulkan/vulkan.h ]]; then
    echo 'Optional Vulkan headers absent; using existing CPU toolchain. No package installation.'
fi
python3 "$repo/platforms/linux/build/native-runtime-evidence.py" prepare "$repo" "$scratch"
common=(-G Ninja -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DGGML_NATIVE=OFF -DGGML_CUDA=OFF -DGGML_VULKAN=OFF)
cmake -S "$scratch/llama-source" -B "$scratch/llama-build" "${common[@]}" \
    -DLLAMA_OPENSSL=OFF -DLLAMA_BUILD_NUMBER=9254 -DLLAMA_BUILD_COMMIT=e947228222147356bc7e64154d3439e142481632 \
    -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=ON -DLLAMA_BUILD_SERVER=ON \
    -DLLAMA_BUILD_UI=OFF -DLLAMA_USE_PREBUILT_UI=OFF -DLLAMA_LLGUIDANCE=OFF \
    -DLLAMA_BUILD_BORINGSSL=OFF -DLLAMA_BUILD_LIBRESSL=OFF -DFETCHCONTENT_FULLY_DISCONNECTED=ON \
    > "$scratch/llama-configure.log" 2>&1
cmake -S "$scratch/whisper-source" -B "$scratch/whisper-build" "${common[@]}" \
    -DWHISPER_CURL=OFF -DWHISPER_BUILD_TESTS=OFF -DWHISPER_BUILD_SERVER=OFF \
    > "$scratch/whisper-configure.log" 2>&1
python3 "$repo/platforms/linux/build/native-runtime-evidence.py" check-graph "$repo" "$scratch" > "$scratch/build-graph-check.log" 2>&1
cmake --build "$scratch/llama-build" --target llama-server llama-mtmd-cli -j "${FILEID_NATIVE_BUILD_JOBS:-8}" > "$scratch/llama-build.log" 2>&1
cmake --build "$scratch/whisper-build" --target whisper-cli -j "${FILEID_NATIVE_BUILD_JOBS:-8}" > "$scratch/whisper-build.log" 2>&1
python3 "$repo/platforms/linux/build/native-runtime-evidence.py" record "$repo" "$scratch"
echo "Native CPU runtimes and auditable evidence: $scratch/native-build-evidence.json"
