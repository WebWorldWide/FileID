# FileID — Linux platform

Targeting Linux x86_64 and aarch64 with the canonical macOS app as the 1:1 feature reference. The current GTK app has been exercised under x86_64 WSL/Xvfb, not verified on native Linux hardware or aarch64.

This file covers the Linux code under `platforms/linux/`. For the macOS reference see `platforms/apple/CLAUDE.md`. For the Windows sibling see `platforms/windows/CLAUDE.md`. For cross-platform contracts see `shared/`.

## Stack

- **Engine**: Rust (`fileid-engine`), single-binary release with LTO. Talks newline-delimited JSON over stdio. Owns the SQLite WAL DB, scan pipeline, ML inference. **Shared with the Windows port** — same crate at `platforms/windows/src/engine/`, referenced via Cargo path dependency. V15.5 cfg-gated the Win32 surface (`shell/*.rs` modules + `ort` DirectML feature) so the same code compiles on Linux.
- **App**: GTK4 + libadwaita via `gtk4-rs`. Rust binary, single executable; the staged distribution also contains the separately built shared engine executable. Native GTK window and navigation use the brand palette (gold #FFCC00, lavender #B19BCE, cyan #A0E2EA, pink #F2A6C0) via CSS, with dark mode forced in the current app.
- **Distribution**: Flatpak (planned, primary), AppImage (planned, secondary). Both produced by the same Cargo binary; the manifest just wraps it.

## Layout

```
platforms/linux/
├── CLAUDE.md
├── README.md
├── Cargo.toml                      # GTK app workspace; shared engine remains at platforms/windows/src/engine/
├── src/
│   └── app/                        # GTK4 + libadwaita app
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs             # GTK app entrypoint
│           ├── window.rs           # Six-tab navigation + remembered selection
│           ├── engine_client.rs    # shared engine child process, typed NDJSON IPC
│           └── tabs/
│               ├── library.rs      # wired Library
│               ├── people.rs       # wired indexed People, engine IPC mutations
│               └── settings.rs     # wired required scan-model Settings
├── data/                            # XDG desktop entry, AppStream metadata, icon
└── build/
    └── build.sh                    # builds engine/app, stages dist/fileid/, privacy gate
```

## Toolkit choice rationale

Considered:
- **GTK4 + libadwaita (chosen)** — GNOME-native; mature gtk4-rs bindings; libadwaita supplies native primitives (`adw::PreferencesGroup`, `adw::SpringAnimation`) for the macOS-referenced design. The current app forces dark mode and applies the FileID palette through CSS; no web technology.
- **Qt 6 with cxx-qt** — more cross-platform, but C++ centric, the design language feels less Linux-native, and Rust bindings are less mature than gtk4-rs.
- **Iced / egui / Slint** — pure Rust but immature for complex apps; not native widgets.
- **Tauri / Electron** — violates the "no web tech" guarantee.

GTK4 + libadwaita wins.

## Build and current status

From the repository root on Linux (with Rust, Python 3, and build tools installed):

```bash
sudo apt install build-essential libgtk-4-dev libadwaita-1-dev  # Debian/Ubuntu; or distro equivalent
./build.sh -linux --no-run
./platforms/linux/dist/fileid/fileid-linux
```

The root `./build.sh -linux` delegates to `platforms/linux/build/build.sh`, which separately builds the engine at `platforms/windows/src/engine/` and GTK app at `platforms/linux/src/app/`, then stages both executables under `platforms/linux/dist/fileid/` and runs the binary privacy gate. Linux never wipes user data. By default it launches the staged app when `DISPLAY` or `WAYLAND_DISPLAY` is set; a headless shell only stages it. `--no-run` always skips launching; `--debug` selects debug instead of the default release profile. Other build flags, including `--tests`, are unsupported on Linux. Isolated WSL/Xvfb proof used generated photos and SQLite state to exercise native People cards, detail, rename, unknown recovery, and merge through the staged real engine. Real Linux hardware and model-backed scans have not been verified.

For isolated staging, set `FILEID_LINUX_DIST_DIR` to a new absolute directory; `CARGO_TARGET_DIR` likewise keeps Cargo artifacts outside the repository.

The compiled UI wires Library, People, Cleanup, Deep Analyze, Restructure, and required-model Settings. Library has native folder picking, scan status, filename/tag/text search, kind filtering, and image thumbnails/preview. Cleanup uses read-only SQLite queries and full-file SHA-256 for exact duplicates, indexed perceptual hashes for similar images, typed engine IPC for user-confirmed Trash/restore, and the no-overwrite Linux XDG Trash implementation. People reads indexed faces from the shared engine's SQLite DB, displays face crops and photo details, and saves names, unknown status, and merges via engine IPC. All-hidden people remain reviewable with Show them. Face reassignment is hidden because the shared engine has no `reassignFace` contract. Deep Analyze routes VLM selection, download progress, batch/file analysis, and smart-name actions through typed engine IPC. Restructure routes plan, preview, apply, cancel, and undo through the shared engine. The window remembers the last valid tab and picked library path; it never starts a scan automatically on launch.

Settings offers only `mobileclip_s2` (CLIP image encoder) and `arcface` (YuNet + SFace): user-initiated prewarm/cancel, progress, error/retry, and Installed status gated on pinned SHA256 verification. There is no startup download. Missing-model scan errors open Settings. Offline fake-engine interaction exercised model progress/cancellation, but no real weight download, verified installed-bundle UI state, or inference-backed scan has run. Native Linux VLM runtime/download/inference, video thumbnails, Flatpak, and AppImage remain unsupported.

## Conventions (Rust app)

- **GTK4 idioms.** Subclass `gtk::Application` / `adw::Window` via `glib::object_subclass!`. Use `clone!` macro for signal handlers (defaults to weak refs).
- **No new dependencies without asking.** Locked set in `src/app/Cargo.toml`. Community-toolkit crates like `gtk4-rs` extension libs require justification in `shared/docs/DECISIONS.md`.
- **No telemetry, ever.** `build/build.sh` runs the shared binary privacy gate on the staged app and engine; no download instrumentation.
- **Path redaction in logs.** Reuse the engine's `redact_path_for_log` for any user file path that hits a log call.
- **Default to no comments.** Add only when the WHY is non-obvious.
- **Springs everywhere.** Use `adw::SpringAnimation` (libadwaita 1.4+); map SwiftUI/WinUI `response`/`dampingFraction` 1:1 via `SpringParams::new(damping_ratio, mass, stiffness)` — derive stiffness from response via `(2π/response)² × mass`.

## Cross-platform shared code

- **Engine crate**: `platforms/windows/src/engine/` is still the canonical shared engine location. The Linux GTK app uses Cargo `fileid-engine = { path = "../../../windows/src/engine" }` from `src/app/Cargo.toml`; `build/build.sh` builds its engine executable separately. Do not claim it has moved to `shared/engine/`.
- **IPC schema**: `shared/ipc-schema/ipc.schema.json` is the contract. The GTK app imports the engine's typed `IpcCommand`/`IpcEvent` payloads for scan and model commands/events; engine transport is newline-delimited JSON over stdio.

## Linux-specific TODOs (open work)

These are blockers for full feature parity on Linux but not for the scaffold. See `shared/docs/NEXT.md` for the schedule.

| Module | Linux implementation | Complexity |
|---|---|---|
| `shell/thumbnail` | Image thumbnails/preview are wired through the existing image decoder; XDG thumbnail caching and video thumbnails remain unsupported | open |
| `shell/ocr` | tesseract via `tesseract-rs` | ~5 days |
| `shell/video` | ffmpeg via `ffmpeg-next` for keyframe extraction | ~2 days |
| `shell/reveal` | `xdg-open` subprocess + DBus `org.freedesktop.FileManager1.ShowItems` | ~1 day |
| `shell/tags` | xattr `user.xdg.tags` (XDG standard) via `xattr-rs` | ~1 day |
| `shell/sleep` | DBus `org.freedesktop.ScreenSaver.Inhibit` | ~1 day |

The remaining integrations return `Err("…not implemented on this platform")` from stubs in `platforms/windows/src/engine/src/shell/mod.rs`; Trash is implemented in `shell/trash_linux.rs` without new crates.

## Working principles

- User runs the build. `cargo check` passing isn't proof of correctness — verify on real Linux hardware.
- Update `shared/docs/STATE.md` (latest entry on top) and `shared/docs/NEXT.md` after meaningful work.
- Append to `shared/docs/DECISIONS.md` for non-obvious calls.
- Preserve the user's favorite touches: gold #FFCC00, springs-everywhere motion language. The Linux port is a port, not a reinterpretation.

## Persistence files

See root `CLAUDE.md` and `shared/docs/`. The Linux port doesn't introduce its own persistence files; it appends to the shared ones.
