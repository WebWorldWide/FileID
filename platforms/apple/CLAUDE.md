# FileID — Apple platform (macOS)

macOS 15+ SwiftUI app that tags, renames, and restructures local file libraries on-device using Apple's Neural Engine and MLX VLMs. Sole dev on M1 Pro 16 GB testing against ~50K files. GitHub-targeted.

This file covers the macOS code under `platforms/apple/`. For the Windows port see `platforms/windows/CLAUDE.md`. For cross-platform contracts see `shared/`.

## Architecture

Two binaries talking newline-delimited JSON over stdin/stdout.

- `app/Sources/FileID/` — SwiftUI viewer. UI state in `EngineClient` (`@MainActor @Observable`).
- `engine/Sources/FileIDEngine/` — Swift CLI spawned as a child of the app. Owns DB, scan pipeline, ANE/GPU model loading. Auto-respawns with backoff on crash.
- `shared/Sources/FileIDShared/` — `IPCProtocol.swift` (`IPCCommand` / `IPCEvent` enums), DB row types, AI model registry. Generated portion comes from `../../shared/ipc-schema/ipc.schema.json`.
- `Tests/SharedTests/` — Swift Testing, 28 tests.
- `scripts/iterate.sh` — corpus regression harness, 11 assertions.

Storage: GRDB.swift on SQLite WAL. Single writer (engine), many readers (app via `ReadStore`). Schema v7. Migrations append-only in `Database.swift`. Not SwiftData.

## Tabs

| Tab | Purpose | Key file |
|---|---|---|
| Library | FTS5 search + thumbnail grid + preview sheet | `LibraryView.swift` |
| People | Face clusters → name them | `PeopleView.swift`, `engine/.../FaceClustering.swift` |
| Cleanup | Duplicate groups via phash | `CleanupView.swift` |
| Deep Analyze | On-device VLM captions/renames (Qwen / Gemma / SmolVLM / PaliGemma via MLX) | `DeepAnalyzeViews.swift`, `engine/.../DeepAnalyze.swift` |
| Restructure | Folder reorg with Sankey + recommendation rows + drill-down | `RestructureView.swift`, `Restructure/` |
| Settings | AI models, engine info, logs, privacy | `ReviewSettingsViews.swift` |

## AI models

Under `~/Library/Application Support/FileID/Models/`:
- `arcface_*.mlpackage` — face embedder
- `mobileclip_image/...` — per-file image embedding (scan-time)
- `clip_text/...` + `vocab.json` + `merges.txt` — query-time semantic search
- VLMs under `~/Documents/huggingface/models/<repo>/`

## Build

Run from `platforms/apple/`:

```bash
bash run.sh                                     # wipes DB, fresh-state testing
swift build -c release --product FileID         # preserves DB, manual bundle
swift build -c release --product FileIDEngine
cp .build/release/FileID FileID.app/Contents/MacOS/FileID
cp .build/release/FileIDEngine FileID.app/Contents/MacOS/FileIDEngine
chmod +x FileID.app/Contents/MacOS/{FileID,FileIDEngine}
open FileID.app
```

`run.sh` needs cmake + Xcode Metal Toolchain for `mlx.metallib`.

Quick syntax check:

```bash
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer swift build
```

## Conventions

- Swift 6 strict concurrency. `@MainActor` for UI, `actor` for shared mutable services, `@unchecked Sendable` only with explicit lock coverage.
- Engine emits `IPCEvent.error(EngineError(kind:message:))`. App-side non-critical paths use `try?` swallow.
- Always wrap user paths in `redactPathForLog(_:)` before logging — paths leak PII.
- No new third-party Swift packages without asking. In already: GRDB, swift-transformers, MLX, swift-async-algorithms.
- Default to no comments. Add only when the WHY is non-obvious (workaround, constraint, invariant).
- **No telemetry.** Local-only logs. See `../../shared/docs/PRIVACY.md`.

## Working principles

- User runs the build. Type-check passing isn't proof of correctness — verify via `bash run.sh` or rebundle and run.
- Update `../../shared/docs/STATE.md` and `../../shared/docs/NEXT.md` after meaningful work.
- Append to `../../shared/docs/DECISIONS.md` for non-obvious calls.
- Preserve `LavaLampBackground.swift`. User's favorite.

## Persistence files

- `../../shared/docs/STATE.md` — session log, top entry is latest.
- `../../shared/docs/NEXT.md` — next-session priorities + acceptance criteria.
- `../../shared/docs/DECISIONS.md` — append-only rationale (cross-platform).
- `../../shared/docs/SHIP.md` — v1.0 release-readiness inventory.
- `~/.claude/projects/<project-key>/memory/MEMORY.md` — auto-memory (path key derives from the absolute project path).
