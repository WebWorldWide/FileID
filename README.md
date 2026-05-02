<p align="center">
  <img src="shared/docs/assets/FileID-Logo.png" width="380" alt="FileID">
</p>

<p align="center">
  <strong>On-device AI file organization. macOS today, Windows next, Linux soon.</strong><br>
  <em>Tag, dedupe, restructure, and rename tens of thousands of files — privately, on hardware you own.</em>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/macOS-15%2B-blue?style=flat-square">
  <img src="https://img.shields.io/badge/Windows-10%2F11%20%2B%20WoA-0078d4?style=flat-square">
  <img src="https://img.shields.io/badge/Linux-Phase%205-orange?style=flat-square">
  <img src="https://img.shields.io/badge/100%25-on--device-green?style=flat-square">
  <img src="https://img.shields.io/badge/0%25-telemetry-brightgreen?style=flat-square">
</p>

---

Point FileID at a folder. It reads every file inside — images, video, PDFs, docs — and builds one searchable library that understands what's *in* them. Faces cluster into named cards. Duplicates group by perceptual hash. A local vision-language model writes captions and proposes filenames. Folder reorganization previews as shortcuts before anything moves on disk.

Nothing leaves your machine.

## Platforms

| Platform | Status | Stack |
|---|---|---|
| **macOS 15+** (Apple Silicon) | Shipping | SwiftUI · MLX · CoreML · ONNX Runtime via CoreML EP. See [`platforms/apple/`](platforms/apple/) |
| **Windows 10/11** (x64 + ARM64 / Snapdragon WoA) | In progress | WinUI 3 (.NET 8/9) · Rust engine · ONNX Runtime DirectML/CUDA/OpenVINO/QNN · llama.cpp Vulkan/CUDA. See [`platforms/windows/`](platforms/windows/) |
| **Linux** (Ubuntu/Arch/Fedora/all) | Phase 5 (deferred) | Rust engine reused · UI TBD (likely Avalonia or GTK4) |

The macOS app is the canonical visual reference; Windows and Linux are 1:1 ports of every feature, with native UI primitives on each platform.

## Features

| Tab | |
| --- | --- |
| **Library** | FTS5 search over filenames + OCR. Semantic CLIP search ("a dog at the beach"). Thumbnail grid + preview sheet. |
| **People** | Face clusters from ArcFace embeddings. Drag to merge. Name them once and Deep Analyze captions use real names. |
| **Cleanup** | Duplicate groups by perceptual hash. Trashed files stay recoverable. |
| **Deep Analyze** | Local vision-language model (Qwen 2.5-VL · Gemma 3 · SmolVLM · MiniCPM-V) writes a caption + smart filename per image, PDF, video keyframe, or doc thumbnail. |
| **Restructure** | Folder reorganization with a Sankey flow diagram. Apply as shortcuts (reversible), then convert to real moves when you're happy. |
| **Settings** | Model downloads, GPU acceleration picker, engine info, logs, privacy. |

## Privacy

Zero telemetry. No analytics SDK, no crash-reporting service, no update pings, no model-download instrumentation. The only network code in the app is the user-initiated model downloader, which talks directly to the upstream HuggingFace repo for each model. CI grep-gates every shipped binary for telemetry-related strings.

See [`shared/docs/PRIVACY.md`](shared/docs/PRIVACY.md) for the full guarantees.

## Build

Per-platform — see each platform's README:

- macOS: [`platforms/apple/README.md`](platforms/apple/README.md) — `bash run.sh`
- Windows: [`platforms/windows/README.md`](platforms/windows/README.md) — `pwsh build/build.ps1` _(Phase 0+)_
- Linux: pending Phase 5

## Architecture

Two binaries per platform, talking newline-delimited JSON over stdin/stdout. The IPC contract is canonical at [`shared/ipc-schema/ipc.schema.json`](shared/ipc-schema/) and code-generated into Swift, Rust, and C# DTOs.

- **App** — native UI per platform (SwiftUI on macOS, WinUI 3 on Windows). Spawns the engine as a child process. Auto-respawns with bounded backoff on crash. Verifies the engine binary's signature before each spawn (Authenticode on Windows, codesign on macOS).
- **Engine** — owns the SQLite WAL database, scan pipeline, ML inference. Single writer; the app reads via a separate connection.

GPU acceleration is auto-picked per hardware: CUDA on NVIDIA, OpenVINO on Intel, DirectML universal fallback, QNN on Snapdragon NPU, CPU floor. Optional Performance Packs for the highest-perf vendor SDKs.

See [`shared/docs/ARCHITECTURE.md`](shared/docs/ARCHITECTURE.md).

## License

TBD. App code is yours to keep / re-license. Model weights remain governed by their upstream licenses.

---

<p align="center">
  <sub>Made with <a href="https://claude.com/claude-code">Claude</a>.</sub>
</p>
