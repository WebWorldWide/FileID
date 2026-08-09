# FileID — Windows platform

Windows 10/11 (x64 + ARM64 / Snapdragon WoA) build of FileID. 1:1 feature parity with the macOS reference, native Windows UI, native Windows performance.

Covers `platforms/windows/`. For the macOS reference see `platforms/apple/CLAUDE.md`; for cross-platform contracts + principles see the root `CLAUDE.md` and `shared/`.

## Stack

- **Engine** — Rust (`fileid-engine`), single release `.exe` (LTO). Newline-delimited JSON over stdio; owns the SQLite WAL DB, scan pipeline, and ML inference (ONNX Runtime + llama.cpp).
- **App** — WinUI 3 (Windows App SDK 1.6+, .NET 8, C#/XAML), unpackaged desktop. Self-contained `dotnet publish` — users don't install .NET.
- **Distribution** — WiX v4 → `FileID-x64.msi` / `FileID-arm64.msi`, wrapped in a Burn bundle (`FileIDSetup.exe`); Authenticode-signed.

## Layout

```
platforms/windows/
├── FileID.sln
├── src/
│   ├── FileID.App/        # WinUI 3 desktop app (C# + XAML)
│   ├── FileID.Theme/      # GlassCard, LavaLamp, Sankey, motion primitives
│   ├── FileID.IpcSchema/  # C# DTOs mirroring shared/ipc-schema/ipc.schema.json
│   └── engine/            # Rust crate
│       └── src/
│           ├── main.rs        # stdio loop + parent-PID watchdog + WAL checkpoint
│           ├── ipc/           # IpcCommand / IpcEvent + newline-JSON sink
│           ├── db/            # rusqlite + bundled SQLite + FTS5; v1–v12 migrations, GRDB-faithful
│           ├── pipeline/      # discovery, tagging, dbwriter, face/identity clustering,
│           │                  #   deep_analyze, restructure (+ restructure_semantic = butler)
│           ├── models/        # ORT EP picker, RAM++, CLIP ViT-B/32 + text, YuNet + SFace, llama.cpp
│           └── shell/         # IFileOperation, Windows.Media.Ocr, IThumbnailProvider, …
├── installer/    # WiX v4 MSI + Burn bundle
├── build/        # build-all.ps1, publish-bundle.ps1, iterate.ps1 (+ scan_assertions.py)
└── Tests/        # xUnit: FileID.App.Tests, FileID.IpcSchema.Tests
```

## Build

```powershell
# Dev build + run (from repo root). Use Windows PowerShell 5.1 or pwsh 7.
.\platforms\windows\build\build-all.ps1                 # incremental engine + app
.\platforms\windows\build\build-all.ps1 -Clean -Run      # clean rebuild then launch
.\platforms\windows\build\build-all.ps1 -WipeDbOnly      # fresh scan, keep models
.\platforms\windows\build\publish-bundle.ps1 -SkipSign   # release MSIs + FileIDSetup.exe
```

Self-verify headlessly (this is the dev-env loop): from `src/engine`, `cargo clippy --all-targets -- -D warnings` + `cargo test`; for the app, `dotnet build` / `dotnet test` / `dotnet format --verify-no-changes` on `FileID.sln`. On-hardware: `build\iterate.ps1 -Corpus <path>` drives a full scan + cluster + assertions against the RTX 2060 / `G:\TrueNAS`.

## Current status

Engine and app are both feature-complete across the six tabs. The commercial-clean / Apache-2.0 model stack is merged to `main` and CI-green, on-hardware verified (RTX 2060, DirectML):
- **Tagging:** RAM++ (Swin-L @384, 4585-tag ONNX) primary, per-class thresholds + generic-tag suppress-list; CLIP zero-shot scene tags are the fallback.
- **Search:** CLIP ViT-B/32 (512-d image + text).
- **Faces:** YuNet detect + SFace embed (128-d) + 5-point alignment; density clustering.
- **Deep Analyze (opt-in):** llama.cpp VLMs — Qwen2.5-VL 7B (default) / Gemma 3 / Mistral-Small-3.2.
- EP auto-select (CUDA / TensorRT / DirectML / OpenVINO / QNN / CPU); NVIDIA without the CUDA pack runs DirectML. Windows.Media.Ocr; pdfium; Media Foundation. Parent-PID watchdog; WAL checkpoint; local-only tracing.

In progress / not done: butler restructure P2–P4 (VLM group naming, confidence tiers, Win2D Sankey upgrade — see `shared/docs/RESTRUCTURE.md`); Authenticode EV signing; per-vendor (AMD/Intel/Snapdragon NPU) on-hardware verification; ORT CUDA Performance Pack hosting.

## Conventions — Rust engine

- **Edition 2021, MSRV 1.90** (toolchain pinned in `rust-toolchain.toml`, both Windows targets).
- **No new dependencies without asking.** Locked set in `Cargo.toml`; new crates need a `DECISIONS.md` justification.
- **No telemetry.** The only network call site is `engine/src/downloader.rs` (HuggingFace fetch). CI grep-gates the binary for telemetry strings.
- **Path redaction.** `redact_path_for_log(path)` before any `tracing::*!()` that includes a user path.
- **Default to no comments** — only a non-obvious *why* (workaround, invariant, perf pitfall).
- **Sync mirrors for cancellation.** Hot loops check `AtomicBool::load(Relaxed)` instead of `await`-ing the coordinator (no per-file actor hop).
- **Single-writer DB.** The engine owns the only writer connection; reads fan out via fresh read-only connections.
- **Migrations are append-only** and byte-faithful with macOS GRDB — never edit a committed migration; add `vN+1`.

## Conventions — WinUI 3 app

- **Unpackaged desktop app.** No MSIX / Store / `Package.appxmanifest`. Self-contained .NET publish (runtime bundled).
- **Forced dark mode** (`RequestedTheme = Dark` + `DwmSetWindowAttribute(DWMWA_USE_IMMERSIVE_DARK_MODE)`). **Mica window backdrop** (`MicaController`, falling back to `DesktopAcrylicController` when Mica is unsupported); GlassCard surfaces render a XAML `AcrylicBrush` in their template — real DWM materials, not a fake.
- **Springs via `SpringScalarNaturalMotionAnimation`** — map SwiftUI `.spring(response:dampingFraction:)` 1:1 (`Period = response`, `DampingRatio = dampingFraction`).
- **Custom rendering**: LavaLampBackground via `Microsoft.UI.Composition`, the Restructure Sankey via pure-XAML `Path`/Bézier geometry, IridescentBorder via Win2D (`CanvasSweepGradient`); pause when occluded. **No third-party UI libraries** beyond Windows App SDK + Win2D.
- **Every `EngineClient.PropertyChanged` handler is wrapped in `DebugLog.SafeRun`** and logs an `[ENGINE-SUB:ClassName] {PropertyName}` line after its filter. The handler nominally runs on the UI thread (because `Apply()` is dispatched there), but treat that as untrusted: post XAML writes through `DispatcherQueue.TryEnqueue` and never construct `DispatcherObject`-derived types (BitmapImage, SolidColorBrush, …) on a thread you didn't capture. A naked handler that touches a DispatcherObject is a native fast-fail in waiting (V15.2 ThumbnailService, V15.2.1 ModelSlot, V15.4 SidebarQueueList — three bugs of the same shape). The SafeRun wrap + `[APPLY:N] enter/exit` tracing in `EngineClient.Apply` are the diagnostic pair that surfaces the next variant — **do not strip them**.
- **Cache UI-thread-affined resources at ctor time**, not per event (`SidebarPipelineProgress` once allocated four `SolidColorBrush` per `LastProgress` at 10 Hz — cache in fields instead).
- **Never imperatively mutate a XAML parent's `Children` mid-event-burst.** Own a stable container created once and mutate only its `Children`; rebuilding a sibling per event races the layout pass and fast-fails the renderer (V15.4 `SidebarQueueList`).

## Working principles

- `cargo check`/`build` passing is not proof of correctness — self-verify with clippy + tests headlessly, then confirm the runtime/GPU path on real Windows hardware (and ARM64 hardware for arm64).
- Land on a branch, then merge to `main` and confirm both GitHub workflows are green.
- Keep `STATE.md` (newest on top) + `NEXT.md` current; append non-obvious calls to `DECISIONS.md`.
- The Windows app is a port, not a reinterpretation — preserve LavaLampBackground, the gold `#FFCC00` palette, and the springs-everywhere motion.
