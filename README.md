<p align="center">
  <img src="shared/docs/assets/FileID-Logo.png" width="380" alt="FileID">
</p>

<p align="center">
  <strong>On-device AI file organization for macOS, Windows, and Linux — plus a cross-platform CLI and TUI.</strong><br>
  <em>Tag, dedupe, restructure, and rename tens of thousands of files — privately, on hardware you own.</em>
</p>

<p align="center">
  <a href="https://webworldwide.github.io/FileID/">Website</a> ·
  <a href="#features">Features</a> ·
  <a href="#front-ends">Front-ends</a> ·
  <a href="#using-the-cli-and-tui">CLI &amp; TUI</a> ·
  <a href="#install">Install</a> ·
  <a href="shared/docs/CONTRIBUTING.md">Build from source</a>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/100%25-on--device%20%C2%B7%20no%20telemetry-green?style=flat-square">
  <img src="https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square">
</p>

<p align="center">
  <a href="https://github.com/WebWorldWide/FileID/actions/workflows/macos.yml"><img src="https://github.com/WebWorldWide/FileID/actions/workflows/macos.yml/badge.svg" alt="macOS"></a>
  <a href="https://github.com/WebWorldWide/FileID/actions/workflows/windows-engine.yml"><img src="https://github.com/WebWorldWide/FileID/actions/workflows/windows-engine.yml/badge.svg" alt="Windows engine"></a>
  <a href="https://github.com/WebWorldWide/FileID/actions/workflows/windows-app.yml"><img src="https://github.com/WebWorldWide/FileID/actions/workflows/windows-app.yml/badge.svg" alt="Windows app"></a>
  <a href="https://github.com/WebWorldWide/FileID/actions/workflows/linux.yml"><img src="https://github.com/WebWorldWide/FileID/actions/workflows/linux.yml/badge.svg" alt="Linux"></a>
</p>

---

Point FileID at a folder. It reads every file inside — images, video, PDFs, docs — and builds one searchable library that understands what's *in* them. Faces cluster into named cards. Duplicates group by perceptual hash. A local vision-language model writes captions and proposes filenames. Folder reorganization previews before anything moves on disk.

**No cloud and no telemetry.** Model downloads are explicit user actions and the release policy requires Hugging Face-only egress; remaining runtime-mirror blockers are tracked in [`shared/docs/SHIP.md`](shared/docs/SHIP.md). FileID itself is Apache-2.0. Default weights are commercially usable under their upstream terms (mostly Apache-2.0/MIT; Gemma requires separate acceptance).

---

## Quickstart

One command, every platform — from the repo root in any bash shell (Git Bash on Windows, Terminal on macOS):

```bash
./build.sh -windows                    # Windows: fresh-install build + run
./build.sh -mac                        # macOS:   build + launch
./build.sh -linux                      # Linux:   build + launch the GTK4 app
```

The platform scripts build Release and launch by default; Windows also stages a runnable copy at `~/Desktop/FileID/`. `./build.sh -windows` wipes your local install (including multi-GB model weights) — pass `--no-wipe` to iterate. Full build outputs, flags, packaging, and troubleshooting live in [`shared/docs/CONTRIBUTING.md`](shared/docs/CONTRIBUTING.md).

---

## Features

Six tabs across the three desktop apps:

- **Library** — FTS5 search over filenames + OCR, semantic CLIP search ("a dog at the beach"), RAM++ auto-tagging, thumbnail grid + preview.
- **People** — face clusters from on-device SFace embeddings; drag to merge, name a cluster once, and Deep Analyze captions use real names.
- **Cleanup** — duplicate groups by perceptual hash; trashed files stay recoverable.
- **Deep Analyze** — a local VLM (Qwen2.5-VL 7B · Gemma 3 · Mistral-Small-3.2) writes a caption + smart filename per image, PDF, video keyframe, or doc thumbnail. Linux packages do not yet bundle `llama-mtmd-cli`; Deep Analyze is unavailable in the Flatpak and requires a compatible runner visible on `PATH` for unsandboxed builds.
- **Restructure** — folder reorganization with a Sankey flow diagram; apply as reversible shortcuts, then convert to real moves when you're happy.
- **Settings** — model downloads, GPU acceleration picker, engine info, logs, privacy.

