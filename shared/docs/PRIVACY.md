# Privacy — what we don't do

FileID is on-device software. Your photos, documents, faces, OCR text, EXIF, file paths, and folder structure stay on your machine. This document spells out exactly what FileID *does not* do, so the product proposition is verifiable rather than rhetorical.

It applies to all three desktop platforms — Windows (Rust `fileid-engine` + WinUI 3 / .NET 8), macOS (Swift/SwiftUI), and Linux (the shared Rust engine + GTK4/libadwaita).

## What we don't ship

- **No analytics SDK.** Not Sentry, not Application Insights, not Firebase, not Segment, not Mixpanel, not Amplitude, not PostHog, not Datadog, not Bugsnag, not Rollbar, not Honeycomb, not NewRelic, not Raygun, not Google Analytics, not App Center. None of them.
- **No crash-reporting service.** No Crashpad, no Breakpad, no remote dump upload. Crashes write a structured tracing log to a local-only directory. You can attach the file to a GitHub issue manually if you choose to share it. We never receive it automatically.
- **No update pings.** No "checking for updates" call at startup or anywhere else. If we add an auto-updater later it will be user-initiated and disclosed.
- **No model-download telemetry.** The engine fetches model weights over plain HTTPS GETs. No metadata exfil. No "user X downloaded model Y" beacon. Not before the download, not during, not after.
- **No license-server check, no DRM phone-home, no entitlement validation, no "user count" reporting.**
- **No A/B test framework.** Every user gets the same code path.
- **No `User-Agent` fingerprinting.** Model-download requests send a generic, version-only User-Agent (`FileID/<version> (+local)`) with no machine-, install-, or user-identifying fields.

## What we do — explicitly, only when you trigger it

The shipping contract permits one outbound destination family: **Hugging Face** (`huggingface.co`, `hf.co`, and their subdomains). A model or runtime download starts only from an explicit onboarding, Settings, Deep Analyze, or CLI install action. Every artifact has a reviewed SHA-256 pin. Once installed, inference is offline. Clicking a help/documentation link opens your browser through the OS shell; browser traffic is not FileID binary egress. The local VLM server binds an ephemeral `127.0.0.1` port for process-local IPC and never leaves the machine.

> **Audit release blocker:** the current Windows development registry still contains six hash-pinned runtime archives hosted on GitHub/NVIDIA (llama.cpp Vulkan/CUDA, Whisper, cuDNN, CUDA runtime, and ONNX Runtime CUDA). Those URLs disclose installation/hardware metadata to hosts outside the shipping contract. They are preserved only as an exact reviewed development baseline while byte-identical Hugging Face mirrors are provisioned. `check_runtime_egress.py --known-blockers` rejects any additional host or URL; the strict no-flag gate runs before release staging and prevents `publish=true` until all six URLs and the redirect allowlist are Hugging Face-only. Do not represent a local build that uses those packs as release-conforming.

## How to verify

- **Source audit.** The engine's only outbound-HTTP code lives in `platforms/windows/src/engine/src/downloader.rs` (Windows, Rust) and `platforms/apple/shared/.../StreamingDownload.swift` (macOS, Swift). Downloadable artifact URLs are SHA-256-pinned in the shared manifest or Windows registry.
- **Source URL policy.** CI scans source URLs and separately parses every production Windows `FileEntry`, the initial URL predicate, and redirect policy. During mirror migration, CI permits only the exact six-URL/four-host known-blocker baseline; any addition fails. Signed publication uses the strict Hugging Face-only mode.
- **Telemetry-string scan (CI gate, release blocker).** CI scans every shipped binary — the engine `.exe`, the app `.exe`, and every bundled `.dll` — for a deny-list of 23 telemetry/crash-SDK strings, in both ASCII and UTF-16. Zero hits required. The identical list runs in all three workflows (`windows-engine.yml`, `windows-app.yml`, `macos.yml`) and in the release script `platforms/windows/build/publish-bundle.ps1`. A build containing a forbidden string cannot ship.
- **Network capture.** Run FileID with Wireshark / Fiddler / mitmproxy attached. Idle FileID must emit zero packets. Explicit installs emit only their artifact request and TLS handshakes; current development builds may show the six documented off-policy runtime sources and cannot pass the release gate.
- **Path redaction in logs.** Even local logs redact paths before they're written: `redact_path_for_log(...)` (Rust engine/Linux), `PathRedactor.Redact(...)` (Windows app), and `redactPathForLog(_:)` (macOS). Each keeps only the last one or two path components (`…/Vacation/IMG.jpg`), including FileID's own model/database paths, so usernames and full folder layouts never reach the log. One residual: `engine-stderr.log` (macOS) captures raw third-party library diagnostics (MLX/Metal/ONNX) rerouted off the IPC wire — those libraries occasionally print paths and are outside our redaction reach; the file is local-only and never leaves the machine, like every other log.

