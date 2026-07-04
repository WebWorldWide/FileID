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

**No cloud, no telemetry, ever.** The only network egress is user-initiated model downloads from HuggingFace. Apache-2.0, and every default model weight is permissively licensed — so FileID is free to be open-sourced *and* commercialized.

---

## Quickstart

One command, every platform — from the repo root in any bash shell (Git Bash on Windows, Terminal on macOS):

```bash
./build.sh -windows                    # Windows: fresh-install build + run
./build.sh -mac                        # macOS:   build + launch
bash platforms/linux/build/build.sh    # Linux:   build the GTK4 app + run
```

Defaults build Release, stage a runnable copy at `~/Desktop/FileID/`, and launch. `./build.sh -windows` wipes your local install (including multi-GB model weights) — pass `--no-wipe` to iterate. Full build steps, flag tables, release packaging, and troubleshooting live in [`shared/docs/CONTRIBUTING.md`](shared/docs/CONTRIBUTING.md).

---

## Features

Six tabs, shared across every front-end:

- **Library** — FTS5 search over filenames + OCR, semantic CLIP search ("a dog at the beach"), RAM++ auto-tagging, thumbnail grid + preview.
- **People** — face clusters from on-device SFace embeddings; drag to merge, name a cluster once, and Deep Analyze captions use real names.
- **Cleanup** — duplicate groups by perceptual hash; trashed files stay recoverable.
- **Deep Analyze** — a local VLM (Qwen2.5-VL 7B · Gemma 3 · Mistral-Small-3.2) writes a caption + smart filename per image, PDF, video keyframe, or doc thumbnail.
- **Restructure** — folder reorganization with a Sankey flow diagram; apply as reversible shortcuts, then convert to real moves when you're happy.
- **Settings** — model downloads, GPU acceleration picker, engine info, logs, privacy.

On macOS, FileID writes **real Finder tags** (the system-wide `tagNamesKey` xattrs, not a private database), so they show up in the Finder sidebar, Smart Folders, and Spotlight — and "Undo last tags" removes only the tags FileID added, never your own.

On first launch a **Welcome sheet** offers to install the on-device models (RAM++, CLIP ViT-B/32, YuNet + SFace, and a VLM for Deep Analyze). Every default weight is permissively licensed (Apache-2.0 / MIT) and downloads directly from its upstream HuggingFace repo — FileID never redistributes weights. Defer with "Skip for now" and install later from Settings → AI Models.

---

## Front-ends

One engine, five clients — three native desktop GUIs and two headless front-ends. None use web tech: each GUI is native to its OS, and the CLI/TUI link the engine crate in-process so they can never drift from the apps.

| Front-end | Stack | Best for |
| --- | --- | --- |
| **macOS app** | SwiftUI · MLX · CoreML | the reference experience on Apple Silicon |
| **Windows app** | WinUI 3 · .NET 8 | Windows 10/11 + Snapdragon WoA; DirectML / CUDA / QNN |
| **Linux app** | GTK4 · libadwaita | GNOME-native desktop; the same six tabs |
| **`fileid` CLI** | Rust (links the engine) | scripting, headless servers, NAS boxes |
| **TUI** | Rust · ratatui | a terminal dashboard over the same library |

