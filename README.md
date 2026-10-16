<p align="center">
  <img src="shared/docs/assets/FileID-Logo.png" width="380" alt="FileID">
</p>

<p align="center">
  <strong>Your files, finally sorted. On your computer, not someone else's.</strong><br>
  <em>FileID looks at every photo, video, PDF and document you own, works out what's in it, and helps you find, clean up and organize it. The AI runs on your own computer and nothing is uploaded.</em>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/macOS-15%2B%20Apple%20Silicon-black?style=flat-square">
  <img src="https://img.shields.io/badge/Windows-10%2F11%20x64%20%2B%20ARM64-0078d4?style=flat-square">
  <img src="https://img.shields.io/badge/Linux-GTK4%20%2B%20libadwaita-orange?style=flat-square">
  <img src="https://img.shields.io/badge/telemetry-none-2ea043?style=flat-square">
  <img src="https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square">
</p>

<p align="center">
  <a href="https://github.com/WebWorldWide/FileID/actions/workflows/macos.yml"><img src="https://github.com/WebWorldWide/FileID/actions/workflows/macos.yml/badge.svg" alt="macOS"></a>
  <a href="https://github.com/WebWorldWide/FileID/actions/workflows/windows-engine.yml"><img src="https://github.com/WebWorldWide/FileID/actions/workflows/windows-engine.yml/badge.svg" alt="Windows engine"></a>
  <a href="https://github.com/WebWorldWide/FileID/actions/workflows/windows-app.yml"><img src="https://github.com/WebWorldWide/FileID/actions/workflows/windows-app.yml/badge.svg" alt="Windows app"></a>
  <a href="https://github.com/WebWorldWide/FileID/actions/workflows/linux.yml"><img src="https://github.com/WebWorldWide/FileID/actions/workflows/linux.yml/badge.svg" alt="Linux"></a>
</p>

<p align="center">
  <a href="https://fileid.webworldwide.online">Website</a> ·
  <a href="https://github.com/WebWorldWide/FileID/releases/latest">Download</a> ·
  <a href="#how-it-works">How it works</a> ·
  <a href="#performance">Performance</a> ·
  <a href="#build-from-source">Build from source</a>
</p>

---

## Why FileID

Most of us have the same problem: years of photos called `IMG_4821.JPG`, a Downloads folder nobody has looked at since 2019, three copies of the same video, and a drive so full that finding anything means scrolling and guessing.

Cloud services will sort that out if you upload everything to them. FileID does the same job **on your own machine**:

- **It reads your files.** Every image is tagged ("beach", "dog", "birthday cake"), faces are grouped into people, text in screenshots and scans is read, and videos are split into moments you can jump to.
- **It finds things the way you'd describe them.** Type *"a dog at the beach"* or *"the receipt from the hardware store"* and get the file, even if its name says nothing.
- **It cleans up.** Exact duplicates and near-identical photos are grouped so you can pick the keeper and send the rest to the Trash, where they stay recoverable.
- **It organizes, carefully.** Restructure suggests a tidy folder layout, keeps the folders you already made, shows every move before it happens, and can undo all of it.
- **You can ask it.** Chat answers questions about your library ("how many photos of Kyle at the baseball game?") and cites the exact files.

**Private by design.** There is no account, no cloud, no analytics, no crash reporting and no update pings. The only time FileID touches the internet is when *you* click to download an AI model, and that download comes straight from Hugging Face. CI scans every shipped binary for telemetry code and fails the release if it finds any.

**Free and open.** FileID is Apache-2.0, and every AI model it installs by default is Apache-2.0 or MIT. You can use it, change it, and build a business on it.

---

## What's inside

FileID has the same layout on every platform: three areas in the sidebar (**Browse**, **Analyze**, **Act**) and one search box at the top that also lets you ask questions.

| | What you get |
|---|---|
| **Library** | Every file in a fast grid that keeps up with libraries of 100,000+ files. Search by name, tag, text inside the file, or plain-English description. Click a file to see its details; double-click for a full preview with video and audio playback built in. |
| **People** | Faces grouped into people automatically. Name someone once and FileID uses the name everywhere, including in captions, file names and Chat. |
| **Cleanup** | Exact duplicates (by content hash) and look-alike photos (by perceptual hash), with the space you'd get back. Deleting moves files to the Trash, and you can restore them. |
| **Deep Analyze** | A local vision-language model writes a caption and suggests a clear file name for photos, PDFs, documents and video frames. You review the suggestions before anything is renamed. |
| **Search & Moments** | Searches across names, extracted text, captions and timestamps inside videos. Long videos get chapters and highlighted moments. |
| **Best Takes** | Picks the sharpest shot from a burst or a run of similar photos. |
| **File Tools** | Exports for photos and video (crop, fit-and-pad for social, stabilize, reframe), with preview, history and Undo. Originals are never changed. |
| **Restructure** | A proposed folder layout shown as a flow diagram, with each move listed. Your named folders are kept, loose camera dumps are grouped into dated events, and RAW/JPEG/video folders stay separate. Apply it, and undo it if you change your mind. |
| **Chat** | Ask questions about your library. Answers come from what FileID found in your files, with numbered citations you can open and play in place. |
| **Settings** | AI model browser (with a "speed on this Mac" benchmark), GPU choice, appearance, logs and privacy. |

