# Privacy — what we don't do

FileID is on-device software. Your photos, documents, faces, OCR text, EXIF, file paths, and folder structure stay on your machine. This document spells out exactly what FileID *does not* do, so the product proposition is verifiable rather than rhetorical.

## What we don't ship

- **No analytics SDK.** Not Sentry, not Application Insights, not Firebase, not Segment, not Mixpanel, not Amplitude, not PostHog, not Datadog, not Bugsnag, not Rollbar, not Honeycomb, not NewRelic, not Raygun, not Google Analytics. None of them.
- **No crash-reporting service.** Crashes write a structured tracing log to a local-only directory. You can attach the file to a GitHub issue manually if you want to share it with us. We never receive it automatically.
- **No update pings.** No "checking for updates" call at startup or anywhere else. If we add an auto-updater later it will be user-initiated and disclosed.
- **No model-download telemetry.** The engine fetches model weights from HuggingFace via plain HTTPS GETs. No metadata exfil. No "user X downloaded model Y" beacon. Not before the download, not during, not after.
- **No license-server check, no DRM phone-home, no entitlement validation, no "user count" reporting.**
- **No A/B test framework.** Every user gets the same code path.
- **No `User-Agent` fingerprinting.** When we make HTTPS requests for model downloads, the User-Agent is the default `reqwest`/curl string with FileID's version — generic and uncorrelated.

## What we do — explicitly, only when you trigger it

Every network egress is initiated by you, with visible UI:

- **Model downloads.** First time you open Deep Analyze (or click "Get model" in Settings), FileID fetches a model from HuggingFace via `reqwest` (Rust engine). Progress bar, ETA, cancel button. SHA256-pinned per-model. After it lands the app works fully offline.
- **Performance Pack downloads.** Optional CUDA / OpenVINO / Snapdragon-NPU runtime packages. Same flow — user-initiated, disclosed, SHA256-pinned.
- **Help / docs links.** Clicking a help link in Settings opens your browser via the OS shell (`ShellExecuteW` on Windows, `NSWorkspace.open` on macOS). This is your browser making the request, not FileID.

That's the entire surface. There are no other network code paths in the binaries.

## How to verify

- **Source audit.** The engine's only HTTP code lives in `platforms/windows/src/engine/src/downloader.rs` (Windows, Rust) and `platforms/apple/shared/Sources/FileIDShared/StreamingDownload.swift` (macOS, Swift). Both files are short and the call sites are explicit.
- **Binary scan.** CI grep-gates every shipped binary for telemetry-related strings before release. Zero hits required. The list is in `.github/workflows/windows-engine.yml` (and the macOS equivalent when CI lands for Apple builds).
- **Network capture.** Run FileID with Wireshark / Fiddler / mitmproxy attached. The only packets you'll see are model downloads (when you trigger them) and TLS handshakes for those. Idle FileID = zero packets.
- **Path redaction in logs.** Even local logs redact user file paths via `redactPathForLog(_:)` (macOS) / `redact_path_for_log(...)` (Rust). The redactor strips `~/Users/<name>/...` to `~/...`. So even if you share a log file, it doesn't doxx your username + folder layout.

## Where data lives

| Platform | Database | Logs | Models | Thumbnails | Face crops |
|---|---|---|---|---|---|
| macOS | `~/Library/Application Support/FileID/fileid.sqlite` | `~/Library/Application Support/FileID/logs/` | `~/Library/Application Support/FileID/Models/` + `~/Documents/huggingface/models/` | `~/Library/Application Support/FileID/thumbs.cache/` | `~/Library/Application Support/FileID/face_crops/` |
| Windows | `%LOCALAPPDATA%\FileID\fileid.sqlite` | `%LOCALAPPDATA%\FileID\logs\` | `%LOCALAPPDATA%\FileID\Models\` | `%LOCALAPPDATA%\FileID\thumbs.cache\` | `%LOCALAPPDATA%\FileID\face_crops\` |

Uninstalling deletes the binaries. The user data dir is intentionally NOT auto-deleted on uninstall — we don't want to surprise-wipe a 50 GB cache. `wipe_local_state.sh` (macOS) / a Settings button (Windows, future) clears it explicitly.

## What we promise about future versions

- These guarantees apply to every shipping build. No "minor exceptions" for "anonymized opt-in metrics later". If we ever decide telemetry is necessary, it requires a major version bump and a banner-level disclosure on first launch.
- The CI binary scan is part of the release process. A build that contains a forbidden string can't ship.
- No third-party SDK gets added without an explicit privacy review documented in `shared/docs/DECISIONS.md`. The dependency lockfiles (`Package.resolved`, `Cargo.lock`) are reviewed for new transitive deps.

## What we *can't* promise

- The OS sees your files. macOS Spotlight indexes paths even if FileID doesn't talk to it. Windows Search may indirectly observe FileID activity through file-modification timestamps. We don't control the OS.
- Models you download are governed by their upstream license. The model weights themselves are static files — they don't phone home — but the upstream HuggingFace download is observable to your network operator (it's a CDN GET). Use a VPN if that's a concern.
- The HuggingFace mirror could be observed (e.g. a network operator could see "this IP downloaded Qwen2.5-VL-3B"). FileID's role ends once the file is on disk.

If you find behavior that contradicts anything in this document, file a GitHub issue — it's a release blocker.
