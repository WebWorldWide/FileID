# FileID AppImage (secondary channel)

A single-file, no-install build that runs on most x86_64 desktop Linux distros.
The **Flatpak is the primary channel** (`../flatpak/`) — prefer it. The AppImage
exists for users who want one download with no runtime, no sandbox, and no store.

## What it bundles

`build-appimage.sh` produces `FileID-x86_64.AppImage` containing:

- `fileid-linux` — the GTK4 app, and `FileIDEngine` — the engine it spawns.
- ONNX Runtime is statically linked into `FileIDEngine`; there is no runtime
  `.so` to locate or bundle.
- The full **GTK4 + libadwaita** runtime, GdkPixbuf loaders, GLib schemas, the
  Adwaita icon theme and GIO modules — pulled in by `linuxdeploy-plugin-gtk`.

## Build

```bash
# from the repo root
./packaging/appimage/build-appimage.sh
# -> packaging/appimage/build/FileID-x86_64.AppImage
# -> packaging/appimage/build/FileID-x86_64.AppImage.sha256
```

Requirements on the build host: `cargo` (Rust 1.90), `curl`, `libgtk-4-dev`,
`libadwaita-1-dev`, `pkg-config`, `build-essential`, and FUSE (for AppImages to
self-mount at runtime). The linuxdeploy binary and GTK plugin are pinned to a
release/commit and SHA256-verified on every run, including cached copies.

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
22.04/24.04 floor. The Flatpak sidesteps this entirely by shipping the GNOME 49
runtime, which is why it is primary.

## ONNX Runtime

Linux portable builds enable `ort`'s `download-binaries` without
`load-dynamic`, which statically links the CPU runtime into `FileIDEngine`.
CPU inference therefore works out of the box with no AppRun loader hook. GPU
execution providers remain future work.

## Privacy

No telemetry. The only network use at runtime is user-initiated model downloads
from huggingface.co — identical to the macOS/Windows builds. An AppImage is
**unsandboxed**, so it has the same filesystem access as the user (it can read
NAS mounts directly, unlike the Flatpak which needs an explicit override).
