# Performance Packs — status

> Windows-only. Last updated 2026-05-15.

## Status: not shipped

The CUDA / OpenVINO / QNN GPU pack download feature was **removed in V14.8.2**. The
welcome sheet no longer advertises GPU packs, and `engine/src/models/registry.rs`
no longer carries `cuda_pack_x64` / `openvino_pack_x64` / `qnn_pack_arm64` entries.
The user directive that drove the removal: "If you can't find anything remove it
cause we don't want fake features."

## Why packs were removed

None of the three GPU runtime archives can be redistributed as a single
drop-in ZIP we host:

| Vendor | Blocker |
|---|---|
| **NVIDIA (CUDA)** | Microsoft ships `onnxruntime-win-x64-cuda12-*.zip` (~150 MB) on github.com/microsoft/onnxruntime/releases — real and downloadable — but it does NOT include cuDNN. The CUDA EP requires cuDNN at LoadLibrary time. Bundling cuDNN as part of a composite pack we host would mean operating under NVIDIA's redistribution license. **Update V14.9-U → V15.1:** cuDNN is fetched directly from NVIDIA's public CDN (no composite-ZIP redistribution; we're a downstream fetcher just like with HuggingFace model weights), but as of V15.1 the fetch is user-initiated via Settings → Performance, not automatic. See `## NVIDIA scanning acceleration` below + `DECISIONS.md` 2026-05-15. The original composite-pack scenario is still deferred. |
| **Intel (OpenVINO)** | Intel publishes the OpenVINO runtime at github.com/openvinotoolkit/openvino/releases, but ORT's OpenVINO EP needs a specific Intel-built ONNX Runtime distribution that isn't redistributed as a standalone ZIP. Wiring up two parallel ORT installs to share weights is more complex than the perf gain justifies. |
| **Qualcomm (QNN)** | QNN SDK is gated behind Qualcomm's developer portal. There is no public download URL — every consumer has to register and accept terms individually. |

## What this means for users

**Scanning works on every vendor without any pack.** The engine's EP priority
chain (`engine/src/models/runtime.rs::priority_chain`) handles every detected
GPU vendor:

| Vendor | EP used | Throughput vs. native CUDA / QNN |
|---|---|---|
| NVIDIA | DirectML | ~80–90% of CUDA (see `DECISIONS.md` 2026-05-02) |
| AMD | DirectML | full DirectML speed (the recommended AMD path) |
| Intel iGPU / Arc | DirectML | full DirectML speed |
| Snapdragon X | CPU | slower; no Hexagon NPU acceleration |
| CPU-only / no GPU | CPU | AVX2/FMA optimized |

DirectML ships in Windows + bundled into ONNX Runtime, so no extra install is
ever required. CPU is the floor.

## What stays in the codebase

The plumbing for *re-introducing* packs later is intact and inert:

- `engine/src/platform.rs::register_dll_dirs_under` — walks an extracted root
  for DLLs and calls `AddDllDirectory` so the loader can find them. No-ops when
  the dir doesn't exist.
- `engine/src/main.rs` startup-replay of `%LOCALAPPDATA%\FileID\Models\packs\`
  subdirs — silently no-ops because those dirs are never created now.
- `engine/src/models/runtime.rs::is_cuda_pack_present` /
  `is_openvino_pack_present` / `is_qnn_pack_present` — filesystem-existence
  probes still wired into `RuntimeProbe`. If a power user manually drops
  CUDA / OpenVINO / QNN DLLs into the expected directory shape, the EP picker
  will use them. "Bring your own pack" is supported; "we'll download it for
  you" is not.
- `ipc.schema.json` `pack_not_available` error kind — documented; engine no
  longer emits it. Reserved for future re-introduction.

## What does still download

Two real runtime ZIPs are in the registry:

- `llama_runtime_x64` →
  `https://github.com/ggml-org/llama.cpp/releases/download/b4475/llama-b4475-bin-win-vulkan-x64.zip`
  (~80 MB, Vulkan x64). Used by Deep Analyze (VLM inference) via the
  llama-mtmd-cli subprocess. Vulkan covers NVIDIA + AMD + Intel + Adreno on
  one binary; no separate per-vendor build needed. URL is real, server is
  live, downloads work.

- `llama_runtime_cuda_x64` (added V14.8.3, opt-in via Settings → Performance
  on NVIDIA hardware) →
  `https://github.com/ggml-org/llama.cpp/releases/download/b4475/llama-b4475-bin-win-cuda-cu12.4-x64.zip`
  (~200 MB, CUDA 12.4 x64). Same llama-mtmd-cli but with the CUDA backend
  enabled — uses cuBLAS + custom CUDA kernels. Crucially, this build does
  NOT require cuDNN (cuDNN is for traditional CNN/RNN ops; llama.cpp does
  attention + matmul which use different primitives). The CUDA runtime ships
  with the NVIDIA driver, so any Win11 box from the past 2 years has it.
  Expected speedup: 15-25% vs the Vulkan build on NVIDIA.

## NVIDIA scanning acceleration

The engine's ORT-based scanning pipeline (MobileCLIP, ArcFace, SCRFD) prefers
the CUDA execution provider on NVIDIA hardware. CUDA EP needs cuDNN at
LoadLibrary time. Two paths cover this — preferring whichever is already
present:

**Path 1 (user has system CUDA Toolkit installed):** `engine/src/models/runtime.rs::system_cuda_toolkit_dir()`
searches `CUDA_PATH`, versioned variants, and `%ProgramFiles%\NVIDIA GPU
Computing Toolkit\CUDA\V*\bin\`. If found, engine startup calls
`platform::register_dll_dirs_under` on the toolkit's `bin` directory so the
LoadLibrary policy (System32 + app dir + USER_DIRS per SEC-3) can find
cuDNN.

**Path 2 (V15.1 — manual install via Settings → Performance):** if no system
toolkit is detected, the Settings → Performance panel surfaces an "Install"
button next to the "NVIDIA acceleration (cuDNN)" row. Clicking it pulls a
pinned cuDNN Windows redistributable from NVIDIA's public CDN
(`developer.download.nvidia.com/compute/cudnn/redist/cudnn/windows-x86_64/`).
The archive extracts under `%LOCALAPPDATA%\FileID\Models\cudnn\` and
`register_dll_dirs_under` is called for that path at engine startup. Engine
restart picks up the new directory automatically.

V15.1 superseded V14.9-U's silent auto-install — the policy reversal is in
`DECISIONS.md` 2026-05-15 (TL;DR: DirectML at 38 fps is sufficient for the
6 GB target, the 10-15% gain doesn't justify a silent 430 MB download +
startup-time GPU pressure during what was already a hang-prone period). The
legal framing for the fetch itself is unchanged (NVIDIA's own CDN, downstream
fetcher, same shape as HuggingFace weight downloads).

Either path lights up the same downstream behavior:
- `is_cuda_pack_present` returns true.
- `priority_chain` prepends `ExecutionProvider::Cuda` for NVIDIA hardware.
- ORT CUDA EP loads cuDNN from whichever directory got registered.

Expected speedup: 10-15% scanning throughput vs DirectML on RTX-class GPUs.
DirectML remains the default for NVIDIA users who haven't clicked Install, and
the fallback for any edge case where the CUDA EP fails to initialize.

## If we re-introduce packs later

Three preconditions per pack:

1. **Build a composite ZIP** that includes ALL DLLs the EP needs at load time
   (e.g. CUDA pack = ORT CUDA EP + cuDNN + CUDA runtime DLLs + license files).
   Authenticode-signed by the original vendor preserved.
2. **Host with a redistribution-licensed mirror.** HuggingFace dataset works
   as a host; the license file inside the ZIP carries the redistribution
   terms.
3. **Re-add the registry entry** in `engine/src/models/registry.rs` with the
   SHA256 pin + URL. Re-add the welcome-sheet pack row + `EvaluateRecommendedPack`
   in `ModelInstallerService.cs`. Re-add the Settings install button.

Until those three preconditions hold for each vendor, the feature stays
absent rather than fake.
