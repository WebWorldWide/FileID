<p align="center">
  <img src="shared/docs/assets/FileID-Logo.png" width="380" alt="FileID">
</p>

<p align="center">
  <strong>On-device AI file organization for macOS, Windows, and Linux — plus a cross-platform CLI and TUI.</strong><br>
  <em>Tag, dedupe, restructure, and rename tens of thousands of files — privately, on hardware you own.</em>
</p>

<p align="center">
  <a href="https://adamnolle.github.io/FileID/">Website</a> ·
  <a href="#front-ends">Front-ends</a> ·
  <a href="#install--packaging">Install</a> ·
  <a href="#build-from-source">Build from source</a>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/macOS-15%2B-blue?style=flat-square">
  <img src="https://img.shields.io/badge/Windows-10%2F11%20%2B%20WoA-0078d4?style=flat-square">
  <img src="https://img.shields.io/badge/Linux-GTK4%20%2B%20libadwaita-success?style=flat-square">
  <img src="https://img.shields.io/badge/CLI%20%2B%20TUI-cross--platform-8957e5?style=flat-square">
  <img src="https://img.shields.io/badge/100%25-on--device%20%C2%B7%20no%20telemetry-green?style=flat-square">
</p>

<p align="center">
  <a href="https://github.com/AdamNolle/FileID/actions/workflows/windows-engine.yml"><img src="https://github.com/AdamNolle/FileID/actions/workflows/windows-engine.yml/badge.svg" alt="Windows engine"></a>
  <a href="https://github.com/AdamNolle/FileID/actions/workflows/windows-app.yml"><img src="https://github.com/AdamNolle/FileID/actions/workflows/windows-app.yml/badge.svg" alt="Windows app"></a>
  <a href="https://github.com/AdamNolle/FileID/actions/workflows/macos.yml"><img src="https://github.com/AdamNolle/FileID/actions/workflows/macos.yml/badge.svg" alt="macOS app"></a>
  <a href="https://github.com/AdamNolle/FileID/actions/workflows/linux.yml"><img src="https://github.com/AdamNolle/FileID/actions/workflows/linux.yml/badge.svg" alt="Linux (engine + CLI + TUI + GTK app)"></a>
</p>

---

Point FileID at a folder. It reads every file inside — images, video, PDFs, docs — and builds one searchable library that understands what's *in* them. Faces cluster into named cards. Duplicates group by perceptual hash. A local vision-language model writes captions and proposes filenames. Folder reorganization previews before anything moves on disk.

---

## Contents

