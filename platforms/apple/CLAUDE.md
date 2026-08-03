# FileID — Apple platform (macOS)

macOS 15+ SwiftUI app + Swift engine that tag, dedupe, restructure, and rename local file libraries on-device (Apple Neural Engine, ONNX Runtime via the CoreML EP, and MLX VLMs). The visual + behavioral reference the Windows port mirrors.

Covers `platforms/apple/`. For the Windows build see `platforms/windows/CLAUDE.md`; for cross-platform contracts + principles see the root `CLAUDE.md` and `shared/`.

## Architecture

Two binaries, newline-delimited JSON over stdin/stdout:
- `app/Sources/FileID/` — SwiftUI app. UI state in `EngineClient` (`@MainActor @Observable`); spawns the engine, auto-respawns with backoff on crash.
- `engine/Sources/FileIDEngine/` — Swift CLI child. Owns the DB, scan pipeline, ANE/GPU model loading.
- `shared/Sources/FileIDShared/` — `IPCProtocol.swift` (`IPCCommand`/`IPCEvent`), DB row types, the AI model registry (`AIModels.swift`), mirrored against `../../shared/ipc-schema/ipc.schema.json`.
- `Tests/` — Swift Testing (Shared + Engine suites). `scripts/iterate.sh` — corpus regression harness.

Storage: GRDB.swift on SQLite WAL. Single writer (engine), many readers (app via `ReadStore`). **Schema v20**, with append-only migrations in `Database.swift` mirrored against the Windows engine so a library round-trips across platforms. Not SwiftData.

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
- **Tagging** — RAM++ primary, CLIP zero-shot scene tags as fallback.
- **Deep Analyze** — Qwen3-VL 4B is the measured recommendation for 8 GB Macs and Qwen3-VL 8B for 16 GB Macs; Qwen2.5-VL 7B, Gemma 3, and Mistral-Small-3.2 remain curated alternatives.

> **Lockstep status (updated 2026-08):** the commercial-clean model swap (ArcFace→SFace, MobileCLIP-S2→ViT-B/32, Qwen-3B→Qwen3/Qwen2.5/Mistral) has **LANDED and is wired as the primary stack** — `FaceEmbedderKind` is SFace-only (128-d), RAM++ Swin-L is the primary tagger, `MobileCLIPService` loads ViT-B/32, and all three are prewarmed at engine startup. Native Apple-Silicon validation against Adlon is complete; an actual Windows-written/macOS-written embedding comparison and other hardware matrices remain external gates. See `platforms/apple/MACOS_LOCKSTEP_NOTES.md`.

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

- Every macOS change requires a native strict Swift build/test and, when behavior changes, `bash run.sh --no-wipe` plus on-device validation. Never infer macOS runtime correctness from Windows or hosted compilation alone.
- Keep `STATE.md` (newest on top) + `NEXT.md` current; append non-obvious calls to `DECISIONS.md`.
- Preserve `LavaLampBackground.swift` — the user's favorite.

## Persistence files

See the root `CLAUDE.md` and `shared/docs/` (STATE, NEXT, DECISIONS, MODELS, ARCHITECTURE, RESTRUCTURE, SHIP) + the auto-memory at `~/.claude/projects/<project-key>/memory/MEMORY.md`.
