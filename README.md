<p align="center">
  <img src="shared/docs/assets/FileID-Logo.png" width="320" alt="FileID">
</p>

<h1 align="center">FileID</h1>

<p align="center">
  <strong>On-device AI file organization for macOS, Windows, and Linux — plus a cross-platform CLI and TUI.</strong><br>
  <em>Tag, dedupe, restructure, and rename tens of thousands of files — privately, on hardware you own.</em>
</p>

<p align="center">
  <a href="https://fileid.webworldwide.online/"><strong>Website</strong></a> ·
  <a href="#features">Features</a> ·
  <a href="#front-ends">Front-ends</a> ·
  <a href="#using-the-cli-and-tui">CLI &amp; TUI</a> ·
  <a href="#install">Install</a> ·
  <a href="#architecture">Architecture</a> ·
  <a href="shared/docs/CONTRIBUTING.md">Build from source</a>
</p>

<p align="center">
  <img alt="100% on-device, no telemetry" src="https://img.shields.io/badge/100%25-on--device%20%C2%B7%20no%20telemetry-2ea043?style=for-the-badge">
  <img alt="Apache-2.0" src="https://img.shields.io/badge/license-Apache--2.0-1f6feb?style=for-the-badge">
  <img alt="macOS Windows Linux" src="https://img.shields.io/badge/macOS%20%C2%B7%20Windows%20%C2%B7%20Linux-native-8957e5?style=for-the-badge">
</p>

<p align="center">
  <a href="https://github.com/WebWorldWide/FileID/actions/workflows/macos.yml"><img src="https://github.com/WebWorldWide/FileID/actions/workflows/macos.yml/badge.svg" alt="macOS"></a>
  <a href="https://github.com/WebWorldWide/FileID/actions/workflows/windows-engine.yml"><img src="https://github.com/WebWorldWide/FileID/actions/workflows/windows-engine.yml/badge.svg" alt="Windows engine"></a>
  <a href="https://github.com/WebWorldWide/FileID/actions/workflows/windows-app.yml"><img src="https://github.com/WebWorldWide/FileID/actions/workflows/windows-app.yml/badge.svg" alt="Windows app"></a>
  <a href="https://github.com/WebWorldWide/FileID/actions/workflows/linux.yml"><img src="https://github.com/WebWorldWide/FileID/actions/workflows/linux.yml/badge.svg" alt="Linux"></a>
  <a href="https://github.com/WebWorldWide/FileID/actions/workflows/policy.yml"><img src="https://github.com/WebWorldWide/FileID/actions/workflows/policy.yml/badge.svg" alt="Policy"></a>
</p>

---

Point FileID at a folder. It reads every file inside — images, video, PDFs, docs — and builds one searchable library that understands what's *in* them. Faces cluster into named cards. Duplicates group by perceptual hash. A local vision-language model writes captions and proposes filenames. Folder reorganization previews before anything moves on disk.

> **No cloud. No telemetry. Ever.**
> No analytics SDKs, no crash reporters, no update pings. The only network egress is a model download you explicitly ask for. CI scans every shipped binary against a 23-string deny-list as a release blocker.

<table>
<tr><td width="50%" valign="top">

**What it does**

- Searches by *content*, not just filename
- Groups the same person across your whole library
- Finds true and near-duplicate files
- Captions and renames with a local VLM
- Previews folder reorganization before moving anything

</td><td width="50%" valign="top">

**What it never does**

- Upload your files anywhere
- Phone home, count launches, or report crashes
- Redistribute model weights
- Move or delete anything without an explicit action
- Ship a non-commercial-only model in the default stack

</td></tr>
</table>

---

## Quickstart

One command, every platform — from the repo root in any bash shell (Git Bash on Windows, Terminal on macOS):

```bash
./build.sh -windows    # Windows: fresh-install build + run
./build.sh -mac        # macOS:   build + launch
./build.sh -linux      # Linux:   build + launch the GTK4 app
```

The platform scripts build Release and launch by default; Windows also stages a runnable copy at `~/Desktop/FileID/`.

> [!WARNING]
> `./build.sh -windows` wipes your local install — **including multi-GB model weights**. Pass `--no-wipe` to iterate.

Full build outputs, flags, packaging, and troubleshooting live in [`shared/docs/CONTRIBUTING.md`](shared/docs/CONTRIBUTING.md).

---

## Features

Six tabs, identical across all three desktop apps:

| Tab | What it gives you | Powered by |
| :-- | :-- | :-- |
| **Library** | FTS5 search over filenames + OCR, semantic search ("a dog at the beach"), auto-tagging, thumbnail grid + preview | RAM++ · CLIP ViT-B/32 · FTS5 · OCR |
| **People** | Face clusters you name once — every later caption uses real names | YuNet detect · SFace embed |
| **Cleanup** | Duplicate groups by perceptual hash; trashed files stay recoverable | dHash · pHash |
| **Deep Analyze** | A local VLM writes a caption + smart filename per image, PDF, video keyframe, or doc | Qwen2.5-VL 7B · Gemma 3 · Mistral-Small-3.2 |
| **Restructure** | Folder reorganization with a Sankey flow diagram; apply as reversible shortcuts, then convert to real moves | Semantic clustering |
| **Settings** | Model downloads, GPU acceleration picker, engine info, logs, privacy | DirectML · CoreML · AVX2/NEON floor |

**Real Finder tags on macOS.** FileID writes the system-wide `tagNamesKey` xattrs — not a private database — so tags appear in the Finder sidebar, Smart Folders, and Spotlight. "Undo last tags" removes only the tags FileID added, never your own.

**First-launch Welcome sheet.** Offers the on-device scan/search models (RAM++, CLIP ViT-B/32, YuNet + SFace); macOS/Windows also offer a Deep Analyze VLM, and Linux points to its separate VLM/runtime setup. Weights download from pinned upstream Hugging Face repositories and are never redistributed by FileID.

> Deep Analyze needs a compatible `llama-mtmd-cli`. Linux packages don't bundle one yet, so it is unavailable in the Flatpak and requires a runner on `PATH` for unsandboxed builds.

---

## Front-ends

One engine, five clients — three native desktop GUIs and two terminal front-ends. **None use web tech**: each GUI is native to its OS.

| Front-end | Stack | Best for |
| :-- | :-- | :-- |
| **macOS app** | SwiftUI · MLX · CoreML | the reference experience on Apple Silicon |
| **Windows app** | WinUI 3 · .NET 8 | Windows 10/11 + Snapdragon WoA; DirectML across every vendor |
| **Linux app** | GTK4 · libadwaita | GNOME-native desktop; the same six tabs |
| **`fileid` CLI** | Rust (links the engine) | scripting, headless servers, NAS boxes |
| **`fileid-tui`** | Rust · ratatui | a terminal dashboard over the same library |

macOS is the canonical visual + behavioral reference; the Windows and Linux apps are 1:1 ports, not reinterpretations — same gold palette, same springs, same `LavaLampBackground`. **A library scanned on one platform opens on another** (migrations are byte-faithful across the Swift and Rust engines). Platform hardware, packaging, signing, and hosted-CI release gates are tracked in [`shared/docs/SHIP.md`](shared/docs/SHIP.md).

---

## Using the CLI and TUI

`fileid` and `fileid-tui` share the engine crate and the **same library** as the desktop apps. Read/query and model-free paths run in-process; full-ML scans spawn `FileIDEngine` over the canonical IPC.