On macOS, FileID writes **real Finder tags** (the system-wide `tagNamesKey` xattrs, not a private database), so they show up in the Finder sidebar, Smart Folders, and Spotlight — and "Undo last tags" removes only the tags FileID added, never your own.

On first launch a **Welcome sheet** offers the on-device scan/search models (RAM++, CLIP ViT-B/32, YuNet + SFace). macOS/Windows also offer a Deep Analyze VLM; Linux points to its separate VLM/runtime setup. Weights download from pinned upstream Hugging Face repositories and are never redistributed by FileID. Most use Apache-2.0/MIT; restricted models such as Gemma require explicit acceptance of separate upstream terms.

---

## Front-ends

One engine, five clients — three native desktop GUIs and two terminal front-ends. None use web tech: each GUI is native to its OS. CLI/TUI query and model-free paths link the engine crate in-process; their full-ML scan path spawns the same engine binary and IPC used by the desktop clients.

| Front-end | Stack | Best for |
| --- | --- | --- |
| **macOS app** | SwiftUI · MLX · CoreML | the reference experience on Apple Silicon |
| **Windows app** | WinUI 3 · .NET 8 | Windows 10/11 + Snapdragon WoA; DirectML / CUDA / QNN |
| **Linux app** | GTK4 · libadwaita | GNOME-native desktop; the same six tabs |
| **`fileid` CLI** | Rust (links the engine) | scripting, headless servers, NAS boxes |
| **TUI** | Rust · ratatui | a terminal dashboard over the same library |

