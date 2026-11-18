# Licence audit and compliance

Audit of 2026-11-18 on branch `agent/prod-license` (from `agent/prod-integrate`), plus the
`agent/prod-tract` dependency changes (tract replaces ONNX Runtime as the default inference
backend). FileID is **Apache-2.0** (`LICENSE`); it must stay open source and commercially
distributable. Result: **no blocker**. Every default dependency and model weight is permissive,
the one file-level copyleft family (MPL-2.0, `symphonia`) is documented, LGPL components are
dynamically linked and unmodified, and nothing non-commercial ships or is offered. What was
missing was a distributable notices file; it now exists and is enforced. Open owner/legal
items are at the end.

Status key: **OK** = verified, nothing to do; **FIXED** = changed on this branch; **OWNER** = needs
a decision or a check this audit could not do (no network, no Windows machine).

## How it is enforced

| Gate | What it blocks |
|---|---|
| `cargo deny check licenses bans` (`platforms/windows/src/engine/deny.toml`, CI `windows-engine.yml`) | any engine dependency outside the allowlist (Apache-2.0, MIT, BSD-2/3, ISC, Unicode, Zlib, MPL-2.0, CC0, 0BSD, CDLA-Permissive-2.0) |
| `shared/scripts/gen_third_party_notices.py --check` (CI `policy.yml`, new) | a crate in any of the four `Cargo.lock` files, a `Package.resolved` pin, or a `Directory.Packages.props` package with no reviewed licence entry; a licence outside the allowlist (GPL/AGPL/LGPL/EPL/CDDL/unknown have no election and fail); a stale `THIRD_PARTY_NOTICES.txt` |
| `shared/scripts/check_model_license_policy.py` (CI `policy.yml`) | a model entry whose licence is not Apache-2.0/MIT, or Gemma/NVIDIA without `termsRequired`; denylist of non-commercial families (Qwen 3B/72B, Llama, Apple research, LFM, ...) |
| `shared/scripts/check_binary_privacy.py` | telemetry strings in shipped binaries |

