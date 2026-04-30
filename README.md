<p align="center">
  <img src="docs/assets/FileID-Logo.png" width="380" alt="FileID">
</p>

<p align="center">
  <strong>On-device AI file organization for macOS.</strong><br>
  <em>Tag, dedupe, restructure, and rename tens of thousands of files — privately, on Apple Silicon.</em>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/macOS-14%2B-blue?style=flat-square">
  <img src="https://img.shields.io/badge/Apple%20Silicon-required-orange?style=flat-square">
  <img src="https://img.shields.io/badge/Swift-6-fa7343?style=flat-square">
  <img src="https://img.shields.io/badge/100%25-on--device-green?style=flat-square">
</p>

---

Point FileID at a folder. It reads every file inside — images, video, PDFs, docs — and builds one searchable library that understands what's *in* them. Faces cluster into named cards. Duplicates group by perceptual hash. A local vision-language model writes captions and proposes filenames. Folder reorganization previews as shortcuts before anything moves on disk.

Nothing leaves the Mac.

## Features

| Tab | |
| --- | --- |
| **Library** | FTS5 search over filenames + OCR. Semantic CLIP search ("a dog at the beach"). Thumbnail grid + preview sheet. |
| **People** | Face clusters from ArcFace embeddings. Drag to merge. Name them once and Deep Analyze captions use real names. |
| **Cleanup** | Duplicate groups by perceptual hash. Trashed files stay recoverable from the Bin. |
| **Deep Analyze** | Qwen 3 VL · Gemma 3 · SmolVLM 2 · PaliGemma (via MLX) writes a caption + smart filename per image, PDF, video keyframe, or doc thumbnail. |
| **Restructure** | Folder reorganization with a Sankey flow diagram. Apply as shortcuts (reversible), then convert to real moves when you're happy. |
| **Settings** | Model downloads, engine info, logs, privacy. |

## Architecture

Two binaries, newline-delimited JSON over stdin/stdout.

- **`FileID`** — SwiftUI viewer.
- **`FileIDEngine`** — Swift CLI spawned as a child of the app. Owns the GRDB / SQLite WAL database, the scan pipeline, and ANE/GPU model loading. Auto-respawns with bounded backoff on crash.

Models live under `~/Library/Application Support/FileID/Models/` (ArcFace + CLIP) and `~/Documents/huggingface/models/` (VLMs via swift-transformers).

## Requirements

- macOS 14+, Apple Silicon
- Xcode 16+ (Swift 6)
- `cmake` and Xcode's Metal Toolchain (for the MLX `.metallib` Deep Analyze needs)

## Run

```bash
bash run.sh
```

Wipes the DB, rebuilds, bundles, opens. Model weights are preserved.

To preserve your library across rebuilds:

```bash
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer \
  swift build -c release --product FileID
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer \
  swift build -c release --product FileIDEngine
cp .build/release/FileID FileID.app/Contents/MacOS/FileID
cp .build/release/FileIDEngine FileID.app/Contents/MacOS/FileIDEngine
chmod +x FileID.app/Contents/MacOS/{FileID,FileIDEngine}
open FileID.app
```

## Test

```bash
swift test                  # unit tests (Swift Testing)
bash scripts/iterate.sh     # corpus regression harness
```

## Privacy

Every operation runs locally. Models download from HuggingFace once at first use; the app then works fully offline. No analytics. No telemetry. No remote logging.

## Security

See [`docs/SECURITY.md`](docs/SECURITY.md) for the threat model, audit findings, and what's deferred to v1.0.

## License

TBD.

---

<p align="center">
  <sub>Made with <a href="https://claude.com/claude-code">Claude</a>.</sub>
</p>
