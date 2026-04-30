# FileID — Orientation for Claude

You're working on **FileID**, a macOS 14+ SwiftUI app that uses Apple's Neural Engine and on-device LLMs to tag, rename, and reorganize photos / videos / PDFs / documents. Sole developer is on an M1 MacBook Pro 16 GB, testing against ~50,000 files. Targeting open-source release on GitHub.

## Architecture in one screen

Split-process. Two binaries that talk via newline-delimited JSON over stdin/stdout.

- **`app/Sources/FileID/`** — SwiftUI viewer. `@main` is `FileIDApp.swift`. Tabs in order: **Library / People / Cleanup / Deep Analyze / Restructure / Settings** (Review was folded into Settings → Advanced; Memories was added then removed). UI state lives in `EngineClient` (`@MainActor @Observable`) — receives engine events, drives view updates.
- **`engine/Sources/FileIDEngine/`** — Swift CLI binary, spawned as a child of the app via `Process`. Owns the database, the scan pipeline, ANE/GPU model loading, and all heavy work. Stays alive across scans; auto-respawns with backoff on crash.
- **`shared/Sources/FileIDShared/`** — `IPCProtocol.swift` (`IPCCommand` / `IPCEvent` enums), DB row types, AI model registry. App ↔ engine contract.
- **`Tests/SharedTests/`** — Swift Testing tests. 28 passing as of V7.
- **`scripts/iterate.sh`** — end-to-end driver: spins up the engine, scans a public-domain test corpus in `Tests/Corpus/`, asserts on results. 11 assertions. Run after material changes.

Storage is **GRDB.swift** over SQLite WAL — single writer (engine), many readers (app via `ReadStore`). Schema version 7. **Not SwiftData.** Migrations are append-only in `Database.swift`.

## Tabs (current state — V7)

| Tab | Purpose | Backed by |
|---|---|---|
| **Library** | FTS5 search + thumbnail grid + per-file preview sheet | `LibraryView.swift`, `ReadStore.files(...)`. CLIP semantic search routes through `CLIPTextEncoder.shared.embedText` when ≥ 3 char query + model installed. |
| **People** | Face clusters → name them. Drag-and-drop merge between cards. Skip-naming escape hatch when user wants to jump straight to Deep Analyze. | ArcFace embeddings + `IdentityClustering` (V2) two-pass density + quality validation. **Chinese Whispers is deleted — don't re-introduce.** Naming required to gate Deep Analyze (or Skip). |
| **Cleanup** | Duplicate groups by phash. Trash via `FileManager.trashItem`. Optional auto-tag of keepers. | `ReadStore.duplicateGroups()`. |
| **Deep Analyze** | On-device VLM (Qwen / Gemma / SmolVLM / PaliGemma via MLX) writes captions + smart filenames. Now handles **images, PDFs (first-page render), videos (keyframe at 25%), and Office docs (Quick Look thumbnail)**. | `DeepAnalyzeRunner` + `DeepAnalyze` actor. Hard-blocked until ≥1 person is named (with "Skip" escape). |
| **Restructure** | Folder reorganization preview. Sankey flow diagram (default) + dual-pane tree (power-user toggle). Per-source destination chips, drill-down sheet, two-step apply (shortcuts → real moves). | `RestructureEngine.compute()` + `Restructure/SankeyFlowView.swift` / `RecommendationCard.swift` / `TreeDiffView.swift`. V7. |
| **Settings** | AI model status + downloads (CLIP, ArcFace, VLMs), engine info, recent scans, logs, Privacy disclosure. | `ReviewSettingsViews.swift`. CLIP installer fetches direct from `huggingface.co/apple/coreml-mobileclip` + `huggingface.co/openai/clip-vit-base-patch32` — no self-hosted artifact required. |

## AI models on disk

All under `~/Library/Application Support/FileID/Models/`:

- `arcface_iresnet50.mlpackage` / `arcface_mobileface.mlpackage` — face embedder. Convert via `scripts/convert_arcface.py`.
- `mobileclip_image/mobileclip_s2_image.mlpackage` — per-file image embedding (scan-time).
- `clip_text/clip_text.mlpackage` + `vocab.json` + `merges.txt` — query-time semantic search.
- VLMs (Qwen / Gemma / SmolVLM / PaliGemma) live under `~/Documents/huggingface/models/<repo>/` via swift-transformers `HubApi`. Downloaded on first use through Settings.

CLIP downloader: `app/Sources/FileID/Services/CLIPModelInstaller.swift` — pulls 8 files from HuggingFace, atomic-replace, validates, auto-loads `CLIPTextEncoder`.

## Build & run

Two paths:

```bash
# Wipes the SQLite DB + caches every run. Use for fresh-state testing.
bash run.sh
```

