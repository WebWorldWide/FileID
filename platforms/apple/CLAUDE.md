# FileID — Apple platform (macOS)

macOS 15+ SwiftUI app + Swift engine that tag, dedupe, restructure, and rename local file libraries on-device (Apple Neural Engine, ONNX Runtime via the CoreML EP, and MLX VLMs). The visual + behavioral reference the Windows port mirrors.

Covers `platforms/apple/`. For the Windows build see `platforms/windows/CLAUDE.md`; for cross-platform contracts + principles see the root `CLAUDE.md` and `shared/`.

## Architecture

Two binaries, newline-delimited JSON over stdin/stdout:
- `app/Sources/FileID/` — SwiftUI app. UI state in `EngineClient` (`@MainActor @Observable`); spawns the engine, auto-respawns with backoff on crash.
- `engine/Sources/FileIDEngine/` — Swift CLI child. Owns the DB, scan pipeline, ANE/GPU model loading.
- `shared/Sources/FileIDShared/` — `IPCProtocol.swift` (`IPCCommand`/`IPCEvent`), DB row types, the AI model registry (`AIModels.swift`), mirrored against `../../shared/ipc-schema/ipc.schema.json`.
- `Tests/` — Swift Testing (Shared + Engine suites). `scripts/iterate.sh` — corpus regression harness.

Storage: GRDB.swift on SQLite WAL. Single writer (engine), many readers (app via `ReadStore`). **Schema v12**, migrations append-only in `Database.swift`, byte-faithful with the Windows engine so a library round-trips across platforms. Not SwiftData.

## Tabs

| Tab | Purpose | Key file |
|---|---|---|
| Library | FTS5 + semantic CLIP search, thumbnail grid, preview | `LibraryView.swift` |
| People | Face clusters → name them | `PeopleView.swift`, `engine/.../FaceClustering.swift` |
| Cleanup | Duplicate groups via phash | `CleanupView.swift` |
| Deep Analyze | On-device VLM captions / smart renames (MLX) | `DeepAnalyzeViews.swift`, `engine/.../DeepAnalyze.swift` |
| Restructure | Folder reorg — Sankey + recommendation rows + drill-down | `RestructureView.swift`, `Restructure/` |
| Settings | AI models, engine info, logs, privacy | `ReviewSettingsViews.swift` |

## AI models (commercial-clean target)

Under `~/Library/Application Support/FileID/Models/` (VLMs under `~/Documents/huggingface/models/<repo>/`). The project is **Apache-2.0**; every default weight is Apache/MIT (see `shared/docs/MODELS.md`):
- **Faces** — SFace embedder (Apache, 128-d ONNX via the CoreML EP) + 5-point alignment; detection stays Apple Vision.
- **CLIP** — OpenAI/OpenCLIP ViT-B/32 (MIT) image + text, 512-d.
- **Tagging** — RAM++ primary (planned mirror of Windows), CLIP zero-shot scene tags as fallback.
- **Deep Analyze** — MLX VLMs: Qwen2.5-VL 7B / Gemma 3 / Mistral-Small-3.2.

> **Lockstep status:** the commercial-clean model swap (ArcFace→SFace, MobileCLIP-S2→ViT-B/32, Qwen-3B→7B/Mistral) is written on the `macos-lockstep` branch and **needs a Mac to `swift build` + verify embedding parity** (see `platforms/apple/MACOS_LOCKSTEP_NOTES.md`). The RAM++ tagger and the butler-restructure mirror (`shared/docs/RESTRUCTURE.md`) are the remaining macOS work. Until merged, `main`'s macOS engine still loads the prior weights.

## Build

```bash
# From platforms/apple/
bash run.sh                                                  # wipe DB + build + launch (fresh-state)
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer swift build   # quick type-check
swift test                                                   # Shared + Engine suites
```

`run.sh` needs cmake + the Xcode Metal Toolchain (for `mlx.metallib`). Release bundling: `swift build -c release --product {FileID,FileIDEngine}`, copy both into `FileID.app/Contents/MacOS/`, `open`.

## Conventions

- **Swift 6 strict concurrency.** `@MainActor` for UI, `actor` for shared mutable services, `@unchecked Sendable` only with explicit lock coverage.
- Engine surfaces failures as `IPCEvent.error(EngineError(kind:message:))`; app-side non-critical paths `try?`-swallow.
- Wrap user paths in `redactPathForLog(_:)` before logging — paths leak PII.
- **No new third-party packages without asking.** In already: GRDB, swift-transformers, MLX, swift-async-algorithms, onnxruntime.
- **Default to no comments** — only a non-obvious *why*.
- **No telemetry.** Local-only logs. See `../../shared/docs/PRIVACY.md`.

## Working principles

- I write the Swift here, **but cannot build or run it in the Windows dev env** — type-check/build/verify happen on the user's Mac (`bash run.sh` or rebundle). Treat all macOS edits as unverified until the user confirms a build.
- Keep `STATE.md` (newest on top) + `NEXT.md` current; append non-obvious calls to `DECISIONS.md`.
- Preserve `LavaLampBackground.swift` — the user's favorite.

## Persistence files

See the root `CLAUDE.md` and `shared/docs/` (STATE, NEXT, DECISIONS, MODELS, ARCHITECTURE, RESTRUCTURE, SHIP) + the auto-memory at `~/.claude/projects/<project-key>/memory/MEMORY.md`.
