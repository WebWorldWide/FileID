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

### Front-ends over the contract

The SwiftUI / WinUI 3 / GTK4 apps are not the only clients. There is also a cross-platform **CLI** (`platforms/cli`, the `fileid` binary) over the same library DB + engine. Where the GUI apps *spawn* the engine and stream IPC, the CLI *links the engine crate as a library* and calls its public surface in-process (`db::open_writer`/`open_read` + migrations, `pipeline::discovery::FileKind`, `pipeline::restructure::classify`, `paths::db_path`). The two integration styles are deliberate: the MVP's read/query commands (search, info, people, dedupe) have **no IPC command** — the GUIs run them as direct read-only SQL too — and the engine's `startScan` IPC hard-requires ML models, so the CLI's model-free FTS `scan` writes through the engine's own schema instead. Linking the engine (path dependency) guarantees the CLI shares the exact tables, migrations, and classification logic — it cannot drift from the contract. See `platforms/cli/README.md`.

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

### Restructure apply (file moves + symlinks)

`pipeline/restructure_apply.rs` executes an approved restructure plan: it relocates each file (real move) or, when the user picks the "use shortcuts/symlinks instead of moving" option, creates a link in place. The on-disk primitives are platform-gated:

- **Windows** — `MoveFileExW` (with the `\\?\` extended-length prefix for >260-char paths, `MOVEFILE_COPY_ALLOWED`, no `REPLACE_EXISTING`) and `CreateSymbolicLinkW` (unprivileged-create flag).
- **Non-Windows (Linux/macOS, `#[cfg(not(windows))]`)** — a portable `std`-only path: `std::fs::rename` for the common same-filesystem move, falling back to `std::fs::copy` + `remove_file` on `EXDEV` (cross-device, e.g. a NAS mount → local disk) so the file is preserved; `std::os::unix::fs::symlink` for the symlink option. Both arms create the destination parent on demand and never clobber an existing destination (parity with the Windows no-`REPLACE_EXISTING` contract). The collision-uniquify logic (case-folded `claimed` set) sits above this and is platform-agnostic.

### Shell / system integrations (`engine/src/shell/`)

`shell/mod.rs` is the per-platform surface for OS-level actions (reveal-in-file-manager, trash, file tags, OCR, video keyframes, thumbnails, HEIC decode). Each module is gated three ways with one identical public signature so every caller is platform-agnostic:

- **Windows (`#[cfg(windows)]`)** — the real Win32/WinRT backends (`SHOpenFolderAndSelectItems`, `IFileOperation`, `IPropertyStore` `System.Keywords`, `Windows.Media.Ocr`, Media Foundation, `IThumbnailProvider`).
- **Linux (`#[cfg(target_os = "linux")]`)** — dependency-free backends on **std + libc + subprocess** (no new crates):
  - **trash** — freedesktop Trash spec via `std::fs`: move the file into `$XDG_DATA_HOME/Trash/files/` (default `~/.local/share/Trash`), write `Trash/info/<name>.trashinfo` (`Path=` percent-encoded, `DeletionDate=` local ISO-8601 from libc `localtime_r`), atomically claim the name with `create_new` + numeric suffix on collision, and copy-fallback on `EXDEV`.
  - **reveal** — `org.freedesktop.FileManager1.ShowItems` over the session bus (spawning `dbus-send`, then `gdbus`), selecting the item; falls back to `xdg-open` on the parent directory.
  - **tags** — the `user.xdg.tags` extended attribute (comma-separated, the Nautilus/Tracker convention) via libc `setxattr`/`getxattr`/`listxattr`/`removexattr`. Moves need no sidecar: `rename(2)` carries the xattr with the inode.
  - **ocr** — best-effort: write the RGB buffer to a temp P6 PPM and run the `tesseract` CLI (`tesseract <ppm> stdout`); returns empty text (never an error) when tesseract is not on `PATH`.
  - **video** — best-effort keyframe: `ffprobe` for the duration → `ffmpeg -ss <25%> … -vcodec ppm` to a temp P6 PPM we parse directly (no image decoder); graceful `Err` when ffmpeg is absent, which the callers already tolerate.
- **macOS / other Unix (`#[cfg(all(not(windows), not(target_os = "linux")))]`)** — graceful stubs (bail or empty) so the macOS engine compiles unchanged; macOS file actions are handled app-side. `thumbnail` + `heic` stay stubbed on every non-Windows OS (TODO: gdk-pixbuf / libheif on Linux).

The Linux arms compile and are clippy/test-gated only on the Linux target (`.github/workflows/linux.yml`); the macOS build parses but cfg-strips them.

## ML inference

### macOS
- Apple Vision (face rects + quality + OCR)
- CoreML (MobileCLIP image, CLIP text)
- ONNX Runtime + CoreML EP (ArcFace face embedder)
- MLX (VLMs for Deep Analyze: Qwen, Gemma, PaliGemma)

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
- Windows: `WinVerifyTrust` (Authenticode) against an independently pinned signer public-key identity for both app assembly and engine

The app refuses to spawn the engine if the signature doesn't match.

## Cross-platform discipline

Three rules every change should follow:

1. **The IPC schema is the source of truth.** Changing a payload means editing `shared/ipc-schema/ipc.schema.json` first, then updating all three (current: two) DTO files in lockstep.
2. **The macOS app is the visual reference.** The Windows port is 1:1 with macOS, not a "Windows-style reinterpretation". Linux will be the same against macOS.
3. **No telemetry, ever.** Don't propose features that violate this even if the integration is "tiny". The privacy posture is a product feature.
