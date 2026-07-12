# FileID — Linux packaging

How the GTK4 app reaches every desktop Linux distribution. One Cargo binary
(`fileid-linux`) + the engine it spawns (`FileIDEngine`); each channel below just
wraps them. All channels reuse the **single** desktop/metadata/icon source in
[`platforms/linux/data/`](../platforms/linux/data/) — never a copy.

## Distro support matrix

| Channel | Covers | Status | Where |
|---|---|---|---|
| **Flatpak** (primary) | **Every distro** (Debian, Ubuntu, Arch, Gentoo, Fedora, NixOS, openSUSE, …) via the GNOME 46 runtime | developer manifest is a required CI build; offline Flathub sources remain | [`flatpak/`](flatpak/) |
| **AppImage** (secondary) | Most x86_64 distros, no install/sandbox | tool downloads are version/commit + SHA256 pinned; glibc/GTK baseline needs iteration | [`appimage/`](appimage/) |
| **Nix flake** | NixOS / any Nix user | input revisions pinned; links nixpkgs ONNX Runtime; real Nix build verification remains | [`nix/`](nix/) |
| **AUR `PKGBUILD`** | Arch / Manjaro / EndeavourOS | written; native `onnxruntime` dep | [`aur/`](aur/) |
| `.deb` / `.rpm` | Debian/Ubuntu / Fedora natively | **off the metadata** — the `.desktop` + `metainfo.xml` + icon are packaging-system-agnostic; a future `nfpm`/`fpm` spec reuses `platforms/linux/data/` | — |
| Gentoo ebuild | Gentoo | **off the metadata** — same data assets; a future `*.ebuild` mirrors the AUR build | — |

**Why Flatpak is primary.** It bundles the GNOME 46 runtime (GTK 4.14 +
libadwaita 1.5 — the exact versions the app's `gtk4 0.8` / `libadwaita 0.6`
bindings target), so it runs identically on a 2020 Debian stable box and an Arch
rolling box. The native channels (AUR, Nix, future .deb/ebuild) exist for users
who prefer their distro's package manager and already have a new-enough GTK.

## ONNX Runtime by channel

Portable Linux builds use `ort` with `download-binaries` and no
`load-dynamic`, statically linking the CPU runtime into `FileIDEngine`. Native
package managers can instead set `ORT_LIB_LOCATION` and
`ORT_PREFER_DYNAMIC_LINK=1` to link their managed shared library.

Per-channel handling of that download:

| Channel | Build-time network? | Approach |
|---|---|---|
| Flatpak | sandboxed (none by default) | developer manifest grants build-only network for Cargo + the static ORT archive; Flathub submission must vendor both as pinned offline sources. |
| AppImage | host has network | statically linked engine; no runtime ORT file or loader hook. |
| Nix | sandboxed (none) | link nixpkgs `onnxruntime` with `ORT_LIB_LOCATION` + `ORT_PREFER_DYNAMIC_LINK=1`. |
| AUR | makepkg has network | link Arch's `onnxruntime` package with the same build variables. |

The required CI job proves the developer manifest builds and bundles. Cargo
verifies locked crate checksums and ort-sys verifies its runtime archive SHA256,
so network transport cannot silently change build inputs. Store submission
still requires expressing those same inputs as pinned offline sources.

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
