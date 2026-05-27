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
</p>

<p align="center">
  <a href="https://github.com/AdamNolle/FileID/actions/workflows/windows-engine.yml"><img src="https://github.com/AdamNolle/FileID/actions/workflows/windows-engine.yml/badge.svg" alt="Windows engine"></a>
  <a href="https://github.com/AdamNolle/FileID/actions/workflows/windows-app.yml"><img src="https://github.com/AdamNolle/FileID/actions/workflows/windows-app.yml/badge.svg" alt="Windows app"></a>
  <a href="https://github.com/AdamNolle/FileID/actions/workflows/macos.yml"><img src="https://github.com/AdamNolle/FileID/actions/workflows/macos.yml/badge.svg" alt="macOS app"></a>
</p>

---

Point FileID at a folder. It reads every file inside — images, video, PDFs, docs — and builds one searchable library that understands what's *in* them. Faces cluster into named cards. Duplicates group by perceptual hash. A local vision-language model writes captions and proposes filenames. Folder reorganization previews before anything moves on disk.

---

## Contents

**For users**
- [Quickstart](#quickstart) — get FileID running in under a minute
- [Features](#features) — what the six tabs do
- [Install](#install) — Windows + macOS download instructions

**For developers**
- [Build from source](#build-from-source) — Windows + macOS
  - [Windows](#build--windows) — engine + WinUI 3 app
  - [macOS](#build--macos) — engine + SwiftUI app
- [Repository layout](#repository-layout) — where things live
- [Architecture](#architecture) — two-binary IPC design, GPU acceleration, ML stack
- [Continuous integration](#continuous-integration) — Windows + macOS workflows + privacy gate
- [Troubleshooting](#troubleshooting) — common build / first-launch errors
- [Contributing](#contributing) — conventions + persistence files

---

## Quickstart

**One command, every platform.** From the repo root, in any bash shell (Git Bash on Windows, Terminal on macOS, anything on Linux):

```bash
./build.sh -windows         # Windows: full fresh-install build + run
./build.sh -mac             # macOS:   build + launch
./build.sh -linux           # Linux:   Phase 5 (deferred — engine builds standalone today)
```

That's the only command you need to remember. Defaults pick a sensible "I want to see this run" path: it wipes any prior install, builds Release, drops a runnable copy at `~/Desktop/FileID/`, and launches the app.

**On Windows without a bash shell?** `build.sh` is just a dispatcher — it shells out to a PowerShell script. Call that script directly from PowerShell (works in the built-in Windows PowerShell 5.1 *and* PowerShell 7):

```powershell
# From the repo root. Equivalent to ./build.sh -windows
.\platforms\windows\build\build-all.ps1 -Wipe -Release -Desktop -Run
```

> ℹ️ Use `.\platforms\windows\build\build-all.ps1`, **not** `pwsh ...`. If you copied a `pwsh` command and got `'pwsh' is not recognized`, you have Windows PowerShell 5.1 (no `pwsh` on PATH) — just drop the `pwsh` prefix and run the `.ps1` directly as shown above, or `winget install Microsoft.PowerShell` to get PowerShell 7.

> ⚠️ **`./build.sh -windows` defaults to wiping your local install.** It deletes `%LOCALAPPDATA%\FileID\` — including any downloaded model weights (multi-GB) and your scan database. Pass `--no-wipe` to iterate without re-downloading.

If `./build.sh -windows` is too aggressive (it wipes downloaded models — re-downloading is multi-GB), use `--no-wipe`:

```bash
./build.sh -windows --no-wipe       # iterate without re-downloading models
./build.sh -windows --no-run        # just build, don't launch
./build.sh -windows --debug         # debug build (faster cycle)
./build.sh --help                   # full flag list
```

**Want a release installer?** Once on Windows:

```powershell
.\platforms\windows\build\publish-bundle.ps1 -SkipSign
```

Produces `platforms\windows\dist\installer\FileIDSetup.exe` — one downloadable file that auto-detects the user's CPU (x64 or ARM64) and installs the right build. Pass `-Sign -Thumbprint <your-EV-cert-sha1>` (no angle brackets) to produce a signed release.

Detailed instructions: [Build from source](#build-from-source).

---

## Features

| Tab | What it does |
| --- | --- |
| **Library** | FTS5 search over filenames + OCR. Semantic CLIP search ("a dog at the beach"). Thumbnail grid + preview sheet. |
| **People** | Face clusters from ArcFace embeddings. Drag to merge. Name a cluster once and Deep Analyze captions use real names. |
| **Cleanup** | Duplicate groups by perceptual hash. Trashed files stay recoverable. |
| **Deep Analyze** | Local vision-language model (Qwen 2.5-VL · Gemma 3 · MiniCPM-V) writes a caption + smart filename per image, PDF, video keyframe, or doc thumbnail. |
| **Restructure** | Folder reorganization with a Sankey flow diagram. Apply as shortcuts (reversible), then convert to real moves when you're happy. |
| **Settings** | Model downloads, GPU acceleration picker, engine info, logs, privacy. |

### Platform status

macOS is the canonical reference and ships every tab end-to-end. The Windows port is feature-complete on the six tabs (Library / People / Cleanup / Deep Analyze / Restructure / Settings) and the first-run Welcome sheet — engine + IPC schema + scan pipeline + UI all wired. Release build is warning-free across both Rust and .NET; on-hardware GPU verification is ongoing. Database migrations v1–v7 are byte-faithful with macOS GRDB, so a library scanned on one platform opens on the other. Linux is deferred to Phase 5 — the Rust engine builds standalone today, but the UI port (Avalonia or GTK4) hasn't started. See `shared/docs/SHIP.md` for the per-phase breakdown.

### First launch

On first launch the **Welcome sheet** offers to install the on-device models: MobileCLIP-S2 (~210 MB, semantic search), ArcFace + SCRFD (~190 MB, face clustering), and a VLM for Deep Analyze (Qwen 2.5-VL 3B ~3 GB recommended, or pick a smaller one). Each model downloads directly from its upstream HuggingFace repo — FileID never redistributes weights. You can defer with "Skip for now" and install later from Settings → AI Models.

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

**Dev build (the unified command):**

```bash
./build.sh -windows
```

That maps to `pwsh platforms\windows\build\build-all.ps1 -Wipe -Release -Desktop -Run` and does, in order:

1. **Wipe** any prior FileID install — `~\Desktop\FileID\`, `%LOCALAPPDATA%\FileID\` (DB + models + logs), and build artifacts (`target/`, `bin/`, `obj/`, `dist/`). This is the fresh-install path; pass `--no-wipe` to iterate without losing downloaded models.
2. Probe toolchains; prints the exact `winget` install command if any are missing.
3. `cargo build --release --target x86_64-pc-windows-msvc` → `FileIDEngine.exe`.
4. `dotnet publish FileID.App --self-contained` → `FileID.exe` + companion DLLs.
5. Stage `FileIDEngine.exe` alongside `FileID.exe`.
6. Copy the publish folder to `~\Desktop\FileID\`.
7. Launch `FileID.exe`.

Want to call the underlying PowerShell script directly? Equivalent:

```powershell
.\platforms\windows\build\build-all.ps1 -Wipe -Run
```

Useful unified-script flags:

| Flag | What it does |
| --- | --- |
| (default) `-windows` | Wipe + Release + Desktop staging + Run |
| `--no-wipe` | Skip the destructive wipe (preserves models + DB) |
| `--no-run` | Build only, don't launch |
| `--no-desktop` | Build but don't stage to Desktop |
| `--debug` | Debug build (faster iteration; needs .NET SDK on host to launch) |
| `--tests` | Run cargo + dotnet tests |
| `--arm64` | Cross-compile for Snapdragon WoA |
| `--vlm-native` | Build with native llama.cpp bindings (requires cmake) |
| `--sign` | Authenticode-sign every binary (needs `FILEID_EV_THUMBPRINT` env var) |
| `--help` | Full flag list |

Underlying `build-all.ps1` flags (use directly when you want finer control):

| Flag | What it does |
| --- | --- |
| `-Wipe` | Full destructive wipe (Desktop + LocalAppData + build artifacts) |
| `-Wipe -PreserveModels` | Full wipe **except** downloaded model weights — DB + logs + settings + sentinels cleared, no multi-GB re-download |
| `-WipeDbOnly` | Lightest wipe — delete only `fileid.sqlite{,-wal,-shm}` for a fresh scan; keeps models, logs, settings, and build artifacts |
| `-Clean` | Wipe build artifacts only (cargo + dotnet + `dist/`; preserves all user data) |
| `-Desktop` | Stage to Desktop (implies `-Release`) |
| `-Run` | Launch the app after build |
| `-Release` | Release build (default for the unified script) |
| `-RunTests` | Run cargo + xUnit tests |
| `-SkipEngine` | Only rebuild the WinUI 3 app |
| `-SkipApp` | Only rebuild the Rust engine |
| `-Arm64` | Cross-compile for ARM64 |
| `-VlmNative` | Native llama.cpp bindings |
| `-Sign -Thumbprint <hex>` | Authenticode-sign every binary |

**The three iteration commands you'll reach for most** — run from the repo root in Windows Terminal. These assume PowerShell 7 (`pwsh`); on built-in Windows PowerShell 5.1 just drop the `pwsh` prefix and call `.\platforms\windows\build\build-all.ps1 …` directly. All three build **Debug** (engine + app) by default — add `-Release` for the slower self-contained build that ships, and `-Run` to launch the app when the build finishes.

```powershell
# 1. Build clean — clear build artifacts (cargo clean + dotnet clean + dist/),
#    then a full from-scratch rebuild. Your library DB and downloaded models are
#    left untouched. Use when a build is behaving stale or after switching branches.
pwsh platforms\windows\build\build-all.ps1 -Clean

# 2. Build + database wipe, keep models — incremental rebuild, then delete ONLY
#    fileid.sqlite{,-wal,-shm} so the next launch re-scans and re-tags from scratch.
#    Downloaded models (and logs/settings) survive, so nothing re-downloads.
#    Close the app first: a running engine holds the SQLite file open.
pwsh platforms\windows\build\build-all.ps1 -WipeDbOnly

# 3. Just rebuild — fast incremental build of engine + app, no wipe of anything.
pwsh platforms\windows\build\build-all.ps1
```

Examples with the optional add-ons: `... -Clean -Run` (clean rebuild then launch), `... -WipeDbOnly -Run` (fresh scan then launch), `... -Run` (rebuild then launch). Want the heavier "fresh install but don't re-download the multi-GB models" reset instead of just the DB? Use `-Wipe -PreserveModels`.

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
./build.sh -mac
```

Or call the underlying script directly:

```bash
bash platforms/apple/run.sh
```

Either builds the engine + app and launches. See `platforms/apple/CLAUDE.md` for the macOS-specific dev guide. Pass `./build.sh -mac --tests` to run `swift test` first.

### Build — Linux

```bash
./build.sh -linux
```

Linux is **deferred to Phase 5** — see [`shared/docs/SHIP.md`](shared/docs/SHIP.md). The Rust engine is cross-platform-clean and will build on Linux today; the blocker is the UI (WinUI 3 is Windows-only). Engine-only standalone build:

```bash
cd platforms/windows/src/engine && cargo build --release
```

The engine binary at `target/release/fileid-engine` is fully functional headless.

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
| VLM (Deep Analyze) | MLX (Qwen, Gemma, PaliGemma) | llama.cpp + GGUF (Vulkan/CUDA/DirectML/CPU backends) |
| PDF | PDFKit | pdfium-render |
| Video frame | AVAssetImageGenerator | Media Foundation `IMFSourceReader` |

Full mapping: [`shared/docs/ARCHITECTURE.md`](shared/docs/ARCHITECTURE.md).

### Continuous integration

Three GitHub Actions workflows run on every push + PR. All three must stay green.

| Workflow | What it runs | Matrix |
| --- | --- | --- |
| [`windows-engine.yml`](.github/workflows/windows-engine.yml) | `cargo fmt`, functional-only clippy, `cargo audit`, `cargo build --release`, `cargo test`, startup smoke (engine emits `ready` + executes a `verifyCudaPack` reprobe), telemetry-string privacy gate | x64 (`windows-latest`) · arm64-native (`windows-11-arm`) · arm64-cross |
| [`windows-app.yml`](.github/workflows/windows-app.yml) | NuGet restore (locked), `dotnet build` Debug + Release for the WinUI 3 app, IpcSchema xUnit tests | x64 + arm64 (`windows-latest`) |
| [`macos.yml`](.github/workflows/macos.yml) | SwiftPM resolve + cache, `swift build -c release` for engine + app, `swift test` (Shared + Engine tests), binary smoke, telemetry-string privacy gate | `macos-15` |

The **privacy gate** in the engine + macOS workflows scans every shipped binary for telemetry-SDK URLs (Sentry, Datadog, Firebase, Crashpad, Breakpad, and ~20 others). Zero hits required to ship — same gate `publish-bundle.ps1` enforces locally.

### State directories

User data lives outside the install dir so an uninstall doesn't wipe it. Use Settings → Advanced → "Wipe local state" when you want a fresh start.

| Path (Windows) | Path (macOS) | Contents |
| --- | --- | --- |
| `%LOCALAPPDATA%\FileID\fileid.sqlite` | `~/Library/Application Support/FileID/fileid.sqlite` | Main library DB (WAL mode) |
| `%LOCALAPPDATA%\FileID\logs\` | `~/Library/Logs/FileID/` | Engine + app logs (local-only, daily rotation) |
| `%LOCALAPPDATA%\FileID\Models\` | `~/Library/Application Support/FileID/Models/` | ONNX/CoreML weights |
| `%LOCALAPPDATA%\FileID\Models\HuggingFace\` | same parent | VLM weights (Qwen, Gemma, MiniCPM-V) |
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
| WinAppSDK runtime missing at app launch | Self-contained publish bundles it — but for non-self-contained Debug builds, install the runtime once: `winget install Microsoft.WindowsAppRuntime.1.7` (pinned in `Directory.Packages.props`). |
| Welcome sheet shows **"Failed: Couldn't download &lt;model&gt;.onnx: HTTP 404"** | An upstream HuggingFace repo was reorganized after the URL was wired. Check `shared/docs/STATE.md` for the most recent URL-refresh entry; the canonical paths live in `platforms/windows/src/engine/src/models/registry.rs` and `shared/docs/MODELS.md`. Update + rebuild the engine, restage `FileIDEngine.exe` beside `FileID.exe`, click Retry. |

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
