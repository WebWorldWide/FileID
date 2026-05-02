# FileID — Windows

Native Windows 10/11 build of FileID, with first-class Snapdragon WoA (ARM64) support.

> **Status (Phase 0):** Engine binary scaffolded. WinUI 3 app + ML pipeline + UI tabs ship in subsequent phases. See `shared/docs/DECISIONS.md` for the phase plan.

## Requirements

### Runtime (end users)
- Windows 10 22H2+ or Windows 11 (any build)
- x64 (Intel/AMD) or ARM64 (Snapdragon WoA)
- ~150 MB free disk space for the base install (Performance Packs + models add up to several GB on demand)

### Development
- Windows 10 22H2+ / Windows 11 host (or WSL2 for the engine; WinUI requires native Windows)
- Rust toolchain 1.78+ (`rustup target add aarch64-pc-windows-msvc` for ARM64 builds)
- PowerShell 7+ (`pwsh`)
- For Phase 1+: Visual Studio 2022 + Windows App SDK 1.6+, .NET 8 SDK or .NET 9 SDK, WiX Toolset v4 (Phase 4)

## Quick build

```powershell
# x64 release
pwsh platforms/windows/build/build.ps1

# x64 release + tests
pwsh platforms/windows/build/build.ps1 -RunTests

# ARM64 cross-compile (works from x64 host)
pwsh platforms/windows/build/build-arm64.ps1
```

Outputs land under `platforms/windows/dist/<arch>/FileID/`.

## Architecture

Two binaries, talking newline-delimited JSON over stdio:

- **`FileIDEngine.exe`** (Rust) — owns the SQLite WAL DB, scan pipeline, ML inference. Phase 0 ships the IPC + DB scaffold; ML lands Phase 1+.
- **`FileID.exe`** (WinUI 3, .NET 8/9 self-contained) — UI. Spawns the engine as a child process, auto-respawns with backoff on crash, verifies the engine's Authenticode signature before each spawn. Lands Phase 1.

See [`shared/docs/ARCHITECTURE.md`](../../shared/docs/ARCHITECTURE.md) for the cross-platform overview.

## GPU acceleration

Out of the box, FileID uses your GPU regardless of vendor:

- **NVIDIA, AMD, Intel** — DirectML (ships in Windows; no extra install)
- **Snapdragon WoA Adreno** — DirectML
- **CPU floor** (AVX2/AVX-512 on x64; NEON on arm64) — when no GPU is present

For maximum perf, optional Performance Packs (Settings → Performance, Phase 4):

| Pack | Hardware | Size |
|---|---|---|
| NVIDIA CUDA Pack | NVIDIA RTX-class | ~600 MB |
| Intel OpenVINO Pack | Intel iGPU + Arc dGPU | ~300 MB |
| Snapdragon NPU Pack | Snapdragon X Elite Hexagon NPU | ~150 MB |

See [`shared/docs/MODELS.md`](../../shared/docs/MODELS.md) for sources + sizes.

## Privacy

Zero telemetry. Local-only logs at `%LOCALAPPDATA%\FileID\logs\`. The only network code in the engine is the user-initiated model downloader. CI grep-gates every shipped binary for telemetry-related strings; zero hits required for release. See [`shared/docs/PRIVACY.md`](../../shared/docs/PRIVACY.md).

## State / cache directories

| Path | Contents |
|---|---|
| `%LOCALAPPDATA%\FileID\fileid.sqlite` | Main library DB (WAL mode) |
| `%LOCALAPPDATA%\FileID\logs\` | Engine + app structured logs (local-only, daily rotation) |
| `%LOCALAPPDATA%\FileID\Models\` | ONNX/OpenAI/Apple-CLIP/ArcFace weights |
| `%LOCALAPPDATA%\FileID\Models\HuggingFace\` | VLM weights (Qwen, Gemma, SmolVLM, MiniCPM-V) |
| `%LOCALAPPDATA%\FileID\thumbs.cache\` | Thumbnail cache |
| `%LOCALAPPDATA%\FileID\face_crops\` | Face crop JPEGs for People view |
| `%LOCALAPPDATA%\FileID\settings.json` | Per-user settings (GPU EP override, etc.) |

Uninstalling deletes the binaries. User data is intentionally NOT auto-cleared on uninstall — use Settings → Advanced → "Wipe local state" (Phase 2+) when you want a fresh start.

## Contributing

See `platforms/windows/CLAUDE.md` for conventions. The cross-platform development principles in the root `CLAUDE.md` apply here too.
