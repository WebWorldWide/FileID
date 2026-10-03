# FileID — Linux

GTK4 + libadwaita Rust app using the shared Rust scan engine over typed stdio IPC. The compiled Linux UI exposes Library, Cleanup, People, and scan-model Settings; Deep Analyze and Restructure are not wired.

See [`CLAUDE.md`](./CLAUDE.md) for the full platform conventions, toolkit rationale, and TODO list.

## Build

```bash
sudo apt install build-essential libgtk-4-dev libadwaita-1-dev  # or distro equivalent
./build/build.sh
./dist/fileid/fileid-linux
```

## Status

| Surface | Status |
|---------|--------|
| Shared engine | Builds and launches on Linux/WSL. Scans require `mobileclip_s2` (CLIP image encoder) and `arcface` (YuNet + SFace); real model-backed scans were not exercised without those bundles. |
| GTK app shell | Native dark window with gold brand palette, keyboard-accessible Library / Cleanup / People / Settings navigation, folder picker, scan control, and live engine status. A missing-model scan error opens Settings with install guidance. |
| Library | Wired to read-only engine SQLite queries: filename/tag/text search, kind filters, image thumbnails, and image preview. Verified with generated files under an isolated XDG data directory. |
| Settings: scan models | Only the two required bundles are offered. Install sends user-initiated `prewarmModel`; engine events drive queued/progress/error/cancel/retry states. Installed status requires the engine registry's revision-keyed sentinel and full pinned-file SHA-256 validation in a background worker. An offline fake-engine fixture covered IPC and rejection of an unverified completion; no weights were downloaded. |
| People | Native SQLite-backed face cards with representative crops, active photo/face counts, photo detail, structured names, mark-unknown/hide/show, manual cluster merging, and engine-suggested merge review. Mutations use the real shared engine IPC writer and refresh from SQLite after confirmation. `Group photos by face` uses the engine's clustering command when ungrouped faces exist; no real model-backed clustering was exercised. Face reassignment is not exposed because the shared IPC has no `reassignFace` command. |
| Cleanup | Exact duplicate groups are verified with full-file SHA-256; visually similar image groups use indexed perceptual hashes and start with nothing selected. Native confirmation moves user-selected files to freedesktop.org Trash through typed engine IPC, with an in-app restore action for the last batch and a system Trash shortcut. Verification, empty, partial, loading, failure, and restore-conflict states are visible. Exercised with generated images, an isolated SQLite library, and staged GTK plus engine under WSL/Xvfb; no user library or model bundle was touched. |
| Deep Analyze / Restructure | Source modules exist but are not compiled or presented. Linux Deep Analyze VLM downloads/inference remain unsupported. |
| Shell ops | Image thumbnails decode using the existing image crate. Cleanup uses reversible XDG Trash metadata and no-overwrite restore on Linux; video thumbnails and other native Linux shell integrations remain unsupported/stubbed. |