On macOS FileID also writes real **Finder tags**, so they show up in the Finder sidebar, Smart Folders and Spotlight.

There's also a **command-line tool** (`fileid`) and a **terminal UI** (`fileid-tui`) for servers, NAS boxes and people who live in a shell. Both run on macOS, Linux and Windows.

---

## Performance

Speed is treated as a feature, and every number below was measured, not estimated. Unless noted, the machine is a 16 GB M1 Pro MacBook Pro reading from an external USB SSD (exFAT), on a real camera library of Sony RAW and JPEG files.

### Scanning

| What changed | Before | After |
|---|---|---|
| End-to-end scan, 105 Sony ARW files (warm cache) | 4.1 files/s, 1,090 MB peak | **40–42 files/s, ~660 MB** |
| Larger RAW run, 382 files | — | **55–56 files/s** |
| Decoding one Sony RAW (uses the camera's embedded full-size preview) | 606 ms | **35–55 ms** |
| CLIP image embedding (search vectors), per image | 190 ms on CPU under load | **3.2 ms** batched on the GPU (MLX), >0.999 cosine match with the reference model |
| Same RAW preview trick in the Rust engine (Windows/Linux), 7008×4672 ARW | couldn't decode on Linux | **72–92 ms** |

The target is ≥140 files/s on comparable mid-tier hardware. Today's limits are disk throughput on USB drives and RAW decoding cost; the remaining work is tracked in [`NEXT.md`](shared/docs/NEXT.md).

### Browsing and memory

| What changed | Before | After |
|---|---|---|
| Flinging through a 3,447-file library: memory | 1.6 GB | **318 MB** |
| Same, main-thread busy time | 86% | **53%** |
| Library paging | first 200 files only | **all files**, 300 per page, six parallel read connections |
| Duplicate count at 100,000 files | 251 ms | **54 ms** |
| AI models sitting idle (parked after 5 minutes, reload on demand) | 3.1 GB | **~107 MB** |

### Organizing

| What changed | Before | After |
|---|---|---|
| Restructure planning on a 3,000-file camera folder | 10+ minutes | **~19 s** (one Accelerate matrix multiply instead of pairwise loops) |
| Restructure moves proposed on an already-organized camera folder | 2,966 moves, many across events | **0 moves, 28 folders kept** |
| 42 loose photos in one Downloads folder | one folder per tag | **3 correctly dated event folders** |

### On-device language models (Apple MLX, 16 GB M1 Pro)

| Model | Load | Reading speed (prefill) | Writing speed (decode) | Per image | Peak memory |
|---|---|---|---|---|---|
| Qwen3.5 0.8B, 4-bit | 3.1 s | 611 tokens/s | 45 tokens/s | 3.5 s | 1.1 GB |
| Qwen3-VL 4B, 4-bit | 3.5 s | 145 tokens/s | 24 tokens/s | 4.4 s | 3.6 GB |

Chat sends the model compact numbered evidence instead of JSON. That cut time-to-first-word on the 0.8B model from 1.07 s to **0.58 s**.

### Download size

The macOS disk image is **63 MB** (was 110 MB) after switching to LZMA compression. AI models are separate downloads, so you only install the ones you want.

---

## How it works

### The short version

FileID is two programs that work together. The **app** is the window you click around in, built with each platform's own native toolkit. The **engine** is a background worker that reads files, runs the AI models and keeps the database. They talk through a small, strict message format. If the engine hits a bad file or a GPU driver problem and crashes, the app stays open, restarts the engine, and you carry on.

### The technical version

```
┌──────────────────────────┐   newline-delimited JSON    ┌─────────────────────────────────────┐
│  Native app              │ ──────── stdin ───────────▶ │  Engine (child process)             │
│  SwiftUI · WinUI 3 · GTK4│ ◀─────── stdout ─────────── │  scan pipeline · ML · SQLite writer │
│  read-only SQLite pool   │                             │  single writer, WAL                 │
└──────────────────────────┘                             └─────────────────────────────────────┘
              ▲                                                         │
              └───────────── reads the same WAL database ◀──────────────┘
```

- **One contract.** Every command and event is defined once in [`shared/ipc-schema/ipc.schema.json`](shared/ipc-schema/) (currently v1.16.0) and mirrored as Swift, Rust and C# types. If they drift, the build breaks.
- **Two engines, one database format.** macOS uses a Swift engine (GRDB, MLX, Core ML, Vision). Windows, Linux, the CLI and the TUI share one Rust engine (`rusqlite`, ONNX Runtime, llama.cpp). Both apply the same 24 append-only migrations byte for byte, so a library scanned on one platform opens on another.
- **Crash isolation.** The app spawns the engine, verifies its signature, and restarts it with backoff (1 s / 4 s / 16 s) if it exits. A parent-process watchdog stops the engine if the app goes away.
- **Single writer, many readers.** The engine owns the only write connection to a SQLite WAL database. The app reads through a pool of read-only connections, so scrolling never waits on a scan.

### The scan pipeline

1. **Discover**: walk the folder, skip what hasn't changed (path + size + modification time), and queue the rest.
2. **Decode**: ImageIO / Media Foundation / `image` crate, with embedded RAW previews used when they're big enough.
3. **Understand** (in parallel): tags (RAM++ or Apple Vision labels), a CLIP vector for semantic search, faces (Vision or YuNet → SFace), text (Vision OCR / Windows.Media.Ocr / Tesseract), PDF and document text, and video keyframes.
4. **Hash**: SHA-256 for exact duplicates, a perceptual hash for look-alikes.
5. **Write**: batched transactions into SQLite with FTS5 full-text indexes and stored embeddings.

GPU and Neural Engine work is batched (`CLIPGPUBatcher` gathers concurrent requests into one MLX call), overlaps with CPU work, and falls back to CPU when no accelerator is available. Models are admitted only when the OS reports enough free memory, MLX's buffer cache is capped, and idle models are unloaded.

### AI models

Every default is permissively licensed and downloaded from its upstream Hugging Face repository. FileID doesn't redistribute weights. Full registry and license notes: [`shared/docs/MODELS.md`](shared/docs/MODELS.md).

| Job | macOS | Windows / Linux | License |
|---|---|---|---|
| Image tags | RAM++ Swin-L (4,585 tags), Apple Vision labels | RAM++ Swin-L (ONNX, fp16) | Apache-2.0 |
| Semantic search | CLIP ViT-B/32 on the GPU via MLX (ONNX fallback) | CLIP ViT-B/32 (ONNX) | MIT |
| Faces | Apple Vision detection → SFace 128-d | YuNet → SFace 128-d | MIT / Apache-2.0 |
| Text in images | Vision OCR | Windows.Media.Ocr / Tesseract | built-in |
| Captions, renames, Chat | MLX: Qwen3-VL 4B (8 GB Macs) · Qwen3-VL 8B (16 GB) · Qwen3.5 0.8B · more in the browser | llama.cpp GGUF: Qwen2.5-VL 7B · Qwen3-VL 4B/8B · Gemma 3 · Mistral-Small-3.2 | Apache-2.0 (Gemma opt-in under its own terms) |

### Hardware acceleration

| Hardware | Path |
|---|---|
| Apple Silicon | Metal (MLX), Neural Engine and GPU (Core ML), Vision |
| NVIDIA | DirectML by default; CUDA if you install NVIDIA's runtime yourself |
| AMD, Intel | DirectML; OpenVINO opt-in on Intel |
| Snapdragon X (Windows on ARM) | DirectML; QNN NPU opt-in |
| Anything else | CPU with AVX2/AVX-512 or NEON |

---

## Install

| Platform | Download | Notes |
|---|---|---|
| **macOS 15+** (Apple Silicon) | `FileID.dmg` | Drag to Applications. |
| **Windows 10 22H2+ / 11** (x64 and ARM64) | `FileIDSetup.exe` | One installer that picks the right architecture. |
| **Linux** (x86_64, aarch64) | build from source | Flatpak and AppImage are planned. |

Get them from the [latest release](https://github.com/WebWorldWide/FileID/releases/latest). Current builds aren't yet signed with Developer ID or EV certificates, so macOS Gatekeeper and Windows SmartScreen will warn on first launch.

On first launch a Welcome sheet offers the core models: RAM++ (~882 MB, tags), CLIP (~335 MB, search) and YuNet + SFace (~39 MB, faces). You can skip it and install later from Settings. Deep Analyze and Chat models are optional and live in the model browser.

---

## Build from source

One script at the repo root handles every platform:

```bash
./build.sh -mac             # macOS: build the engine + app and launch
./build.sh -windows         # Windows: fresh-install release build, staged to ~/Desktop/FileID, then launch
./build.sh -linux           # Linux: build + stage the shared engine and GTK app
./build.sh --help           # all flags
```

### macOS

Needs Xcode 16+ with the Metal toolchain and CMake.

```bash
bash platforms/apple/run.sh              # wipe the dev database, build, launch
bash platforms/apple/run.sh --no-wipe    # keep your library
cd platforms/apple && swift test         # Shared + Engine + App test suites
```

Dev guide: [`platforms/apple/CLAUDE.md`](platforms/apple/CLAUDE.md).

### Windows

Needs Rust 1.90+ (`rustup`), the .NET 8 SDK and Visual Studio Build Tools 2022 with the UWP MSBuild component.

```powershell
.\platforms\windows\build\build-all.ps1                    # incremental engine + app
.\platforms\windows\build\build-all.ps1 -WipeDbOnly -Run   # fresh scan, keep downloaded models
.\platforms\windows\build\publish-bundle.ps1 -SkipSign     # FileIDSetup.exe + per-arch MSIs
```

> ⚠️ `./build.sh -windows` deletes `%LOCALAPPDATA%\FileID\` by default, including downloaded models. Add `--no-wipe` to keep them.

Dev guide: [`platforms/windows/CLAUDE.md`](platforms/windows/CLAUDE.md).

### Linux

Needs Rust and the GTK4 + libadwaita development packages (`sudo apt install build-essential libgtk-4-dev libadwaita-1-dev`).

```bash
./build.sh -linux --no-run
./platforms/linux/dist/fileid/fileid-linux
```

Dev guide: [`platforms/linux/CLAUDE.md`](platforms/linux/CLAUDE.md).

### CLI and TUI

```bash
bash scripts/build-tools.sh     # installs fileid, fileid-tui and the engine into ~/.cargo/bin
fileid --help
fileid-tui
```

---

## Repository layout

```
FileID/
├── platforms/
│   ├── apple/      macOS: SwiftUI app + Swift engine (MLX, Core ML, Vision, GRDB)
│   ├── windows/    WinUI 3 app (.NET 8) + the shared Rust engine (src/engine)
│   ├── linux/      GTK4 / libadwaita app on the shared engine
│   ├── cli/        `fileid` command-line tool
│   └── tui/        `fileid-tui` terminal UI
├── shared/
│   ├── ipc-schema/ the app ↔ engine contract (JSON Schema)
│   ├── docs/       architecture, decisions, models, privacy, roadmap
│   ├── test-corpus/regression corpus + assertions
│   └── scripts/    release gates and model tooling
├── website/        fileid.webworldwide.online
└── build.sh        one entry point for every platform
```

## Where your data lives

| | macOS | Windows | Linux |
|---|---|---|---|
| Library database | `~/Library/Application Support/FileID/fileid.sqlite` | `%LOCALAPPDATA%\FileID\fileid.sqlite` | `$XDG_DATA_HOME/FileID/fileid.sqlite` |
| Models | `…/FileID/Models/` | `%LOCALAPPDATA%\FileID\Models\` | `$XDG_DATA_HOME/FileID/Models/` |
| Logs (local only) | `~/Library/Logs/FileID/` | `%LOCALAPPDATA%\FileID\logs\` | `$XDG_DATA_HOME/FileID/logs/` |

FileID never modifies the contents of your files. Renames, moves and deletions happen only when you confirm them; they're journaled so they can be undone, and deletions go to the system Trash.

---

## Quality gates

Every push runs GitHub Actions on macOS, Windows (x64 and ARM64) and Linux:

- **Builds and tests**: Swift test suites (600+ tests), Rust engine with strict Clippy and ~950 unit tests, .NET app/IPC tests, GTK app tests, CLI and TUI suites.
- **Privacy**: every shipped binary is scanned for telemetry strings, and every network-capable source file must match a reviewed digest that allows only Hugging Face downloads.
- **Supply chain**: pinned actions, `cargo deny` license/advisory checks, model license verification against `MODELS.md`.

## Project status

macOS is the reference implementation and the most complete. The shared Rust engine powers Windows, Linux, the CLI and the TUI, and is tested headlessly in CI. Code signing, notarization, native GPU checks on each vendor's hardware, and Linux packaging are the remaining release gates. They're tracked in [`SHIP.md`](shared/docs/SHIP.md) and [`NEXT.md`](shared/docs/NEXT.md), with the session log in [`STATE.md`](shared/docs/STATE.md) and design rationale in [`DECISIONS.md`](shared/docs/DECISIONS.md).

## Contributing

Read the root [`CLAUDE.md`](CLAUDE.md) for cross-platform principles (no telemetry, permissive models only, IPC schema first, no new dependencies without discussion), then the guide for the platform you're working on. New models go through [`MODELS.md`](shared/docs/MODELS.md) with their license checked.

## License

[Apache-2.0](LICENSE). Bundled fonts (Instrument Serif, Archivo) are SIL Open Font License.