**For users**
- [Quickstart](#quickstart) — get FileID running in under a minute
- [Features](#features) — what the six tabs do
- [Front-ends](#front-ends) — the three native apps, the CLI, and the TUI
- [Install / packaging](#install--packaging) — Flatpak / AppImage / Nix / AUR · .msi · .app

**For developers**
- [Build from source](#build-from-source) — Windows · macOS · Linux
  - [Windows](#build--windows) — engine + WinUI 3 app
  - [macOS](#build--macos) — engine + SwiftUI app
  - [Linux](#build--linux) — engine + GTK4 app + CLI + TUI
- [Repository layout](#repository-layout) — where things live
- [Architecture](#architecture) — two-binary IPC design, GPU acceleration, ML stack
- [Continuous integration](#continuous-integration) — Windows · macOS · Linux workflows + privacy gate
- [Troubleshooting](#troubleshooting) — common build / first-launch errors
- [Contributing](#contributing) — conventions + persistence files

---

## Quickstart

**One command, every platform.** From the repo root, in any bash shell (Git Bash on Windows, Terminal on macOS, anything on Linux):

```bash
./build.sh -windows                    # Windows: full fresh-install build + run
./build.sh -mac                        # macOS:   build + launch
bash platforms/linux/build/build.sh    # Linux:   build the GTK4 + libadwaita app + run
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
| **Library** | FTS5 search over filenames + OCR. Semantic CLIP search ("a dog at the beach"). RAM++ auto-tags every image with specific labels. Thumbnail grid + preview sheet. |
| **People** | Face clusters from on-device SFace embeddings. Drag to merge. Name a cluster once and Deep Analyze captions use real names. |
| **Cleanup** | Duplicate groups by perceptual hash. Trashed files stay recoverable. |
| **Deep Analyze** | Local vision-language model (Qwen2.5-VL 7B · Gemma 3 · Mistral-Small-3.2) writes a caption + smart filename per image, PDF, video keyframe, or doc thumbnail. |
| **Restructure** | Folder reorganization with a Sankey flow diagram. Apply as shortcuts (reversible), then convert to real moves when you're happy. |
| **Settings** | Model downloads, GPU acceleration picker, engine info, logs, privacy. |

### Finder tags (macOS)

FileID writes **real Finder tags** — the system-wide `tagNamesKey` xattrs, not a private database — so they show up everywhere macOS shows tags: the Finder sidebar, Smart Folders, and Spotlight (`tag:Vacation` queries). Tagging is reversible: "Undo last tags" removes only the tags FileID added, never tags you applied yourself, and the tags survive even if FileID is uninstalled.

### Platform status

macOS is the canonical visual + behavioral reference and ships every tab end-to-end. The **Windows** port (WinUI 3 / .NET) is feature-complete on the six tabs (Library / People / Cleanup / Deep Analyze / Restructure / Settings) and the first-run Welcome sheet — engine + IPC schema + scan pipeline + UI all wired; the Release build is warning-free across both Rust and .NET, with on-hardware GPU verification ongoing. The **Linux** app (GTK4 + libadwaita) is feature-complete across the same six tabs and compile-verified in CI; on-hardware polish is ongoing. Two headless front-ends — the cross-platform `fileid` **CLI** and a **ratatui TUI** — build and pass CI alongside the GUIs. Everything builds CI-green across every front-end. Database migrations are byte-faithful across platforms, so a library scanned on one platform opens on another. Every default model is permissively licensed (Apache-2.0 / MIT) — the project is commercial-clean. See `shared/docs/SHIP.md` for the per-phase breakdown.

### First launch

On first launch the **Welcome sheet** offers to install the on-device models: RAM++ (~882 MB, the image auto-tagger), CLIP ViT-B/32 (~335 MB, semantic search), YuNet + SFace (~39 MB, face detection + clustering), and a VLM for Deep Analyze (Qwen2.5-VL 7B recommended). Every default model is permissively licensed (Apache-2.0 / MIT) and downloads directly from its upstream HuggingFace repo — FileID never redistributes weights. You can defer with "Skip for now" and install later from Settings → AI Models.

---

## Front-ends

One engine, five clients — three native desktop GUIs and two headless front-ends. None use web tech: each GUI is native to its OS, and the CLI/TUI link the engine crate in-process so they can never drift from the apps.

| Front-end | Stack | Best for |
| --- | --- | --- |
| **macOS app** | SwiftUI · MLX · CoreML | the reference desktop experience on Apple Silicon |
| **Windows app** | WinUI 3 · .NET 8 | Windows 10/11 + Snapdragon WoA; DirectML / CUDA / QNN acceleration |
| **Linux app** | GTK4 · libadwaita | GNOME-native desktop; the same six tabs |
| **`fileid` CLI** | Rust (links the engine) | scripting, headless servers, NAS boxes |
| **TUI** | Rust · ratatui | a terminal dashboard over the same library |

### `fileid` CLI

A single cross-platform binary (macOS / Linux / Windows). It reads and writes the *same* library the desktop apps use, so anything you scan in the GUI is queryable from the shell and vice-versa.

```bash
fileid scan ~/Pictures                  # index a folder into the library (model-free FTS)
fileid scan ~/Pictures --models         # full ML pipeline: tags, CLIP, faces, hashes
fileid search "a dog at the beach"      # FTS5 keyword search over text + OCR
fileid search --similar 1234            # visual / semantic nearest-neighbours (CLIP)
fileid dedupe --similar                 # list perceptual near-duplicate groups
fileid dedupe --apply --yes             # keep one per group, trash the rest (recoverable)
fileid restructure --plan ~/Downloads   # preview a butler-grade reorg (read-only)
fileid restructure --apply --symlinks   # apply as reversible symlinks first
```

Add `--json` to any command for machine-readable output. Full reference: [`platforms/cli/README.md`](platforms/cli/README.md).

### TUI

```bash
cd platforms/tui && cargo run --release   # browse library / people / duplicates / restructure plan
```

A read-only ratatui dashboard over the same SQLite library — pure Rust, no system libraries. Details: [`platforms/tui/README.md`](platforms/tui/README.md).

---

## Install / packaging

> Pre-built release binaries aren't published from the repo yet — build your own with the one-command [Build from source](#build-from-source) flow, or assemble a distributable package with the recipes in [`packaging/`](packaging/) ([`packaging/README.md`](packaging/README.md)).

**Linux** — packaging targets every distribution, with **Flatpak as the primary channel** (it bundles a pinned GNOME runtime, so one build runs on Debian / Ubuntu / Arch / Gentoo / NixOS / Fedora):

| Channel | Recipe | Notes |
| --- | --- | --- |
| **Flatpak** (primary) | [`packaging/flatpak/`](packaging/flatpak/) | pinned `org.gnome.Platform` runtime; distro-agnostic |
| **AppImage** | [`packaging/appimage/`](packaging/appimage/) | single self-contained file |
| **Nix flake** | [`packaging/nix/`](packaging/nix/) | `nix build` on NixOS or any Nix |
| **AUR** | [`packaging/aur/PKGBUILD`](packaging/aur/PKGBUILD) | Arch / Manjaro |

**Windows** — `FileIDSetup.exe` embeds per-arch **.msi** installers (x64 + ARM64) and auto-picks the right one at install; build it with `publish-bundle.ps1` (see [Build — Windows](#build--windows)).

**macOS** — a `FileID.app` bundle for Apple Silicon; build it with `./build.sh -mac`.

Building from source on any platform: see [Build from source](#build-from-source) and [`shared/docs/CONTRIBUTING.md`](shared/docs/CONTRIBUTING.md).

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

The Linux front-end is a **GTK4 + libadwaita** app that shares the cross-platform Rust engine with Windows. Install the GTK toolchain, then build + run via the platform script (see [`platforms/linux/README.md`](platforms/linux/README.md) and [`platforms/linux/CLAUDE.md`](platforms/linux/CLAUDE.md)):

```bash
sudo apt install build-essential libgtk-4-dev libadwaita-1-dev   # or your distro's equivalent
bash platforms/linux/build/build.sh                              # build the GTK4 app
./platforms/linux/dist/fileid/fileid-linux                       # run it
```

The app is feature-complete across the six tabs and compile-verified in CI ([`linux.yml`](.github/workflows/linux.yml)); on-hardware polish is ongoing. The headless **CLI** and **TUI** build standalone and run anywhere:

```bash
cd platforms/cli && cargo build --release && ./target/release/fileid --help
cd platforms/tui && cargo run --release
```

To package the app for distribution (Flatpak / AppImage / Nix / AUR), see [Install / packaging](#install--packaging) and [`packaging/README.md`](packaging/README.md).

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
│   │   │   └── engine/             # Rust crate — DB + ML + scan pipeline (cross-platform)
│   │   ├── installer/
│   │   │   ├── FileID.Msi/         # Per-arch WiX v4 MSI project
│   │   │   └── FileID.Bundle/      # WiX Burn bootstrapper bundle
│   │   ├── build/
│   │   │   ├── build-all.ps1       # Dev build (engine + app + run)
│   │   │   ├── publish-bundle.ps1  # Release build (sign + MSI + bundle)
│   │   │   └── build.ps1           # Engine-only Phase 0 build
│   │   └── Tests/                  # xUnit tests for the IPC schema
│   ├── linux/                  # Linux — GTK4 + libadwaita app (shares the engine)
│   │   ├── src/                    # GTK4 app shell + six tabs
│   │   ├── data/                   # .desktop, AppStream metainfo, app icon SVG
│   │   └── build/build.sh          # Dev build (app + run)
│   ├── cli/                    # `fileid` — cross-platform CLI (links the engine in-process)
│   └── tui/                    # `fileid-tui` — ratatui terminal UI
├── packaging/                  # Linux distribution recipes
│   ├── flatpak/                    # Flatpak manifest (primary channel)
│   ├── appimage/                   # AppImage build script
│   ├── nix/                        # Nix flake
│   └── aur/                        # Arch PKGBUILD
├── shared/
│   ├── ipc-schema/             # Canonical IPC contract (JSON Schema)
│   ├── docs/                   # Architecture, decisions, models, contributing
│   ├── test-corpus/            # Cross-platform regression assertions
│   └── scripts/                # Shared helpers (model installers, etc.)
└── README.md                   # ← you are here
```

---

## Architecture

### Two binaries, one IPC contract

Each desktop app ships two processes that talk newline-delimited JSON over `stdin`/`stdout`:

- **App** (native UI per platform — SwiftUI on macOS, WinUI 3 on Windows, GTK4 + libadwaita on Linux). Spawns the engine as a child process. Auto-respawns with bounded backoff (1s/4s/16s) on crash. Verifies the engine binary's signature before each spawn (Authenticode on Windows, codesign on macOS).
- **Engine** (Rust — the same cross-platform crate on Windows and Linux; Swift on macOS). Owns the SQLite WAL database, scan pipeline, ML inference. Single writer; the app reads via a separate connection.

The IPC contract lives at [`shared/ipc-schema/ipc.schema.json`](shared/ipc-schema/) — language-neutral JSON Schema, code-generated into Swift, Rust, and C# DTOs. Schema drift = build break.

The headless front-ends take a shortcut: the **`fileid` CLI** and **TUI** link the Rust engine crate directly and call its public surface in-process (same tables, migrations, and dedupe/restructure/apply code as the IPC handlers), so they can't drift from the apps. The one exception is `fileid scan --models`, which spawns the engine binary and speaks the same newline-JSON IPC the desktop apps use.

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

All default weights are permissively licensed (Apache-2.0 / MIT). The Windows column is live; **Linux runs the same Rust engine and ONNX stack as Windows**, and macOS is adopting it (rows marked *lockstep pending* — see [`shared/docs/MODELS.md`](shared/docs/MODELS.md)).

| Capability | macOS | Windows |
| --- | --- | --- |
| Image tagging | RAM++ *(lockstep pending)* | **RAM++ Swin-L @384** (ONNX, Apache-2.0) — 4585-tag auto-tagger |
| Image embedding | CLIP ViT-B/32 *(lockstep pending)* | **CLIP ViT-B/32** (ONNX, MIT) — 512-d, byte-compatible |
| Text embedding | OpenAI CLIP text | OpenAI CLIP text (ONNX) + BPE tokenizer port |
| Face detect | Vision (`VNDetectFaceRectangles`) | **YuNet** (ONNX, MIT) |
| Face embed | SFace *(lockstep pending)* | **SFace** (ONNX, Apache-2.0, DirectML/CUDA EP) — 128-d |
| OCR | `VNRecognizeText` | `Windows.Media.Ocr` (built-in WinRT) |
| VLM (Deep Analyze) | MLX (Qwen 7B · Gemma) | llama.cpp + GGUF — Qwen2.5-VL 7B · Gemma 3 · Mistral-Small-3.2 |
| PDF | PDFKit | pdfium-render |
| Video frame | AVAssetImageGenerator | Media Foundation `IMFSourceReader` |

Full mapping: [`shared/docs/ARCHITECTURE.md`](shared/docs/ARCHITECTURE.md).

### Continuous integration

Four GitHub Actions workflows run on every push + PR. All must stay green.

| Workflow | What it runs | Matrix |
| --- | --- | --- |
| [`windows-engine.yml`](.github/workflows/windows-engine.yml) | `cargo fmt`, `clippy --all-targets -D warnings`, `cargo deny` (license + advisory), source-URL allowlist scan, `cargo build --release`, `cargo test`, startup smoke (engine emits `ready` + executes a `verifyCudaPack` reprobe), telemetry-string privacy gate | x64 (`windows-latest`) · arm64-native (`windows-11-arm`) · arm64-cross |
| [`windows-app.yml`](.github/workflows/windows-app.yml) | NuGet restore (locked), `dotnet build` Debug + Release for the WinUI 3 app, IpcSchema xUnit tests | x64 + arm64 (`windows-latest`) |
| [`macos.yml`](.github/workflows/macos.yml) | SwiftPM resolve + cache, `swift build -c release` for engine + app, `swift test` (Shared + Engine tests), binary smoke, telemetry-string privacy gate | `macos-15` |
| [`linux.yml`](.github/workflows/linux.yml) | Four jobs — **Engine** (`fmt`, `clippy -D warnings`, `build --release`, `cargo test`, telemetry scan), **CLI** (`fileid` — clippy + build + smoke test), **TUI** (`fileid-tui` — clippy + build + headless test), **GTK4 app** (`cargo build` against system GTK; clippy advisory) | `ubuntu-latest` (x64) |

The **privacy gate** in the engine, macOS, and Linux workflows scans every shipped binary for telemetry-SDK URLs (Sentry, Datadog, Firebase, Crashpad, Breakpad, and ~20 others). Zero hits required to ship — same gate `publish-bundle.ps1` enforces locally.

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

On **Linux** the same tree lives under `$XDG_DATA_HOME/FileID/` (default `~/.local/share/FileID/`) — the CLI, TUI, and GTK app all read/write this one library.

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

Start with [`shared/docs/CONTRIBUTING.md`](shared/docs/CONTRIBUTING.md). Conventions per front-end:

- Windows: [`platforms/windows/CLAUDE.md`](platforms/windows/CLAUDE.md)
- macOS: [`platforms/apple/CLAUDE.md`](platforms/apple/CLAUDE.md)
- Linux: [`platforms/linux/CLAUDE.md`](platforms/linux/CLAUDE.md)
- CLI: [`platforms/cli/README.md`](platforms/cli/README.md) · TUI: [`platforms/tui/README.md`](platforms/tui/README.md)
- Packaging: [`packaging/README.md`](packaging/README.md)

Cross-platform principles live in the root [`CLAUDE.md`](CLAUDE.md).

**Persistence files** the team updates over time:

- [`shared/docs/STATE.md`](shared/docs/STATE.md) — cross-platform session log
- [`shared/docs/NEXT.md`](shared/docs/NEXT.md) — next-session priorities + acceptance criteria
- [`shared/docs/DECISIONS.md`](shared/docs/DECISIONS.md) — append-only rationale for non-obvious calls
- [`shared/docs/SHIP.md`](shared/docs/SHIP.md) — v1.0 release-readiness inventory

---

## License

**Apache-2.0** — see [`LICENSE`](LICENSE). Every default model weight is permissively licensed (Apache-2.0 / MIT), so the project is free to be open-sourced *and* commercialized — no non-commercial weights in the shipped feature set. FileID downloads model weights at runtime and never redistributes them; they remain governed by their upstream licenses.

---

<p align="center">
  <sub>Made with <a href="https://claude.com/claude-code">Claude</a>.</sub>
</p>