**Build & install** — one command builds the engine, CLI, and TUI in release and installs `fileid`, `fileid-tui`, and the engine binary to `~/.cargo/bin` (make sure that's on your `PATH`):

```bash
bash scripts/build-tools.sh
```

**Scan, then explore.** The model-free scan indexes files + text (filenames, OCR, document text) into a searchable library — the working flow on every platform:

```bash
fileid scan ~/Pictures --db ~/fileid-test.sqlite   # index files + text (FTS) — searchable now

fileid people                --db ~/fileid-test.sqlite
fileid search "beach"        --db ~/fileid-test.sqlite
fileid dedupe --similar      --db ~/fileid-test.sqlite
fileid restructure --plan    --db ~/fileid-test.sqlite
```

> **Full ML scanning** (tags + faces + CLIP) via `--models` uses the native Rust engine on all three platforms. Install the two required models with `fileid models download mobileclip_s2 arcface`. `--all` also installs optional multi-GB Deep Analyze models and is not required for scanning. On **macOS**, CLI weights live under `~/.local/share/FileID/Models`, separate from the desktop app's CoreML set.

On macOS, omit `--db` to browse your desktop app's library automatically. Add `--json` for machine-readable output or `--quiet` to silence progress.

**Terminal dashboard:**

```bash
fileid-tui --db ~/fileid-test.sqlite
```

Keys: **s** scan a folder · **r** reload · **Tab** switch tabs · **/** search · **↑↓**/**jk** navigate · **q** quit. The TUI paints its own dark theme, so it stays readable on light terminals.

### Safety

Read-only by default. Destructive actions are gated behind explicit flags:

- `dedupe --apply` and `restructure --apply` only touch disk with that flag (`--dry-run` previews)
- `dedupe --similar --apply` additionally requires `--yes`
- On non-Windows systems, real Restructure moves must stay on one filesystem; cross-filesystem moves **fail closed with the source untouched**

Deeper reference: [`platforms/cli/README.md`](platforms/cli/README.md) · [`platforms/tui/README.md`](platforms/tui/README.md).

---

## Install

> [!NOTE]
> Clearly labeled **unsigned** prerelease artifacts are published on [GitHub Releases](https://github.com/WebWorldWide/FileID/releases). They are not public-trust signed. Building from source (see [Quickstart](#quickstart)) remains the recommended path, or use the recipes in [`packaging/`](packaging/).

| Platform | Format | Notes |
| :-- | :-- | :-- |
| **Windows** | `FileIDSetup.exe` | Burn bundle embedding per-arch **.msi** (x64 + ARM64); auto-picks at install. Build with `publish-bundle.ps1`. |
| **Linux** | Flatpak · AppImage · Nix flake · AUR `PKGBUILD` | Native clean-sandbox/ARM64 validation remains a release gate in `SHIP.md`. |
| **macOS** | `FileID.app` (Apple Silicon) | Build with `./build.sh -mac`; no published bundle yet. |

---

## Architecture

Each desktop app ships **two processes** that talk newline-delimited JSON over stdio:

```
┌─────────────────────────┐        ┌──────────────────────────────┐
│  UI                     │  JSON  │  Engine                      │
│  SwiftUI / WinUI 3 /    │ ◄────► │  Swift (macOS)               │
│  GTK4                   │ stdio  │  Rust  (Windows · Linux)     │
│                         │        │  SQLite WAL · single writer  │
│  respawns the engine    │        │  scan pipeline · ML inference│
└─────────────────────────┘        └──────────────────────────────┘
```

The split buys **crash isolation** — a panic in the ML pipeline restarts the engine, not the UI. The CLI/TUI link the Rust engine library for local operations and spawn the engine for full-ML scans.

The IPC contract lives at [`shared/ipc-schema/ipc.schema.json`](shared/ipc-schema/), mirrored by hand-maintained Swift, Rust, and C# DTOs that per-language schema-conformance suites hold to the canonical schema. **Schema drift is a build break.**

FileID's release-approved Windows GPU path is **DirectML** across every vendor, with an AVX2/NEON CPU floor; CUDA / OpenVINO / QNN remain owner-provisioned development paths, not product Performance Packs. Apple Silicon uses CoreML + ANE.

Full design, the GPU matrix, and the ML-model stack: [`shared/docs/ARCHITECTURE.md`](shared/docs/ARCHITECTURE.md). Build, CI, and troubleshooting: [`shared/docs/CONTRIBUTING.md`](shared/docs/CONTRIBUTING.md).

---

## Repository layout

```
FileID/
├── platforms/
│   ├── apple/      # macOS — SwiftUI / MLX / CoreML
│   ├── windows/    # Windows — WinUI 3 (.NET 8) + Rust engine
│   ├── linux/      # Linux — GTK4 + libadwaita app (shares the engine)
│   ├── cli/        # `fileid` — cross-platform CLI (links the engine)
│   └── tui/        # `fileid-tui` — ratatui terminal UI
├── packaging/      # Linux distribution recipes (Flatpak / AppImage / Nix / AUR)
├── shared/
│   ├── ipc-schema/ # Canonical IPC contract (JSON Schema)
│   ├── docs/       # Architecture, decisions, models, contributing
│   ├── models/     # Model export/registry helpers
│   ├── security/   # Pinned TLS roots for model downloads
│   ├── test-corpus/# Cross-platform regression assertions
│   └── scripts/    # Shared helpers + repository policy gates
├── website/        # Marketing site (GitHub Pages)
├── tools/          # Repo tooling (git hooks, …)
├── scripts/        # Top-level dev/setup scripts
├── build.sh        # One-command per-platform build + run
└── README.md
```

---

## Contributing

Start with [`CONTRIBUTING.md`](CONTRIBUTING.md) for the short version, then [`shared/docs/CONTRIBUTING.md`](shared/docs/CONTRIBUTING.md) for the full guide — setup, build-from-source, CI gates, troubleshooting, and contribution recipes.

Found a security problem? Please don't open a public issue — see [`SECURITY.md`](SECURITY.md).

Per-front-end conventions: [Windows](platforms/windows/CLAUDE.md) · [macOS](platforms/apple/CLAUDE.md) · [Linux](platforms/linux/CLAUDE.md) · [CLI](platforms/cli/README.md) · [TUI](platforms/tui/README.md) · [packaging](packaging/README.md). Cross-platform principles live in the root [`CLAUDE.md`](CLAUDE.md).

---

## License

**Apache-2.0** — see [`LICENSE`](LICENSE).

Default model weights are commercially usable and contain no non-commercial-only set; most are Apache-2.0/MIT, while Gemma is governed by separately accepted Gemma Terms. FileID downloads weights at runtime and **never redistributes them**; every weight remains governed by its upstream license or terms. See [`shared/docs/MODELS.md`](shared/docs/MODELS.md) for the canonical registry.

---

<p align="center">
  <sub>Made with <a href="https://claude.com/claude-code">Claude</a>.</sub>
</p>
