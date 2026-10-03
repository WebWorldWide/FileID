# Architecture — cross-platform overview

## Native persistent retrieval (catalog v23 / IPC v1.6)

The macOS engine owns a model-specific CLIP HNSW index and a bounded file-ID/fingerprint manifest next to its internal catalog. SQLite v23 records per-namespace instance IDs, generations, random checkpoint nonces, and bounded change logs for CLIP, text, and catalog embeddings. Transactions roll these back together. Missing history, database divergence/reset, incompatible models, corrupt snapshots, or mismatched graph/manifest hashes cause a rebuild from SQLite. Source size/mtime changes discard derived CLIP/text vectors; failed-file changes update index eligibility without discarding accepted names, tags, people, or Undo.

Library semantic search and image similarity now use engine catalog requests rather than UI full-table cosine scans. Hybrid search fuses FTS file/evidence rankings and CLIP rankings; chapters/passages keep their timestamps/pages. Optional file scope deduplicates moments before the result limit for Library grids; all scope retains detailed hits. Refreshed matching retains actor ownership of the graph across coalesced workers, avoiding false empty results during concurrent requests. Before publication, the engine checks current embedding fingerprints and excludes failed files. Cold preparation returns an explicit `indexing` response; Library displays keyword results while waiting. Graph work runs off the command loop, and UI searches cancel their own obsolete waits. Snapshot files enforce Adlon write protection.

Only the pinned 512-dimensional CLIP namespace is indexed in this milestone. Text/catalog streams are tracked for later indexes. Limits are 200,000 active files and bounded graph/manifest reads. Full rebuilds are not yet budgeted, durably scheduled, or cancellable; keyword fallback remains available after failure. The synthetic graph benchmark is not an end-to-end search, real accuracy, or hardware release result. Rust/C# mirror optional v1.6 search fields; PC vector execution returns an explicit unsupported response until the owner resumes port work.

## macOS semantic-cache compatibility (2026-10-02)

The engine writes CLIP embeddings with the shared pinned artifact/preprocessing identity. Native semantic search and restructure accept only compatible finite normalized 512-dimensional vectors. Rescans refresh legacy image/video embeddings through fresh inference without rebuilding user evidence. SQLite remains authoritative and the native retrieval loop is still a flat scan; persistent incremental indexes and engine-owned hybrid search remain outstanding.

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

Two binaries per platform. The app spawns the engine as a child process. They talk newline-delimited JSON over stdin (app → engine) and the engine event stream (macOS fd 2/stderr; Rust stdout). New catalog/tools/chat writes go through the engine. Legacy macOS ReadStore correction and mutation paths still write directly; the application-wide sole-writer boundary remains a migration target. SQLite WAL allows concurrent readers without blocking the writer.

When the engine crashes the app respawns it with bounded backoff (1 s / 4 s / 16 s within a 60 s window). Three failures in a row puts the app in `.crashed` state; user dismisses or retries.

## Storage

SQLite via WAL journaling. Schema versioned through v23 (see `platforms/apple/engine/Sources/FileIDEngine/Storage/Database.swift` for the canonical migration list, and `platforms/windows/src/engine/src/db/migrations.rs` for the byte-faithful Rust port). Both engines use the same `grdb_migrations` tracking table so a database created on one platform can be opened by the other.

PRAGMAs:
- `journal_mode = WAL`
- `synchronous = NORMAL`
- `temp_store = MEMORY`
- `mmap_size = 268435456` (256 MB)
- `cache_size = -65536` (64 MB)
- `wal_autocheckpoint = 10000` (~40 MB)
- `foreign_keys = ON`

Tables: `files`, `tags`, `ocr_text`, `ocr_fts` (FTS5 virtual), `persons`, `face_prints`, `face_verifications`, `clip_embeddings`, `scan_sessions`, plus `grdb_migrations` for tracking.

