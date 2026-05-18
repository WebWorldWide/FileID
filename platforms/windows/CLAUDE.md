# FileID — Windows platform

Windows 10/11 (x64 + ARM64 / Snapdragon WoA) port of the macOS FileID app. 1:1 feature parity with macOS, native Windows UI, native Windows performance.

This file covers the Windows code under `platforms/windows/`. For the macOS reference see `platforms/apple/CLAUDE.md`. For cross-platform contracts see `shared/`.

## Stack

- **Engine**: Rust (`fileid-engine`), single-binary release `.exe` with LTO. Talks newline-delimited JSON over stdio. Owns the SQLite WAL DB, scan pipeline, ML inference.
- **App**: WinUI 3 (Windows App SDK 1.6+, .NET 8/9, C#, XAML), unpackaged desktop app. Self-contained `dotnet publish` so users don't need .NET installed.
- **Distribution**: WiX Toolset v4 → `FileID-x64.msi` and `FileID-arm64.msi`. Authenticode-signed.

## Layout

```
platforms/windows/
├── FileID.sln                        # .NET solution (added in Phase 1)
├── src/
│   ├── FileID.App/                   # WinUI 3 desktop app — Phase 1
│   ├── FileID.Theme/                 # GlassCard, LavaLamp, motion primitives — Phase 1
│   ├── FileID.IpcSchema/             # C# DTOs mirroring shared/ipc-schema/ipc.schema.json — Phase 1
│   └── engine/                       # Rust crate (this is where Phase 0 lives)
│       ├── Cargo.toml
│       ├── rust-toolchain.toml       # pin Rust 1.78, both Windows targets
│       ├── .cargo/config.toml        # AVX2/FMA on x64, NEON/dotprod on arm64
│       └── src/
│           ├── main.rs               # entrypoint; stdio loop + parent-pid watchdog + WAL checkpoint
│           ├── ipc/                  # IpcCommand / IpcEvent + sink (newline-delimited JSON over stdout)
│           ├── db/                   # rusqlite + bundled SQLite + FTS5; v1–v7 migrations byte-faithful with GRDB
│           ├── paths.rs              # %LOCALAPPDATA%/FileID/{logs,Models,thumbs,...}
│           ├── platform.rs           # parent-pid watchdog, worker_cap, memory probe, SleepGuard
│           ├── pipeline/             # discovery, tagging, dbwriter, face_clustering, deep_analyze, restructure (Phase 1+)
│           ├── models/               # ORT EP picker, ArcFace, SCRFD, MobileCLIP, CLIP text, llama.cpp wrapper (Phase 1+)
│           └── shell/                # IFileOperation, Windows.Media.Ocr, IThumbnailProvider, ... (Phase 1+)
├── installer/                        # WiX v4 .wixproj — Phase 4
├── build/
│   ├── build.ps1                     # x64 dev build (Phase 0 ships this)
│   ├── build-arm64.ps1               # ARM64 cross-compile (Phase 0)
│   ├── publish.ps1                   # release publish + sign + WiX (Phase 4)
│   └── iterate.ps1                   # regression harness — port of platforms/apple/scripts/iterate.sh (Phase 4)
└── Tests/
```

## Build (Phase 0)

```powershell
# From repo root or platforms/windows/
pwsh platforms/windows/build/build.ps1                 # x64
pwsh platforms/windows/build/build.ps1 -RunTests        # x64 + cargo test
pwsh platforms/windows/build/build-arm64.ps1            # arm64 (cross-compile)
```

Outputs `FileIDEngine.exe` under `platforms/windows/dist/<arch>/FileID/`. Phase 0 ships only the engine — the WinUI 3 app lands in Phase 1.

## What Phase 0 ships (current commit)

- Repo restructure: macOS code in `platforms/apple/`, Windows scaffolding under `platforms/windows/`, shared docs in `shared/`
- Canonical IPC schema at `shared/ipc-schema/ipc.schema.json`
- Rust engine: cargo workspace, IPC types, stdio loop, parent-PID watchdog, WAL checkpoint at shutdown, v1–v7 migrations, paths + platform helpers, structured local-only tracing
- Build scripts (x64 + ARM64 cross-compile)
- GitHub Actions CI matrix: `windows-latest` (x64) + `windows-11-arm` (ARM64) + arm64-cross. Includes a privacy gate that scans the shipped binary for telemetry-related strings.

What Phase 0 does NOT ship (deferred to Phase 1):
- WinUI 3 app (Visual Studio + Windows App SDK install gates this)
- ML pipeline (ONNX Runtime + llama.cpp wiring)
- Scan pipeline (discovery / tagging / dbwriter)
- Deep Analyze
- Restructure
- WiX MSI

## Conventions (Rust engine)

- **Edition 2021, MSRV 1.78** (pinned in `rust-toolchain.toml`).
- **No new dependencies without asking.** Locked set in `Cargo.toml`. New crates require justification in `shared/docs/DECISIONS.md`.
- **No telemetry, ever.** The only network call site is `engine/src/downloader.rs` (HuggingFace model fetch). CI grep-gates the binary for telemetry-related strings as a release blocker.
- **Path redaction in logs.** Use `redact_path_for_log(path)` (Phase 1+) before any `tracing::*!()` that includes a user file path.
- **Default to no comments.** Add only when the WHY is non-obvious (workaround, subtle invariant, performance pitfall).
- **Sync mirrors for cancellation.** Hot loops use `AtomicBool::load(Relaxed)` for cancellation checks instead of `await`-ing into the ScanCoordinator (matches macOS pattern, avoids per-file actor hops).
- **Single-writer DB.** Engine owns the only writer connection; all writes serialize through it. Reads can fan out via fresh read-only connections.

## Conventions (WinUI 3 app — Phase 1+)

- **WinUI 3 unpackaged desktop app.** No MSIX, no Microsoft Store, no `Package.appxmanifest`. Standard `<UseWinUI>true</UseWinUI>` csproj.
- **Self-contained .NET publish.** Users do not install .NET — runtime is bundled.
- **Forced dark mode.** `RequestedTheme = Dark`, plus `dwmapi DwmSetWindowAttribute(DWMWA_USE_IMMERSIVE_DARK_MODE, 1)` for the title bar.
- **Mica + Acrylic via Composition.** `MicaController` for the window backdrop, `DesktopAcrylicController` for GlassCard. Real DWM-rendered, not a software approximation.
- **Springs via `SpringScalarNaturalMotionAnimation`.** Map SwiftUI `.spring(response:dampingFraction:)` 1:1: set `Period = response`, `DampingRatio = dampingFraction`. No math port.
- **Custom canvas via Win2D.** LavaLampBackground, SankeyFlowControl, IridescentBorder all Win2D-rendered. Pause when occluded.
- **No third-party UI libraries** beyond Windows App SDK + Win2D (community toolkit only if a strong justification lands in `DECISIONS.md`).
- **Every `EngineClient.PropertyChanged` handler is wrapped in `DebugLog.SafeRun`** and logs an `[ENGINE-SUB:ClassName] {PropertyName}` debug line after its property filter. The handler nominally runs on the UI thread (because `Apply()` is dispatched there), but treat that as untrusted: post any XAML writes through `DispatcherQueue.TryEnqueue` and don't construct `DispatcherObject`-derived types (BitmapImage, SolidColorBrush, etc.) on a thread you didn't capture. A naked handler that touches a DispatcherObject is a native fast-fail in waiting (V15.2 ThumbnailService, V15.2.1 ModelSlot, V15.4 SidebarQueueList — three bugs of the same shape). The SafeRun wrap + `[APPLY:N] enter/exit` tracing in `EngineClient.Apply` are the diagnostic pair that surfaces the next variant; do not strip them.
- **Cache UI-thread-affined resources at ctor time**, not on each event. `SidebarPipelineProgress` previously allocated four `SolidColorBrush` instances on every `LastProgress` event (10 Hz during a scan) and re-evaluated three `Application.Current.Resources` lookups. Cache in fields populated from the ctor instead.
- **Never imperatively mutate a XAML parent's `Children` mid-event-burst.** `SidebarQueueList` used to nuke `JobsRepeater.ItemsSource` and rebuild a sibling on every QueueState event; under burst load this raced with the layout pass and fast-failed the renderer. Pattern: own a stable container (created lazily once), and only mutate that container's `Children`. The parent's child list never changes after first sync.

## Working principles

- User runs the build. `cargo check` / `cargo build` passing isn't proof of correctness — verify on real Windows hardware (and ARM64 hardware for arm64 builds).
- Update `shared/docs/STATE.md` (latest entry on top) and `shared/docs/NEXT.md` after meaningful work.
- Append to `shared/docs/DECISIONS.md` for non-obvious calls — cross-platform.
- Preserve the user's favorite touches: LavaLampBackground (visual), gold #FFCC00 (palette), the springs-everywhere motion language. The Windows port is a port, not a reinterpretation.

## Persistence files

See root `CLAUDE.md` and `shared/docs/`. The Windows port doesn't introduce its own persistence files; it appends to the shared ones.