`gen_third_party_notices.py --refresh` rebuilds `shared/licenses/rust-crates.json` from
`cargo metadata` (use `--root <checkout>` to merge another branch's locks). The committed
inventory is the union of this branch and `agent/prod-tract`, so merging tract does not break the
gate; after that merge run `--refresh` and drop stale entries if wanted.

## Licence table

### Rust crates (engine, CLI, TUI, Linux app: 671 registry crates over the four locks plus tract)

| Component | Version | Licence | How distributed | Obligation | Status |
|---|---|---|---|---|---|
| 671 registry crates (all four `Cargo.lock`, three targets, plus `prod-tract` locks) | locked | every one allowlisted: overwhelmingly MIT / Apache-2.0 / dual, then Unicode-3.0, BSD-2/3, ISC, Zlib, CDLA-Permissive-2.0, MPL-2.0 (below), WTFPL (TUI only: `terminfo`) | statically linked into `fileidd` / `FileIDEngine` / `fileid` / `fileid-tui` / `fileid-linux` | notices (copyright + permission text) in `THIRD_PARTY_NOTICES.txt` | OK; `cargo deny` licenses ok on all four manifests, bans ok on the engine |
| `symphonia` 0.5.5 and 15 `symphonia-*` crates | 0.5.5 | MPL-2.0 | statically linked, unmodified | file-level copyleft: keep the notice, offer the upstream source (crates.io exact version / repository, listed in the notices); FileID's own code stays Apache-2.0 | OK, documented (comment in `deny.toml` corrected; "safe to dynamically link" was wrong, static linking an unmodified MPL crate is fine) |
| `ring` | 0.17.14 | Apache-2.0 AND ISC (+ OpenSSL/BoringSSL-style parts) | statically linked | full `LICENSE` incl. BoringSSL text | OK, full text in notices |
| `encoding_rs` | 0.8.35 | (Apache-2.0 OR MIT) AND BSD-3-Clause | statically linked | notice | OK |
| `termina` 0.3.3 | 0.3.3 | MIT OR MPL-2.0 | TUI | elect MIT | OK |
| tract (`tract-onnx` 0.23.10 and 47 new crates: `tract-core/-data/-hir/-linalg/-nnef/-onnx-opl/-extra/-transformers`, `rustfft`, `ndarray` 0.17, `prost` 0.14, `minijinja` 3.1, `metrics`, `nom` 8, `borsh`, `libm`, ...) | 0.23.10 | all MIT / Apache-2.0 / dual (one `anymap3`: BlueOak-1.0.0 OR MIT OR Apache-2.0; elect MIT) | statically linked into the engine on every platform | notices | OK. No copyleft, no unknown, no missing licence. `tract-metal` is **not** in the tree (no Metal GPU crate was added). tract also drops the `native-tls`/`openssl*` chain from the locks |

Evidence: `~/FileIDDev/scratch/lic/deny-*.txt` (engine `bans ok, licenses ok`; CLI/TUI/Linux
`licenses ok`; their `bans` failures are only `wildcard` on the path dependency `fileid-engine`,
which the engine-only `deny.toml` does not apply to, not a licence matter), and
`shared/licenses/rust-crates.json` (every crate with its SPDX string and extracted copyright lines).

### Swift (macOS app, `Package.resolved`)

| Component | Version | Licence | How distributed | Obligation | Status |
|---|---|---|---|---|---|
| mlx-swift (+ mlx, mlx-c, fmt, nlohmann/json: MIT; metal-cpp: Apache-2.0) | 0.32.3 | MIT | in `libFileIDAppleKit.dylib`; `mlx.metallib` built from the MLX kernels | notice | OK, in notices |
| mlx-swift-lm (+ XGrammar Apache-2.0, `NOTICE` kept) | 3.32.3 | MIT | same | notice + Apache NOTICE | OK |
| swift-transformers, swift-huggingface, swift-jinja | 1.3.4 / 0.13.0 / 2.5.1 | Apache-2.0 | same | LICENSE | OK |
| EventSource | 1.5.1 | MIT | same | notice | OK |
| GRDB.swift | 7.11.1 | MIT (SQLite: public domain, system library) | Mac helpers | notice | OK |
| swift-async-algorithms, swift-collections, swift-numerics | 1.1.3 / 1.4.1 / 1.1.1 | Apache-2.0 | static | LICENSE | OK |
| swift-asn1, swift-crypto (incl. BoringSSL, Keccak) | 1.7.3 / 4.5.2 | Apache-2.0 (+ OpenSSL/ISC in BoringSSL) | static | LICENSE + `NOTICE` files (kept in notices) | OK |
| yyjson | 0.12.0 | MIT | static | notice | OK |
| swift-syntax | 603.0.2 | Apache-2.0 with Swift runtime exception | build-time macros only | none shipped | OK |
| onnxruntime-swift-package-manager (vendored, `platforms/apple/vendor/`) + ORT 1.24.2 pod | 1.24.2 | MIT, Microsoft | vendored sources keep their MIT headers and `LICENSE`; pod binary linked statically | notice | OK (notices carry the Microsoft MIT text). ORT's own `ThirdPartyNotices.txt` is not in the bundle: OWNER item 4 |
| ONNX Runtime dylib | 1.22.0 | MIT | no longer shipped: `assemble_app.sh` fails if the bundle holds any `*onnxruntime*` file or `fileidd` links one (the engine runs on tract) | none | OK. Row kept for history; the Swift package row above still links ORT 1.24.2 statically into the legacy Swift `FileIDEngine`/app binary |
| Archivo, Instrument Serif fonts | - | OFL-1.1 | bundled; OFL text next to each font in the app, website and Linux dist | keep copyright + OFL with the fonts | OK |
| App icon, logos | - | project-owned | bundle | - | OK |
| `shared/test-corpus` media (`camera.jpg`, `clip-rotated.mov`, ...) | - | synthetic fixtures (flat-colour render, no photograph) | repo only, never shipped | - | OK; noted in the corpus README |

### Windows (.NET, installers)

| Component | Version | Licence | How distributed | Obligation | Status |
|---|---|---|---|---|---|
| Microsoft.WindowsAppSDK | 1.7.250606001 | MIT upstream repo; framework package under Microsoft's redistribution terms | MSI/Store use the Store-managed runtime dependency | none beyond Microsoft terms | OWNER item 3 (confirm package licence on Windows) |
| Microsoft.Graphics.Win2D | 1.3.0 | MIT | DLL in the app | notice | OK (listed in notices; not re-fetched offline) |
| CsWinRT, System.Reactive, Microsoft.Data.Sqlite, Caching.Memory | 2.1.5 / 6.0.1 / 9.0.0 / 9.0.0 | MIT | app DLLs | notice | OK (same caveat) |
| SQLitePCLRaw.lib.e_sqlite3 | 3.53.3 | Apache-2.0 (SQLite: public domain) | native DLL | notice | OK |
| Windows SDK BuildTools, test packages, WiX 4.0.5 | - | Microsoft SDK terms / MIT / Apache-2.0 / MS-RL | build and test only; WiX's compiled bootstrapper stub lands in the setup executable | MS-RL applies to WiX files only, not to FileID | OK; OWNER item 3 asks for a one-line legal read of the stub |
| ONNX Runtime 1.22.0 (source build, no telemetry) + DirectML EP, OpenVINO and CUDA/cuDNN packs | 1.22.0 | MIT / Microsoft redistributable / Apache-2.0 / NVIDIA EULAs | **no longer shipped**: the `ort-accel` feature, `onnxruntime.dll`, `DirectML.dll`, the OpenVINO/QNN/CUDA packs and their build workflows were removed (tract runs every model) | none | OK. Row kept for history; the CUDA llama.cpp runtime is installed by the user and FileID does not ship or download it |
| PDFium (bblanchon chromium/7857) | 7857 | Apache-2.0 + BSD-3 third-party | `pdfium.dll` | its `licenses/` folder | OK; notice entry added, staging of the folder is OWNER item 4 |
| llama.cpp b9254, whisper.cpp v1.9.0 | - | MIT | Store bundle (`THIRD-PARTY-NOTICES.txt` + `LICENSE` staged by `fetch-store-runtimes.ps1`) | notice | OK |
| `THIRD_PARTY_NOTICES.txt`, `LICENSE.txt` | - | - | now copied into the publish directory (MSI, harvested) and the MSIX payload; `verify-store-package.ps1` requires them | - | FIXED (Windows scripts untested here) |

### Linux

| Component | Version | Licence | How distributed | Obligation | Status |
|---|---|---|---|---|---|
| GTK 4, libadwaita, GLib/GIO, Pango, Cairo, GdkPixbuf | system / GNOME 49 | LGPL-2.1-or-later | **dynamic**: gtk4-rs/libadwaita-rs (MIT) bind to shared objects; Flatpak/AUR/Nix use the runtime or system copies, AppImage bundles replaceable `.so` files via linuxdeploy-plugin-gtk | LGPL text + notice, no modification, user can replace the libraries | OK; LGPL text and note added to the notices |
| llama.cpp b9254 (`llama-server`) | b9254 | MIT | built from the pinned source with the outbound-HTTP patch; `LICENSE` + `THIRD-PARTY-NOTICES.txt` staged | notice | OK |
| FFmpeg 9.0.2 (LGPL build, GPL/nonfree off, no network) + OpenH264 2.6.0 (BSD-2-Clause) | 9.0.2 / 2.6.0 | LGPL-2.1-or-later / BSD-2-Clause | **not in any release artifact**: local development bundle only; `stage-media.ps1` refuses `CI_RELEASE`, build script states no codec patent clearance | LGPL source + text travel with any bundle; Cisco's patent grant covers only Cisco's binaries, not a source build | OK; OWNER item 5 before ever shipping it |
| `LICENSE` + `THIRD_PARTY_NOTICES.txt` in Flatpak (`/app/share/licenses/io.github.fileid.FileID/`), AppImage, AUR, Nix, `build.sh` dist | - | - | added to every channel | Apache-2.0 §4(a) | FIXED |

### Models (`shared/models/manifest.json`, `catalog.json`, Mac `model-catalog.json`)

`check_model_license_policy.py`: 22 artifacts, 9 repos, 5 restricted entries (all Gemma) require
explicit terms acceptance; 215 permissive Mac catalog entries; its 14 unit tests pass. Shared
catalog: 309 entries = 238 Apache-2.0 + 45 MIT + 26 Gemma (all `termsRequired`, none default).
The weights are never bundled; they are downloaded at a pinned revision after the user asks.

| Model | Source | Licence claimed | Verified against | Status |
|---|---|---|---|---|
| RAM++ Swin-L (ONNX self-export) | `Web-World-Wide/ram-plus-onnx` from `xinyu1205/recognize-anything-plus-model` | Apache-2.0 | repo docs (`MODELS.md`, `export_ram_plus_onnx.py` header): upstream code and weights Apache-2.0. Not re-read online | OK; OWNER item 1 (confirm the HF card/LICENSE/NOTICE of the self-hosted repo) |
| YuNet | `opencv/face_detection_yunet` | MIT | OpenCV Zoo model directory licence (MIT) | OK |
| SFace | `opencv/face_recognition_sface` | Apache-2.0 | OpenCV Zoo model directory licence (Apache-2.0) | OK; OWNER item 6 (training-data provenance not stated) |
| Face Mesh (Face Landmarker) | `naklitechie/face-landmarks-onnx` | Apache-2.0 | repo `LICENSE` byte-identical to MediaPipe's (documented in `MODELS.md`) | OK |
| CLIP ViT-B/32 image + text + BPE | `Xenova/clip-vit-base-patch32`, `openai/clip-vit-base-patch32` | MIT | OpenAI CLIP is MIT | OK |
| BGE-small-en-v1.5 | `Xenova/bge-small-en-v1.5` | MIT | BAAI BGE is MIT | OK |
| Qwen2.5-VL 7B, Qwen3-VL 2B/4B/8B, Qwen3.5 (0.8B/2B/4B), Mistral-Small-3.2 24B | `ggml-org`, `Qwen/*`, `lmstudio-community`, `mlx-community`, `bartowski` | Apache-2.0 | by-size policy: the Qwen **3B** (Qwen Research, non-commercial) and **72B** (Qwen licence, MAU cap) variants are denylisted and rejected by the generator and by `check_model_license_policy.py` | OK |
| Gemma 3 4B/12B, PaliGemma 3B (and 26 shared-catalog entries) | `ggml-org`, `mlx-community` | Gemma Terms of Use (`LicenseRef-Gemma`) | opt-in only; Rust engine `terms_accepted()` and the Mac/Windows `ModelLicenseGate` require acceptance with the terms URL; unit-tested | OK |
| Gemma 4 | catalog | Apache-2.0 (Google changed the terms 2026-11-06, see `MODELS.md`) | generator rule | OK |

### Distribution obligations

| Item | Before | After |
|---|---|---|
| Root `LICENSE` (Apache-2.0) | in repo; MSIX had `LICENSE.txt`; installer `license.rtf` | now also in the Mac bundle (`Contents/Resources/LICENSE.txt`), MSI publish dir, Flatpak, AppImage, AUR, Nix, Linux dist |
| `NOTICE` file | none; none needed: FileID itself has no NOTICE, and the dependencies' Apache `NOTICE` texts (swift-crypto, swift-asn1, XGrammar) are reproduced in the notices | unchanged |
| Third-party notices (MIT/BSD/ISC/Apache §4 attribution in binary distributions) | only the Windows Store runtime bundle and the Linux llama.cpp runtime had a notices file | `THIRD_PARTY_NOTICES.txt` (repo root, generated, ~230 KB): components, runtimes, fonts, models, 671 crates with copyright lines, every licence text. Shipped in all channels listed above |
| About / Settings link | none | Mac: Settings → Advanced → Acknowledgements ("Open-source acknowledgements", "FileID license") opens the bundled files. Windows/Linux UI link: OWNER item 7 |
| Policy gate | none | `gen_third_party_notices.py --check` in `policy.yml` |

## Owner / legal decisions

1. **RAM++ provenance.** Confirm on Hugging Face that `Web-World-Wide/ram-plus-onnx` carries the
   upstream Apache-2.0 `LICENSE`, a `NOTICE`, and a statement that the ONNX file is a modified
   (converted) work (Apache §4(b)); re-check `xinyu1205/recognize-anything-plus-model`'s card says
   `apache-2.0`. This audit could not reach the network.
2. **DirectML.** No longer shipped (resolved by removing ONNX Runtime). Nothing to confirm.
3. **Windows NuGet.** On a Windows machine run
   `dotnet list package --include-transitive` and check each nuspec licence against
   `shared/licenses/components.json` (WindowsAppSDK especially). WiX's MS-RL bootstrapper stub in the
   setup executable deserves a one-line legal confirmation (the WiX FAQ says output is not
   subject to MS-RL).
4. **ONNX Runtime third-party notices.** No longer applies: no FileID binary carries ONNX Runtime. (The Mac app still links ONNX Runtime 1.24.2 through the vendored Swift package until its own lane removes it.) PDFium's `licenses/` folder still needs staging.
5. **FFmpeg/OpenH264.** If a media runtime is ever shipped: ship the LGPL source + licence it
   already records, and get codec patent clearance (a source-built OpenH264 has no Cisco grant).
6. **SFace training data.** The weights are Apache-2.0 but the repo does not state the training
   data; face-recognition data sets have a history of consent problems. Not a licence blocker;
   legal may want to look before a public release.
7. **Windows and Linux UI.** Add an About/Acknowledgements entry that opens the shipped
   `THIRD_PARTY_NOTICES.txt` (WinUI Settings; GTK about dialog). The files ship; only the link is
   missing.
8. **Merge order.** Done: `agent/prod-tract` is merged with the licence lane; the inventories
   were refreshed (`gen_third_party_notices.py --refresh`, `--check`) and the reviewed digests in
   `check_bootstrap_supply_chain.py` recomputed.
