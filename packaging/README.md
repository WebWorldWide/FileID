# FileID — Linux packaging

How the GTK4 app reaches every desktop Linux distribution. One Cargo binary
(`fileid-linux`) + the engine it spawns (`FileIDEngine`); each channel below just
wraps them. All channels reuse the **single** desktop/metadata/icon source in
[`platforms/linux/data/`](../platforms/linux/data/) — never a copy.

## Distro support matrix

| Channel | Covers | Status | Where |
|---|---|---|---|
| **Flatpak** (primary) | **Every distro** (Debian, Ubuntu, Arch, Gentoo, Fedora, NixOS, openSUSE, …) via the GNOME 46 runtime | manifest written; **build is advisory in CI** (ONNX-in-sandbox needs Linux-side verification) | [`flatpak/`](flatpak/) |
| **AppImage** (secondary) | Most x86_64 distros, no install/sandbox | build script written; glibc/GTK baseline needs iteration | [`appimage/`](appimage/) |
| **Nix flake** | NixOS / any Nix user | `flake.nix` written; ONNX sourcing needs verification | [`nix/`](nix/) |
| **AUR `PKGBUILD`** | Arch / Manjaro / EndeavourOS | written; native `onnxruntime` dep | [`aur/`](aur/) |
| `.deb` / `.rpm` | Debian/Ubuntu / Fedora natively | **off the metadata** — the `.desktop` + `metainfo.xml` + icon are packaging-system-agnostic; a future `nfpm`/`fpm` spec reuses `platforms/linux/data/` | — |
| Gentoo ebuild | Gentoo | **off the metadata** — same data assets; a future `*.ebuild` mirrors the AUR build | — |

**Why Flatpak is primary.** It bundles the GNOME 46 runtime (GTK 4.14 +
libadwaita 1.5 — the exact versions the app's `gtk4 0.8` / `libadwaita 0.6`
bindings target), so it runs identically on a 2020 Debian stable box and an Arch
rolling box. The native channels (AUR, Nix, future .deb/ebuild) exist for users
who prefer their distro's package manager and already have a new-enough GTK.

## The ONNX Runtime question (read this before touching any channel)

The engine links ONNX Runtime through the `ort` crate configured with
`load-dynamic` + `download-binaries`
([`platforms/windows/src/engine/Cargo.toml`](../platforms/windows/src/engine/Cargo.toml),
`cfg(not(windows))` deps — **not editable from packaging**):

- `download-binaries` ⇒ `ort-sys`' build script **downloads** `libonnxruntime.so`
  from pyke's CDN **at build time**. This needs network during build.
- `load-dynamic` ⇒ onnxruntime is **dlopen'd at runtime**, not linked. The loader
  finds it via `ORT_DYLIB_PATH` (or the default library search path).

Per-channel handling of that download:

| Channel | Build-time network? | Approach |
|---|---|---|
| Flatpak | sandboxed (none by default) | grant `--share=network` to the **build step only**; stage the downloaded `.so` into `/app/lib`; `--env=ORT_DYLIB_PATH=/app/lib/libonnxruntime.so`. *Flathub forbids build network → hardening: vendor onnxruntime offline + `cargo-sources.json` + `ORT_LIB_LOCATION`.* |
| AppImage | host has network | bundle the downloaded `.so`; `ORT_DYLIB_PATH` via an AppRun hook. |
| Nix | sandboxed (none) | use nixpkgs `onnxruntime`; `ORT_LIB_LOCATION` (build) + `ORT_DYLIB_PATH` (runtime). |
| AUR | makepkg has network | depend on system `onnxruntime`; default loader path resolves `/usr/lib/libonnxruntime.so`. |

**This is the riskiest part and the reason the Flatpak CI job is advisory.**
Whether `ORT_LIB_LOCATION` fully suppresses the `download-binaries` fetch under
the `load-dynamic` combination for `ort 2.0-rc.10` must be confirmed on a real
Linux box. Each manifest documents its assumption inline.

## No telemetry

Every channel honors the project rule: the **only** network egress at runtime is
user-initiated AI model downloads from `huggingface.co`. The Flatpak `finish-args`
grant exactly `--share=network` for that and nothing host-wide; CI scans the
shipped binaries for telemetry strings (see `.github/workflows/linux.yml`).

## Build quick reference

```bash
# Flatpak (primary)
flatpak install -y flathub org.gnome.Platform//46 org.gnome.Sdk//46 \
    org.freedesktop.Sdk.Extension.rust-stable//23.08   # GNOME 46 base = freedesktop 23.08
flatpak-builder --user --install --force-clean build-dir \
    packaging/flatpak/io.github.fileid.FileID.yaml
flatpak run io.github.fileid.FileID

# AppImage
./packaging/appimage/build-appimage.sh        # -> FileID-x86_64.AppImage

# Nix
nix run ./packaging/nix#fileid

# AUR
cd packaging/aur && makepkg -si
```
