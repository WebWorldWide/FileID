# FileID — Linux packaging

How the GTK4 app reaches desktop Linux. Every channel wraps the same two
binaries (`fileid-linux` + the spawned `FileIDEngine`) and reuses the desktop,
AppStream, and icon assets in [`platforms/linux/data/`](../platforms/linux/data/).

## Distro support matrix

| Channel | Covers | Status | Where |
|---|---|---|---|
| **Flatpak** (primary) | Every Flatpak-capable distro via GNOME 49 | required offline-sandbox CI build; immutable release source substitution remains for Flathub submission | [`flatpak/`](flatpak/) |
| **AppImage** (secondary) | Most x86_64 distros | pinned build tools; final construction/launch remains a native runtime gate | [`appimage/`](appimage/) |
| **Nix flake** | NixOS / any Nix user | pinned inputs; native build verification remains | [`nix/`](nix/) |
| **AUR `PKGBUILD`** | Arch-family distributions | immutable canonical release source + SHA-256 | [`aur/`](aur/) |

Flatpak is primary because it carries a stable GTK/libadwaita runtime while the
application remains native GTK4. The current GNOME 49 runtime is backward
compatible with the app's GTK 4.14/libadwaita 1.5 API floor.

## Deep Analyze runtime

The packages install FileID's engine and model downloader, but they do not yet
bundle a reviewed llama.cpp executable. Scan tags, faces, CLIP search, Cleanup,
and Restructure are unaffected. Deep Analyze is currently unavailable in the
Flatpak because a host executable is not visible inside the sandbox. AppImage,
AUR, Nix, and source builds require a compatible `llama-mtmd-cli` on the
launched process's `PATH`. No optional package dependency is declared until a
package providing the required executable/version is verified. The GTK model
picker discloses this before users download VLM weights.

## Flatpak source integrity

`flatpak/io.github.fileid.FileID.yaml` gives the build sandbox no network
access. It uses the Rust SDK extension instead of downloading rustup, builds
Cargo with `--locked --offline`, and stages the exact ONNX Runtime 1.22.0 static
archive as a SHA-256-pinned manifest source for each supported architecture.

`flatpak/cargo-sources.json` contains every registry crate from
`platforms/linux/Cargo.lock` as an immutable URL + checksum. Regenerate and
verify it from the repository root:

```bash
python packaging/flatpak/generate-cargo-sources.py
python packaging/flatpak/generate-cargo-sources.py --check
python packaging/flatpak/test_generate_cargo_sources.py
```

The generator fails closed if a future lockfile introduces a non-registry
source; that source must be handled explicitly before packaging can pass.
Flathub submission still requires replacing the manifest's bounded local `dir`
sources with the immutable archive of the audited release commit. This is done
at release cut because the commit cannot be pinned before it exists.

## ONNX Runtime by channel

Linux portable builds statically link the CPU ONNX Runtime. Flatpak supplies
its pinned archive through `ORT_LIB_LOCATION` and sets `ORT_SKIP_DOWNLOAD=1`;
Cargo cannot fall back to network. AppImage uses ort-sys's checksum-verified
archive fetch on the host. Nix and AUR may instead point `ORT_LIB_LOCATION` at
their package-managed runtime.

## No telemetry

The only runtime network permission is for user-initiated model downloads from
Hugging Face. Flatpak grants `--share=network` for that purpose and never grants
host-wide filesystem access. Build-time source downloads happen outside the
sandbox and every input is checksum-pinned.

## Build quick reference

```bash
# Flatpak
flatpak install -y --user flathub \
    org.gnome.Platform//49 org.gnome.Sdk//49 \
    org.freedesktop.Sdk.Extension.rust-stable//25.08
flatpak-builder --user --force-clean --repo=repo build-dir \
    packaging/flatpak/io.github.fileid.FileID.yaml
flatpak build-bundle repo FileID.flatpak io.github.fileid.FileID

# AppImage
./packaging/appimage/build-appimage.sh

# Nix
nix run ./packaging/nix#fileid

# AUR
cd packaging/aur && makepkg -si
```