The `fileid` CLI and `fileid-tui` read and write the *same* library as the desktop apps — no app required. See [**Using the CLI and TUI**](#using-the-cli-and-tui) for build, scan, and explore steps.

macOS is the canonical visual + behavioral reference and ships every tab end-to-end; the Windows and Linux apps are feature-complete on the same six tabs and CI-green, with on-hardware polish ongoing. A library scanned on one platform opens on another (migrations are byte-faithful across engines). Per-phase status: [`shared/docs/SHIP.md`](shared/docs/SHIP.md).

---

## Using the CLI and TUI

`fileid` (CLI) and `fileid-tui` link the engine in-process and read/write the **same library** as the desktop apps — handy for scripting, headless servers, or a quick scan straight from a terminal.

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

> **Full ML scanning** (tags + faces + CLIP) via `--models` works on **all three platforms** (native Rust engine). Install the engine's own models once with **`fileid models download --all`** — on **macOS** these install under `~/.local/share/FileID/Models`, separate from the desktop app's CoreML set. (Scanning with full ML in the FileID desktop app and exploring that library here also works.)

On macOS, omit `--db` to browse your desktop app's library automatically — the primary CLI use there. Add `--json` for machine-readable output or `--quiet` to silence progress.

**Browse and scan — TUI.** A terminal dashboard over the same library:

```bash
fileid-tui --db ~/fileid-test.sqlite
```

Keys: **s** scan a folder (type the path, `Enter`) · **r** reload after a scan · **Tab** switch tabs · **/** search · **↑↓**/**jk** navigate · **q** quit. The TUI paints its own dark theme, so it stays readable on light terminals.

The **s** scan drives the same full-ML engine as the CLI's `--models` on every platform: install the models once (press **D** on the Settings tab, or run `fileid models download --all`) and full-ML scanning works on **macOS too**. You can also index model-free with `fileid scan <folder>` (CLI), or browse an existing desktop-app library by pointing at it with `--db`.

**Safety.** Read-only by default. Destructive actions are gated behind explicit flags: `dedupe --apply` and `restructure --apply` only touch disk with that flag (add `--dry-run` to preview), and `dedupe --similar --apply` additionally requires `--yes`.

Deeper reference: [`platforms/cli/README.md`](platforms/cli/README.md) · [`platforms/tui/README.md`](platforms/tui/README.md).

---

## Install

> Pre-built release binaries aren't published from the repo yet — build your own with the one-command [Quickstart](#quickstart), or assemble a distributable with the recipes in [`packaging/`](packaging/) ([`packaging/README.md`](packaging/README.md)).

- **Linux** — Flatpak (primary; bundles a pinned GNOME runtime, so one build runs on Debian / Ubuntu / Arch / NixOS / Fedora), plus AppImage, a Nix flake, and an AUR `PKGBUILD`.
- **Windows** — `FileIDSetup.exe` embeds per-arch **.msi** installers (x64 + ARM64) and auto-picks the right one at install; build it with `publish-bundle.ps1`.
- **macOS** — a `FileID.app` bundle for Apple Silicon; build it with `./build.sh -mac`.

---

## Architecture

Each desktop app ships **two processes** that talk newline-delimited JSON over stdio: a native **UI** (SwiftUI / WinUI 3 / GTK4) and a **Rust engine** (Swift on macOS) that owns the SQLite WAL database, scan pipeline, and ML inference. The split buys crash isolation — a panic in the ML pipeline restarts the engine, not the UI. The `fileid` CLI and TUI link the engine crate in-process, so they can't drift either. The IPC contract lives at [`shared/ipc-schema/ipc.schema.json`](shared/ipc-schema/), mirrored by hand-maintained Swift, Rust, and C# DTOs that per-language schema-conformance suites hold to the canonical schema — so casing or shape drift is a test break.

FileID picks the best GPU/NPU path per machine: DirectML across every Windows vendor, CUDA / OpenVINO / QNN Performance Packs opt-in, CoreML + ANE on Apple Silicon, and an AVX2/NEON CPU floor. Full design, the GPU matrix, and the ML-model stack: [`shared/docs/ARCHITECTURE.md`](shared/docs/ARCHITECTURE.md). Build, CI, and troubleshooting detail: [`shared/docs/CONTRIBUTING.md`](shared/docs/CONTRIBUTING.md).

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

**Apache-2.0** — see [`LICENSE`](LICENSE). Every default model weight is permissively licensed (Apache-2.0 / MIT), so the project is free to be open-sourced *and* commercialized — no non-commercial weights in the shipped feature set. FileID downloads model weights at runtime and never redistributes them; they remain governed by their upstream licenses.

---

<p align="center">
  <sub>Made with <a href="https://claude.com/claude-code">Claude</a>.</sub>
</p>
