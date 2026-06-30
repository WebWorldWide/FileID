# FileID — Linux platform

Linux x86_64 + aarch64 port of the macOS/Windows FileID app. 1:1 feature parity with the canonical macOS reference, native Linux UI, native Linux performance.

This file covers the Linux code under `platforms/linux/`. For the macOS reference see `platforms/apple/CLAUDE.md`. For the Windows sibling see `platforms/windows/CLAUDE.md`. For cross-platform contracts see `shared/`.

## Stack

- **Engine**: Rust (`fileid-engine`), single-binary release with LTO. Talks newline-delimited JSON over stdio. Owns the SQLite WAL DB, scan pipeline, ML inference. **Shared with the Windows port** — same crate at `platforms/windows/src/engine/`, referenced via Cargo path dependency. V15.5 cfg-gated the Win32 surface (`shell/*.rs` modules + `ort` DirectML feature) so the same code compiles on Linux. On **Linux** the `ort` dependency is configured to **statically link the CPU ONNX Runtime** (`download-binaries` without `load-dynamic`): pyke ships only a static `libonnxruntime.a` for Linux x64, so a load-dynamic build had no `.so` to `dlopen` and ML silently failed — static linking bakes the runtime in and makes full-ML work. CPU EP only on Linux; GPU is future work. See `shared/docs/DECISIONS.md` (2026-06-30).
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
│           ├── main.rs             # entrypoint: adw::Application + theme install
│           ├── theme.rs            # design system: brand CSS (gold palette),
│           │                       #   .glass-card / .pill / .gold-button, force-dark
│           ├── lavalamp.rs         # LavaLampBackground — Cairo blob background
│           │                       #   (gtk::DrawingArea + frame-clock tick)
│           ├── spring.rs           # adw::SpringAnimation helper (macOS spring map)
│           ├── engine_client.rs    # spawn engine (NDJSON stdio via real IpcCommand/
│           │                       #   IpcEvent types), event fan-out, DB reads,
│           │                       #   thumbnail worker, crash respawn
│           ├── window.rs           # app shell: Overlay(LavaLamp→scrim→UI),
│           │                       #   adw::ViewStack + ViewSwitcher (6 tabs), pick/scan
│           └── tabs/
│               ├── mod.rs          # placeholder StatusPage builder
│               └── library.rs      # Library tab: SearchEntry + GridView + preview
├── data/
│   ├── io.github.fileid.FileID.desktop      # XDG desktop entry
│   └── io.github.fileid.FileID.metainfo.xml # AppStream metadata (Flathub)
├── build/
│   └── build.sh                    # cargo build + stage assets
└── flatpak/                        # Phase 2: Flatpak manifest + repo bootstrap
```

## App structure (current)

Foundation + app shell + **Library tab implemented**; the other five tabs are
feature-shaped and compile CI-green (their GTK runtime isn't yet verified on real
hardware). The **`fileid` CLI and `fileid-tui`** (sibling crates `platforms/cli`,
`platforms/tui`) are the most-verified Linux surfaces — both run the shared engine
end-to-end on Linux, including full-ML `scan --models`.

- **Design system** (`theme.rs`): one `gtk::CssProvider` carries the brand
  palette as `@define-color` tokens + the reusable classes — `.glass-card`
  (GlassCard/ultraThinMaterial analog), `.fileid-scrim`, `.pill`/`.pill-active`,
  `.gold-button`, `.file-tile`. Force-dark via `adw::StyleManager`.
- **LavaLamp** (`lavalamp.rs`): a `gtk::DrawingArea` paints a near-black base +
  four drifting radial-gradient blobs (gold/lavender/cyan/pink), redrawn on a
  frame-clock tick (auto-stops while unmapped). Layered as the bottom of a
  `gtk::Overlay` → muted scrim → transparent UI, matching macOS's
  LavaLamp → material → content stack.
- **Engine client** (`engine_client.rs`): spawns the engine, sends commands as
  NDJSON using the engine crate's **real `IpcCommand`/`IpcEvent` types** (no
  hand-rolled wire shape — the old scaffold's flat `{cmd,id,rootPath}` was
  contract drift; correct shape is `{id, payload:{startScan:{rootPath}}}`).
  Events parse on a reader thread and fan out to every UI subscriber on the
  main context. Engine crash → capped backoff respawn.
- **Library read path**: there is **no file-listing IPC command** — the engine
  is the single DB *writer*; the app reads file rows directly from the same
  SQLite WAL DB via `fileid_engine::db::open_read` + `paths::db_path`, exactly
  like macOS/Windows `ReadStore`. Search = filename/tag `LIKE` + OCR `ocr_fts`
  MATCH. Needs `rusqlite` (already transitive via the engine; see DECISIONS).
- **Thumbnails**: a worker thread reads raw image bytes off the main loop; the
  Library decodes + scales them into a `gdk::Texture` on the main thread
  (GdkPixbuf isn't `Send`). Non-images get a themed icon. Video thumbnails
  (engine `generateVideoThumbnail`) are a follow-up.
- **Library tab** (`tabs/library.rs`): debounced `gtk::SearchEntry`, gold kind
  pills, a virtualized `gtk::GridView` + `SignalListItemFactory` over a
  `gio::ListStore` of `BoxedAnyObject(FileRow)` with lazy per-tile thumbnails
  (recycle-guarded), and an `adw::Dialog` preview (image + metadata) on
  activation. Live-scan: throttled reloads on `batchSummary`, final on
  `scanComplete`.

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
- **No telemetry, ever.** Enforced by CI binary scan — `.github/workflows/linux.yml` builds the engine + CLI + GTK app on ubuntu and mirrors the Windows + macOS telemetry-string scan on the engine and app binaries.
- **Path redaction in logs.** Reuse the engine's `redact_path_for_log` for any user file path that hits a log call.
- **Default to no comments.** Add only when the WHY is non-obvious.
- **Springs everywhere.** Use `adw::SpringAnimation` (libadwaita 1.4+); map SwiftUI/WinUI `response`/`dampingFraction` 1:1 via `SpringParams::new(damping_ratio, mass, stiffness)` — derive stiffness from response via `(2π/response)² × mass`.

## Cross-platform shared code

- **Engine crate**: `platforms/windows/src/engine/` is the canonical location today. The Linux app references it via Cargo `path = "../../windows/src/engine"`. **TODO**: move to `shared/engine/` so neither platform "owns" the engine. Captured in `shared/docs/NEXT.md`.
- **IPC schema**: `shared/ipc-schema/ipc.schema.json` is the contract. Both the engine and the GTK app generate types from it (engine via existing `IpcCommand`/`IpcEvent` enums; GTK app via `serde_json` against schema-shaped Rust structs).

## Linux-specific TODOs (open work)

These are blockers for full feature parity on Linux but not for the scaffold. See `shared/docs/NEXT.md` for the schedule.

| Module | Linux implementation | Status |
|---|---|---|
| `shell/trash` | freedesktop Trash spec via `std::fs` (move to `$XDG_DATA_HOME/Trash/files/` + `.trashinfo`, collision suffixing, `EXDEV` copy-fallback) | **Done** (no crate) |
| `shell/reveal` | DBus `org.freedesktop.FileManager1.ShowItems` via `dbus-send`/`gdbus`, `xdg-open` parent-dir fallback | **Done** (no crate) |
| `shell/tags` | xattr `user.xdg.tags` (XDG standard) via libc `{set,get,list,remove}xattr` | **Done** (no crate) |
| `shell/ocr` | `tesseract` CLI on a temp PPM, best-effort (empty when absent) | **Done** (no crate) |
| `shell/video` | `ffmpeg` keyframe → P6 PPM we parse, best-effort (`ffprobe` for the 25% seek) | **Done** (no crate) |
| `shell/thumbnail` | `gdk-pixbuf` thumbnail factory + xdg thumbnail spec at `~/.cache/thumbnails/` | TODO (~3 days) |
| `shell/heic` | best-effort `heif-dec`/`heif-convert` CLI → temp PNG → `image` decode (no GPL libheif linked; graceful skip when the tools are absent) | **Done** (subprocess) |
| `shell/sleep` | DBus `org.freedesktop.ScreenSaver.Inhibit` | TODO (~1 day) |

The five "Done" backends are gated `#[cfg(target_os = "linux")]` in `platforms/windows/src/engine/src/shell/mod.rs` and built only with **std + libc + subprocess** (no new crates). macOS / other Unix keep the `#[cfg(all(not(windows), not(target_os = "linux")))]` graceful stub; `thumbnail` + `heic` are still stubbed on every non-Windows OS. CI: `linux.yml` runs `cargo clippy --all-targets -D warnings` + `cargo test --lib` on the Linux target (where these arms actually compile).

