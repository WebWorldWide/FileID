# FileID AppImage (secondary channel)

A single-file, no-install build that runs on most x86_64 desktop Linux distros.
The **Flatpak is the primary channel** (`../flatpak/`) — prefer it. The AppImage
exists for users who want one download with no runtime, no sandbox, and no store.

## What it bundles

`build-appimage.sh` produces `FileID-x86_64.AppImage` containing:

- `fileid-linux` — the GTK4 app, and `FileIDEngine` — the engine it spawns.
- `libonnxruntime.so` — the ONNX Runtime the engine dlopen's (see *ONNX Runtime*
  below).
- The full **GTK4 + libadwaita** runtime, GdkPixbuf loaders, GLib schemas, the
  Adwaita icon theme and GIO modules — pulled in by `linuxdeploy-plugin-gtk`.

## Build

```bash
# from the repo root
./packaging/appimage/build-appimage.sh
# -> packaging/appimage/build/FileID-x86_64.AppImage
```

Requirements on the build host: `cargo` (Rust 1.90), `curl`, `libgtk-4-dev`,
`libadwaita-1-dev`, `pkg-config`, `build-essential`, and FUSE (for AppImages to
self-mount at runtime).

## glibc baseline (important)

AppImages are **forward-compatible only** — a binary built against a newer glibc
will not start on an older host. Build on the **oldest** glibc you want to
support:

| Build host       | glibc | Ships GTK 4.14? | Notes |
|------------------|-------|-----------------|-------|
| Ubuntu 20.04     | 2.31  | ❌ no            | Widest reach, but GTK4/libadwaita must come from a PPA or be built. |
| Ubuntu 22.04     | 2.35  | ❌ (4.6)         | Needs a GTK 4.14 backport PPA for the `v4_14`/`v1_5` bindings. |
| Ubuntu 24.04     | 2.39  | ✅ yes           | Easiest to build; highest floor (won't run on pre-2024 distros). |

The GTK 4.14 / libadwaita 1.5 requirement (from the app's `gtk4 0.8` / `adw 0.6`
bindings) collides with the "old glibc" goal — old distros don't ship a new
enough GTK. **This is the AppImage's main open item and needs Linux-side
iteration**: either build GTK 4.14 into the old-baseline image, or accept the
22.04/24.04 floor. The Flatpak sidesteps this entirely by shipping the GNOME 46
runtime, which is why it is primary.

## ONNX Runtime

The engine uses `ort` with `load-dynamic` + `download-binaries`. The build
downloads `libonnxruntime.so` into `target/` (build-time network), and the
script bundles it into `usr/lib/` and exports `ORT_DYLIB_PATH` from an AppRun
hook (`apprun-hooks/ort-dylib-path.sh`) so the loader finds it at runtime. CPU
inference works out of the box; GPU execution providers (CUDA/OpenVINO) are not
bundled and would need their EP `.so`s added. If the `libonnxruntime.so not
found` warning prints during the build, ONNX sourcing needs Linux-side
verification (the same risk flagged in the Flatpak manifest header).

## Privacy

No telemetry. The only network use at runtime is user-initiated model downloads
from huggingface.co — identical to the macOS/Windows builds. An AppImage is
**unsandboxed**, so it has the same filesystem access as the user (it can read
NAS mounts directly, unlike the Flatpak which needs an explicit override).