## Where data lives

| Platform | Database | Logs | Models | Thumbnails | Face crops |
|---|---|---|---|---|---|
| Windows | `%LOCALAPPDATA%\FileID\fileid.sqlite` | `%LOCALAPPDATA%\FileID\logs\` | `%LOCALAPPDATA%\FileID\Models\` | `%LOCALAPPDATA%\FileID\thumbs.cache\` | `%LOCALAPPDATA%\FileID\face_crops\` |
| macOS | `~/Library/Application Support/FileID/fileid.sqlite` | `~/Library/Application Support/FileID/logs/` | `~/Library/Application Support/FileID/Models/` + `~/Documents/huggingface/models/` | `~/Library/Application Support/FileID/thumbs.cache/` | `~/Library/Application Support/FileID/face_crops/` |
| Linux | `${XDG_DATA_HOME:-~/.local/share}/FileID/fileid.sqlite` | `${XDG_DATA_HOME:-~/.local/share}/FileID/logs/` | `${XDG_DATA_HOME:-~/.local/share}/FileID/Models/` | `${XDG_DATA_HOME:-~/.local/share}/FileID/thumbs.cache/` | `${XDG_DATA_HOME:-~/.local/share}/FileID/face_crops/` |

The engine owns the SQLite WAL database (migrations v1–v19, byte-faithful with the macOS GRDB schema). Downloaded VLM weights cache under `%LOCALAPPDATA%\FileID\Models\HuggingFace\` (Windows) / `~/Documents/huggingface/models/` (macOS).

Uninstalling deletes the binaries. The user-data directory is intentionally **not** auto-deleted on uninstall — we don't want to surprise-wipe a multi-GB model + thumbnail cache. Clear it explicitly: `scripts/wipe_local_state.sh` on macOS, or delete `%LOCALAPPDATA%\FileID\` on Windows (a one-click Settings button is planned).

## What we promise about future versions

- These guarantees apply to every shipping build. No "minor exceptions" for "anonymized opt-in metrics later." If we ever decide telemetry is necessary, it requires a major version bump and a banner-level disclosure on first launch.
- The CI telemetry-string scan and source-URL allowlist are part of the release process. A build that trips either gate cannot ship.
- No third-party SDK gets added without an explicit privacy review documented in `shared/docs/DECISIONS.md`. The dependency lockfiles (`Cargo.lock`, `Package.resolved`, `Directory.Packages.props`) are reviewed for new transitive deps.

## What we *can't* promise

- **The OS sees your files.** macOS Spotlight indexes paths whether or not FileID talks to it; Windows Search may observe FileID's file activity through modification timestamps. We don't control the OS.
- **Downloaded weights are governed by their upstream license.** The weights are static files — they don't phone home — but the HuggingFace download itself is observable to your network operator (it's a CDN GET). A network operator could see, for example, "this IP downloaded Qwen2.5-VL-7B." FileID's role ends once the file is on disk. Use a VPN if that's a concern.

If you find behavior that contradicts anything in this document, file a GitHub issue — it's a release blocker.