Embedding columns are raw `BLOB` of L2-normalized float32 little-endian arrays — 512-d for CLIP ViT-B/32 image/text (2048 bytes), 128-d for SFace face prints (512 bytes). Cross-platform compatible.

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
    │   - YuNet face detection + 5-point alignment + SFace embedding (per face)
    │   - OCR (fast tier)
    │   - RAM++ auto-tagging (primary) + CLIP ViT-B/32 image embedding
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
- ONNX Runtime (CLIP ViT-B/32 image/text, RAM++, BGE-small; CoreML EP when supported, CPU fallback)
- ONNX Runtime + CoreML EP/CPU (SFace 128-d embedder; legacy service names still say ArcFace)
- MLX (Deep Analyze: Qwen2.5-VL, Qwen3-VL, Gemma 3, Mistral Small 3.2, PaliGemma)

### Windows
- ONNX Runtime with auto-detected EP (CUDA / OpenVINO / DirectML / QNN / CPU) — see GPU acceleration strategy below
- llama.cpp (VLMs: Qwen2.5-VL 7B, Gemma 3, Mistral-Small-3.2 — all commercial-clean) with backend auto-pick (CUDA / Vulkan / DirectML / CPU)
- RAM++ ONNX (4585-tag auto-tagger, primary; CLIP scene tags as fallback) + CLIP ViT-B/32 ONNX (image + text)
- YuNet ONNX (face detection) + SFace ONNX (128-d embedding) with 5-point similarity alignment; landmarks → PnP for pose
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

## Next-version catalog and timeline (2026-10-01)

Canonical `shared/catalog/v21.sql` is included directly by Rust and mirrored byte-for-byte in Swift `CatalogSchema.swift`; `check_catalog_schema.py` enforces parity. Rust now includes the existing Swift v19 text-stage and v20 full-model migrations before v21. Existing names, identities, corrections, and legacy undo records remain in their original tables. New catalog records include revisions, chapters, coverage, passages, observations/tracks, events/takes, derivative relationships, jobs, operations, recipes, corrections, and local chat. Most entities are storage foundations, not implemented product features.

Persistent FTS indexes are incrementally maintained for file names/descriptions and chapter/passage evidence. Source size/mtime changes stale dependent observations, discard model-separated catalog embeddings, and clear generated filename/caption proposals without renaming originals. User markers remain available for reconfirmation. Embedding storage enforces model/dimensionality separation; persistent ANN retrieval is pending.

IPC v1.1 adds bounded typed catalog requests/responses. The new macOS panel sends all catalog writes to the engine. Existing `ReadStore` user-correction and mutation paths still use writable queues: the broad claim that the UI is entirely read-only is a target, not a description of every existing path. Move those legacy writes behind IPC before claiming a sole-writer architecture application-wide.

macOS timeline jobs persist state/checkpoints, recover interrupted work as paused, and reuse captions keyed by source revision, pinned model, and sample time. They still run on the existing serial major-job queue. A separate engine subprocess decodes each requested frame, reports actual presentation time, and is killed on timeout/cancellation. Model inference remains in-process. Ten-second frame sampling records incomplete coverage and unverified captions; automatic event outcomes, ASR, shot analysis, tracks, chapters, and best takes are pending.

Manual chapter edits and deletion atomically journal corrections and inverse operations. Undo survives restart and marks restored evidence stale if its source revision changed. Decoder output, caches, databases, downloads, logs, and original mutations use protected-location checks; full application write tracing remains required.

See [NEXT_VERSION.md](NEXT_VERSION.md) for implementation boundaries and release gates.

## Typed exports (IPC v1.2)

Tool requests carry supported recipes or saved operation IDs. Preview pins file selection, source SHA-256, chapter snapshots, and destination names; execution rejects changed evidence and publishes new outputs without overwriting. Both engines store export relationships and persistent Undo receipts in the catalog. macOS photo decoding runs in a bounded engine subprocess with cancellation and parent-death checks. Rust currently uses bounded in-process image-rs decoding and reports cancellation unavailable. See TOOLS.md; the general resource scheduler, restart reconciliation, video tools, and native port toolbox remain pending.

