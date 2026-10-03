# Privacy — what we don't do

FileID is on-device software. Your photos, documents, faces, OCR text, EXIF, file paths, and folder structure stay on your machine. This document spells out exactly what FileID *does not* do, so the product proposition is verifiable rather than rhetorical.

It applies to both shipping platforms — Windows (Rust `fileid-engine` + WinUI 3 / .NET 8 app) and macOS (Swift/SwiftUI engine + app). Linux is deferred.

## What we do — Windows Store version

The Windows Store package includes its required Vulkan llama.cpp runtime, whisper.cpp runtime, and OpenVINO provider. These runtimes are staged from SHA-256-pinned release archives during packaging; the installed app does not fetch runtime binaries at startup.

The Windows Store app downloads model files only from `huggingface.co`, only after you choose to install a model. FileID verifies pinned model hashes where the model registry provides them. Your selected model and download size may be visible to your network provider as ordinary HTTPS traffic. Requests do not include your file contents, filenames, folder paths, or search history. Installed models work offline.

FileID reads only files and folders you select or explicitly ask it to scan. Indexes, tags, face embeddings, OCR text, and analysis results remain in FileID's local data directory. The app does not send that data to FileID or other servers and does not include analytics, crash reporting, background update checks, or license-server checks.

## How to verify

- **Source audit.** The engine's only outbound-HTTP code lives in `platforms/windows/src/engine/src/downloader.rs` (Windows, Rust) and `platforms/apple/shared/.../StreamingDownload.swift` (macOS, Swift). Every URL the downloader hits comes from the SHA256-pinned manifest in `shared/docs/MODELS.md`. The call sites are explicit and short.
- **Source URL allowlist (CI gate).** CI scans every `.rs` / `.cs` / `.xaml` / `.swift` source file for any `https?://` URL and fails the build if a host isn't on the allowlist (the five hosts above, plus XAML namespace URNs that are never resolved). A contributor can't silently add a download site, telemetry endpoint, or analytics URL. See `.github/workflows/windows-engine.yml` and `macos.yml`.
- **Telemetry-string scan (CI gate, release blocker).** CI scans every shipped binary — the engine `.exe`, the app `.exe`, and every bundled `.dll` — for a deny-list of 23 telemetry/crash-SDK strings, in both ASCII and UTF-16. Zero hits required. The identical list runs in all three workflows (`windows-engine.yml`, `windows-app.yml`, `macos.yml`) and in the release script `platforms/windows/build/publish-bundle.ps1`. A build containing a forbidden string cannot ship.
- **Network capture.** Run FileID with Wireshark / Fiddler / mitmproxy attached. The only packets you'll see are the downloads listed above, when you trigger them, plus their TLS handshakes. Idle FileID = zero packets.
- **Path redaction in logs.** Even local logs redact user file paths before they're written: `redact_path_for_log(...)` (Rust engine), `PathRedactor.Redact(...)` (Windows app), `redactPathForLog(_:)` (macOS). Each keeps only the last one or two path components (`…/Vacation/IMG.jpg`), so the username and folder layout never reach the log. The single exception is FileID's **own** state tree (`%LOCALAPPDATA%\FileID\…` / `~/Library/Application Support/FileID/…`), which passes through verbatim because those paths are structural, useful for debugging, and the passthrough is anchored to the resolved root so no user path can ride along. One residual: `engine-stderr.log` (macOS) captures raw third-party library diagnostics (MLX/Metal/ONNX) rerouted off the IPC wire — those libraries occasionally print paths and are outside our redaction reach; the file is local-only and never leaves the machine, like every other log.

## Where data lives

| Platform | Database | Logs | Models | Thumbnails | Face crops |
|---|---|---|---|---|---|
| Windows | `%LOCALAPPDATA%\FileID\fileid.sqlite` | `%LOCALAPPDATA%\FileID\logs\` | `%LOCALAPPDATA%\FileID\Models\` | `%LOCALAPPDATA%\FileID\thumbs.cache\` | `%LOCALAPPDATA%\FileID\face_crops\` |
| macOS | `~/Library/Application Support/FileID/fileid.sqlite` | `~/Library/Application Support/FileID/logs/` | `~/Library/Application Support/FileID/Models/` + `~/Documents/huggingface/models/` | `~/Library/Application Support/FileID/thumbs.cache/` | `~/Library/Application Support/FileID/face_crops/` |

The engine owns the SQLite WAL database (migrations v1–v12, byte-faithful with the macOS GRDB schema). Downloaded VLM weights cache under `%LOCALAPPDATA%\FileID\Models\HuggingFace\` (Windows) / `~/Documents/huggingface/models/` (macOS).

Uninstalling deletes the binaries. The user-data directory is intentionally **not** auto-deleted on uninstall — we don't want to surprise-wipe a multi-GB model + thumbnail cache. Clear it explicitly: `scripts/wipe_local_state.sh` on macOS, or delete `%LOCALAPPDATA%\FileID\` on Windows (a one-click Settings button is planned).

## What we promise about future versions

- These guarantees apply to every shipping build. No "minor exceptions" for "anonymized opt-in metrics later." If we ever decide telemetry is necessary, it requires a major version bump and a banner-level disclosure on first launch.
- The CI telemetry-string scan and source-URL allowlist are part of the release process. A build that trips either gate cannot ship.
- No third-party SDK gets added without an explicit privacy review documented in `shared/docs/DECISIONS.md`. The dependency lockfiles (`Cargo.lock`, `Package.resolved`, `Directory.Packages.props`) are reviewed for new transitive deps.

## What we *can't* promise

- **The OS sees your files.** macOS Spotlight indexes paths whether or not FileID talks to it; Windows Search may observe FileID's file activity through modification timestamps. We don't control the OS.
- **Downloaded weights are governed by their upstream license.** The weights are static files — they don't phone home — but the HuggingFace download itself is observable to your network operator (it's a CDN GET). A network operator could see, for example, "this IP downloaded Qwen2.5-VL-7B." FileID's role ends once the file is on disk. Use a VPN if that's a concern.

If you find behavior that contradicts anything in this document, file a GitHub issue — it's a release blocker.
