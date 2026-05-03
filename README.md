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

Point FileID at a folder. It reads every file inside — images, video, PDFs, docs — and builds one searchable library that understands what's *in* them. Faces cluster into named cards. Duplicates group by perceptual hash. A local vision-language model writes captions and proposes filenames. Folder reorganization previews before anything moves on disk.

**Nothing leaves your machine.**

---

## Contents

**For users**
- [Quickstart](#quickstart) — get FileID running in under a minute
- [Features](#features) — what the six tabs do
- [Privacy](#privacy) — what we don't do (and how it's enforced)
- [Install](#install) — Windows + macOS download instructions

**For developers**
- [Build from source](#build-from-source) — Windows + macOS
  - [Windows](#build--windows) — engine + WinUI 3 app
  - [macOS](#build--macos) — engine + SwiftUI app
- [Repository layout](#repository-layout) — where things live
- [Architecture](#architecture) — two-binary IPC design, GPU acceleration, ML stack
- [Troubleshooting](#troubleshooting) — common build / first-launch errors
- [Contributing](#contributing) — conventions + persistence files

---

## Quickstart

You're a Windows user who wants to **build and run** locally:

```powershell
# From the repo root, in any PowerShell prompt:
.\platforms\windows\build\build-all.ps1 -Desktop -Run
```

That builds everything (Rust engine + WinUI 3 app), installs the app under `%LOCALAPPDATA%\FileID-App\`, drops a `FileID` shortcut on your Desktop, and launches it. Future builds — same command. The shortcut always points at the latest build.

You're a Windows user who wants to **ship a release** to other people:

```powershell
.\platforms\windows\build\publish-bundle.ps1 -SkipSign
```

Produces `platforms\windows\dist\installer\FileIDSetup.exe` — one downloadable file that auto-detects the user's CPU (x64 or ARM64) and installs the right build. Pass `-SignThumbprint <your-EV-cert-sha1>` (no angle brackets) to produce a signed release.

You're a macOS user:

```bash
bash platforms/apple/run.sh
```

Builds the SwiftUI app + engine and launches.

Detailed instructions: [Build from source](#build-from-source).

---

## Features

| Tab | What it does |
| --- | --- |
| **Library** | FTS5 search over filenames + OCR. Semantic CLIP search ("a dog at the beach"). Thumbnail grid + preview sheet. |
| **People** | Face clusters from ArcFace embeddings. Drag to merge. Name a cluster once and Deep Analyze captions use real names. |
| **Cleanup** | Duplicate groups by perceptual hash. Trashed files stay recoverable. |
| **Deep Analyze** | Local vision-language model (Qwen 2.5-VL · Gemma 3 · SmolVLM · MiniCPM-V) writes a caption + smart filename per image, PDF, video keyframe, or doc thumbnail. |
| **Restructure** | Folder reorganization with a Sankey flow diagram. Apply as shortcuts (reversible), then convert to real moves when you're happy. |
| **Settings** | Model downloads, GPU acceleration picker, engine info, logs, privacy. |

---

## Privacy

Zero telemetry. Forever.

- **No analytics SDK.** Not Sentry, not Application Insights, not Firebase, not Segment, not Mixpanel, not Google Analytics, not anything.
- **No crash-reporting service.** Crashes write a structured log to `%LOCALAPPDATA%\FileID\logs\` (Windows) or `~/Library/Logs/FileID/` (macOS) and stay there.
- **No update pings.** No "checking for updates" call.
- **No model-download instrumentation.** The downloader fetches the bytes you asked for from HuggingFace and that's it.

The only network code in the app is the user-initiated model downloader. CI grep-gates every shipped binary for telemetry-related strings — zero hits required to ship.

Full guarantees: [`shared/docs/PRIVACY.md`](shared/docs/PRIVACY.md).

---

## Install

End users (no source needed):

| Platform | Download | Notes |
| --- | --- | --- |
| **Windows 10 22H2+ / 11** (x64 + ARM64) | `FileIDSetup.exe` (single download, auto-picks architecture) | Standard MSI install under `C:\Program Files\FileID\`. Start menu shortcut. Uninstall via Settings → Apps. |
| **macOS 15+** (Apple Silicon) | `FileID.dmg` | Drag to Applications. |

Release builds aren't yet shipping — see [Build from source](#build-from-source) below to compile your own.

---

## Build from source

### Build — Windows

**One-time setup** (~10 minutes if you don't have the toolchains):

| Tool | Version | Install |
| --- | --- | --- |
| Rust | 1.90+ | https://rustup.rs |
| .NET SDK | 8 or 9 | `winget install Microsoft.DotNet.SDK.8` |
| Visual Studio Build Tools 2022 | 17.x with UWP MSBuild component | `winget install Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Component.UWP.MSBuild"` |
| (ARM64 cross-compile only) | MSVC ARM64 toolchain | `winget install Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Component.VC.Tools.ARM64"` |

PowerShell — either built-in Windows PowerShell 5.1 or PowerShell 7 (`winget install Microsoft.PowerShell`) works.

**Dev build (debug, your own machine):**

```powershell
.\platforms\windows\build\build-all.ps1 -Desktop -Run
```

What that does, step by step:
1. Probes toolchains; prints the exact `winget` install command if any are missing.
2. `cargo build --release --target x86_64-pc-windows-msvc` → `FileIDEngine.exe`.
3. `dotnet publish FileID.App --self-contained` → `FileID.exe` plus its companion DLLs.
4. Stages `FileIDEngine.exe` alongside `FileID.exe`.
5. Installs everything under `%LOCALAPPDATA%\FileID-App\`.
6. Drops a `FileID` shortcut on your Desktop.
7. (`-Run`) launches the app.

Useful flags:

| Flag | What it does |
| --- | --- |
| `-Desktop` | Install + Desktop shortcut (recommended for "build and run" loops) |
| `-Run` | Launch the app after building |
| `-Release` | Release build (vs. default Debug). `-Desktop` implies this. |
| `-RunTests` | Also run cargo tests + xUnit tests |
| `-Clean` | Wipe build artifacts first |
| `-SkipEngine` | Only rebuild the WinUI 3 app (fast iteration) |
| `-SkipApp` | Only rebuild the Rust engine |

**Release build (one downloadable installer for everyone):**

```powershell
# Local test build (no signing)
.\platforms\windows\build\publish-bundle.ps1 -SkipSign

# Signed release - paste your EV cert thumbprint, no angle brackets
.\platforms\windows\build\publish-bundle.ps1 -SignThumbprint A1B2C3D4E5F60718293A4B5C6D7E8F90A1B2C3D4
```

Produces under `platforms\windows\dist\installer\`:

| Artifact | Audience |
| --- | --- |
| `FileIDSetup.exe` | **End users** — one download, auto-picks x64 vs ARM64 at install |
| `FileID-x64.msi` | IT admins (SCCM/Intune for x64 desktops/laptops) |
| `FileID-arm64.msi` | IT admins (Snapdragon WoA fleets) |

The `publish-bundle.ps1` script:
1. Cross-compiles the Rust engine for x64 + ARM64.
2. Publishes the WinUI 3 app for both architectures (self-contained .NET, ReadyToRun).
3. Stages the engine alongside the app in each publish dir.
4. Signs every binary (skip with `-SkipSign`).
5. Builds both per-arch MSIs via WiX v4.
6. Signs both MSIs.
7. Builds the WiX Burn bundle (`FileIDSetup.exe` with both MSIs embedded).
8. Re-signs the bundle (required because Burn re-attaches embedded MSIs at build time).
9. Smoke-checks artifact sizes + Authenticode signature validity.
10. **Privacy gate**: greps every shipped binary for telemetry strings. Zero hits required.

Pass `-SkipArm64` for an x64-only release.

### Build — macOS

```bash
bash platforms/apple/run.sh
```

That builds the engine + app and launches. See `platforms/apple/CLAUDE.md` for the macOS-specific dev guide.

---

## Repository layout

```
FileID/
├── platforms/
│   ├── apple/                  # macOS — SwiftUI / MLX / CoreML
│   ├── windows/                # Windows — WinUI 3 (.NET 8) + Rust engine
│   │   ├── src/
│   │   │   ├── FileID.App/         # WinUI 3 desktop app (C# + XAML)
│   │   │   ├── FileID.Theme/       # Reusable theme + motion primitives
│   │   │   ├── FileID.IpcSchema/   # Generated C# DTOs for the IPC contract
│   │   │   └── engine/             # Rust crate — DB + ML + scan pipeline
│   │   ├── installer/
│   │   │   ├── FileID.Msi/         # Per-arch WiX v4 MSI project
│   │   │   └── FileID.Bundle/      # WiX Burn bootstrapper bundle
│   │   ├── build/
│   │   │   ├── build-all.ps1       # Dev build (engine + app + run)
│   │   │   ├── publish-bundle.ps1  # Release build (sign + MSI + bundle)
│   │   │   └── build.ps1           # Engine-only Phase 0 build
│   │   └── Tests/                  # xUnit tests for the IPC schema
│   └── linux/                  # Phase 5 placeholder
├── shared/
│   ├── ipc-schema/             # Canonical IPC contract (JSON Schema)
│   ├── docs/                   # Architecture, decisions, models, privacy
│   ├── test-corpus/            # Cross-platform regression assertions
│   └── scripts/                # Shared helpers (model installers, etc.)
└── README.md                   # ← you are here
```

---

## Architecture

### Two binaries, one IPC contract

Every platform ships two processes that talk newline-delimited JSON over `stdin`/`stdout`:

- **App** (native UI per platform — SwiftUI on macOS, WinUI 3 on Windows). Spawns the engine as a child process. Auto-respawns with bounded backoff (1s/4s/16s) on crash. Verifies the engine binary's signature before each spawn (Authenticode on Windows, codesign on macOS).
- **Engine** (Rust on Windows, Swift on macOS). Owns the SQLite WAL database, scan pipeline, ML inference. Single writer; the app reads via a separate connection.

The IPC contract lives at [`shared/ipc-schema/ipc.schema.json`](shared/ipc-schema/) — language-neutral JSON Schema, code-generated into Swift, Rust, and C# DTOs. Schema drift = build break.

Why two binaries? **Crash isolation.** A panic in the ML pipeline (corrupted ONNX file, GPU driver bug, OOM on a huge image) kills the engine, not the UI. The app surfaces a "engine restarted" pill in the sidebar and the user keeps going. Same architecture as VS Code's renderer/extension-host split.

### GPU acceleration — every vendor

Out of the box, FileID picks the best path for the user's hardware:

| Hardware | EP / backend | Performance Pack? |
| --- | --- | --- |
| NVIDIA RTX | DirectML default; CUDA opt-in | NVIDIA CUDA Pack (~600 MB) |
| AMD | DirectML | — |
| Intel iGPU + Arc | DirectML default; OpenVINO opt-in | Intel OpenVINO Pack (~300 MB) |
| Snapdragon X Elite (WoA) | DirectML default; QNN NPU opt-in | Snapdragon NPU Pack (~150 MB) |
| Apple Silicon (macOS) | CoreML + ANE | — |
| CPU floor | AVX2/AVX-512 (x64) or NEON (arm64) | — |

DirectML covers every Windows GPU vendor in one shipped backend. Performance Packs (Settings → Performance) are user-initiated downloads that swap in the vendor-native EP for a perf bump on detected hardware.

### ML stack

| Capability | macOS | Windows |
| --- | --- | --- |
| Image embedding | MobileCLIP-S2 (CoreML) | MobileCLIP-S2 (ONNX, byte-compatible embeddings — DBs migrate cleanly) |
| Text embedding | OpenAI CLIP text (CoreML) | OpenAI CLIP text (ONNX) + BPE tokenizer port |
| Face detect | Vision (`VNDetectFaceRectangles`) | SCRFD (Buffalo bundle ONNX) |
| Face embed | ArcFace (CoreML EP) | ArcFace (DirectML/CUDA EP) |
| OCR | `VNRecognizeText` | `Windows.Media.Ocr` (built-in WinRT) |
| VLM (Deep Analyze) | MLX (Qwen, Gemma, SmolVLM, PaliGemma) | llama.cpp + GGUF (Vulkan/CUDA/DirectML/CPU backends) |
| PDF | PDFKit | pdfium-render |
| Video frame | AVAssetImageGenerator | Media Foundation `IMFSourceReader` |

Full mapping: [`shared/docs/ARCHITECTURE.md`](shared/docs/ARCHITECTURE.md).

### State directories

User data lives outside the install dir so an uninstall doesn't wipe it. Use Settings → Advanced → "Wipe local state" when you want a fresh start.

| Path (Windows) | Path (macOS) | Contents |
| --- | --- | --- |
| `%LOCALAPPDATA%\FileID\fileid.sqlite` | `~/Library/Application Support/FileID/fileid.sqlite` | Main library DB (WAL mode) |
| `%LOCALAPPDATA%\FileID\logs\` | `~/Library/Logs/FileID/` | Engine + app logs (local-only, daily rotation) |
| `%LOCALAPPDATA%\FileID\Models\` | `~/Library/Application Support/FileID/Models/` | ONNX/CoreML weights |
| `%LOCALAPPDATA%\FileID\Models\HuggingFace\` | same parent | VLM weights (Qwen, Gemma, SmolVLM, MiniCPM-V) |
| `%LOCALAPPDATA%\FileID\thumbs.cache\` | same parent | Thumbnail cache |
| `%LOCALAPPDATA%\FileID\face_crops\` | same parent | Face crop JPEGs for People view |
| `%LOCALAPPDATA%\FileID\settings.json` | same parent | Per-user settings (GPU EP override, etc.) |

---

## Troubleshooting

### Windows — build / run errors

| Symptom | Fix |
| --- | --- |
| `pwsh: command not found` | You have Windows PowerShell 5.1, not PowerShell 7. Either drop the `pwsh` prefix (`.\platforms\windows\build\build-all.ps1 ...`) or `winget install Microsoft.PowerShell`. |
| `The '<' operator is reserved for future use` | You typed a literal `<placeholder>` from a code block. PowerShell parses `<` as redirection. Strip the angle brackets, pass the value directly. |
| `cargo: command not found` | Install Rust: https://rustup.rs |
| `dotnet SDK not found` | `winget install Microsoft.DotNet.SDK.8` |
| `Microsoft.Build.Packaging.Pri.Tasks.dll missing` | VS Build Tools UWP component missing: `winget install Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Component.UWP.MSBuild"` |
| ARM64 cross-compile fails: `cl.exe not found` | `winget install Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Component.VC.Tools.ARM64"`, or pass `-SkipArm64`. |
| App launches but says **"side-by-side configuration is incorrect"** | Check `Get-WinEvent -LogName Application \| Where ProviderName -eq SideBySide` for the actual missing assembly / unsupported manifest setting. Common causes: (a) `app.manifest` declares a setting in an XML namespace the OS doesn't know (e.g. `2024/WindowsSettings` is invalid; use `2020/WindowsSettings`); (b) `Bootstrap.TryInitialize`'s major.minor in `Program.cs` doesn't match the WinAppSDK package version in `Directory.Packages.props`. |
| App launches then immediately exits with **`Microsoft.UI.Xaml.dll` faulting at `0xC000027B`** | The main app's `FileID.pri` is missing from the publish folder. `dotnet publish` strips it on .NET 8 + WinAppSDK 1.7+. The `CopyPriFilesToPublish` MSBuild target in `FileID.App.csproj` fixes this — verify with `dir "%LOCALAPPDATA%\FileID-App\FileID.pri"`. |
| App launches then exits with **`CoreMessagingXP.dll` fault** after activation | Win2D's `CanvasAnimatedControl` is incompatible with the OS build. LavaLamp uses one; if you re-enable it on Windows 11 26200+ you'll see this. Stays disabled until LavaLamp is rewritten on `Microsoft.UI.Composition`. |
| App launches but engine pill stays **"Starting…"** | `FileIDEngine.exe` isn't beside `FileID.exe`. The build script copies it automatically — verify with `dir "%LOCALAPPDATA%\FileID-App\FileIDEngine.exe"`. |
| WinAppSDK runtime missing at app launch | Self-contained publish bundles it — but for non-self-contained Debug builds, install the runtime once: `winget install Microsoft.WindowsAppRuntime.1.6` |

### macOS

See [`platforms/apple/CLAUDE.md`](platforms/apple/CLAUDE.md).

---

## Contributing

Conventions per platform live in `platforms/<platform>/CLAUDE.md`:

- Windows: [`platforms/windows/CLAUDE.md`](platforms/windows/CLAUDE.md)
- macOS: [`platforms/apple/CLAUDE.md`](platforms/apple/CLAUDE.md)

Cross-platform principles live in the root [`CLAUDE.md`](CLAUDE.md).

**Persistence files** the team updates over time:

- [`shared/docs/STATE.md`](shared/docs/STATE.md) — cross-platform session log
- [`shared/docs/NEXT.md`](shared/docs/NEXT.md) — next-session priorities + acceptance criteria
- [`shared/docs/DECISIONS.md`](shared/docs/DECISIONS.md) — append-only rationale for non-obvious calls
- [`shared/docs/SHIP.md`](shared/docs/SHIP.md) — v1.0 release-readiness inventory

---

## License

TBD. App code is yours to keep / re-license. Model weights remain governed by their upstream licenses.

---

<p align="center">
  <sub>Made with <a href="https://claude.com/claude-code">Claude</a>.</sub>
</p>