```bash
# Preserves the DB. Use when you want to keep your scanned library.
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer \
  swift build -c release --product FileID
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer \
  swift build -c release --product FileIDEngine
cp .build/release/FileID FileID.app/Contents/MacOS/FileID
cp .build/release/FileIDEngine FileID.app/Contents/MacOS/FileIDEngine
chmod +x FileID.app/Contents/MacOS/FileID FileID.app/Contents/MacOS/FileIDEngine
open FileID.app
```

Quick syntax check during iteration:

```bash
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer swift build
```

`run.sh` requires **cmake + Xcode's Metal Toolchain** for `mlx.metallib` (Deep Analyze GPU kernels). Without them it fails loudly with install instructions.

## Critical files (where things actually live)

- `app/Sources/FileID/EngineClient.swift` — engine spawn, IPC parsing, `@Observable` state. The auto-pilot stage machine + auto-tab-switch via `.task(id:)` (NOT `.onChange` with optional keypaths — has a Swift 6 race issue) live here.
- `app/Sources/FileID/Database/ReadStore.swift` — read-only DB queries. New helpers when you need them; `topVisionTagsBulk` shows the bulk-query pattern (avoid N+1).
- `app/Sources/FileID/AppSupportPath.swift` — `.first!` force-unwraps were removed in V6; everything goes through `AppSupportPath.fileID` / `.models` with a `temporaryDirectory` fallback.
- `engine/Sources/FileIDEngine/FaceClustering.swift` — V2 clustering pipeline. ArcFace required (no Vision-print fallback). Identity persistence via centroid + anchor radius on `persons`.
- `engine/Sources/FileIDEngine/DeepAnalyze.swift` — VLM dispatch. `loadCGImage` handles images / PDFs / videos / docs (Quick Look fallback for unrecognized formats).
- `engine/Sources/FileIDEngine/JSONLog.swift` — structured logger. **Always wrap user file paths with `redactPathForLog(_:)`** before logging — paths can leak PII like `Mom_Birthday_2024/`.
- `engine/Sources/FileIDEngine/Tagging.swift` — scan-time per-file pipeline. Vision classifier runs on images only today (videos/PDFs deliberately skip Vision because of `VNControlledCapacityTasksQueue` deadlocks; they get captioned post-scan via Deep Analyze instead).
- `app/Sources/FileID/Views/Restructure/` — V7 Sankey + Tree + RecommendationCard. Sankey caps to top 8 nodes per side with rollup, hard-bound 380pt height, slot-authoritative frame heights (no `max(28, ...)` overlap bug).
- `app/Sources/FileID/Views/LavaLampAesthetics.swift` — animated gold/orange `Canvas` background with `.ultraThinMaterial`. **The user loves this. Don't replace it.**

## Working principles

- **The user runs the build.** Type-check passing isn't proof of correctness — verify via `bash run.sh` (or the manual rebundle path) and the running UI. After UI changes, screenshot or describe what you see.
- **Update `docs/STATE.md` and `docs/NEXT.md` at the end of meaningful work.** Future Claude sessions read them first.
- **Append to `docs/DECISIONS.md` for non-obvious calls.** Apple API quirks, paths-not-taken, deliberate non-fixes.
- **Plan large changes in `~/.claude/plans/in-media-library-i-temporal-acorn.md`** — that's the live planning doc with V1 → V7 history. New tracks become `## Vn` sections.
- **No new third-party Swift packages without asking.** GRDB, swift-transformers, MLX, swift-async-algorithms are already in. Apple frameworks (Vision, AVFoundation, QuickLookThumbnailing, NaturalLanguage, Compression) are preferred.
- **Preserve LavaLamp.** It's the user's favorite.

## Conventions

- Swift 6 language mode + strict concurrency. Sendable closure-capture warnings will become errors.
- `@MainActor` for UI state, `actor` for shared mutable services, `@unchecked Sendable` only with explicit lock coverage. The `MutexBox<T>` pattern in `EngineClient` is the template for cross-actor mutable state.
- Errors use `try?` swallowing in non-critical paths. Engine-side errors emit `IPCEvent.error(EngineError(kind:message:))`.
- File naming: `Service.swift` for actors / static helpers, `View.swift` for SwiftUI views (subdirs OK — see `Restructure/`), DB types in `Database/`.

## Plan/state files for AI sessions

- `~/.claude/plans/in-media-library-i-temporal-acorn.md` — primary planning doc. Read this first for "what's the current state of work?"
- `docs/STATE.md` — running session log. Top entry is the latest snapshot.
- `docs/NEXT.md` — top priorities + acceptance criteria for the next session.
- `docs/DECISIONS.md` — append-only "why we did X this way" rationale.
- `docs/SHIP.md` — release-readiness inventory. v1.0 gap items live here.
- `~/.claude/projects/-Users-adamnolle-Desktop-FileID/memory/MEMORY.md` — auto-memory; user-level prefs that survive sessions.
