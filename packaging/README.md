# FileID — Linux packaging

How the GTK4 app reaches desktop Linux. Every channel wraps the same two
binaries (`fileid-linux` + the spawned `FileIDEngine`), plus the llama.cpp
runtime where noted below, and reuses the desktop, AppStream, and icon assets
in [`platforms/linux/data/`](../platforms/linux/data/).

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

## Deep Analyze runtime (llama.cpp)

Deep Analyze, speech and chat generation run through `llama-server`. Linux
packages build it from the pinned llama.cpp b9254 source (commit
`e947228222147356bc7e64154d3439e142481632`, archive SHA-256
`b45827df371f9a418680f0137f58a3a27e0e577b4c5e99e54519eea28be67241`) with
[`platforms/linux/build/build-llama-runtime.sh`](../platforms/linux/build/build-llama-runtime.sh).
The builder never downloads: the channel fetches the archive and the builder
verifies it, the vendored license sources, and the three files the FileID patch
changes, then applies
[`llama-b9254-no-outbound.patch`](../platforms/linux/build/patches/llama-b9254-no-outbound.patch)
and checks the patched hashes. The patch disables llama.cpp's outbound HTTP
client (model download, remote image/audio URLs, router proxy); the loopback
inference server that the engine talks to is unchanged.

The build is CPU-only, shared-library, with every ggml CPU variant loaded at run
time (`GGML_BACKEND_DL` + `GGML_CPU_ALL_VARIANTS`), so one build runs on any
x86-64 CPU and uses AVX2/AVX-512 where present (aarch64 gets the ARMv8/v9
variants; only x86-64 is CI-built so far). OpenSSL, the web UI, the
prebuilt-UI fetch, LLGuidance, RPC, KleidiAI and OpenMP are off. The staged
directory holds `llama-server`, its `lib*.so*`, `LICENSE`,
`THIRD-PARTY-NOTICES.txt` (llama.cpp/ggml, cpp-httplib, nlohmann/json, stb_image,
miniaudio, subprocess.h) and `SHA256SUMS`.

| Channel | Runtime location | Status |
|---|---|---|
| Flatpak | `/app/lib/fileid/llama.cpp/` (module `llama-runtime`, source archive pinned in the manifest) | CI builds it in the offline sandbox and runs `llama-server --version` plus the outbound-HTTP probe inside the runtime |
| AppImage | `usr/bin/RuntimeBundles/llama.cpp/` | `build-appimage.sh` fetches and builds it; the AppImage itself is still a native build gate |
| `build.sh` (dev) | `dist/fileid/RuntimeBundles/llama.cpp/` | opt-in: `FILEID_LINUX_LLAMA_DIR=<builder output>` |
| AUR, Nix | not bundled yet | the PKGBUILD pins the v0.1.0 tag, which predates the builder; add it at the next release cut. Until then set `FILEID_LLAMA_SERVER` |

The engine looks for `llama-server` in this order: `FILEID_LLAMA_SERVER`, the
models folder (`llama.cpp-cuda/`, `llama.cpp/`), `RuntimeBundles/llama.cpp/`
beside the engine, then `../lib/fileid/llama.cpp/` relative to the engine's
folder. Each candidate must be an executable 64-bit ELF for the host
architecture; nothing is downloaded at run time. The GTK app enables Deep
Analyze only when one is found.

A Vulkan variant (`--vulkan`) builds in CI against lavapipe but is not packaged:
it needs a check on real GPUs first, and a Vulkan backend that fails at run time
is not retried on CPU by the engine.

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
sandbox and every input is checksum-pinned. The bundled `llama-server` listens
on loopback only, with a per-launch API key, and its outbound HTTP client is
patched out.

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