The `fileid` CLI and `fileid-tui` read and write the *same* library as the desktop apps — no app required. See [**Using the CLI and TUI**](#using-the-cli-and-tui) for build, scan, and explore steps.

macOS is the canonical visual + behavioral reference. The Windows and Linux apps implement the same six tabs; platform hardware, packaging, signing, and hosted-CI release gates remain tracked in [`shared/docs/SHIP.md`](shared/docs/SHIP.md). A library scanned on one platform opens on another (migrations are byte-faithful across engines).

---

## Using the CLI and TUI

`fileid` (CLI) and `fileid-tui` share the engine crate and the **same library** as the desktop apps. Read/query and model-free paths run in-process; full-ML scans spawn `FileIDEngine` over the canonical IPC.

**Build &amp; install.** One command from the repo root builds the engine, CLI, and TUI in release and installs `fileid`, `fileid-tui`, and the engine binary to `~/.cargo/bin` (make sure that's on your `PATH`):

```bash
bash scripts/build-tools.sh
```

**Scan a folder, then explore it — CLI.** The model-free scan indexes files + text (filenames, OCR, document text) into a searchable library — the working flow on every platform:

```bash
fileid scan ~/Pictures --db ~/fileid-test.sqlite   # index files + text (FTS) — searchable now
```

Then explore that library:

```bash
fileid people     --db ~/fileid-test.sqlite
fileid search "beach" --db ~/fileid-test.sqlite
fileid dedupe --similar --db ~/fileid-test.sqlite
fileid restructure --plan --db ~/fileid-test.sqlite
```

> **Full ML scanning** (tags + faces + CLIP) via `--models` uses the native Rust engine on all three platforms. Install the two required models with **`fileid models download mobileclip_s2 arcface`**. `--all` also installs optional multi-GB Deep Analyze models and is not required for scanning. On **macOS** CLI weights live under `~/.local/share/FileID/Models`, separate from the desktop app's CoreML set.

On macOS, omit `--db` to browse your desktop app's library automatically — the primary CLI use there. Add `--json` for machine-readable output or `--quiet` to silence progress.

**Browse and scan — TUI.** A terminal dashboard over the same library:

```bash
fileid-tui --db ~/fileid-test.sqlite
```

Keys: **s** scan a folder (type the path, `Enter`) · **r** reload after a scan · **Tab** switch tabs · **/** search · **↑↓**/**jk** navigate · **q** quit. The TUI paints its own dark theme, so it stays readable on light terminals.

The **s** scan drives the same full-ML engine as the CLI's `--models`: install the required models once (press **D** on the Settings tab, or run `fileid models download mobileclip_s2 arcface`). You can also index model-free with `fileid scan <folder>` (CLI), or browse an existing desktop-app library by pointing at it with `--db`.

**Safety.** Read-only by default. Destructive actions are gated behind explicit flags: `dedupe --apply` and `restructure --apply` only touch disk with that flag (add `--dry-run` to preview), and `dedupe --similar --apply` additionally requires `--yes`. On non-Windows systems, real Restructure moves must stay on one filesystem; cross-filesystem moves fail closed with the source untouched.

Deeper reference: [`platforms/cli/README.md`](platforms/cli/README.md) · [`platforms/tui/README.md`](platforms/tui/README.md).

---

## Install

> Clearly labeled unsigned prerelease artifacts are available on [GitHub Releases](https://github.com/WebWorldWide/FileID/releases). They are not public-trust signed. Build from source with the [Quickstart](#quickstart), or use the recipes in [`packaging/`](packaging/) ([`packaging/README.md`](packaging/README.md)).

- **Linux** — Flatpak and AppImage manifests, plus a Nix flake and AUR `PKGBUILD`; native clean-sandbox/ARM64 validation remains a release gate in `SHIP.md`.
- **Windows** — `FileIDSetup.exe` embeds per-arch **.msi** installers (x64 + ARM64) and auto-picks the right one at install; build it with `publish-bundle.ps1`.
- **macOS** — a `FileID.app` bundle for Apple Silicon; build it with `./build.sh -mac`.

---

## Architecture

Each desktop app ships **two processes** that talk newline-delimited JSON over stdio: a native **UI** (SwiftUI / WinUI 3 / GTK4) and an engine (Swift on macOS, Rust on Windows/Linux) that owns the SQLite WAL database, scan pipeline, and ML inference. The split buys crash isolation — a panic in the ML pipeline restarts the engine, not the UI. CLI/TUI share the Rust engine library for local operations and spawn the engine for full-ML scans. The IPC contract lives at [`shared/ipc-schema/ipc.schema.json`](shared/ipc-schema/), mirrored by hand-maintained Swift, Rust, and C# DTOs that per-language schema-conformance suites hold to the canonical schema.

FileID's release-approved Windows GPU path is DirectML across every vendor, with an AVX2/NEON CPU floor; CUDA / OpenVINO / QNN remain owner-provisioned development paths, not product Performance Packs. Apple Silicon uses CoreML + ANE. Full design, the GPU matrix, and the ML-model stack: [`shared/docs/ARCHITECTURE.md`](shared/docs/ARCHITECTURE.md). Build, CI, and troubleshooting detail: [`shared/docs/CONTRIBUTING.md`](shared/docs/CONTRIBUTING.md).

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
│   └── scripts/    # Shared helpers (model installers, etc.)
├── website/        # Marketing site (GitHub Pages)
├── tools/          # Repo tooling (git hooks, …)
├── scripts/        # Top-level dev/setup scripts
├── build.sh        # One-command per-platform build + run
└── README.md
```

---

## Contributing

Start with [`shared/docs/CONTRIBUTING.md`](shared/docs/CONTRIBUTING.md) — setup, build-from-source, CI gates, troubleshooting, and contribution recipes. Per-front-end conventions: [Windows](platforms/windows/CLAUDE.md) · [macOS](platforms/apple/CLAUDE.md) · [Linux](platforms/linux/CLAUDE.md) · [CLI](platforms/cli/README.md) · [TUI](platforms/tui/README.md) · [packaging](packaging/README.md). Cross-platform principles live in the root [`CLAUDE.md`](CLAUDE.md).

---

## License

**Apache-2.0** — see [`LICENSE`](LICENSE). Default model weights are commercially usable and contain no non-commercial-only set; most are Apache-2.0/MIT, while Gemma is governed by separately accepted Gemma terms. FileID downloads weights at runtime and never redistributes them; every weight remains governed by its upstream license or terms.

---

<p align="center">
  <sub>Made with <a href="https://claude.com/claude-code">Claude</a>.</sub>
</p>
