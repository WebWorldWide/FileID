# ONNX Runtime — the engine's inference library

The shared Rust engine (`fileid-engine`) runs ML inference (CLIP image search,
RAM++ tagging, YuNet/SFace faces) on **ONNX Runtime**. Runtime linkage is
target-specific: Windows and macOS use `ort`'s `load-dynamic` path, while Linux
release builds statically link the downloaded CPU runtime for a self-contained
binary.

| OS      | Library `ort` loads     | Where it comes from                                     |
|---------|-------------------------|---------------------------------------------------------|
| Windows | `onnxruntime.dll`       | Bundled beside the engine / pinned via accelerator pack |
| Linux   | Statically linked       | `download-binaries` CPU archive (native distro recipes may link a system package) |
| macOS   | `libonnxruntime.dylib`  | **Installed once — see below**                          |

This is **separate from the AI model weights** (`fileid models download`). Models
are the `.onnx` weights; the runtime is the library that loads + executes them.
A full-ML scan needs **both**.

License: ONNX Runtime is **MIT-licensed** (microsoft/onnxruntime) — commercial-clean.

---

## Why macOS needs a one-time install

`ort 2.0.0-rc.10` with `download-binaries` fetches a prebuilt ONNX Runtime for
the build target. For `aarch64-apple-darwin` that prebuilt is a **static**
`libonnxruntime.a` (pyke's "none" tarball contains no `.dylib` — verified). A
`load-dynamic` build can't use a static archive at run time, so **there is no
`libonnxruntime.dylib` unless you install one**. Without it, a full-ML scan fails:

```
model_load_failed: libonnxruntime.dylib (no such file)
```

(`ort`'s `copy-dylibs` feature wouldn't help — there's no dylib in the download to
copy. The alternative would be to statically link the `.a`; we keep `load-dynamic`
so the runtime is swappable and matches the Windows/Linux model.)

### Required version

`ort 2.0.0-rc.10` targets **ONNX Runtime 1.22.0** and **hard-panics if the loaded
dylib's minor version is < 22**. Install **≥ 1.22**. Newer builds load fine via
ONNX Runtime's ABI back-compat — **verified end-to-end on macOS arm64 with
Homebrew's ONNX Runtime 1.27.0**: the engine dlopen'd it, loaded MobileCLIP, and
a full-AI scan completed (real CLIP tags) from both the CLI and the TUI. An
*older* dylib (< 1.22) aborts.

---

## Installing on macOS — pick one

### 1. `fileid runtime install` (recommended)

```sh
fileid runtime install        # idempotent; reports if already present
fileid runtime install --force # always (re)download the pinned 1.22.0 build
fileid runtime status         # where it is / isn't + the search path + source
```

Without `--force` it first reuses any runtime already on the machine (Homebrew,
beside the engine — **zero network**). FileID does not hardcode a third-party
runtime URL under its HuggingFace-only egress policy. To use the download path,
set `FILEID_ORT_DYLIB_URL` to a HuggingFace-hosted mirror of the official 1.22.0
archive; the archive hash is then verified before install. With `--json`, declining
a configured download emits one JSON object with `aborted: true` and exits zero;
requesting an install with no local or configured source emits
`error: "no_source_configured"` and exits nonzero.

### 2. Homebrew

```sh
brew install onnxruntime
```

Installs `/opt/homebrew/lib/libonnxruntime.dylib`, which the engine probes
directly — no further step. (Homebrew tracks the latest ONNX Runtime; ≥ 1.22 is
required, and is what Homebrew ships today.)

### 3. The shell script (hash-pinned HuggingFace mirror)

```sh
FILEID_ORT_DYLIB_URL=https://huggingface.co/<mirror>/onnxruntime-osx-arm64-1.22.0.tgz \
  shared/scripts/install_onnxruntime_macos.sh
shared/scripts/install_onnxruntime_macos.sh --force  # reinstall
```

Downloads a HuggingFace-hosted byte-for-byte mirror of the official,
MIT-licensed `onnxruntime-osx-arm64-1.22.0.tgz`, verifies both the pinned archive
and extracted-dylib SHA256 values, and installs it into the engine runtime dir.

---

## Where the dylib must live (engine search order)

The engine (`src/ort_runtime.rs`) resolves the dylib in this order and pins
`ORT_DYLIB_PATH` to the first hit, before the first ML session:

1. `ORT_DYLIB_PATH` (if already set to an existing file) — explicit override.
2. **Beside the running `FileIDEngine` binary** (`current_exe()` dir).
3. **The engine runtime dir:** `<state-root>/runtime/libonnxruntime.dylib`, where
   `<state-root>` = `$XDG_DATA_HOME/FileID` or `~/.local/share/FileID`. This is
   where `fileid runtime install` and the shell script write.
4. `/opt/homebrew/lib/libonnxruntime.dylib` (Homebrew, Apple silicon).
5. `/usr/local/lib/libonnxruntime.dylib` (Intel-prefix Homebrew / manual).

If none resolve, a full-ML scan fails fast with a clear `runtime_not_installed`
error pointing at `fileid runtime install` — distinct from a missing-model error.
(ONNX Runtime placed on the dyld search path — `/usr/lib`, `DYLD_LIBRARY_PATH` —
also loads, but isn't auto-detected by the pre-flight.)

---

## Pinned artifact hashes (CLI/shell download)

The official macOS arm64 artifact hashes are pinned, but no download URL is
hardcoded. For an arm64 archive mirror, `fileid runtime install` and the shell
script apply them automatically, verifying the archive and then the extracted
dylib before installation. A different or bare artifact must provide
`FILEID_ORT_DYLIB_SHA256` explicitly.

| Constant (`platforms/cli/src/runtime.rs`) | Value |
|-------------------------------------------|-------|
| `PINNED_DYLIB_URL` | `None` (set `FILEID_ORT_DYLIB_URL` to a HuggingFace mirror) |
| `PINNED_DYLIB_SHA256` (the `.tgz`) | `cab6dcbd77e7ec775390e7b73a8939d45fec3379b017c7cb74f5b204c1a1cc07` |
| `PINNED_EXTRACTED_DYLIB_SHA256` (`lib/libonnxruntime.1.22.0.dylib` inside it) | `2b885992d3d6fa4130d39ec84a80d7504ff52750027c547bb22c86165f19406a` |

**Override (self-hosters):** point the installer anywhere with
`FILEID_ORT_DYLIB_URL` (+ `FILEID_ORT_DYLIB_SHA256` for a different artifact).
The URL may be a `.tgz`/`.tar.gz` or a bare `.dylib`; its host must be
HuggingFace-owned.

**No-network fallback:** `brew install onnxruntime`; the engine finds it via the
search order above.

### Creating or updating the HuggingFace mirror

Mirror the **same artifact** on huggingface.co and set
`FILEID_ORT_DYLIB_URL` to it. A bare-`.dylib` mirror also works when its SHA is
provided explicitly. To reproduce or update the pinned hashes:

```sh
ORT=1.22.0
curl -fL -o ort.tgz \
  "https://github.com/microsoft/onnxruntime/releases/download/v${ORT}/onnxruntime-osx-arm64-${ORT}.tgz"
shasum -a 256 ort.tgz                      # → PINNED_DYLIB_SHA256 (also the script's EXPECTED_SHA256)
tar -xzf ort.tgz
DY=$(find . -path '*/lib/libonnxruntime*.dylib' -not -path '*.dSYM/*' | head -1)
shasum -a 256 "$DY"                         # → PINNED_EXTRACTED_DYLIB_SHA256
```

(Exclude `*.dSYM/*`: the archive ships a debug bundle with a same-named DWARF file
that is **not** the loadable dylib — the CLI locator skips it for the same reason.)

---

## Windows / Linux

No action needed:

- **Windows** bundles `onnxruntime.dll` beside the engine and, for GPU vendors,
  pins `ORT_DYLIB_PATH` to a matched accelerator-pack runtime (`main.rs`).
- **Linux** statically links the CPU ONNX Runtime in portable release builds.
  Nix/AUR recipes may point `ort-sys` at a package-manager-provided shared
  library at build time instead.

`fileid runtime status` reports "provided by the platform" on these OSes, and
`fileid runtime install` is a no-op there.