### Done

- **Restructure apply file-move + symlink fallback** — `pipeline/restructure_apply.rs` `move_file`/`make_symlink` now have a portable `#[cfg(not(windows))]` implementation (was previously a `requires Windows` bail). Real moves use `std::fs::rename` with a copy + `remove_file` fallback on `EXDEV` (cross-device, e.g. a NAS mount → local disk); the symlink ("use shortcuts") option uses `std::os::unix::fs::symlink`. Both create the destination parent on demand and refuse to clobber an existing destination (parity with the Windows `MoveFileExW`/`CreateSymbolicLinkW` path). cargo-verified on the macOS host (the not-windows arm compiles there); portable tests `move_file_relocates_creates_parent_and_refuses_clobber` + `make_symlink_creates_link_to_original` cover it.

## Working principles

- User runs the build. `cargo check` passing isn't proof of correctness — verify on real Linux hardware.
- Update `shared/docs/STATE.md` (latest entry on top) and `shared/docs/NEXT.md` after meaningful work.
- Append to `shared/docs/DECISIONS.md` for non-obvious calls.
- Preserve the user's favorite touches: gold #FFCC00, springs-everywhere motion language. The Linux port is a port, not a reinterpretation.

## Persistence files

See root `CLAUDE.md` and `shared/docs/`. The Linux port doesn't introduce its own persistence files; it appends to the shared ones.