Face backfill matches Vision landmarks by overlapping normalized bbox with an ambiguity check, then aligns all faces against a single per-image pixel buffer. SFace consumes raw RGB values and normalizes internally. Swift/Rust share deterministic alignment fixtures; identity accuracy and old-cache refresh remain separate gates.

## Initial local chat (IPC v1.3)

ChatService/commands::chat own local catalog_chat history. Retrieval reuses persistent FTS, exact user-edited event titles, and explicit time filters over timestamped catalog evidence; it returns evidence independently of inference. macOS queues bounded summaries on its existing loaded MLX container, with interactive priority and request-owned cancellation; it does not load models. Runtime remains serial for heavy work and is not yet a memory-budgeted multi-model scheduler. Rust exposes keyword/history parity and explicitly lacks generation. Native port panels, hybrid retrieval, and typed operation execution remain pending. See CHAT.md for exact limits. Legacy macOS ReadStore mutations for cleanup, People, naming, and organization remain exceptions to the intended sole-writer boundary and must move to engine operations before full acceptance.

Initial model admission and native residency leases are documented in SCHEDULER.md. Available memory no longer double-counts speculative pages. Native inference/load/unload ownership is exclusive and cancellable; this remains one heavy model lane, not parallel task-model routing. Rust startup admission is conservative host-memory sizing, not free-VRAM certification.

## Versioned face evidence (v22)

Canonical shared/catalog/v22.sql adds nullable weight, processing, and source-revision metadata without relabeling legacy vectors. New native refreshes and Rust scan results create model-separated 128-d catalog vectors and uncalibrated observations, synchronize person assignment and exclusion, and invalidate changed boxes. Native processing refreshes bounded batches with source and database revision checks; JPEG encoding happens outside the transaction and publication follows committed rows. Legacy clustering still consumes its older embedding table: full space isolation, durable rebuild/backlog scheduling, incremental identity assignment and held-out calibration remain required.

## Native video tools (IPC v1.4)

The existing export journal now admits a video/mp4 recipe at 1280 or 1920 pixels on macOS. Probing, decoding, export and output verification run in a cancellable engine subprocess. Inputs are restricted to single-video SDR containers with at most one audio track; unsupported tracks/HDR/alpha/protected content are rejected. External asset references are forbidden. Output validation runs before the original/derived transaction and collision-safe publication. The portable adapter advertises this capability as unavailable and revalidates saved recipes before touching outputs. See TOOLS.md for exact limits; broad media operations and port parity remain pending.

Face comparison rejects unknown or mixed model/processing namespaces, stale revisions and invalid 128-d vectors before persistence. Legacy person centroids lack provenance and cannot drive inheritance. Reclustering reuses stable identities transactionally and preserves named/unknown/offline rows and correction references. Explicit merges validate selections, transfer one complete name into an empty destination and roll back every SQL failure; they still delete source identities, so aliases/history remain pending. See FACE_CACHE.md for whole-pass refresh limitations and remaining incremental/calibration gates.


## Command-channel health (IPC v1.5)

`healthCheck(requestID:)` replies as `healthCheckResult._0` with the unchanged nonce and actual engine PID. It acknowledges command-loop responsiveness only, with no catalog access, queue admission, model loading or ready-state refresh. Request IDs contain 1–128 ASCII letters/digits/underscores/hyphens; invalid values produce `invalid_health_request` without a success reply or input echo.

Clients must install a waiter before flushing the command, then correlate nonce/PID against the captured process generation and retire old waiters on cleanup. Canonical DTOs and both engines now support the wire operation; the Windows lifecycle integration remains unfinished. A health reply does not prove storage, model readiness or full engine health.


Native `CatalogVectorIndex` preparation uses a persistent `catalogIndex` job alongside the existing snapshot/revision protocol. Job controls dispatch by kind; resumable/retry states do not require a visual model. Checkpoints report progress, while only verified snapshots plus authoritative SQLite history establish index state. One coalesced worker owns graph mutation; general resource reservations and concurrent model routing remain unfinished. See SCHEDULER.md.
