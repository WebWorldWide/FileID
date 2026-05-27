# Architecture — cross-platform overview

FileID is split across three platform implementations that share a contract, a database schema, and a visual language. This document describes the parts that are common; per-platform `CLAUDE.md` files describe what's specific.

## Process model

```
   ┌─────────────────────────┐                ┌──────────────────────────┐
   │  FileID (UI)            │  stdin (cmds)  │  FileIDEngine (CLI)      │
   │                         │ ─────────────▶ │                          │
   │  - SwiftUI / WinUI 3    │                │  - SQLite WAL writer     │
   │  - reads DB read-only   │                │  - scan pipeline         │
   │  - spawns engine        │                │  - ML inference          │
   │  - auto-respawn 1/4/16s │ ◀───────────── │  - logs (local-only)     │
   └─────────────────────────┘  stdout (events│                          │
              │                  newline-     │                          │
              ▼                  delimited    └──────────────────────────┘
        SQLite (R/O)             JSON)               │
        snapshot                                     ▼
                                              SQLite WAL (R/W)
                                              fileid.sqlite
```

Two binaries per platform. The app spawns the engine as a child process. They talk newline-delimited JSON over stdin (app → engine) and stdout (engine → app). The app reads the DB via a read-only connection; the engine is the sole writer. SQLite WAL allows concurrent readers without blocking the writer.

When the engine crashes the app respawns it with bounded backoff (1 s / 4 s / 16 s within a 60 s window). Three failures in a row puts the app in `.crashed` state; user dismisses or retries.

## Storage

SQLite via WAL journaling. Schema versioned at v7 (see `platforms/apple/engine/Sources/FileIDEngine/Storage/Database.swift` for the canonical migration list, and `platforms/windows/src/engine/src/db/migrations.rs` for the byte-faithful Rust port). Both engines use the same `grdb_migrations` tracking table so a database created on one platform can be opened by the other.

PRAGMAs:
- `journal_mode = WAL`
- `synchronous = NORMAL`
- `temp_store = MEMORY`
- `mmap_size = 268435456` (256 MB)
- `cache_size = -65536` (64 MB)
- `wal_autocheckpoint = 10000` (~40 MB)
- `foreign_keys = ON`

Tables: `files`, `tags`, `ocr_text`, `ocr_fts` (FTS5 virtual), `persons`, `face_prints`, `face_verifications`, `clip_embeddings`, `scan_sessions`, plus `grdb_migrations` for tracking.

Embedding columns are raw `BLOB` of L2-normalized float32 little-endian arrays (512-d for ArcFace and MobileCLIP; 2048 bytes each). Cross-platform compatible.

## IPC contract

Single source of truth: `shared/ipc-schema/ipc.schema.json`. Per-platform DTOs hand-maintained against the schema (codegen lands later). The wire format is Swift Codable's externally-tagged shape:

- `IPCCommand`: `{"id": "<uuid>", "payload": {"<variant>": <body>}}`
- `IPCEvent`: `{"t": "<iso8601>", "payload": {"<variant>": <body>}}`
- Variants with no payload encode their body as `{}` (e.g. `{"shutdown": {}}`)
- Variants whose Swift case has a single unnamed associated value wrap the body in `{"_0": ...}` (e.g. `{"ready": {"_0": {...}}}`)

Object keys are emitted in alphabetical order on the macOS side for byte-deterministic round-trips. Date fields are ISO8601 strings; binary blobs are base64. Newline-terminated, one frame per line.

## Scan pipeline

Three stages, each connected by a bounded async channel for backpressure:

```
Discovery (1 task, walkdir)
    │
    │  AsyncChannel<DiscoveredFile>, capacity 1024
    ▼
Tagging (N workers, N = num_physical_cores * 1.7)
    │   - read file
    │   - compute dHash (perceptual hash)
    │   - decode image (or PDF page / video keyframe / doc thumbnail)
    │   - SCRFD face detection + ArcFace embedding (per face)
    │   - OCR (fast tier)
    │   - MobileCLIP image embedding
    │   - parse EXIF / GPS / camera model
    │  AsyncChannel<TaggedFile>, capacity 256
    ▼
DBWriter (1 task, batched)
    │   - 100 files OR 200 ms per transaction
    │   - resume cursor in same transaction as inserts
    │   - p95 insert latency target: ≤ 50 ms
    ▼
Post-scan (orphan sweep, face clustering job auto-enqueued)
```

ANE/GPU semaphores (3-4 for ORT inference, 2 for CLIP) bound concurrent ML calls. Sync mirrors (atomic-bool) for hot-path cancellation checks avoid the actor-hop tax inside tight loops.

Performance target: ≥ 140 files/s on M1 Pro (macOS) or comparable mid-tier x64 with DirectML, scaling per hardware tier (see `shared/docs/SHIP.md`).

## ML inference

### macOS
- Apple Vision (face rects + quality + OCR)
- CoreML (MobileCLIP image, CLIP text)
- ONNX Runtime + CoreML EP (ArcFace face embedder)
- MLX (VLMs for Deep Analyze: Qwen, Gemma, PaliGemma)

### Windows
- ONNX Runtime with auto-detected EP (CUDA / OpenVINO / DirectML / QNN / CPU) — see GPU acceleration strategy below
- llama.cpp (VLMs: Qwen2.5-VL, Gemma 3, MiniCPM-V) with backend auto-pick (CUDA / Vulkan / DirectML / CPU)
- SCRFD ONNX (face detection — landmarks → solve PnP for pose)
- Windows.Media.Ocr (built-in WinRT OCR; PaddleOCR ONNX as opt-in)
- pdfium-render, Media Foundation (PDF + video)

### GPU acceleration strategy (Windows)

At first launch the engine probes hardware in priority order:

```
1. NVIDIA → CUDA EP (if CUDA + cuDNN runtime present), else TensorRT, else DirectML
2. Intel → OpenVINO EP (if OpenVINO present), else DirectML
3. Snapdragon WoA → QNN EP (if QNN present), else DirectML on Adreno
4. AMD → DirectML
5. CPU floor (AVX2/AVX-512 on x64; NEON on arm64)
```

**Base install ships DirectML + CPU + Vulkan (llama.cpp)** — covers every GPU vendor without extra runtime install. **Optional Performance Packs** (CUDA / OpenVINO / QNN) downloaded from Settings when matching hardware is detected. Same downloader pattern as model downloads. No telemetry.

## Visual language

Single palette across platforms. Documented in `shared/docs/VISUAL-LANGUAGE.md`. Per-platform Theme files (`Theme.swift`, `Theme.xaml`) reference the same hex values. Custom motion primitives (Shimmer, CompletionRipple, IridescentBorder, LavaLamp) are visually identical across platforms; their implementations differ (SwiftUI Canvas / Win2D / Skia) but their parameters (colors, durations, easings) match.

## Privacy & security

Zero telemetry. Every guarantee is in `shared/docs/PRIVACY.md`. CI grep-gates shipped binaries for telemetry-related strings. The only network code in the engine is the model downloader. Logs are local-only and path-redacted.

Engine binary integrity verified at app spawn time:
- macOS: `SecCode` / `SecStaticCode` against the embedded code-signing identity
- Windows: `WinVerifyTrust` (Authenticode) against the EV cert thumbprint

The app refuses to spawn the engine if the signature doesn't match.

## Cross-platform discipline

Three rules every change should follow:

1. **The IPC schema is the source of truth.** Changing a payload means editing `shared/ipc-schema/ipc.schema.json` first, then updating all three (current: two) DTO files in lockstep.
2. **The macOS app is the visual reference.** The Windows port is 1:1 with macOS, not a "Windows-style reinterpretation". Linux will be the same against macOS.
3. **No telemetry, ever.** Don't propose features that violate this even if the integration is "tiny". The privacy posture is a product feature.
