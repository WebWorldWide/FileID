# FileID — Linux platform

Linux x86_64 + aarch64 port of the macOS/Windows FileID app. 1:1 feature parity with the canonical macOS reference, native Linux UI, native Linux performance.

This file covers the Linux code under `platforms/linux/`. For the macOS reference see `platforms/apple/CLAUDE.md`. For the Windows sibling see `platforms/windows/CLAUDE.md`. For cross-platform contracts see `shared/`.

## Stack

- **Engine**: Rust (`fileid-engine`), single-binary release with LTO. Talks newline-delimited JSON over stdio. Owns the SQLite WAL DB, scan pipeline, ML inference. **Shared with the Windows port** — same crate at `platforms/windows/src/engine/`, referenced via Cargo path dependency. V15.5 cfg-gated the Win32 surface (`shell/*.rs` modules + `ort` DirectML feature) so the same code compiles on Linux.
- **App**: GTK4 + libadwaita via `gtk4-rs`. Rust binary, single executable. Adwaita HeaderBar / NavigationView / dark mode follows the system; brand palette (gold #FFCC00, lavender #B19BCE, cyan #A0E2EA, pink #F2A6C0) applied via custom CSS provider.
- **Distribution**: Flatpak (planned, primary), AppImage (planned, secondary). Both produced by the same Cargo binary; the manifest just wraps it.

## Layout

```
platforms/linux/
├── CLAUDE.md
├── README.md
├── Cargo.toml                      # workspace; references the shared engine
├── src/
│   └── app/                        # GTK4 + libadwaita app
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs             # gtk app entrypoint, adw::Application
│           ├── window.rs           # main window + HeaderBar + tab nav
│           └── engine_client.rs    # spawn engine subprocess, NDJSON stdio
├── data/
│   ├── io.github.fileid.FileID.desktop      # XDG desktop entry
│   └── io.github.fileid.FileID.metainfo.xml # AppStream metadata (Flathub)
├── build/
│   └── build.sh                    # cargo build + stage assets
└── flatpak/                        # Phase 2: Flatpak manifest + repo bootstrap
```

## Toolkit choice rationale

Considered:
- **GTK4 + libadwaita (chosen)** — GNOME-native; mature gtk4-rs bindings; libadwaita matches the design language we want (GlassCard analog = `adw::PreferencesGroup`, springs via Composition-equivalent `adw::SpringAnimation`); dark mode + accent color follow the system; aligns with "no web tech" + "native primitives" from root CLAUDE.md.
- **Qt 6 with cxx-qt** — more cross-platform, but C++ centric, the design language feels less Linux-native, and Rust bindings are less mature than gtk4-rs.
- **Iced / egui / Slint** — pure Rust but immature for complex apps; not native widgets.
- **Tauri / Electron** — violates the "no web tech" guarantee.

GTK4 + libadwaita wins.

## Build (Phase 0 / scaffold)

```bash
# System deps (Debian/Ubuntu):
sudo apt install libgtk-4-dev libadwaita-1-dev

# Build the engine (shared with Windows port):
cd platforms/windows/src/engine
cargo build --release --target x86_64-unknown-linux-gnu

# Build the GTK app:
cd ../../../linux
cargo build --release

# Run:
./target/release/fileid-linux
```

The engine and the app build separately today. Phase 1 plans a unified `build/build.sh` that produces a single staged `dist/fileid/` folder containing both.

## Conventions (Rust app)

- **GTK4 idioms.** Subclass `gtk::Application` / `adw::Window` via `glib::object_subclass!`. Use `clone!` macro for signal handlers (defaults to weak refs).
- **No new dependencies without asking.** Locked set in `src/app/Cargo.toml`. Community-toolkit crates like `gtk4-rs` extension libs require justification in `shared/docs/DECISIONS.md`.
- **No telemetry, ever.** Enforced by CI binary scan (Linux scan will mirror the Windows + macOS one once `linux-app.yml` lands).
- **Path redaction in logs.** Reuse the engine's `redact_path_for_log` for any user file path that hits a log call.
- **Default to no comments.** Add only when the WHY is non-obvious.
- **Springs everywhere.** Use `adw::SpringAnimation` (libadwaita 1.4+); map SwiftUI/WinUI `response`/`dampingFraction` 1:1 via `SpringParams::new(damping_ratio, mass, stiffness)` — derive stiffness from response via `(2π/response)² × mass`.

## Cross-platform shared code

- **Engine crate**: `platforms/windows/src/engine/` is the canonical location today. The Linux app references it via Cargo `path = "../../windows/src/engine"`. **TODO**: move to `shared/engine/` so neither platform "owns" the engine. Captured in `shared/docs/NEXT.md`.
- **IPC schema**: `shared/ipc-schema/ipc.schema.json` is the contract. Both the engine and the GTK app generate types from it (engine via existing `IpcCommand`/`IpcEvent` enums; GTK app via `serde_json` against schema-shaped Rust structs).

## Linux-specific TODOs (open work)

These are blockers for full feature parity on Linux but not for the scaffold. See `shared/docs/NEXT.md` for the schedule.

| Module | Linux implementation | Complexity |
|---|---|---|
| `shell/trash` | `gio::File::trash()` (gio-rs) or `xdg-trash` spec | ~3 days |
| `shell/thumbnail` | `gdk-pixbuf` thumbnail factory + xdg thumbnail spec at `~/.cache/thumbnails/` | ~3 days |
| `shell/ocr` | tesseract via `tesseract-rs` | ~5 days |
| `shell/video` | ffmpeg via `ffmpeg-next` for keyframe extraction | ~2 days |
| `shell/reveal` | `xdg-open` subprocess + DBus `org.freedesktop.FileManager1.ShowItems` | ~1 day |
| `shell/tags` | xattr `user.xdg.tags` (XDG standard) via `xattr-rs` | ~1 day |
| `shell/sleep` | DBus `org.freedesktop.ScreenSaver.Inhibit` | ~1 day |

Each currently returns `Err("…not implemented on this platform")` from the stubs in `platforms/windows/src/engine/src/shell/mod.rs`.

## Working principles

- User runs the build. `cargo check` passing isn't proof of correctness — verify on real Linux hardware.
- Update `shared/docs/STATE.md` (latest entry on top) and `shared/docs/NEXT.md` after meaningful work.
- Append to `shared/docs/DECISIONS.md` for non-obvious calls.
- Preserve the user's favorite touches: gold #FFCC00, springs-everywhere motion language. The Linux port is a port, not a reinterpretation.

## Persistence files

See root `CLAUDE.md` and `shared/docs/`. The Linux port doesn't introduce its own persistence files; it appends to the shared ones.
